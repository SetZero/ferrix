//! The structures native calls read and write through pointers.
//!
//! Every one is laid out so that it has no padding and the same size and
//! offsets on all three targets. A `u64` is 8-aligned under both the x86-64
//! System V ABI and AAPCS, so an ARMv7-A program and the kernel agree on
//! these without a 32-bit variant — which is the property Linux's `stat`
//! does not have, and why `fstat64` exists.

/// The most bytes one channel message may carry.
///
/// Sixty-four kibibytes: large enough for any control message a driver
/// sends, and small enough that a message is copied through the kernel
/// rather than mapped. Bulk data belongs in a shared VMO, which is what
/// `docs/ARCHITECTURE.md` §7 says the data path is.
pub const CHANNEL_MAX_BYTES: usize = 64 * 1024;

/// The most handles one channel message may carry.
pub const CHANNEL_MAX_HANDLES: usize = 64;

/// A packet on a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct PortPacket {
    /// Whatever the registrant chose, so one waiter can tell its sources
    /// apart.
    pub key: u64,
    /// Which of the `PACKET_*` kinds this is.
    pub kind: u32,
    /// For a signal packet, the signals that were asserted when it fired.
    /// Zero for the other kinds.
    pub signals: u32,
    /// For a user packet, what the sender queued. For an interrupt packet,
    /// the first word is the time it fired in nanoseconds since boot.
    pub data: [u64; 2],
}

/// A packet queued by a program through `port_queue`.
pub const PACKET_USER: u32 = 0;
/// A packet an `object_wait_async` registration produced.
pub const PACKET_SIGNAL: u32 = 1;
/// A packet a bound interrupt produced.
pub const PACKET_INTERRUPT: u32 = 2;

/// `vmo_map`'s protection: the mapping may be read. Every mapping asks for it.
pub const MAP_READ: u32 = 1 << 0;
/// `vmo_map`'s protection: the mapping may also be written.
pub const MAP_WRITE: u32 = 1 << 1;

/// What a `channel_read` found.
///
/// Written on success, and on `BUFFER_TOO_SMALL` too, when it is how the
/// caller learns what to allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct ReadActual {
    /// The message's size in bytes.
    pub bytes: u32,
    /// How many handles it carries.
    pub handles: u32,
}

/// The aperture an `io_mapping_create` claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct IoMappingSpec {
    /// The physical address of the first byte.
    pub phys: u64,
    /// The length in bytes.
    pub len: u64,
}
