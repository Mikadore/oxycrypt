use enumflags2::BitFlag;
use enumflags2::BitFlags;
use enumflags2::bitflags;
use packed_struct::prelude::*;
use rootcause::prelude::ResultExt;
use rootcause::report;

use crate::NbdError;
use crate::Result;

const NBD_REQUEST_MAGIC: u32 = 0x25609513;
const NBD_REPLY_MAGIC: u32 = 0x67446698;
const NBD_COMMAND_MASK: u32 = 0x0000_FFFF;
const NBD_COMMAND_FLAG_SHIFT: u32 = 16;

#[bitflags(default = HasFlags | SendFlush)]
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq)]
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

#[derive(PackedStruct, Debug, Clone)]
#[packed_struct(endian = "msb")]
struct NbdRequestHeader {
    /// Always 0x25609513
    pub magic:          u32,
    pub type_and_flags: u32,
    pub cookie:         u64,
    pub offset:         u64,
    pub length:         u32,
}

impl NbdRequestHeader {
    fn command(&self) -> Option<NbdCommandType> {
        NbdCommandType::from_raw((self.type_and_flags & NBD_COMMAND_MASK) as u16)
    }

    fn flags(&self) -> NbdCommandFlags {
        NbdCommandFlag::from_bits_truncate((self.type_and_flags >> NBD_COMMAND_FLAG_SHIFT) as u16)
    }
}

#[derive(Clone, Debug)]
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

    // bytes must be at least 28 bytes long
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        let header = NbdRequestHeader::unpack_from_slice(&bytes[..Self::HEADER_LEN])
            .context(NbdError::Protocol)
            .attach("Invalid request header")?;

        if header.magic != NBD_REQUEST_MAGIC {
            return Err(report!(NbdError::Protocol).attach("invalid request magic"));
        }

        let typ = header.command().ok_or_else(|| {
            report!(NbdError::Protocol).attach(format!(
                "invalid command type '{}'",
                header.type_and_flags & NBD_COMMAND_MASK
            ))
        })?;

        let flags = header.flags();

        Ok(NbdRequest {
            typ,
            flag_fua: flags.contains(NbdCommandFlag::NoHole),
            flag_no_hole: flags.contains(NbdCommandFlag::Fua),
            cookie: header.cookie,
            offset: header.offset,
            length: header.length,
        })
    }
}

#[derive(PackedStruct, Debug, Clone)]
#[packed_struct(endian = "msb")]
pub struct ReplyHeader {
    pub magic:  u32,
    pub error:  u32,
    pub cookie: u64,
}
