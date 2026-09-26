//! Entropy from firmware's True Random Number Generator, through SMCCC.
//!
//! Arm's TRNG firmware interface (DEN0098) lets EL3 firmware hand out
//! conditioned, full-entropy bits from a hardware source that EL1 cannot
//! reach itself. The Pixel 7's cores have no `RNDR`, and the TRNG on its
//! chip is a security block, but TF-A answers `TRNG_RND64`: it is what
//! Android's `smccc_trng` driver reads. So the kernel asks the same way, and
//! never touches the block's registers.
//!
//! Every call here is a query or a read, and changes nothing that survives a
//! reset. They are asked in the order that makes each safe to ask: PSCI 1.0
//! first, then `PSCI_FEATURES` for `SMCCC_VERSION`, since an SMCCC call no
//! firmware implements is an undefined instruction behind an `hvc` with no EL2,
//! then SMCCC 1.1, which the TRNG interface needs, then `TRNG_VERSION` and
//! `TRNG_FEATURES` for `TRNG_RND64`. Any answer short of that is no entropy,
//! not a failure.

use ferrix_bootinfo::BootView;

use super::cpu;
use super::smp::{self, Conduit};

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
/// `TRNG_RND64`: up to 192 bits in `x1`-`x3`, the lowest 64 in `x3`.
const TRNG_RND64: u64 = 0xC400_0053;

/// The most bits one `TRNG_RND64` gives.
const BITS_PER_CALL: u64 = 192;

/// `TRNG_RND64`'s status when the source has nothing ready yet.
const NO_ENTROPY: i32 = -3;

/// How many times a call that found no entropy ready is asked again.
const ATTEMPTS: usize = 8;

/// Fill `out` with firmware's full-entropy bytes, and say how many it filled:
/// 0 on a machine whose firmware has no TRNG, or that says nothing.
pub(crate) fn fill(view: &BootView<'_>, out: &mut [u8]) -> usize {
    let Ok(conduit) = smp::psci_conduit(view) else {
        return 0;
    };
    if !offers_rnd64(conduit) {
        return 0;
    }
    let mut filled = 0;
    for chunk in out.chunks_mut((BITS_PER_CALL / 8) as usize) {
        let Some(bits) = rnd64(conduit) else {
            break;
        };
        let bytes = [bits[2], bits[1], bits[0]].map(u64::to_le_bytes);
        for (slot, byte) in chunk.iter_mut().zip(bytes.iter().flatten()) {
            *slot = *byte;
        }
        filled += chunk.len();
    }
    filled
}

/// Whether firmware, through `conduit`, offers `TRNG_RND64`.
fn offers_rnd64(conduit: Conduit) -> bool {
    let status = |function, argument| call(conduit, function, argument)[0] as u32 as i32;
    if status(PSCI_VERSION, 0) < 0x1_0000 || status(PSCI_FEATURES, SMCCC_VERSION) < 0 {
        return false;
    }
    if status(SMCCC_VERSION, 0) < 0x1_0001 {
        return false;
    }
    status(TRNG_VERSION, 0) >= 0x1_0000 && status(TRNG_FEATURES, TRNG_RND64) >= 0
}

/// One `TRNG_RND64` for all 192 bits, asked again while firmware reports none
/// ready: `x1`, `x2`, `x3`.
fn rnd64(conduit: Conduit) -> Option<[u64; 3]> {
    for _ in 0..ATTEMPTS {
        let [status, high, middle, low] = call(conduit, TRNG_RND64, BITS_PER_CALL);
        match status as u32 as i32 {
            0 => return Some([high, middle, low]),
            NO_ENTROPY => {}
            _ => return None,
        }
    }
    None
}

/// One SMCCC call through `conduit`, with its four result registers.
fn call(conduit: Conduit, function: u64, argument: u64) -> [u64; 4] {
    match conduit {
        // SAFETY: every function this module calls is a query or a read
        // SMCCC defines to change no state, and SMCCC is asked for only once
        // PSCI has said it is there to answer.
        Conduit::Hvc => unsafe { cpu::hvc_call_x0_x3(function, argument, 0, 0) },
        // SAFETY: as above.
        Conduit::Smc => unsafe { cpu::smc_call_x0_x3(function, argument, 0, 0) },
    }
}
