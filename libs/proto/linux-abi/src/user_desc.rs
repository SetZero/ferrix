//! `struct user_desc`: how an i386 program describes a thread-local segment
//! to `set_thread_area`, and how the kernel describes one back through
//! `get_thread_area`, with the GDT descriptor it stands for.
//!
//! A 32-bit x86 program's thread pointer is a segment, not a register: musl
//! and glibc ask for a flat data segment based at their thread block, load its
//! selector into `%gs`, and read their thread-local variables through it. The
//! kernel keeps three such descriptors per thread, in GDT entries 12 to 14 as
//! Linux does, and the program chooses among them by `entry_number`
//! (`docs/I386.md` §3.5).
//!
//! From `asm/ldt.h`, the flag word's bit-fields allocated from bit 0 as GCC
//! allocates them on a little-endian target. The rules are Linux's, from
//! `arch/x86/kernel/tls.c`: `tls_desc_okay` for what may be installed and
//! `fill_ldt`/`fill_user_desc` for the encoding each way.

use crate::wire;

/// The first GDT entry a thread-local descriptor may occupy: Linux's
/// `GDT_ENTRY_TLS_MIN` on x86-64, which a 32-bit program there sees as its
/// thread-local entries.
pub const TLS_FIRST_ENTRY: u32 = 12;

/// How many thread-local entries a thread has: `GDT_ENTRY_TLS_ENTRIES`.
pub const TLS_ENTRIES: usize = 3;

/// The `entry_number` that asks `set_thread_area` to choose a free entry and
/// write back the one it chose.
pub const ANY_ENTRY: u32 = u32::MAX;

/// Flag bit: 32-bit code or data (`D`/`B`).
const SEG_32BIT: u32 = 1 << 0;
/// Flag field: what the segment is, two bits.
const CONTENTS_SHIFT: u32 = 1;
/// Flag bit: the segment may not be written, or, for code, read.
const READ_EXEC_ONLY: u32 = 1 << 3;
/// Flag bit: the limit counts pages.
const LIMIT_IN_PAGES: u32 = 1 << 4;
/// Flag bit: the segment is absent.
const SEG_NOT_PRESENT: u32 = 1 << 5;
/// Flag bit: the descriptor's `AVL` bit.
const USEABLE: u32 = 1 << 6;

/// Descriptor bit: the segment is present.
const DESC_PRESENT: u64 = 1 << 47;

/// An i386 program's `struct user_desc`, sixteen bytes.
///
/// `lm`, bit 7 of the flag word, exists only in the 64-bit header: a 32-bit
/// program may leave anything there, and the header says the kernel must act
/// as though it were zero. It is not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserDesc {
    /// Which thread-local entry, or [`ANY_ENTRY`].
    pub entry_number: u32,
    /// The segment's base address.
    pub base_addr: u32,
    /// The segment's limit, twenty bits.
    pub limit: u32,
    /// 32-bit code or data rather than 16-bit.
    pub seg_32bit: bool,
    /// 0 data, 1 expand-down data, 2 code.
    pub contents: u32,
    /// Not writable (data) or not readable (code).
    pub read_exec_only: bool,
    /// The limit counts pages rather than bytes.
    pub limit_in_pages: bool,
    /// The segment is marked absent.
    pub seg_not_present: bool,
    /// The descriptor's available bit.
    pub useable: bool,
}

/// Why a `user_desc` was refused: `EINVAL` on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused;

impl UserDesc {
    /// Bytes in the structure.
    pub const SIZE: usize = 16;

