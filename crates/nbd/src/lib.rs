use std::process::ExitStatus;

use rootcause::Report;

mod kernel_device;
pub mod proto;
pub mod server;
pub mod session;

pub use kernel_device::ensure_modprobe_nbd;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NbdError {
    #[error("invalid device configuration: {0}")]
    InvalidDeviceConfiguration(String),
    #[error("failed to modprobe the nbd driver({}): {}", .status, .stderr)]
    ModprobeFailure { stderr: String, status: ExitStatus },
    #[error("NBD protocol error")]
    Protocol,
    #[error("background thread panicked")]
    BackgroundThreadPanic,
    #[error("std i/o error: {}", .0)]
    StdIO(#[from] std::io::Error),
    #[error("linux i/o error: {}", .0)]
    IO(#[from] rustix::io::Errno),
    #[error("tokio task join error: {}", .0)]
    TaskJoin(#[from] tokio::task::JoinError),
}

pub type Result<T> = std::result::Result<T, Report<NbdError>>;
