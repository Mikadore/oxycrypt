use std::io;

use block_device::DeviceInfo;
use enumflags2::BitFlag;
use enumflags2::BitFlags;
use enumflags2::bitflags;
use rootcause::report;
use rustix::io::Errno;

use crate::NbdError;
use crate::Result;

pub(crate) const NBD_REQUEST_MAGIC: u32 = 0x2560_9513;
pub(crate) const NBD_SIMPLE_REPLY_MAGIC: u32 = 0x6744_6698;

#[bitflags]
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NbdDriverFlag {
    HasFlags        = 1 << 0, /* nbd-server supports flags */
    ReadOnly        = 1 << 1, /* device is read-only */
    SendFlush       = 1 << 2, /* can flush writeback cache */
    SendFua         = 1 << 3, /* send FUA (forced unit access) */
    Rotational      = 1 << 4, /* device is rotational */
    SendTrim        = 1 << 5, /* send trim/discard */
    SendWriteZeroes = 1 << 6, /* supports WRITE_ZEROES */
    CanMultiConn    = 1 << 8, /* Server supports multiple connections per export. */
}

pub type NbdDriverFlags = BitFlags<NbdDriverFlag>;

pub fn driver_flags_from_device_info(info: DeviceInfo) -> NbdDriverFlags {
    let mut flags = NbdDriverFlags::empty();
    flags.insert(NbdDriverFlag::HasFlags);

    if info.read_only {
        flags.insert(NbdDriverFlag::ReadOnly);
    }
    if info.supports_flush {
        flags.insert(NbdDriverFlag::SendFlush);
    }
    if info.supports_fua {
        flags.insert(NbdDriverFlag::SendFua);
    }
    if info.supports_trim {
        flags.insert(NbdDriverFlag::SendTrim);
    }
    if info.supports_write_zeroes {
        flags.insert(NbdDriverFlag::SendWriteZeroes);
    }
    if info.can_multi_conn {
        flags.insert(NbdDriverFlag::CanMultiConn);
    }

    flags
}

#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NbdCommandType {
    Read        = 0,
    Write       = 1,
    Disc        = 2,
    Flush       = 3,
    Trim        = 4,
    WriteZeroes = 6,
}

impl NbdCommandType {
    pub fn from_raw(value: u16) -> Option<Self> {
        match value {
            0 => Some(Self::Read),
            1 => Some(Self::Write),
            2 => Some(Self::Disc),
            3 => Some(Self::Flush),
            4 => Some(Self::Trim),
            6 => Some(Self::WriteZeroes),
            _ => None,
        }
    }
}

#[bitflags]
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NbdCommandFlag {
    Fua    = 1 << 0,
    NoHole = 1 << 1,
}

pub type NbdCommandFlags = BitFlags<NbdCommandFlag>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NbdRequest {
    pub typ:          NbdCommandType,
    pub flag_fua:     bool,
    pub flag_no_hole: bool,
    pub cookie:       u64,
    pub offset:       u64,
    pub length:       u32,
}

impl NbdRequest {
    pub const HEADER_LEN: usize = 28;

    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        if bytes.len() < Self::HEADER_LEN {
            return Err(report!(NbdError::Protocol).attach("request header is too short"));
        }

        if read_u32(&bytes[0..4]) != NBD_REQUEST_MAGIC {
            return Err(report!(NbdError::Protocol).attach("invalid request magic"));
        }

        let flags = NbdCommandFlag::from_bits_truncate(read_u16(&bytes[4..6]));
        let typ_raw = read_u16(&bytes[6..8]);
        let typ = NbdCommandType::from_raw(typ_raw)
            .ok_or_else(|| report!(NbdError::Protocol).attach(format!("invalid command type '{}'", typ_raw)))?;

