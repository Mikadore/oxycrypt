use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;

use rootcause::prelude::ResultExt;
use rootcause::report;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::info;
use tracing::trace;

use crate::NbdError;
use crate::Result;
use crate::device;
use crate::device::NbdDevice;
use crate::proto::NbdDriverFlags;
use crate::proto::NbdRequest;

#[derive(Debug, Clone, Copy)]
pub enum Command {
    Stop,
}

pub struct NbdServerController {
    pub commands: mpsc::UnboundedSender<Command>,
    pub handle:   JoinHandle<Result<()>>,
}

impl NbdServerController {
    pub fn stop(&self) -> std::result::Result<(), mpsc::error::SendError<Command>> {
        self.commands.send(Command::Stop)
    }
}

pub struct NbdServer {
    device:         NbdDevice,
    control_device: NbdDevice,
    server:         UnixStream,
}

impl NbdServer {
    pub fn mount(device_index: usize, block_size: usize, block_count: usize) -> Result<Self> {
        device::ensure_modprobe_nbd()?;

        let (client, server) = StdUnixStream::pair()
            .map_err(NbdError::from)
            .attach("Failed to create NBD UnixStream pair")?;
        server
            .set_nonblocking(true)
            .map_err(NbdError::from)
            .attach("Failed to set NBD server socket nonblocking")?;
        let server = UnixStream::from_std(server)
            .map_err(NbdError::from)
            .attach("Failed to convert NBD server socket to Tokio UnixStream")?;

        let mut device = NbdDevice::open(device_index)?;
        let control_device = NbdDevice::open(device_index)?;
        device.set_size(block_size, block_count)?;
        device.set_flags(NbdDriverFlags::default())?;
        device.set_sock(client.into_raw_fd())?;

        Ok(Self {
            device,
            control_device,
            server,
        })
    }

    async fn read_header_bytes(server: &mut UnixStream) -> Result<Option<[u8; 28]>> {
        let mut buf = [0u8; 28];

        match server.read_exact(&mut buf).await {
            Ok(_) => Ok(Some(buf)),
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(err) => Err(report!(NbdError::from(err)).context(NbdError::Protocol)),
        }
    }

    fn shutdown(mut control_device: NbdDevice, do_it_thread: thread::JoinHandle<Result<()>>) -> Result<()> {
        // Tear down the kernel-side NBD state before joining the DO_IT thread.
        // The ioctl can return before that thread has fully unwound, and joining
        // first can deadlock waiting on cleanup that NBD_CLEAR_SOCK must trigger.
        trace!("clearing NBD queue");
        let clear_queue_result = control_device.clear_queue();

        trace!("disconnecting NBD device");
        let disconnect_result = control_device.disconnect();

        trace!("clearing NBD socket");
        let clear_socket_result = control_device.clear_socket();

        trace!("joining NBD_DO_IT thread");
        let do_it_result = match do_it_thread.join() {
            Ok(result) => result,
            Err(_) => Err(report!(NbdError::BackgroundThreadPanic).attach("NBD_DO_IT thread panicked")),
        };

        if let Err(err) = do_it_result {
            return Err(err);
        }

        clear_queue_result?;
        disconnect_result?;
        clear_socket_result?;
        Ok(())
    }

    pub fn run(self) -> NbdServerController {
        let (commands, mut command_rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            let Self {
                device,
                control_device,
                mut server,
            } = self;
            let start_barrier = Arc::new(Barrier::new(2));
            let do_it_barrier = Arc::clone(&start_barrier);

            let do_it_thread = std::thread::spawn(move || {
                do_it_barrier.wait();
                let mut device = device;
                device.do_it()
            });
            start_barrier.wait();

            let loop_result = loop {
                tokio::select! {
                    biased;

                    command = command_rx.recv() => match command {
                        Some(Command::Stop) | None => {
                            trace!("shutdown command received by server loop");
                            break Ok(())
                        },
                    },
                    bytes = Self::read_header_bytes(&mut server) => {
                        let Some(bytes) = bytes
                            .attach("Failed to read from NBD server socket")?
                        else {
                            break Ok(());
                        };
                        let request = NbdRequest::from_bytes(&bytes)?;
                        info!("Read: {:?}", request);
                    }
                }
            };

            drop(server);

            let shutdown_result = Self::shutdown(control_device, do_it_thread);
            match loop_result {
                Ok(()) => shutdown_result,
                Err(err) => {
                    let _ = shutdown_result;
                    Err(err)
                }
            }
        });

        NbdServerController { commands, handle }
    }
}
