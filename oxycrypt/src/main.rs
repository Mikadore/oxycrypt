use std::path::Path;
use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;
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
        file:   PathBuf,
        #[arg(index = 2)]
        target: PathBuf,
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
        Command::Mount { file, target } => mount(&file, &target).await,
        Command::Show { file: _ } => Ok(()),
    }
}

async fn mount(_file: &Path, _target: &Path) -> Result<()> {
    let server = nbd::server::NbdServer::mount(0, 1024, 1024)?;
    let mut controller = server.run();

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
