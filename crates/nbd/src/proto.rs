use enumflags2::{bitflags, BitFlags};
use packed_struct::prelude::*;

const NBD_REQUEST_MAGIC: u32 = 0x25609513;
const NBD_REPLY_MAGIC: u32 = 0x67446698;

#[bitflags(default = HasFlags)]
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NbdFlag {
    HasFlags        = 1 << 0, /* nbd-server supports flags */
    ReadOnly        = 1 << 1, /* device is read-only */
    SendFlush       = 1 << 2, /* can flush writeback cache */
    SendFua         = 1 << 3, /* send FUA (forced unit access) */
    Rotational      = 1 << 4, /* device is rotational */
    SendTrim        = 1 << 5, /* send trim/discard */
    SendWriteZeroes = 1 << 6, /* supports WRITE_ZEROES */
    CanMultiConn    = 1 << 8, /* Server supports multiple connections per export. */
}

pub type NbdFlags = BitFlags<NbdFlag>;

#[derive(PackedStruct, Debug, Clone)]
#[packed_struct(endian = "msb")]
struct RequestHeader {
    /// Always 0x25609513
    magic:  u32,
    flags:  u16,
    typ:    u16,
    cookie: u64,
    offset: u64,
    length: u64,
}

#[derive(PackedStruct, Debug, Clone)]
#[packed_struct(endian = "msb")]
struct SimpleReplyHeader {
    magic: u32,
}
