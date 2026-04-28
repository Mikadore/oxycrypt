use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceGeometry {
    pub block_size:  u32,
    pub block_count: u64,
}

impl DeviceGeometry {
    pub fn new(block_size: u32, block_count: u64) -> Self {
        Self {
            block_size,
            block_count,
        }
    }

    pub fn checked_size_bytes(self) -> Option<u64> {
        u64::from(self.block_size).checked_mul(self.block_count)
    }

    pub fn size_bytes(self) -> u64 {
        self.checked_size_bytes().expect("device size overflowed u64")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub size_bytes:            u64,
    pub block_size:            u32,
    pub read_only:             bool,
    pub supports_flush:        bool,
    pub supports_fua:          bool,
    pub supports_trim:         bool,
    pub supports_write_zeroes: bool,
    pub can_multi_conn:        bool,
}

impl DeviceInfo {
    pub fn from_geometry(geometry: DeviceGeometry) -> Self {
        Self {
            size_bytes:            geometry.size_bytes(),
            block_size:            geometry.block_size,
            read_only:             false,
            supports_flush:        false,
            supports_fua:          false,
            supports_trim:         false,
            supports_write_zeroes: false,
            can_multi_conn:        false,
        }
    }

    pub fn geometry(self) -> DeviceGeometry {
        DeviceGeometry {
            block_size:  self.block_size,
            block_count: self.size_bytes / u64::from(self.block_size),
        }
    }
}

#[derive(Debug)]
pub struct BuiltDevice<D> {
    pub device:      D,
    pub block_size:  u32,
    pub block_count: u64,
}

impl<D> BuiltDevice<D> {
    pub fn new(device: D, geometry: DeviceGeometry) -> Self {
        Self {
            device,
            block_size: geometry.block_size,
            block_count: geometry.block_count,
        }
    }

    pub fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry {
            block_size:  self.block_size,
            block_count: self.block_count,
        }
    }

    pub fn into_parts(self) -> (D, u32, u64) {
        (self.device, self.block_size, self.block_count)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    Buffered,
    Durable,
}

pub trait BlockDevice: Send + Sync + 'static {
    fn info(&self) -> DeviceInfo;
    fn read_at(&self, offset: u64, len: u32) -> io::Result<Vec<u8>>;
    fn write_at(&self, offset: u64, data: &[u8], durability: Durability) -> io::Result<()>;
    fn flush(&self) -> io::Result<()>;

    fn trim(&self, _offset: u64, _len: u32) -> io::Result<()> {
        Err(unsupported_operation())
    }

    fn write_zeroes(&self, _offset: u64, _len: u32, _no_hole: bool, _durability: Durability) -> io::Result<()> {
        Err(unsupported_operation())
    }
}

pub fn unsupported_operation() -> io::Error {
    io::Error::from_raw_os_error(rustix::io::Errno::OPNOTSUPP.raw_os_error())
}
