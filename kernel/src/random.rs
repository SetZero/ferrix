//! The kernel's random numbers: what `getrandom`, `/dev/random`,
//! `/dev/urandom` and every program's `AT_RANDOM` read.
//!
//! `libs/crng` is the generator, `ChaCha20` with fast key erasure. This module
//! seeds it and serialises access to it.
//!
//! # What it is seeded from
//!
//! Three sources, mixed in at [`init`], in order of how much each is trusted:
//!
//! * **Firmware's `EFI_RNG_PROTOCOL`**, 32 bytes the loader asked for before it
//!   left boot services. Credited in full, 256 bits.
//! * **The CPU's random instruction**: `RDSEED`, or `RDRAND` without it, on
//!   x86-64; `RNDR` on AArch64. Eight 64-bit words, each credited at half, as
//!   Linux credits `RDRAND`: 256 bits for all eight.
//! * **Jitter**: the free-running counter read around work whose timing
//!   varies, which is the only source every machine has. Mixed in and credited
//!   nothing, because under an emulator it can be as regular as a metronome.
//!
//! Every [`fill`] also mixes in the counter's current value, which costs one
//! block and makes two boots with identical seeds part ways at the first read
//! whose timing differs.
//!
//! # Not seeded
//!
//! A machine with neither firmware nor CPU randomness gets a generator that is
//! **not seeded**, and the boot line says so. Linux would make `getrandom`
//! wait; that wait never ends on a machine with nothing to wait for, so here a
//! read is answered and the boot report is where the warning goes. A key
//! made on such a machine is only as good as its timer jitter.

use ferrix_bootinfo::{BootInfo, FIRMWARE_SEED};
use ferrix_crng::{Crng, KEY_BYTES};

use crate::arch;
use crate::sync::SpinLock;

/// The one generator.
static RNG: SpinLock<Crng> = SpinLock::new(Crng::new());

/// Most bytes produced under the lock at once, so that a program reading a
/// megabyte from `/dev/urandom` does not hold every other reader off for the
/// length of it.
const CHUNK: usize = 256;

/// Counter reads mixed in at [`init`] as jitter.
const JITTER_SAMPLES: usize = 256;

/// Words asked of the CPU's random instruction at [`init`].
const CPU_WORDS: usize = 8;

/// What [`init`] found, for the boot line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seeding {
    /// Whether firmware gave bytes.
    pub(crate) firmware: bool,
    /// How many words the CPU gave.
    pub(crate) cpu_words: usize,
    /// Bits credited in all.
    pub(crate) credited: u32,
    /// Whether that is enough to call the generator seeded.
    pub(crate) seeded: bool,
}

/// Seed the generator from what the loader handed over, the CPU and jitter.
pub(crate) fn init(info: &BootInfo) -> Seeding {
    let mut rng = RNG.lock();

    let firmware = info.firmware_flags & FIRMWARE_SEED != 0;
    if firmware {
        rng.mix(&info.firmware_seed);
        rng.credit(256);
    }

    let mut cpu_words = 0;
    for _ in 0..CPU_WORDS {
        if let Some(word) = arch::hardware_random() {
            rng.mix(&word.to_le_bytes());
            rng.credit(32);
            cpu_words += 1;
        }
    }

    // Jitter: the time a block of the generator itself takes, read on the
    // counter, which varies with caches and interrupts on a real machine.
    let mut samples = [0_u8; KEY_BYTES];
    let mut scratch = [0_u8; KEY_BYTES];
    for index in 0..JITTER_SAMPLES {
        let before = arch::counter_now();
        rng.fill(&mut scratch);
        let delta = arch::counter_now().wrapping_sub(before);
        if let Some(slot) = samples.get_mut(index % KEY_BYTES) {
            *slot ^= delta.to_le_bytes().iter().fold(0, |acc, byte| acc ^ byte);
        }
    }
    rng.mix(&samples);
    rng.mix(&arch::counter_now().to_le_bytes());

    Seeding {
        firmware,
        cpu_words,
        credited: rng.credited(),
        seeded: rng.seeded(),
    }
}

/// Fill `bytes` from the generator.
pub(crate) fn fill(bytes: &mut [u8]) {
    for piece in bytes.chunks_mut(CHUNK) {
        let mut rng = RNG.lock();
        rng.mix(&arch::counter_now().to_le_bytes());
        rng.fill(piece);
    }
}

/// The boot check: two reads differ from each other and from zero, and the
/// generator's state has moved between them.
pub(crate) fn check() -> Result<(), &'static str> {
    let mut first = [0_u8; 64];
    let mut second = [0_u8; 64];
    fill(&mut first);
    fill(&mut second);
    if first == second {
        return Err("two reads of the random generator were the same");
    }
    if first.iter().all(|&byte| byte == 0) {
        return Err("a read of the random generator was all zeros");
    }
    Ok(())
}
