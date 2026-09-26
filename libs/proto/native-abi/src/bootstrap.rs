//! What the kernel says to pid 1 before pid 1 runs (`docs/INIT.md` §6, K2).
//!
//! The kernel starts init with a bootstrap channel, as it starts `devmgr`,
//! and writes one message on its own end first. Init takes its end with
//! `process_bootstrap` ([`crate::nr::PROCESS_BOOTSTRAP`]) and reads the
//! message with `channel_read`. The kernel keeps its end open for as long as
//! the program runs, so later versions can carry what version 1 does not:
//! a power handle, and in a microkernel the device root and physical memory.
//!
//! # The header, which never changes
//!
//! Eight bytes: the magic `FXIN`, then the version as a little-endian `u32`.
//! Little-endian on every target rather than native, so the bytes are the
//! same on all three and a reader needs no word of the kernel's to parse
//! them. What follows the header, and which handles the message carries, is
//! the version's to say.
//!
//! # Version 1
//!
//! The header alone: [`INIT_HELLO_BYTES`] bytes and no handles. A reader
//! that knows version 1 accepts any version from 1 up and reads the header
//! only, since a later version adds after it and never changes it.

/// The first four bytes of the message.
pub const INIT_HELLO_MAGIC: [u8; 4] = *b"FXIN";

/// The version this crate describes.
pub const INIT_HELLO_VERSION: u32 = 1;

/// The header's length, which is the whole of a version 1 message.
pub const INIT_HELLO_BYTES: usize = 8;

/// How many handles a version 1 message carries.
pub const INIT_HELLO_HANDLES: usize = 0;

/// The version 1 message, as the kernel writes it.
#[must_use]
pub const fn init_hello() -> [u8; INIT_HELLO_BYTES] {
    let [m0, m1, m2, m3] = INIT_HELLO_MAGIC;
    let [v0, v1, v2, v3] = INIT_HELLO_VERSION.to_le_bytes();
    [m0, m1, m2, m3, v0, v1, v2, v3]
}

/// The version a message read on pid 1's bootstrap channel says it is, or
/// `None` for one that is not the kernel's: shorter than the header, or
/// without the magic, or version zero.
#[must_use]
pub fn init_hello_version(message: &[u8]) -> Option<u32> {
    let (magic, rest) = message.split_first_chunk::<4>()?;
    let (version, _) = rest.split_first_chunk::<4>()?;
    let version = u32::from_le_bytes(*version);
    (*magic == INIT_HELLO_MAGIC && version != 0).then_some(version)
}
