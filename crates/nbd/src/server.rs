use std::io::Read;
use std::os::unix::net::UnixStream;

use rootcause::prelude::ResultExt;
use rootcause::report;

use crate::NbdError;
use crate::Result;
use crate::proto::NbdRequest;

pub struct NbdServer {
    server: UnixStream,
}

impl NbdServer {
    pub fn new() -> Result<(UnixStream, Self)> {
        let (client, server) = UnixStream::pair()
            .map_err(NbdError::from)
            .attach("Failed to create NBD UnixStream pair")?;
        Ok((client, Self { server }))
    }

    fn read_header_bytes(&mut self) -> Result<Option<[u8; 28]>> {
        let mut buf = [0u8; 28];

        match self.server.read_exact(&mut buf) {
            Ok(()) => Ok(Some(buf)),
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(err) => Err(report!(NbdError::from(err)).context(NbdError::Protocol)),
        }
    }

    pub fn run(&mut self) -> Result<()> {
        //let mut buf = [0; 1024];
        while let Some(bytes) = self
            .read_header_bytes()
            .attach("Failed to read from NBD server socket")?
        {
            let request = NbdRequest::from_bytes(&bytes)?;
            println!("Read: {:?}", request);
        }
        Ok(())
    }
}
