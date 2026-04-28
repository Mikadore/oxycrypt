use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;

use block_device::BlockDevice;
use block_device::BuiltDevice;
use block_device::DeviceGeometry;
use rootcause::prelude::ResultExt;
use rootcause::report;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::trace;

use crate::NbdError;
use crate::Result;
use crate::kernel_device;
use crate::kernel_device::NbdKernelDevice;
use crate::proto::driver_flags_from_device_info;
use crate::session::NbdSession;

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

pub struct NbdServer<D> {
    device:         NbdKernelDevice,
    control_device: NbdKernelDevice,
    session:        NbdSession<D, UnixStream>,
}

impl<D> NbdServer<D>
where
    D: BlockDevice,
{
    pub fn mount(device_index: usize, built_device: BuiltDevice<D>) -> Result<Self> {
        kernel_device::ensure_modprobe_nbd()?;

        let geometry = built_device.geometry();
        let (backend, block_size, block_count) = built_device.into_parts();
        let backend = Arc::new(backend);
        validate_geometry(geometry, backend.info())?;

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

        let mut device = NbdKernelDevice::open(device_index)?;
        let control_device = NbdKernelDevice::open(device_index)?;
        device.set_size(block_size, block_count)?;
        device.set_flags(driver_flags_from_device_info(backend.info()))?;
        let client: OwnedFd = client.into();
        device.set_sock(client)?;

        Ok(Self {
            device,
            control_device,
            session: NbdSession::new(server, backend),
        })
    }

    fn shutdown(mut control_device: NbdKernelDevice, do_it_thread: thread::JoinHandle<Result<()>>) -> Result<()> {
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
                session,
            } = self;
            let start_barrier = Arc::new(Barrier::new(2));
            let do_it_barrier = Arc::clone(&start_barrier);

            let do_it_thread = std::thread::spawn(move || {
                do_it_barrier.wait();
                let mut device = device;
                device.do_it()
            });
            start_barrier.wait();

            let session_result = session
                .run_until(async move {
                    match command_rx.recv().await {
                        Some(Command::Stop) | None => {
                            trace!("shutdown command received by server loop");
                        }
                    }
                })
                .await;

            let shutdown_result = Self::shutdown(control_device, do_it_thread);
            match session_result {
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

fn validate_geometry(geometry: DeviceGeometry, info: block_device::DeviceInfo) -> Result<()> {
    if geometry.block_size == 0 {
        return Err(report!(NbdError::InvalidDeviceConfiguration(
            "block size must be greater than zero".into()
        )));
    }

    if geometry.block_count == 0 {
        return Err(report!(NbdError::InvalidDeviceConfiguration(
            "block count must be greater than zero".into()
        )));
    }

    if info.block_size != geometry.block_size {
        return Err(report!(NbdError::InvalidDeviceConfiguration(
            "device info block size does not match builder geometry".into()
        )));
    }

    if info.size_bytes != geometry.size_bytes() {
        return Err(report!(NbdError::InvalidDeviceConfiguration(
            "device info size does not match builder geometry".into()
        )));
    }

    Ok(())
}
