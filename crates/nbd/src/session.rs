use std::collections::BTreeSet;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;

use block_device::BlockDevice;
use block_device::DeviceInfo;
use block_device::Durability;
use rootcause::prelude::ResultExt;
use rootcause::report;
use rustix::io::Errno;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::trace;

use crate::NbdError;
use crate::Result;
use crate::proto::NbdCommandType;
use crate::proto::NbdRequest;
use crate::proto::SimpleReply;

const DEFAULT_MAX_IN_FLIGHT: usize = 16;

pub struct NbdSession<D, S> {
    device:        Arc<D>,
    info:          DeviceInfo,
    stream:        S,
    max_in_flight: usize,
}

impl<D, S> NbdSession<D, S>
where
    D: BlockDevice,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(stream: S, device: Arc<D>) -> Self {
        let info = device.info();
        Self {
            device,
            info,
            stream,
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
        }
    }

    pub fn with_max_in_flight(mut self, max_in_flight: usize) -> Self {
        self.max_in_flight = max_in_flight.max(1);
        self
    }

    pub async fn run(self) -> Result<()> {
        self.run_until(std::future::pending::<()>()).await
    }

    pub async fn run_until<F>(self, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        let Self {
            device,
            info,
            stream,
            max_in_flight,
        } = self;
        let semaphore = Arc::new(Semaphore::new(max_in_flight.max(1)));
        let tracker = Arc::new(MutationTracker::default());
        let (mut reader, writer) = tokio::io::split(stream);
        let (reply_tx, reply_rx) = mpsc::channel(max_in_flight.max(1));
        let writer_task = tokio::spawn(Self::write_replies(writer, reply_rx));
        let mut workers = JoinSet::new();
        let mut shutdown = std::pin::pin!(shutdown);

        let read_result = loop {
            tokio::select! {
                _ = &mut shutdown => {
                    trace!("session shutdown requested");
                    break Ok(());
                }
                request = Self::read_request(&mut reader, Arc::clone(&semaphore)) => {
                    let Some((request, payload, permit)) = request
                        .attach("Failed to read request from NBD stream")?
                    else {
                        break Ok(());
                    };

                    if matches!(request.typ, NbdCommandType::Disc) {
                        trace!("client requested disconnect");
                        drop(permit);
                        break Ok(());
                    }

                    if let Some(errno) = validate_request(info, &request) {
                        Self::send_reply(&reply_tx, SimpleReply::from_errno(request.cookie, errno), permit).await?;
                        continue;
                    }

                    Self::spawn_request_worker(
                        &mut workers,
                        Arc::clone(&device),
                        Arc::clone(&tracker),
                        request,
                        payload,
                        reply_tx.clone(),
                        permit,
                    );
                }
            }
        };

        drop(reply_tx);

        while let Some(result) = workers.join_next().await {
            let result = result.map_err(NbdError::from)?;
            result?;
        }

        let writer_result = writer_task.await.map_err(NbdError::from)?;
        match read_result {
            Ok(()) => writer_result,
            Err(err) => {
                let _ = writer_result;
                Err(err)
            }
        }
    }

    fn spawn_request_worker(
        workers: &mut JoinSet<Result<()>>,
        device: Arc<D>,
        tracker: Arc<MutationTracker>,
        request: NbdRequest,
        payload: Vec<u8>,
        reply_tx: mpsc::Sender<ReplyFrame>,
        permit: OwnedSemaphorePermit,
    ) {
        workers.spawn(async move {
            let reply = Self::execute_request(device, tracker, request, payload).await?;
            match reply {
                Some(reply) => Self::send_reply(&reply_tx, reply, permit).await,
                None => {
                    drop(permit);
                    Ok(())
                }
            }
        });
    }

    async fn execute_request(
        device: Arc<D>,
        tracker: Arc<MutationTracker>,
        request: NbdRequest,
        payload: Vec<u8>,
    ) -> Result<Option<SimpleReply>> {
        match request.typ {
            NbdCommandType::Read => {
                let cookie = request.cookie;
                let offset = request.offset;
                let length = request.length;
                let result = tokio::task::spawn_blocking(move || device.read_at(offset, length))
                    .await
                    .map_err(NbdError::from)?;

                Ok(Some(match result {
                    Ok(data) => SimpleReply::with_data(cookie, data),
                    Err(err) => SimpleReply::from_io_error(cookie, &err),
                }))
            }
            NbdCommandType::Write => {
                let sequence = tracker.register_event();
                let cookie = request.cookie;
                let offset = request.offset;
                let durability = if request.flag_fua {
                    Durability::Durable
                } else {
                    Durability::Buffered
                };
                let result = tokio::task::spawn_blocking(move || device.write_at(offset, &payload, durability)).await;
                tracker.mark_complete(sequence);

                let result = result.map_err(NbdError::from)?;
                Ok(Some(match result {
                    Ok(()) => SimpleReply::ok(cookie),
                    Err(err) => SimpleReply::from_io_error(cookie, &err),
                }))
            }
            NbdCommandType::Flush => {
                let sequence = tracker.register_event();
                tracker.wait_for(sequence.saturating_sub(1)).await;

                let cookie = request.cookie;
                let result = tokio::task::spawn_blocking(move || device.flush()).await;
                tracker.mark_complete(sequence);

                let result = result.map_err(NbdError::from)?;
                Ok(Some(match result {
                    Ok(()) => SimpleReply::ok(cookie),
                    Err(err) => SimpleReply::from_io_error(cookie, &err),
                }))
            }
            NbdCommandType::Trim => {
                let sequence = tracker.register_event();
                let cookie = request.cookie;
                let offset = request.offset;
                let length = request.length;
                let result = tokio::task::spawn_blocking(move || device.trim(offset, length)).await;
                tracker.mark_complete(sequence);

                let result = result.map_err(NbdError::from)?;
                Ok(Some(match result {
                    Ok(()) => SimpleReply::ok(cookie),
                    Err(err) => SimpleReply::from_io_error(cookie, &err),
                }))
            }
            NbdCommandType::WriteZeroes => {
                let sequence = tracker.register_event();
                let cookie = request.cookie;
                let offset = request.offset;
                let length = request.length;
                let no_hole = request.flag_no_hole;
                let durability = if request.flag_fua {
                    Durability::Durable
                } else {
                    Durability::Buffered
                };
                let result =
                    tokio::task::spawn_blocking(move || device.write_zeroes(offset, length, no_hole, durability)).await;
                tracker.mark_complete(sequence);

                let result = result.map_err(NbdError::from)?;
                Ok(Some(match result {
                    Ok(()) => SimpleReply::ok(cookie),
                    Err(err) => SimpleReply::from_io_error(cookie, &err),
                }))
            }
            NbdCommandType::Disc => Ok(None),
        }
    }

    async fn read_request<R>(
        reader: &mut R,
        semaphore: Arc<Semaphore>,
    ) -> Result<Option<(NbdRequest, Vec<u8>, OwnedSemaphorePermit)>>
    where
        R: AsyncRead + Unpin,
    {
        let mut header = [0u8; NbdRequest::HEADER_LEN];
        match reader.read_exact(&mut header[0..1]).await {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => {
                return Err(report!(NbdError::from(err))
                    .context(NbdError::Protocol)
                    .attach("failed to read the first byte of an NBD request"));
            }
        }

        reader
            .read_exact(&mut header[1..])
            .await
            .map_err(NbdError::from)
            .context(NbdError::Protocol)
            .attach("truncated NBD request header")?;
        let request = NbdRequest::from_bytes(header)?;
        let permit = semaphore.acquire_owned().await.map_err(|_| {
            report!(NbdError::from(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "request limiter was closed unexpectedly",
            )))
        })?;

        let payload = if request.expects_write_payload() {
            let payload_len = usize::try_from(request.length)
                .map_err(|_| report!(NbdError::Protocol).attach("request length does not fit in usize"))?;
            let mut payload = vec![0; payload_len];
            reader
                .read_exact(&mut payload)
                .await
                .map_err(NbdError::from)
                .context(NbdError::Protocol)
                .attach("truncated NBD write payload")?;
            payload
        } else {
            Vec::new()
        };

        Ok(Some((request, payload, permit)))
    }

    async fn send_reply(
        reply_tx: &mpsc::Sender<ReplyFrame>,
        reply: SimpleReply,
        permit: OwnedSemaphorePermit,
    ) -> Result<()> {
        reply_tx
            .send(ReplyFrame {
                bytes:   reply.into_bytes(),
                _permit: permit,
            })
            .await
            .map_err(|_| {
                report!(NbdError::from(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "reply writer closed unexpectedly",
                )))
            })?;
        Ok(())
    }

    async fn write_replies<W>(mut writer: W, mut reply_rx: mpsc::Receiver<ReplyFrame>) -> Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        while let Some(reply) = reply_rx.recv().await {
            writer
                .write_all(&reply.bytes)
                .await
                .map_err(NbdError::from)
                .attach("Failed to write NBD reply")?;
        }

        writer
            .shutdown()
            .await
            .map_err(NbdError::from)
            .attach("Failed to shutdown NBD reply stream")?;
        Ok(())
    }
}

