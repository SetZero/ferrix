//! Random bytes from ABL, for the kernel's generator.
//!
//! The phone has no `EFI_RNG_PROTOCOL` to ask and the kernel does not drive
//! its TRNG, so without these the kernel starts `NOT SEEDED`. ABL, as Android
//! bootloaders do, puts random bytes in `/chosen`: `rng-seed` for the kernel's
//! generator and `kaslr-seed`, one 64-bit word, for its layout. Both are
//! passed on as `BootInfo.firmware_seed`, folded into its 32 bytes, with
//! their count in `firmware_seed_len`, so that the kernel credits what ABL
//! gave and no more: 8 bytes, 64 bits, on the phone this was written on.
//!
//! Linux removes both properties from the tree once it has read them, so that
//! nothing after it can read them back, and this does the same for the copy
//! the kernel is handed: each is overwritten with `FDT_NOP` tokens, which
//! every reader of the format skips. ABL's own copy is left as it is: the
//! loader reads ABL's memory and never writes it.

use core::ptr;

use ferrix_fdt::Fdt;

/// The properties that hold random bytes, in `/chosen`.
const PROPERTIES: [&str; 2] = ["rng-seed", "kaslr-seed"];

/// The structure block's token that begins a property.
const FDT_PROP: u32 = 3;
/// The token every reader skips.
const FDT_NOP: u32 = 4;
/// A property's header: its token, its length and its name's offset.
const HEADER_BYTES: usize = 12;

/// Bytes the kernel's seed holds.
pub(crate) const SEED_BYTES: usize = 32;

/// What `/chosen` held.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seed {
    /// Every byte found, folded into [`SEED_BYTES`] by exclusive or.
    pub(crate) bytes: [u8; SEED_BYTES],
    /// How many bytes were found.
    pub(crate) found: usize,
    /// Where each property was, as the offset of its value in the tree and
    /// its length, so the copy's can be removed.
    spots: [Option<(usize, usize)>; PROPERTIES.len()],
}

impl Seed {
    /// Whether there was anything to pass on.
    pub(crate) const fn is_any(&self) -> bool {
        self.found > 0
    }
}

/// Read the random bytes in `tree`'s `/chosen`, which was parsed from `blob`.
pub(crate) fn gather(tree: &Fdt<'_>, blob: &[u8]) -> Seed {
    let mut seed = Seed {
        bytes: [0; SEED_BYTES],
        found: 0,
        spots: [None; PROPERTIES.len()],
    };
    let Some(chosen) = tree.find_node("/chosen") else {
        return seed;
    };
    for (name, spot) in PROPERTIES.iter().zip(seed.spots.iter_mut()) {
        let Some(property) = chosen.property(name) else {
            continue;
        };
        for (index, &byte) in property.value.iter().enumerate() {
            if let Some(slot) = seed.bytes.get_mut((seed.found + index) % SEED_BYTES) {
                *slot ^= byte;
            }
        }
        seed.found += property.value.len();
        // The value is a slice of `blob`, so its offset is the distance
        // between the two.
        let offset = (property.value.as_ptr() as usize).wrapping_sub(blob.as_ptr() as usize);
        *spot = Some((offset, property.value.len()));
    }
    seed
}

/// Remove the properties [`gather`] read from the copy of the tree at `copy`,
/// `len` bytes long, byte for byte the tree it read them from.
///
/// A property whose header is not where the offset says is left alone:
/// removing the wrong bytes would leave a tree the kernel cannot parse, which
/// is worse than a seed it can read.
pub(crate) fn remove(seed: &Seed, copy: u64, len: u64) {
    for &(offset, value_len) in seed.spots.iter().flatten() {
        let Some(start) = offset.checked_sub(HEADER_BYTES) else {
            continue;
        };
        let end = offset.saturating_add(value_len.next_multiple_of(4));
        if end as u64 > len || !start.is_multiple_of(4) {
            continue;
        }
        let word = |at: usize| (copy + at as u64) as *mut u32;
        // SAFETY: `start` is an aligned word inside the copy, which the loader
        // made and nothing else refers to.
        let token = u32::from_be(unsafe { ptr::read_volatile(word(start)) });
        // SAFETY: as above, the word after it, still inside the copy.
        let length = u32::from_be(unsafe { ptr::read_volatile(word(start + 4)) });
        if token != FDT_PROP || length as usize != value_len {
            continue;
        }
        for at in (start..end).step_by(4) {
            // SAFETY: as above, every word up to `end`, which is inside it.
            unsafe { ptr::write_volatile(word(at), FDT_NOP.to_be()) };
        }
    }
}
