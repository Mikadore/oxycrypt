use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::path::Path;

use rootcause::prelude::ResultExt;
use rootcause::report;
use rustix::fs::OFlags;
use rustix::ioctl::IntegerSetter;
use rustix::ioctl::NoArg;
use rustix::ioctl::Opcode;
use rustix::ioctl::ioctl;
use rustix::ioctl::opcode::none as _IO;
use tracing::debug;
use tracing::info;
use tracing::trace;

use crate::NbdError;
use crate::Result;
use crate::proto::NbdDriverFlags;

const NBD_SET_SOCK: Opcode = _IO(0xab, 0);
const NBD_SET_BLKSIZE: Opcode = _IO(0xab, 1);
const NBD_DO_IT: Opcode = _IO(0xab, 3);
const NBD_CLEAR_SOCK: Opcode = _IO(0xab, 4);
const NBD_CLEAR_QUE: Opcode = _IO(0xab, 5);
const NBD_SET_SIZE_BLOCKS: Opcode = _IO(0xab, 7);
const NBD_DISCONNECT: Opcode = _IO(0xab, 8);
const NBD_SET_FLAGS: Opcode = _IO(0xab, 10);

pub fn ensure_modprobe_nbd() -> Result<()> {
    if Path::new("/sys/block/nbd").exists() {
        debug!("NBD kernel module already available");
        return Ok(());
    }

    info!("Loading NBD kernel module with modprobe");
    let out = std::process::Command::new("modprobe")
        .arg("nbd")
        .output()
        .map_err(NbdError::from)
        .attach("Failed to spawn 'modprobe' process")?;

    let status = out.status;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        Err(NbdError::ModprobeFailure { stderr, status }.into())
    } else {
        info!("Loaded NBD kernel module");
        Ok(())
    }
}

pub struct NbdKernelDevice {
    device_fd: OwnedFd,
    socket_fd: Option<OwnedFd>,
}

impl NbdKernelDevice {
    pub fn open(device: usize) -> Result<Self> {
        let path = format!("/dev/nbd{}", device);
        debug!(device, path, "Opening NBD block device");
        let device_fd = rustix::fs::open(&path, OFlags::RDWR, 0.into())
            .map_err(NbdError::from)
            .attach("Failed to open NBD block device")?;

        Ok(Self {
            device_fd,
            socket_fd: None,
        })
    }

    pub fn set_size(&mut self, block_size: u32, block_count: u64) -> Result<()> {
        let block_size = usize::try_from(block_size)
            .map_err(|_| report!(NbdError::InvalidDeviceConfiguration("block size exceeds usize".into())))?;
        let block_count = usize::try_from(block_count)
            .map_err(|_| report!(NbdError::InvalidDeviceConfiguration("block count exceeds usize".into())))?;

        debug!(block_size, block_count, "Configuring NBD device size");
        unsafe {
            ioctl(&self.device_fd, IntegerSetter::<NBD_SET_BLKSIZE>::new_usize(block_size))
                .map_err(NbdError::from)
                .attach("NBD_SET_BLKSIZE ioctl")?;
            ioctl(
                &self.device_fd,
                IntegerSetter::<NBD_SET_SIZE_BLOCKS>::new_usize(block_count),
            )
            .map_err(NbdError::from)
            .attach("NBD_SET_SIZE_BLOCKS ioctl")?;
        }
        Ok(())
    }

    pub fn set_flags(&mut self, flags: NbdDriverFlags) -> Result<()> {
        debug!(flags = flags.bits(), "Configuring NBD device flags");
        unsafe {
            ioctl(
                &self.device_fd,
                IntegerSetter::<NBD_SET_FLAGS>::new_usize(flags.bits() as usize),
            )
            .map_err(NbdError::from)
            .attach("NBD_SET_FLAGS ioctl")?;
        }
        Ok(())
    }

    pub fn set_sock(&mut self, socket: OwnedFd) -> Result<()> {
        debug!(fd = socket.as_raw_fd(), "Attaching NBD socket to device");
        unsafe {
            ioctl(
                &self.device_fd,
                IntegerSetter::<NBD_SET_SOCK>::new_usize(socket.as_raw_fd() as usize),
            )
            .map_err(NbdError::from)
            .attach("NBD_SET_SOCK ioctl")?;
        }
        self.socket_fd = Some(socket);
        Ok(())
    }

    pub fn do_it(&mut self) -> Result<()> {
        trace!("Entering blocking NBD_DO_IT ioctl");
        unsafe {
            ioctl(&self.device_fd, NoArg::<NBD_DO_IT>::new())
                .map_err(NbdError::from)
                .attach("NBD_DO_IT ioctl")?;
        }
        trace!("NBD_DO_IT ioctl returned");
        Ok(())
    }

    pub fn disconnect(&mut self) -> Result<()> {
        unsafe {
            ioctl(&self.device_fd, NoArg::<NBD_DISCONNECT>::new())
                .map_err(NbdError::from)
                .attach("NBD_DISCONNECT ioctl")?;
        }
        Ok(())
    }

    pub fn clear_socket(&mut self) -> Result<()> {
        unsafe {
            ioctl(&self.device_fd, NoArg::<NBD_CLEAR_SOCK>::new())
                .map_err(NbdError::from)
                .attach("NBD_CLEAR_SOCK ioctl")?;
        }
        let _ = self.socket_fd.take();
        Ok(())
    }

    pub fn clear_queue(&mut self) -> Result<()> {
        unsafe {
            ioctl(&self.device_fd, NoArg::<NBD_CLEAR_QUE>::new())
                .map_err(NbdError::from)
                .attach("NBD_CLEAR_QUE ioctl")?;
        }
        Ok(())
    }
}
