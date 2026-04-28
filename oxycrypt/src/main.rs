use std::path::Path;
use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;
use device_mem::MemoryDeviceBuilder;
use nbd::Result;
use tokio::signal;
use tracing::error;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Mount {
        #[arg(index = 1)]
        target:      PathBuf,
        #[arg(long, default_value_t = 4096)]
        block_size:  u32,
        #[arg(long, default_value_t = 16_384)]
        block_count: u64,
    },
    Show {
        #[arg(index = 1)]
        file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let args = Cli::parse();

    info!("Starting up");

    match args.command {
        Command::Mount {
            target,
            block_size,
            block_count,
        } => mount(&target, block_size, block_count).await,
        Command::Show { file: _ } => Ok(()),
    }
}

async fn mount(target: &Path, block_size: u32, block_count: u64) -> Result<()> {
    let device_index = parse_nbd_index(target)?;
    let built_device = MemoryDeviceBuilder::new()
        .block_size(block_size)
        .block_count(block_count)
        .build()
        .map_err(nbd::NbdError::from)?;

    let server = nbd::server::NbdServer::mount(device_index, built_device)?;
    let mut controller = server.run();

    info!(
        device_index,
        block_size, block_count, "Mounted memory-backed NBD device"
    );

    signal::ctrl_c().await.map_err(nbd::NbdError::from)?;
    info!("Received shutdown signal");

    controller.stop().map_err(|err| {
        nbd::NbdError::from(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            format!("failed to send stop command: {err}"),
        ))
    })?;

    let server_result = (&mut controller.handle).await.map_err(nbd::NbdError::from)?;
    match &server_result {
        Ok(()) => info!("Server task completed successfully"),
        Err(err) => error!(error = %err, "Server task completed with error"),
    }
    server_result
}

fn parse_nbd_index(target: &Path) -> Result<usize> {
    let file_name = target.file_name().and_then(|name| name.to_str()).ok_or_else(|| {
        nbd::NbdError::from(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("target path '{}' has no valid device name", target.display()),
        ))
    })?;

    let suffix = file_name.strip_prefix("nbd").ok_or_else(|| {
        nbd::NbdError::from(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("target '{}' is not an /dev/nbdN device path", target.display()),
        ))
    })?;

    suffix.parse::<usize>().map_err(|err| {
        nbd::NbdError::from(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid NBD device index in '{}': {err}", target.display()),
        ))
        .into()
    })
}
