//! Choosing a local port nobody else has.
//!
//! The range is Linux's default `ip_local_port_range`, and the choice inside
//! it is random rather than sequential: a sequential port is the second half
//! of a guess an off-path attacker needs, and RFC 6056 is the argument for
//! not making it easy. A port already in use is skipped, and after enough
//! collisions the caller is told there are none rather than looped for ever.

use crate::rand::Random;

/// The lowest port chosen for a socket that did not name one.
pub const FIRST: u16 = 32_768;

/// The highest.
pub const LAST: u16 = 60_999;

/// How many candidates are tried before the range is declared full.
const ATTEMPTS: u32 = 128;

/// Choose a port in the ephemeral range that `taken` says is free.
///
/// Answers `None` when enough candidates in a row were taken, which on a
/// range of twenty-eight thousand ports means the machine has run out.
pub fn choose(random: &mut Random, mut taken: impl FnMut(u16) -> bool) -> Option<u16> {
    for _ in 0..ATTEMPTS {
        let candidate = random.in_range(FIRST, LAST);
        if !taken(candidate) {
            return Some(candidate);
        }
    }
    // The random walk found nothing. Sweep, so that a nearly full range is
    // answered honestly rather than by luck.
    (FIRST..=LAST).find(|port| !taken(*port))
}
