use std::io::Read;
use std::os::fd::IntoRawFd;
use std::sync::{Arc, Barrier};
use std::os::unix::net::UnixStream;

use rootcause::prelude::ResultExt;
use rootcause::report;

use crate::device;
use crate::device::NbdDevice;
use crate::NbdError;
use crate::Result;
use crate::proto::NbdDriverFlags;
use crate::proto::NbdRequest;

pub struct NbdServer {
    device: NbdDevice,
    server: UnixStream,
}

impl NbdServer {
    pub fn mount(device_index: usize, block_size: usize, block_count: usize) -> Result<Self> {
        device::ensure_modprobe_nbd()?;

        let (client, server) = UnixStream::pair()
            .map_err(NbdError::from)
            .attach("Failed to create NBD UnixStream pair")?;

        let mut device = NbdDevice::open(device_index)?;
        device.set_size(block_size, block_count)?;
        device.set_flags(NbdDriverFlags::default())?;
        device.set_sock(client.into_raw_fd())?;

        Ok(Self { device, server })
    }

    fn read_header_bytes(server: &mut UnixStream) -> Result<Option<[u8; 28]>> {
        let mut buf = [0u8; 28];

        match server.read_exact(&mut buf) {
            Ok(()) => Ok(Some(buf)),
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(err) => Err(report!(NbdError::from(err)).context(NbdError::Protocol)),
        }
    }

    pub fn run(self) -> Result<()> {
        let Self {
            mut device,
            mut server,
        } = self;
        let start_barrier = Arc::new(Barrier::new(2));
        let do_it_barrier = Arc::clone(&start_barrier);

        std::thread::spawn(move || {
            do_it_barrier.wait();
            device.do_it().unwrap();
        });
        start_barrier.wait();

        while let Some(bytes) = Self::read_header_bytes(&mut server)
            .attach("Failed to read from NBD server socket")?
        {
            let request = NbdRequest::from_bytes(&bytes)?;
            println!("Read: {:?}", request);
        }
        Ok(())
    }
}