        Ok(NbdRequest {
            typ,
            flag_fua: flags.contains(NbdCommandFlag::Fua),
            flag_no_hole: flags.contains(NbdCommandFlag::NoHole),
            cookie: read_u64(&bytes[8..16]),
            offset: read_u64(&bytes[16..24]),
            length: read_u32(&bytes[24..28]),
        })
    }

    pub fn expects_write_payload(&self) -> bool {
        matches!(self.typ, NbdCommandType::Write)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleReply {
    pub cookie: u64,
    pub error:  u32,
    pub data:   Vec<u8>,
}

impl SimpleReply {
    pub fn ok(cookie: u64) -> Self {
        Self {
            cookie,
            error: 0,
            data: Vec::new(),
        }
    }

    pub fn with_data(cookie: u64, data: Vec<u8>) -> Self {
        Self { cookie, error: 0, data }
    }

    pub fn from_errno(cookie: u64, errno: Errno) -> Self {
        Self {
            cookie,
            error: errno.raw_os_error() as u32,
            data: Vec::new(),
        }
    }

    pub fn from_io_error(cookie: u64, error: &io::Error) -> Self {
        Self {
            cookie,
            error: io_error_to_reply_code(error),
            data: Vec::new(),
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16 + self.data.len());
        bytes.extend_from_slice(&NBD_SIMPLE_REPLY_MAGIC.to_be_bytes());
        bytes.extend_from_slice(&self.error.to_be_bytes());
        bytes.extend_from_slice(&self.cookie.to_be_bytes());
        bytes.extend_from_slice(&self.data);
        bytes
    }
}

pub fn io_error_to_reply_code(error: &io::Error) -> u32 {
    if let Some(code) = error.raw_os_error() {
        return code.unsigned_abs();
    }

    match error.kind() {
        io::ErrorKind::InvalidInput => Errno::INVAL.raw_os_error() as u32,
        io::ErrorKind::PermissionDenied => Errno::PERM.raw_os_error() as u32,
        io::ErrorKind::NotFound => Errno::NOENT.raw_os_error() as u32,
        io::ErrorKind::Unsupported => Errno::OPNOTSUPP.raw_os_error() as u32,
        _ => Errno::IO.raw_os_error() as u32,
    }
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes(bytes.try_into().expect("slice length should be validated"))
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().expect("slice length should be validated"))
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes(bytes.try_into().expect("slice length should be validated"))
}

#[cfg(test)]
mod tests {
    use block_device::DeviceGeometry;

    use super::NbdCommandFlag;
    use super::NbdCommandType;
    use super::NbdDriverFlag;
    use super::NbdRequest;
    use super::driver_flags_from_device_info;
    use super::*;

    #[test]
    fn parses_fua_and_no_hole_flags_correctly() {
        let mut bytes = [0u8; NbdRequest::HEADER_LEN];
        bytes[0..4].copy_from_slice(&NBD_REQUEST_MAGIC.to_be_bytes());

        let flags = (NbdCommandFlag::Fua as u16) | (NbdCommandFlag::NoHole as u16);
        bytes[4..6].copy_from_slice(&flags.to_be_bytes());
        bytes[6..8].copy_from_slice(&(NbdCommandType::WriteZeroes as u16).to_be_bytes());
        bytes[8..16].copy_from_slice(&123u64.to_be_bytes());
        bytes[16..24].copy_from_slice(&4096u64.to_be_bytes());
        bytes[24..28].copy_from_slice(&512u32.to_be_bytes());

        let request = NbdRequest::from_bytes(bytes).expect("request should parse");
        assert!(request.flag_fua);
        assert!(request.flag_no_hole);
    }

    #[test]
    fn derives_driver_flags_from_device_capabilities() {
        let mut info = DeviceInfo::from_geometry(DeviceGeometry::new(512, 8));
        info.read_only = true;
        info.supports_flush = true;
        info.supports_fua = true;
        info.can_multi_conn = false;

        let flags = driver_flags_from_device_info(info);
        assert!(flags.contains(NbdDriverFlag::HasFlags));
        assert!(flags.contains(NbdDriverFlag::ReadOnly));
        assert!(flags.contains(NbdDriverFlag::SendFlush));
        assert!(flags.contains(NbdDriverFlag::SendFua));
        assert!(!flags.contains(NbdDriverFlag::CanMultiConn));
    }
}
