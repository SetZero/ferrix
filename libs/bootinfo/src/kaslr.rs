//! Layout randomisation: where a loader may put the kernel image, the direct
//! map and the top of the vmap arena, and what it tells the kernel it did.
//!
//! Three regions move, each within its own region of [`Layout`], each from
//! its own random word, so that learning one says nothing about the others:
//!
//! * **the kernel image**, anywhere in its region above the link address in
//!   steps of the granule its fixups allow (`ferrix_elf::Elf::fixup_granule`),
//!   the link address itself excluded, so that a moved kernel can check it
//!   moved;
//! * **the direct map**, in steps of [`Layout::physmap_granule`]: a gibibyte
//!   on the 64-bit pair, which keeps the loader's block mappings the shape they
//!   always were, and 2 MiB on ARMv7-A, whose region is barely larger than
//!   RAM. The direct map aliases every byte of RAM, the kernel's own image
//!   included, and firmware places that image at the same physical address
//!   every boot; a direct map that stayed put would give the image's data a
//!   fixed address whatever the image's slide;
//! * **the top of the vmap arena**, where the arena's top-down search starts,
//!   so that kernel stacks and device windows are not where they were last
//!   boot. The fixed early windows below the arena do not move.
//!
//! How many bits each got is [`Slots::bits`] of the candidates there were,
//! rounded down, and the kernel reports them.

use crate::{Layout, PAGE_SIZE, PHYSMAP_ALIGN};

/// Bytes kept free at the very top of the address space, so that the end of
/// a kernel image placed as high as it can go is still an address rather than
/// zero.
pub const KERNEL_TOP_GUARD: u64 = 2 * 1024 * 1024;

/// [`Kaslr::state`]: the loader moved the kernel image, the direct map and
/// the arena, from [`Kaslr::source`]'s randomness.
pub const KASLR_MOVED: u32 = 1;
/// [`Kaslr::state`]: the command line said `nokaslr`, so everything is at its
/// fixed address.
pub const KASLR_DECLINED: u32 = 2;
/// [`Kaslr::state`]: the loader found no source of randomness at all, so
/// everything is at its fixed address.
pub const KASLR_NO_ENTROPY: u32 = 3;
/// [`Kaslr::state`]: the kernel is a fixed-address image without fixups
/// (`--mitigations off`, or a copy stripped of them), so the image is at its
/// link address. The direct map and the arena are fixed too, since moving
/// them alone hides nothing the image does not give away.
pub const KASLR_FIXED_IMAGE: u32 = 4;
/// [`Kaslr::state`]: this loader does not randomise; see its boot log.
pub const KASLR_NOT_OFFERED: u32 = 5;

/// [`Kaslr::source`]: nothing.
pub const SOURCE_NONE: u32 = 0;
/// [`Kaslr::source`]: firmware's `EFI_RNG_PROTOCOL`.
pub const SOURCE_FIRMWARE_RNG: u32 = 1;
/// [`Kaslr::source`]: the processor's random number instruction, `RDRAND`.
pub const SOURCE_CPU_RNG: u32 = 2;
/// [`Kaslr::source`]: a cycle counter read at boot, which is not random: a
/// guess at how long firmware took recovers it.
pub const SOURCE_COUNTER: u32 = 3;

/// What the loader did about layout randomisation.
///
/// Carried at the end of [`crate::BootInfo`]; every field is fixed-width, so
/// the structure is the same on every word width.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Kaslr {
    /// Where the kernel image was linked, which [`crate::BootInfo::kernel_virt`]
    /// is at when it did not move.
    pub link: u64,
    /// One past the last address of the vmap arena, which its search starts
    /// below: [`Layout::vmap_end`] when it did not move.
    pub vmap_end: u64,
    /// [`KASLR_MOVED`] and its siblings.
    pub state: u32,
    /// [`SOURCE_FIRMWARE_RNG`] and its siblings.
    pub source: u32,
    /// Bits of position the image was given: the base-two logarithm of how
    /// many places it could have gone, rounded down.
    pub image_bits: u32,
    /// The same, for the direct map.
    pub physmap_bits: u32,
    /// The same, for the top of the vmap arena.
    pub vmap_bits: u32,
    /// Zero.
    pub reserved: u32,
}

