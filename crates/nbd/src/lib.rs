use std::process::ExitStatus;

use rootcause::Report;

mod device;
pub mod proto;
pub mod server;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NbdError {
    #[error("failed to modprobe the nbd driver({}): {}", .status, .stderr)]
    ModprobeFailure { stderr: String, status: ExitStatus },
    #[error("NBD protocol error")]
    Protocol,
    #[error("std i/o error: {}", .0)]
    StdIO(#[from] std::io::Error),
    #[error("linux i/o error: {}", .0)]
    IO(#[from] rustix::io::Errno),
}

pub type Result<T> = std::result::Result<T, Report<NbdError>>;
