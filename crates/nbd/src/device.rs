use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::path::Path;

use rootcause::prelude::ResultExt;
use rustix::fs::OFlags;
use rustix::ioctl::IntegerSetter;
use rustix::ioctl::NoArg;
use rustix::ioctl::Opcode;
use rustix::ioctl::ioctl;
use rustix::ioctl::opcode::none as _IO;
use rustix::path::Arg;

use crate::NbdError;
use crate::Result;
use crate::proto::NbdFlags;

const NBD_SET_SOCK: Opcode = _IO(0xab, 0);
const NBD_SET_BLKSIZE: Opcode = _IO(0xab, 1);
const NBD_DO_IT: Opcode = _IO(0xab, 3);
const NBD_CLEAR_SOCK: Opcode = _IO(0xab, 4);
const NBD_SET_SIZE_BLOCKS: Opcode = _IO(0xab, 7);
const NBD_DISCONNECT: Opcode = _IO(0xab, 8);
const NBD_SET_TIMEOUT: Opcode = _IO(0xab, 9);
const NBD_SET_FLAGS: Opcode = _IO(0xab, 10);

/*
ioctl constants defined in the linux header, but not used by us:

const NBD_CLEAR_QUE: Opcode = _IO(0xab, 5);
const NBD_PRINT_DEBUG: Opcode = _IO(0xab, 6);
const NBD_SET_SIZE: Opcode = _IO(0xab, 2);
*/

pub fn ensure_modprobe_nbd() -> Result<()> {
    if Path::new("/sys/block/nbd").exists() {
        return Ok(());
    }

    let out = std::process::Command::new("modprobe")
        .arg("nbd")
        .output()
        .map_err(NbdError::from)
        .attach("Failed to spawn 'modprobe' process")?;

    let status = out.status;
    if !status.success() {
        let stderr = out.stderr.to_string_lossy().into();
        Err(NbdError::ModprobeFailure { stderr, status }.into())
    } else {
        Ok(())
    }
}

pub struct NbdDevice {
    device_fd: OwnedFd,
    socket_fd: Option<OwnedFd>,
}

impl NbdDevice {
    pub fn open(device: usize) -> Result<Self> {
        // this is not very nice, but opening the device is done only once
        // at startup. even when iterating over multiple possible devices
        // this should be negligible
        let path = format!("/dev/nbd{}", device);
        let device_fd = rustix::fs::open(&path, OFlags::RDWR, 0.into())
            .map_err(NbdError::from)
            .attach("Failed to open NBD block device")?;

        Ok(Self {
            device_fd,
            socket_fd: None,
        })
    }

    // TODO: Precise errors
    pub fn set_size(&mut self, block_size: usize, block_count: usize) -> Result<()> {
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

    pub fn set_flags(&mut self, flags: NbdFlags) -> Result<()> {
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

    pub fn set_sock(&mut self, socket: RawFd) -> Result<()> {
        unsafe {
            ioctl(
                &self.device_fd,
                IntegerSetter::<NBD_SET_SOCK>::new_usize(socket.as_raw_fd() as usize),
            )
            .map_err(NbdError::from)
            .attach("NBD_SET_SOCK ioctl")?;
        }
        //self.socket_fd = Some(socket);
        Ok(())
    }

    pub fn set_timeout(&mut self, timeout_seconds: usize) -> Result<()> {
        unsafe {
            ioctl(
                &self.device_fd,
                IntegerSetter::<NBD_SET_TIMEOUT>::new_usize(timeout_seconds),
            )
            .map_err(NbdError::from)
            .attach("NBD_SET_TIMEOUT ioctl")?;
        }
        Ok(())
    }

    pub fn do_it(&mut self) -> Result<()> {
        unsafe {
            ioctl(&self.device_fd, NoArg::<NBD_DO_IT>::new())
                .map_err(NbdError::from)
                .attach("NBD_DO_IT ioctl")?;
        }
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
        // Drop the OwnedFd if it exists
        let _ = self.socket_fd.take();
        Ok(())
    }
    /*
    pub fn open_any() -> Result<Self> {
        let nbds_max: usize = std::fs::read_to_string("/sys/module/nbd/parameters/nbds_max")?.trim().parse().expect("");
    } */
}

// TODO: Maybe implement Drop?
