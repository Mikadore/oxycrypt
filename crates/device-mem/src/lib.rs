use std::io;
use std::sync::RwLock;

use block_device::BlockDevice;
use block_device::BuiltDevice;
use block_device::DeviceGeometry;
use block_device::DeviceInfo;
use block_device::Durability;

pub struct MemoryDeviceBuilder {
    geometry:     DeviceGeometry,
    initial_data: Option<Vec<u8>>,
    read_only:    bool,
}

impl Default for MemoryDeviceBuilder {
    fn default() -> Self {
        Self {
            geometry:     DeviceGeometry::new(4096, 16_384),
            initial_data: None,
            read_only:    false,
        }
    }
}

impl MemoryDeviceBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn block_size(mut self, block_size: u32) -> Self {
        self.geometry.block_size = block_size;
        self
    }

    pub fn block_count(mut self, block_count: u64) -> Self {
        self.geometry.block_count = block_count;
        self
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn initial_data(mut self, initial_data: Vec<u8>) -> Self {
        self.initial_data = Some(initial_data);
        self
    }

    pub fn build(self) -> io::Result<BuiltDevice<MemoryDevice>> {
        validate_geometry(self.geometry)?;

        let size_bytes = usize::try_from(self.geometry.size_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "memory device size exceeds addressable memory on this platform",
            )
        })?;

        let mut info = DeviceInfo::from_geometry(self.geometry);
        info.read_only = self.read_only;
        info.supports_flush = true;
        info.supports_fua = true;

        let data = match self.initial_data {
            Some(data) => {
                if data.len() != size_bytes {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "initial data length does not match configured device size",
                    ));
                }
                data
            }
            None => vec![0; size_bytes],
        };

        Ok(BuiltDevice::new(
            MemoryDevice {
                data: RwLock::new(data),
                info,
            },
            self.geometry,
        ))
    }
}

#[derive(Debug)]
pub struct MemoryDevice {
    data: RwLock<Vec<u8>>,
    info: DeviceInfo,
}

impl BlockDevice for MemoryDevice {
    fn info(&self) -> DeviceInfo {
        self.info
    }

    fn read_at(&self, offset: u64, len: u32) -> io::Result<Vec<u8>> {
        let start = usize::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset exceeds usize range"))?;
        let end = start
            .checked_add(len as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read range overflow"))?;

        let data = self
            .data
            .read()
            .map_err(|_| io::Error::other("memory device lock poisoned"))?;
        if end > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read range exceeds device size",
            ));
        }
        Ok(data[start..end].to_vec())
    }

    fn write_at(&self, offset: u64, bytes: &[u8], _durability: Durability) -> io::Result<()> {
        if self.info.read_only {
            return Err(io::Error::from_raw_os_error(rustix::io::Errno::ROFS.raw_os_error()));
        }

        let start = usize::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset exceeds usize range"))?;
        let end = start
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "write range overflow"))?;

        let mut data = self
            .data
            .write()
            .map_err(|_| io::Error::other("memory device lock poisoned"))?;
        if end > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write range exceeds device size",
            ));
        }
        data[start..end].copy_from_slice(bytes);
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

fn validate_geometry(geometry: DeviceGeometry) -> io::Result<()> {
    if geometry.block_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block size must be greater than zero",
        ));
    }

    if geometry.block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block count must be greater than zero",
        ));
    }

    geometry
        .checked_size_bytes()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "device size overflow"))?;

    Ok(())
}
