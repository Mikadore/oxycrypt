use std::os::fd::IntoRawFd;
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

    if args.command.need_root() {
        sudo2::escalate_with_env().expect("Failed to relaunch with sudo");
    }

    info!("Starting up");

    match args.command {
        Command::Mount { file, target } => mount(&file, &target),
        Command::Show { file } => Ok(()),
    }
}

fn mount(file: &Path, target: &Path) -> Result<()> {
    nbd::device::ensure_modprobe_nbd()?;
    let (client, mut server) = nbd::server::NbdServer::new()?;
    let mut dev = nbd::device::NbdDevice::open(0)?;
    dev.set_size(1024, 1024)?;
    dev.set_flags(nbd::proto::NbdDriverFlags::default())?;
    dev.set_sock(client.into_raw_fd())?;
    std::thread::spawn(move || {
        dev.do_it().unwrap();
    });
    server.run()?;
    Ok(())
}