impl Kaslr {
    /// What a loader that did not move anything reports, and why: `state`.
    #[must_use]
    pub const fn fixed(layout: &Layout, state: u32) -> Kaslr {
        Kaslr {
            link: layout.kernel_base,
            vmap_end: layout.vmap_end(),
            state,
            source: SOURCE_NONE,
            image_bits: 0,
            physmap_bits: 0,
            vmap_bits: 0,
            reserved: 0,
        }
    }

    /// Whether the layout moved from a source an attacker cannot predict.
    #[must_use]
    pub const fn is_random(&self) -> bool {
        self.state == KASLR_MOVED
            && (self.source == SOURCE_FIRMWARE_RNG || self.source == SOURCE_CPU_RNG)
    }

    /// Why the layout is where it is, in a phrase for a boot log.
    #[must_use]
    pub const fn state_text(&self) -> &'static str {
        match self.state {
            KASLR_MOVED => "moved",
            KASLR_DECLINED => "NOT randomised: nokaslr on the command line",
            KASLR_NO_ENTROPY => "NOT randomised: the loader found no source of randomness",
            KASLR_FIXED_IMAGE => {
                "NOT randomised: the kernel is a fixed-address image (--mitigations off)"
            }
            KASLR_NOT_OFFERED => "NOT randomised: this loader does not randomise",
            _ => "in a state no loader writes",
        }
    }

    /// Where the randomness came from, in a phrase for a boot log.
    #[must_use]
    pub const fn source_text(&self) -> &'static str {
        match self.source {
            SOURCE_FIRMWARE_RNG => "EFI_RNG",
            SOURCE_CPU_RNG => "RDRAND",
            SOURCE_COUNTER => "the cycle counter, which is guessable and so not KASLR",
            _ => "nothing",
        }
    }
}

/// Evenly spaced candidate addresses, less one run of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slots {
    /// The lowest candidate.
    first: u64,
    /// The distance between two candidates.
    step: u64,
    /// How many candidates there are before any are taken out.
    total: u64,
    /// The index of the first one taken out.
    skip_from: u64,
    /// How many are taken out.
    skip_len: u64,
}

impl Slots {
    /// No candidates at all.
    pub const NONE: Slots = Slots {
        first: 0,
        step: 0,
        total: 0,
        skip_from: 0,
        skip_len: 0,
    };

    /// `total` candidates, `step` apart, from `first`.
    #[must_use]
    pub const fn new(first: u64, step: u64, total: u64) -> Slots {
        Slots {
            first,
            step,
            total,
            skip_from: 0,
            skip_len: 0,
        }
    }

    /// How many candidates there are.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.total - self.skip_len
    }

    /// The base-two logarithm of [`Slots::count`], rounded down: the bits of
    /// position a uniform choice among them gives. Zero for one candidate or
    /// none.
    #[must_use]
    pub const fn bits(&self) -> u32 {
        match self.count() {
            0 => 0,
            count => 63 - count.leading_zeros(),
        }
    }

    /// Candidate `index`, counting only those not taken out.
    #[must_use]
    pub const fn nth(&self, index: u64) -> Option<u64> {
        if index >= self.count() {
            return None;
        }
        let index = if index >= self.skip_from {
            index + self.skip_len
        } else {
            index
        };
        Some(self.first + index * self.step)
    }

    /// The candidate a uniformly random `word` picks, or `None` if there are
    /// none. The modulo's bias is below one part in 2^44 for any count here,
    /// which is under 2^20.
    #[must_use]
    pub const fn pick(&self, word: u64) -> Option<u64> {
        match self.count() {
            0 => None,
            count => self.nth(word % count),
        }
    }

    /// These candidates, less every one whose `len` bytes would overlap
    /// `start..end`.
    #[must_use]
    pub const fn avoiding(self, len: u64, start: u64, end: u64) -> Slots {
        if self.total == 0 || self.step == 0 || end <= start {
            return self;
        }
        // Candidate i overlaps when first + i*step < end and
        // first + i*step + len > start.
        let low = if start < self.first + len {
            0
        } else {
            (start - self.first - len) / self.step + 1
        };
        if end <= self.first {
            return self;
        }
        let high = (end - 1 - self.first) / self.step;
        let high = if high >= self.total {
            self.total - 1
        } else {
            high
        };
        if low > high {
            return self;
        }
        Slots {
            skip_from: low,
            skip_len: high - low + 1,
            ..self
        }
    }
}