    /// Read one from a program's sixteen bytes.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<UserDesc> {
        let word = |at| wire::array::<4>(bytes, at).map(u32::from_le_bytes);
        let flags = word(12)?;
        Some(UserDesc {
            entry_number: word(0)?,
            base_addr: word(4)?,
            limit: word(8)?,
            seg_32bit: flags & SEG_32BIT != 0,
            contents: (flags >> CONTENTS_SHIFT) & 3,
            read_exec_only: flags & READ_EXEC_ONLY != 0,
            limit_in_pages: flags & LIMIT_IN_PAGES != 0,
            seg_not_present: flags & SEG_NOT_PRESENT != 0,
            useable: flags & USEABLE != 0,
        })
    }

    /// The sixteen bytes a program reads back, `lm` clear.
    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let flags = u32::from(self.seg_32bit)
            | ((self.contents & 3) << CONTENTS_SHIFT)
            | if self.read_exec_only {
                READ_EXEC_ONLY
            } else {
                0
            }
            | if self.limit_in_pages {
                LIMIT_IN_PAGES
            } else {
                0
            }
            | if self.seg_not_present {
                SEG_NOT_PRESENT
            } else {
                0
            }
            | if self.useable { USEABLE } else { 0 };
        let mut out = [0_u8; Self::SIZE];
        for (at, word) in [
            (0, self.entry_number),
            (4, self.base_addr),
            (8, self.limit),
            (12, flags),
        ] {
            let _ = wire::put(&mut out, at, &word.to_le_bytes());
        }
        out
    }

    /// Whether this asks for the entry to be emptied rather than filled:
    /// Linux's `LDT_empty` -- the shape a cleared entry reads back as -- or
    /// `LDT_zero`, every field zero.
    #[must_use]
    pub const fn clears(&self) -> bool {
        let zero = self.base_addr == 0
            && self.limit == 0
            && self.contents == 0
            && !self.limit_in_pages
            && !self.useable;
        let empty = zero && self.read_exec_only && !self.seg_32bit && self.seg_not_present;
        let all_zero = zero && !self.read_exec_only && !self.seg_32bit && !self.seg_not_present;
        empty || all_zero
    }

    /// The GDT descriptor this installs, or zero for one that
    /// [clears](UserDesc::clears) its entry.
    ///
    /// # Errors
    ///
    /// [`Refused`] for what `tls_desc_okay` refuses: a 16-bit segment, which
    /// would need espfix, a code segment, and a segment marked absent, which
    /// Linux keeps out of the TLS array as attack surface.
    pub const fn to_descriptor(&self) -> Result<u64, Refused> {
        if self.clears() {
            return Ok(0);
        }
        if !self.seg_32bit || self.contents > 1 || self.seg_not_present {
            return Err(Refused);
        }
        let base = self.base_addr as u64;
        let limit = self.limit as u64;
        // `fill_ldt`: accessed, writable unless read-only, the contents,
        // a code-or-data segment at ring 3.
        let kind = 1 | ((!self.read_exec_only as u64) << 1) | ((self.contents as u64) << 2);
        let access = kind | (1 << 4) | (3 << 5) | (1 << 7);
        let flags = (self.useable as u64)
            | ((self.seg_32bit as u64) << 2)
            | ((self.limit_in_pages as u64) << 3);
        Ok((limit & 0xFFFF)
            | ((base & 0x00FF_FFFF) << 16)
            | (access << 40)
            | (((limit >> 16) & 0xF) << 48)
            | (flags << 52)
            | (((base >> 24) & 0xFF) << 56))
    }

    /// What `get_thread_area` answers for entry `entry_number` holding
    /// `descriptor`: Linux's `fill_user_desc`, whose reading of an empty entry
    /// is the shape [`UserDesc::clears`] accepts.
    #[must_use]
    pub const fn from_descriptor(entry_number: u32, descriptor: u64) -> UserDesc {
        let kind = (descriptor >> 40) & 0xF;
        let flags = (descriptor >> 52) & 0xF;
        UserDesc {
            entry_number,
            base_addr: (((descriptor >> 16) & 0x00FF_FFFF) | (((descriptor >> 56) & 0xFF) << 24))
                as u32,
            limit: ((descriptor & 0xFFFF) | (((descriptor >> 48) & 0xF) << 16)) as u32,
            seg_32bit: flags & 0b0100 != 0,
            contents: ((kind >> 2) & 3) as u32,
            read_exec_only: kind & 0b0010 == 0,
            limit_in_pages: flags & 0b1000 != 0,
            seg_not_present: descriptor & DESC_PRESENT == 0,
            useable: flags & 0b0001 != 0,
        }
    }
}

/// The thread-local entry `entry_number` names, as an index from 0, or
/// `None` for a number outside the three.
#[must_use]
pub const fn tls_index(entry_number: u32) -> Option<usize> {
    match entry_number.checked_sub(TLS_FIRST_ENTRY) {
        Some(index) if (index as usize) < TLS_ENTRIES => Some(index as usize),
        _ => None,
    }
}
