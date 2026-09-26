//! Where the kernel image, the direct map and the vmap arena go this boot.
//!
//! `docs/certification/SPECULATION.md` §6 is the argument; this is the
//! loader's half of it. Randomness comes from the best source there is, and
//! the boot log names it:
//!
//! 1. firmware's `EFI_RNG_PROTOCOL`, which every QEMU machine here offers
//!    (xtask attaches a virtio-rng device) and U-Boot offers where the board
//!    has a driver for its generator;
//! 2. the processor's `RDRAND`, on x86-64;
//! 3. a cycle counter, on every architecture: guessable by anyone who can
//!    estimate how long firmware took, so the log says it is not KASLR, and
//!    the kernel does not count it as randomised.
//!
//! With none of them, and with `nokaslr` on the command line, everything stays
//! at its fixed address and the log says why. A fixed-address kernel with no
//! fixups — one built `--mitigations off` — cannot move, and stays too.
//!
//! The kernel's image, direct map and arena each get their own random word,
//! so that one leaked address gives away one region, not three.

use ferrix_bootinfo::{
    KASLR_DECLINED, KASLR_FIXED_IMAGE, KASLR_MOVED, KASLR_NO_ENTROPY, Kaslr, LAYOUT, PAGE_SIZE,
    SOURCE_COUNTER, SOURCE_CPU_RNG, SOURCE_FIRMWARE_RNG,
};
use ferrix_elf::Elf;

use crate::arch;
use crate::console::println;
use crate::load::DirectMap;
use crate::services::{BootError, Result, Services};

/// Three random words, one per region, and where they came from.
#[derive(Clone, Copy, Debug)]
struct Randomness {
    words: [u64; 3],
    source: u32,
}

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

/// Whether the command line the loader read asks for no randomisation.
pub(crate) fn declined(cmdline: &str) -> bool {
    ferrix_bootinfo::flag_in(cmdline, "nokaslr")
}

/// The best randomness there is, or `None` for none at all.
fn gather(services: &Services) -> Option<Randomness> {
    if let Some(bytes) = services.firmware_random::<24>() {
        let word = |at: usize| {
            bytes
                .get(at..at + 8)
                .and_then(|slice| <[u8; 8]>::try_from(slice).ok())
                .map_or(0, u64::from_le_bytes)
        };
        return Some(Randomness {
            words: [word(0), word(8), word(16)],
            source: SOURCE_FIRMWARE_RNG,
        });
    }
    if let (Some(a), Some(b), Some(c)) =
        (arch::cpu_random(), arch::cpu_random(), arch::cpu_random())
    {
        return Some(Randomness {
            words: [a, b, c],
            source: SOURCE_CPU_RNG,
        });
    }
    // A counter is one number, spread over three words by a mixing function
    // so that their low bits differ: which spreads what little it has and
    // adds nothing to it.
    let count = arch::counter();
    (count != 0).then(|| Randomness {
        words: [mix(count, 1), mix(count, 2), mix(count, 3)],
        source: SOURCE_COUNTER,
    })
}

/// `SplitMix64`'s finaliser over `seed + round`.
const fn mix(seed: u64, round: u64) -> u64 {
    let mut z = seed.wrapping_add(round.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Decide where the image, `span` of it linked at `link`, the direct map and
/// the arena go, and say so.
///
/// `loader` is the loader's own image, which the direct map must not cover on
/// a machine where the loader maps itself inside the kernel's tree.
pub(crate) fn choose(
    services: &Services,
    elf: &Elf<'_>,
    (link, span): (u64, u64),
    direct: DirectMap,
    loader: (u64, u64),
    declined: bool,
) -> Result<Choice> {
    let fixed = |state| Choice {
        kernel_virt: link,
        physmap_base: LAYOUT.physmap_base,
        kaslr: Kaslr::fixed(&LAYOUT, state),
    };
    let choice = if !elf.is_relocatable() {
        fixed(KASLR_FIXED_IMAGE)
    } else if declined {
        fixed(KASLR_DECLINED)
    } else if let Some(randomness) = gather(services) {
        place(elf, (link, span), direct, loader, randomness)?
    } else {
        fixed(KASLR_NO_ENTROPY)
    };
    report(&choice);
    Ok(choice)
}

/// Pick a place for each region from `randomness`.
fn place(
    elf: &Elf<'_>,
    (link, span): (u64, u64),
    direct: DirectMap,
    (loader_base, loader_len): (u64, u64),
    randomness: Randomness,
) -> Result<Choice> {
    let granule = elf
        .fixup_granule()
        .map_err(|_| BootError::plain("the kernel's fixups are of a kind the loader cannot apply"))?
        .max(PAGE_SIZE);
    if link != LAYOUT.kernel_base {
        return Err(BootError::plain(
            "the kernel is not linked at the address the loader moves it from",
        ));
    }
    let [image_word, physmap_word, vmap_word] = randomness.words;

    let images = LAYOUT.kernel_slots(span, granule);
    let kernel_virt = images
        .pick(image_word)
        .ok_or_else(|| BootError::plain("the kernel image is too large to be moved"))?;

    // The loader's image needs keeping clear of only where the loader maps
    // itself in the kernel's tree, which is where its image is above the
    // split. If no place avoids it, the direct map goes anywhere, and the
    // switch runs from a trampoline as it did before the direct map moved.
    let loader_end = loader_base.saturating_add(loader_len);
    let every = LAYOUT.physmap_slots(direct.len, None);
    let physmaps = if loader_end > LAYOUT.user_end {
        let clear = LAYOUT.physmap_slots(direct.len, Some((loader_base, loader_end)));
        if clear.count() == 0 { every } else { clear }
    } else {
        every
    };
    let physmap_base = physmaps
        .pick(physmap_word)
        .ok_or_else(|| BootError::plain("the direct map has no place in its region"))?;

    let ends = LAYOUT.vmap_ends();
    let vmap_end = ends
        .pick(vmap_word)
        .ok_or_else(|| BootError::plain("the vmap arena has no place for its top"))?;

    Ok(Choice {
        kernel_virt,
        physmap_base,
        kaslr: Kaslr {
            link,
            vmap_end,
            state: KASLR_MOVED,
            source: randomness.source,
            image_bits: images.bits(),
            physmap_bits: physmaps.bits(),
            vmap_bits: ends.bits(),
            reserved: 0,
        },
    })
}

/// The boot log's line, which `xtask` reads the slide from.
///
/// Printed by the loader, on firmware's console, which only a person at the
/// serial line or the screen reads: the kernel keeps no log a program can read
/// back (`sys_syslog` returns nothing), so this is not a leak to the programs
/// the layout is hidden from. SPECULATION.md §6 argues it.
fn report(choice: &Choice) {
    let kaslr = choice.kaslr;
    if kaslr.state != KASLR_MOVED {
        println!(
            "  kaslr    {}; kernel at its link address {:#x}",
            kaslr.state_text(),
            kaslr.link
        );
        return;
    }
    println!(
        "  kaslr    kernel at {:#x}, slide {:#x}, {} bits from {}",
        choice.kernel_virt,
        choice.kernel_virt - kaslr.link,
        kaslr.image_bits,
        kaslr.source_text()
    );
    println!(
        "  kaslr    direct map at {:#x}, {} bits; vmap arena top at {:#x}, {} bits",
        choice.physmap_base, kaslr.physmap_bits, kaslr.vmap_end, kaslr.vmap_bits
    );
}
