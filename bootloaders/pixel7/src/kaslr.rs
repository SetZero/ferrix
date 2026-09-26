//! Where the kernel image, the direct map and the vmap arena go this boot.
//!
//! `boot/src/kaslr.rs`'s choice, over `ferrix_bootinfo`'s same slots, with
//! the one source of randomness this phone has for it: TF-A's True Random
//! Number Generator, asked through SMCCC `TRNG_RND64` (Arm DEN0098), the way
//! Android's `smccc_trng` driver and the kernel's own seeding ask it. ABL's 8
//! bytes in `/chosen` are not used: they are the kernel's seed.
//!
//! No other source stands in. With no TRNG, and with `nokaslr` on the command
//! line, everything stays at its fixed address and the log says why. A
//! cycle counter would be guessable, and `boot/` reports one as not KASLR,
//! so this loader does not offer one at all. A fixed-address kernel, one
//! built `--mitigations off`, cannot move and stays too.
//!
//! The image, the direct map and the arena each get their own random word,
//! so that one leaked address gives away one region, not three.

use ferrix_bootinfo::{
    KASLR_DECLINED, KASLR_FIXED_IMAGE, KASLR_MOVED, KASLR_NO_ENTROPY, Kaslr, LAYOUT, PAGE_SIZE,
    SOURCE_SMCCC_TRNG,
};
use ferrix_elf::Elf;
use ferrix_fdt::{Fdt, PsciConduit};

use crate::entry;
use crate::log::say;

/// `PSCI_VERSION`.
const PSCI_VERSION: u64 = 0x8400_0000;
/// `PSCI_FEATURES`.
const PSCI_FEATURES: u64 = 0x8400_000A;
/// `SMCCC_VERSION`.
const SMCCC_VERSION: u64 = 0x8000_0000;
/// `TRNG_VERSION`.
const TRNG_VERSION: u64 = 0x8400_0050;
/// `TRNG_FEATURES`.
const TRNG_FEATURES: u64 = 0x8400_0051;
/// `TRNG_RND64`: up to 192 bits in `x1`-`x3`.
const TRNG_RND64: u64 = 0xC400_0053;
/// `TRNG_RND64`'s status when the source has nothing ready yet.
const NO_ENTROPY: i32 = -3;
/// How many times a call that found no entropy ready is asked again.
const ATTEMPTS: usize = 8;

/// Where the loader is putting what moves, and what it tells the kernel.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Choice {
    /// The kernel image's base address.
    pub(crate) kernel_virt: u64,
    /// The direct map's base address.
    pub(crate) physmap_base: u64,
    /// What goes in the boot info.
    pub(crate) kaslr: Kaslr,
}

impl Choice {
    /// How far the image moved from its link address.
    pub(crate) const fn slide(&self) -> u64 {
        self.kernel_virt - self.kaslr.link
    }
}

/// Whether the command line asks for no randomisation.
pub(crate) fn declined(cmdline: &str) -> bool {
    ferrix_bootinfo::flag_in(cmdline, "nokaslr")
}

/// Three full-entropy words from TF-A's TRNG, or `None` where the device
/// tree does not say PSCI is reached by `smc`, or firmware does not offer
/// `TRNG_RND64`.
///
/// Only the `smc` conduit: this loader dropped from EL2 to EL1 itself, so an
/// `hvc` would reach an EL2 with no handler. Each call is a query or a read,
/// asked in the order that makes it safe to ask: PSCI 1.0, then
/// `PSCI_FEATURES` for `SMCCC_VERSION`, SMCCC 1.1, `TRNG_VERSION`, and
/// `TRNG_FEATURES` for `TRNG_RND64`.
pub(crate) fn gather(tree: &Fdt<'_>) -> Option<[u64; 3]> {
    if tree.psci_conduit() != Some(PsciConduit::Smc) {
        return None;
    }
    // SAFETY: every function asked is one SMCCC or PSCI defines to answer,
    // with NOT_SUPPORTED where it is not implemented, and to change nothing.
    let call = |function, argument| unsafe { entry::smc_call(function, argument) };
    let status = |function, argument| call(function, argument)[0] as u32 as i32;
    if status(PSCI_VERSION, 0) < 0x1_0000
        || status(PSCI_FEATURES, SMCCC_VERSION) < 0
        || status(SMCCC_VERSION, 0) < 0x1_0001
        || status(TRNG_VERSION, 0) < 0x1_0000
        || status(TRNG_FEATURES, TRNG_RND64) < 0
    {
        return None;
    }
    for _ in 0..ATTEMPTS {
        let [result, high, middle, low] = call(TRNG_RND64, 192);
        match result as u32 as i32 {
            0 => return Some([high, middle, low]),
            NO_ENTROPY => {}
            _ => return None,
        }
    }
    None
}

