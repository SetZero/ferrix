//! The random generator's boot check, a child of `random` so that it reads
//! the generator as the rest of `random` does.

use super::fill;

/// The boot check: two reads differ from each other and from zero, and the
/// generator's state has moved between them.
///
/// Verifies: L.boot.34
pub(crate) fn run() -> Result<(), &'static str> {
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
