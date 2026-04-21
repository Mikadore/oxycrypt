use std::path::Path;
use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

use nbd::Result;

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
        file: PathBuf,
        #[arg(index = 2)]
        target: PathBuf,
    },
    Show {
        #[arg(index = 1)]
        file: PathBuf,
    },
}

impl Command {
    fn need_root(&self) -> bool {
        match self {
            Self::Mount { .. } => true,
            Self::Show { .. } => false,
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let args = Cli::parse();
    
    info!("Starting up");

    match args.command {
        Command::Mount { file, target } => mount(&file, &target),
        Command::Show { file } => Ok(()),
    }
}

fn mount(file: &Path, target: &Path) -> Result<()> {
    let server = nbd::server::NbdServer::mount(0, 1024, 1024)?;
    server.run()?;
    Ok(())
}