/// Decide where the image, `span` bytes linked at `link`, the direct map of
/// `direct_len` bytes and the arena go, and say so.
///
/// # Errors
///
/// A relocatable kernel this layout has no place for: one not linked where
/// the layout moves images from, or too large to move, or a direct map too
/// large for its region.
pub(crate) fn choose(
    elf: &Elf<'_>,
    (link, span): (u64, u64),
    direct_len: u64,
    declined: bool,
    randomness: Option<[u64; 3]>,
) -> Result<Choice, &'static str> {
    let fixed = |state| Choice {
        kernel_virt: link,
        physmap_base: LAYOUT.physmap_base,
        kaslr: Kaslr::fixed(&LAYOUT, state),
    };
    let choice = if !elf.is_relocatable() {
        fixed(KASLR_FIXED_IMAGE)
    } else if declined {
        fixed(KASLR_DECLINED)
    } else if let Some(words) = randomness {
        place(elf, (link, span), direct_len, words)?
    } else {
        fixed(KASLR_NO_ENTROPY)
    };
    report(&choice);
    Ok(choice)
}

/// Pick a place for each region from `words`, one each.
fn place(
    elf: &Elf<'_>,
    (link, span): (u64, u64),
    direct_len: u64,
    [image_word, physmap_word, vmap_word]: [u64; 3],
) -> Result<Choice, &'static str> {
    let granule = elf
        .fixup_granule()
        .map_err(|_| "the kernel's fixups are of a kind the loader cannot apply")?
        .max(PAGE_SIZE);
    if link != LAYOUT.kernel_base {
        return Err("the kernel is not linked at the address the loader moves it from");
    }
    let images = LAYOUT.kernel_slots(span, granule);
    let kernel_virt = images
        .pick(image_word)
        .ok_or("the kernel image is too large to be moved")?;
    // This loader maps itself only through TTBR0, the lower half, so the
    // direct map need keep clear of nothing in its own region.
    let physmaps = LAYOUT.physmap_slots(direct_len, None);
    let physmap_base = physmaps
        .pick(physmap_word)
        .ok_or("the direct map has no place in its region")?;
    let ends = LAYOUT.vmap_ends();
    let vmap_end = ends
        .pick(vmap_word)
        .ok_or("the vmap arena has no place for its top")?;
    Ok(Choice {
        kernel_virt,
        physmap_base,
        kaslr: Kaslr {
            link,
            vmap_end,
            state: KASLR_MOVED,
            source: SOURCE_SMCCC_TRNG,
            image_bits: images.bits(),
            physmap_bits: physmaps.bits(),
            vmap_bits: ends.bits(),
            reserved: 0,
        },
    })
}

/// The boot log's lines, as `boot/` prints them. They go only to the
/// `ramoops` record and the screen, which only a person reads; SPECULATION.md
/// §6 argues why that is not a leak to the programs the layout is hidden from.
fn report(choice: &Choice) {
    let kaslr = choice.kaslr;
    if kaslr.state != KASLR_MOVED {
        say!(
            "  kaslr    {}; kernel at its link address {:#x}",
            kaslr.state_text(),
            kaslr.link
        );
        return;
    }
    say!(
        "  kaslr    kernel at {:#x}, slide {:#x}, {} bits from {}",
        choice.kernel_virt,
        choice.slide(),
        kaslr.image_bits,
        kaslr.source_text()
    );
    say!(
        "  kaslr    direct map at {:#x}, {} bits; vmap arena top at {:#x}, {} bits",
        choice.physmap_base,
        kaslr.physmap_bits,
        kaslr.vmap_end,
        kaslr.vmap_bits
    );
}