impl Layout {
    /// The step the direct map's base moves in.
    #[must_use]
    pub const fn physmap_granule(&self) -> u64 {
        if self.address_bits == 64 {
            1 << 30
        } else {
            PHYSMAP_ALIGN
        }
    }

    /// The step the top of the vmap arena moves in: 64 KiB, which is finer
    /// than any allocation there is aligned to, so nothing it hands out lands
    /// on a coarser boundary than it did.
    #[must_use]
    pub const fn vmap_granule(&self) -> u64 {
        64 * 1024
    }

    /// How many places the top of the vmap arena can be: up to 8 GiB below
    /// its end on the 64-bit pair, and 32 MiB of ARMv7-A's 448 MiB arena.
    #[must_use]
    pub const fn vmap_slots(&self) -> u64 {
        if self.address_bits == 64 {
            1 << 17
        } else {
            1 << 9
        }
    }

    /// One past the last byte a kernel image may occupy.
    #[must_use]
    pub const fn kernel_ceiling(&self) -> u64 {
        if self.address_bits >= 64 {
            0u64.wrapping_sub(KERNEL_TOP_GUARD)
        } else {
            (1 << self.address_bits) - KERNEL_TOP_GUARD
        }
    }

    /// Where an image of `len` bytes may be placed, `granule` apart, above its
    /// link address and below [`Layout::kernel_ceiling`]. The link address is
    /// not one of them. No candidates for a granule that is not a power of two
    /// of at least a page.
    #[must_use]
    pub const fn kernel_slots(&self, len: u64, granule: u64) -> Slots {
        if !granule.is_power_of_two() || granule < PAGE_SIZE {
            return Slots::NONE;
        }
        let room = self.kernel_ceiling() - self.kernel_base;
        match room.checked_sub(len) {
            Some(spare) if spare >= granule => {
                Slots::new(self.kernel_base + granule, granule, spare / granule)
            }
            _ => Slots::NONE,
        }
    }

    /// Where a direct map of `len` bytes may begin, less any place it would
    /// overlap `avoid`: the loader's own image, on a machine where the loader
    /// maps itself in the kernel's tree.
    #[must_use]
    pub const fn physmap_slots(&self, len: u64, avoid: Option<(u64, u64)>) -> Slots {
        let granule = self.physmap_granule();
        let room = self.physmap_size();
        let slots = match room.checked_sub(len) {
            Some(spare) => Slots::new(self.physmap_base, granule, spare / granule + 1),
            None => return Slots::NONE,
        };
        match avoid {
            Some((start, end)) => slots.avoiding(len, start, end),
            None => slots,
        }
    }

    /// Where the top of the vmap arena may be.
    #[must_use]
    pub const fn vmap_ends(&self) -> Slots {
        let granule = self.vmap_granule();
        let slots = self.vmap_slots();
        Slots::new(self.vmap_end() - (slots - 1) * granule, granule, slots)
    }

    /// Whether `end` is one of [`Layout::vmap_ends`].
    #[must_use]
    pub const fn is_vmap_end(&self, end: u64) -> bool {
        let slots = self.vmap_ends();
        end >= slots.first
            && end <= self.vmap_end()
            && (end - slots.first).is_multiple_of(slots.step)
    }
}

#[cfg(test)]
mod tests;
