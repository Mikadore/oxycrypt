use std::{io::Read, os::unix::net::UnixStream};

use rootcause::prelude::ResultExt;

use crate::NbdError;
use crate::Result;

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

    pub fn run(&mut self) -> Result<()> {
        let mut buf = [0; 1024];
        loop {
            let n = self
                .server
                .read(&mut buf)
                .map_err(NbdError::from)
                .attach("Failed to read from NBD server socket")?;
            if n == 0 {
                break;
            }
            println!("Read: {:x?}", &buf[..n]);
        }
        Ok(())
    }
}