struct ReplyFrame {
    bytes:   Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

#[derive(Default)]
struct MutationTracker {
    state:  Mutex<MutationState>,
    notify: Notify,
}

#[derive(Default)]
struct MutationState {
    next_sequence:          u64,
    completed_contiguous:   u64,
    completed_out_of_order: BTreeSet<u64>,
}

impl MutationTracker {
    fn register_event(&self) -> u64 {
        let mut state = self.state.lock().expect("mutation tracker lock poisoned");
        state.next_sequence += 1;
        state.next_sequence
    }

    fn mark_complete(&self, sequence: u64) {
        let mut state = self.state.lock().expect("mutation tracker lock poisoned");
        if sequence <= state.completed_contiguous {
            return;
        }

        if sequence == state.completed_contiguous + 1 {
            state.completed_contiguous = sequence;
            loop {
                let next = state.completed_contiguous + 1;
                if !state.completed_out_of_order.remove(&next) {
                    break;
                }
                state.completed_contiguous += 1;
            }
        } else {
            state.completed_out_of_order.insert(sequence);
        }

        self.notify.notify_waiters();
    }

    async fn wait_for(&self, target: u64) {
        if target == 0 {
            return;
        }

        loop {
            let notified = self.notify.notified();
            if self
                .state
                .lock()
                .expect("mutation tracker lock poisoned")
                .completed_contiguous
                >= target
            {
                return;
            }
            notified.await;
        }
    }
}

fn validate_request(info: DeviceInfo, request: &NbdRequest) -> Option<Errno> {
    match request.typ {
        NbdCommandType::Read => {
            if request.flag_fua || request.flag_no_hole {
                return Some(Errno::INVAL);
            }
            validate_range(info, request.offset, request.length)
        }
        NbdCommandType::Write => {
            if request.flag_no_hole {
                return Some(Errno::INVAL);
            }
            if info.read_only {
                return Some(Errno::ROFS);
            }
            if request.flag_fua && !info.supports_fua {
                return Some(Errno::OPNOTSUPP);
            }
            validate_range(info, request.offset, request.length)
        }
        NbdCommandType::Flush => {
            if request.flag_fua || request.flag_no_hole {
                return Some(Errno::INVAL);
            }
            if !info.supports_flush {
                return Some(Errno::OPNOTSUPP);
            }
            None
        }
        NbdCommandType::Trim => {
            if request.flag_fua || request.flag_no_hole {
                return Some(Errno::INVAL);
            }
            if info.read_only {
                return Some(Errno::ROFS);
            }
            if !info.supports_trim {
                return Some(Errno::OPNOTSUPP);
            }
            validate_range(info, request.offset, request.length)
        }
        NbdCommandType::WriteZeroes => {
            if info.read_only {
                return Some(Errno::ROFS);
            }
            if !info.supports_write_zeroes {
                return Some(Errno::OPNOTSUPP);
            }
            if request.flag_fua && !info.supports_fua {
                return Some(Errno::OPNOTSUPP);
            }
            validate_range(info, request.offset, request.length)
        }
        NbdCommandType::Disc => None,
    }
}

fn validate_range(info: DeviceInfo, offset: u64, length: u32) -> Option<Errno> {
    let Some(end) = offset.checked_add(u64::from(length)) else {
        return Some(Errno::INVAL);
    };

    if end > info.size_bytes {
        Some(Errno::INVAL)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::sync::Condvar;
    use std::time::Duration;

    use block_device::DeviceGeometry;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::NbdCommandType;
    use super::NbdRequest;
    use super::NbdSession;
    use super::validate_request;
    use super::*;
    use crate::proto::NbdCommandFlag;

    #[test]
    fn rejects_out_of_bounds_requests() {
        let info = test_info();
        let request = NbdRequest {
            typ:          NbdCommandType::Write,
            flag_fua:     false,
            flag_no_hole: false,
            cookie:       1,
            offset:       info.size_bytes - 256,
            length:       512,
        };

        assert_eq!(validate_request(info, &request), Some(Errno::INVAL));
    }

    #[tokio::test]
    async fn replies_can_be_written_out_of_order() {
        let device = Arc::new(FakeDevice::new(test_info()));
        let gate = device.block_write(0);
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let session = NbdSession::new(server, Arc::clone(&device));
        let handle = tokio::spawn(session.run());

        send_write(&mut client, 1, 0, &[1, 2, 3, 4], false).await;
        send_write(&mut client, 2, 512, &[5, 6, 7, 8], false).await;

        let first = read_reply(&mut client).await;
        assert_eq!(first.cookie, 2);
        assert_eq!(first.error, 0);

        gate.open();

        let second = read_reply(&mut client).await;
        assert_eq!(second.cookie, 1);
        assert_eq!(second.error, 0);

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn flush_waits_for_earlier_mutations() {
        let device = Arc::new(FakeDevice::new(test_info()));
        let gate = device.block_write(0);
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let session = NbdSession::new(server, Arc::clone(&device));
        let handle = tokio::spawn(session.run());

        send_write(&mut client, 1, 0, &[1, 2, 3, 4], false).await;
        send_flush(&mut client, 2).await;

        assert!(
            timeout(Duration::from_millis(50), read_reply(&mut client))
                .await
                .is_err()
        );

        gate.open();

        let first = read_reply(&mut client).await;
        let second = read_reply(&mut client).await;
        assert_eq!(first.error, 0);
        assert_eq!(second.error, 0);
        assert!(matches!((first.cookie, second.cookie), (1, 2) | (2, 1)));

        drop(client);
        handle.await.expect("session join").expect("session should exit");
    }

    #[tokio::test]
    async fn fua_replies_only_after_durable_write_finishes() {
        let device = Arc::new(FakeDevice::new(test_info()));
        let gate = device.block_write(0);
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let session = NbdSession::new(server, Arc::clone(&device));
        let handle = tokio::spawn(session.run());

        send_write(&mut client, 7, 0, &[9, 9, 9, 9], true).await;
        assert!(
            timeout(Duration::from_millis(50), read_reply(&mut client))
                .await
                .is_err()
        );

        gate.open();
        let reply = read_reply(&mut client).await;
        assert_eq!(reply.cookie, 7);
        assert_eq!(reply.error, 0);

        drop(client);
        handle.await.expect("session join").expect("session should exit");
    }

    #[tokio::test]
    async fn shutdown_waits_for_in_flight_work_and_preserves_replies() {
        let device = Arc::new(FakeDevice::new(test_info()));
        let gate = device.block_write(0);
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let session = NbdSession::new(server, Arc::clone(&device));
        let mut handle = tokio::spawn(session.run_until(async move {
            let _ = shutdown_rx.await;
        }));

        send_write(&mut client, 11, 0, &[4, 3, 2, 1], false).await;
        device.wait_for_write_start(0).await;
        shutdown_tx.send(()).expect("shutdown signal should be sent");

        assert!(timeout(Duration::from_millis(50), &mut handle).await.is_err());

        gate.open();
        let reply = read_reply(&mut client).await;
        assert_eq!(reply.cookie, 11);
        assert_eq!(reply.error, 0);

        drop(client);
        handle.await.expect("session join").expect("session should exit");
    }

    fn test_info() -> DeviceInfo {
        let mut info = DeviceInfo::from_geometry(DeviceGeometry::new(512, 16));
        info.supports_flush = true;
        info.supports_fua = true;
        info
    }

    #[derive(Clone, Default)]
    struct Gate {
        state: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Gate {
        fn closed() -> Self {
            Self::default()
        }

        fn wait(&self) {
            let (lock, cvar) = &*self.state;
            let mut open = lock.lock().expect("gate lock poisoned");
            while !*open {
                open = cvar.wait(open).expect("gate wait poisoned");
            }
        }

        fn open(&self) {
            let (lock, cvar) = &*self.state;
            let mut open = lock.lock().expect("gate lock poisoned");
            *open = true;
            cvar.notify_all();
        }
    }

    struct FakeDevice {
        data:             Mutex<Vec<u8>>,
        info:             DeviceInfo,
        write_gates:      Mutex<HashMap<u64, Gate>>,
        started_writes:   Mutex<HashSet<u64>>,
        started_notifier: Notify,
    }

    impl FakeDevice {
        fn new(info: DeviceInfo) -> Self {
            Self {
                data: Mutex::new(vec![0; info.size_bytes as usize]),
                info,
                write_gates: Mutex::new(HashMap::new()),
                started_writes: Mutex::new(HashSet::new()),
                started_notifier: Notify::new(),
            }
        }

        fn block_write(&self, offset: u64) -> Gate {
            let gate = Gate::closed();
            self.write_gates
                .lock()
                .expect("write gate lock poisoned")
                .insert(offset, gate.clone());
            gate
        }

        async fn wait_for_write_start(&self, offset: u64) {
            loop {
                let notified = self.started_notifier.notified();
                if self
                    .started_writes
                    .lock()
                    .expect("started writes lock poisoned")
                    .contains(&offset)
                {
                    return;
                }
                notified.await;
            }
        }
    }

    impl BlockDevice for FakeDevice {
        fn info(&self) -> DeviceInfo {
            self.info
        }

        fn read_at(&self, offset: u64, len: u32) -> io::Result<Vec<u8>> {
            let start =
                usize::try_from(offset).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid offset"))?;
            let end = start
                .checked_add(len as usize)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range overflow"))?;
            let data = self.data.lock().expect("device data lock poisoned");
            Ok(data[start..end].to_vec())
        }

        fn write_at(&self, offset: u64, bytes: &[u8], _durability: Durability) -> io::Result<()> {
            self.started_writes
                .lock()
                .expect("started writes lock poisoned")
                .insert(offset);
            self.started_notifier.notify_waiters();

            let gate = self
                .write_gates
                .lock()
                .expect("write gate lock poisoned")
                .remove(&offset);
            if let Some(gate) = gate {
                gate.wait();
            }

            let start =
                usize::try_from(offset).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid offset"))?;
            let end = start
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range overflow"))?;
            let mut data = self.data.lock().expect("device data lock poisoned");
            data[start..end].copy_from_slice(bytes);
            Ok(())
        }

        fn flush(&self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ParsedReply {
        cookie: u64,
        error:  u32,
    }

    async fn send_write<S>(stream: &mut S, cookie: u64, offset: u64, data: &[u8], fua: bool)
    where
        S: AsyncWrite + Unpin,
    {
        let mut flags = 0u16;
        if fua {
            flags |= NbdCommandFlag::Fua as u16;
        }

        send_request_header(
            stream,
            flags,
            NbdCommandType::Write,
            cookie,
            offset,
            u32::try_from(data.len()).expect("payload too large"),
        )
        .await;
        stream.write_all(data).await.expect("write payload should succeed");
    }

    async fn send_flush<S>(stream: &mut S, cookie: u64)
    where
        S: AsyncWrite + Unpin,
    {
        send_request_header(stream, 0, NbdCommandType::Flush, cookie, 0, 0).await;
    }

    async fn send_request_header<S>(
        stream: &mut S,
        flags: u16,
        command: NbdCommandType,
        cookie: u64,
        offset: u64,
        length: u32,
    ) where
        S: AsyncWrite + Unpin,
    {
        let mut bytes = [0u8; NbdRequest::HEADER_LEN];
        bytes[0..4].copy_from_slice(&super::super::proto::NBD_REQUEST_MAGIC.to_be_bytes());
        bytes[4..6].copy_from_slice(&flags.to_be_bytes());
        bytes[6..8].copy_from_slice(&(command as u16).to_be_bytes());
        bytes[8..16].copy_from_slice(&cookie.to_be_bytes());
        bytes[16..24].copy_from_slice(&offset.to_be_bytes());
        bytes[24..28].copy_from_slice(&length.to_be_bytes());

        stream
            .write_all(&bytes)
            .await
            .expect("request header should be written");
    }

    async fn read_reply<S>(stream: &mut S) -> ParsedReply
    where
        S: AsyncRead + Unpin,
    {
        let mut header = [0u8; 16];
        stream
            .read_exact(&mut header)
            .await
            .expect("reply header should be readable");

        let magic = u32::from_be_bytes(header[0..4].try_into().expect("slice size"));
        assert_eq!(magic, super::super::proto::NBD_SIMPLE_REPLY_MAGIC);

        let error = u32::from_be_bytes(header[4..8].try_into().expect("slice size"));
        let cookie = u64::from_be_bytes(header[8..16].try_into().expect("slice size"));

        ParsedReply { cookie, error }
    }
}
