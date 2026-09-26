//! Bounds checks a mispredicted branch cannot see past.
//!
//! `slots.get(index)` is a comparison, a branch and a load. A processor that
//! predicts the branch taken runs the load before the comparison resolves, and
//! with an `index` a program chose that load can reach anything past the
//! table: Spectre variant 1. Nothing architectural happens -- the load is
//! thrown away when the branch resolves -- but what it touched stays in the
//! cache, where a program can time it.
//!
//! The defence is Linux's `array_index_nospec`: after the ordinary check, clamp
//! the index with *arithmetic* the processor cannot predict, so that on the
//! mispredicted path the load reads slot zero rather than wherever the program
//! aimed. [`bounded`] is that, for the tables that live in the host-testable
//! libraries -- the native ABI's handle table in `ferrix-objects` and the
//! descriptor table in `ferrix-vfs` -- which cannot use the kernel's own
//! version: that one is a line of assembly per architecture in
//! `kernel/src/arch/*/speculation.rs`, and a library here may contain neither
//! assembly nor an architecture conditional.
//!
//! # What makes this arithmetic rather than a branch
//!
//! The compiler, not the processor, is the first threat. Inside
//! `if index < len { .. }` it knows the comparison is true, and a clamp
//! written naively is folded away to nothing. So both operands go through
//! [`core::hint::black_box`] first: the comparison below is made on values the
//! optimiser cannot relate to the check above, and the mask it produces is a
//! data dependency of the load rather than a second branch. The kernel's
//! build was disassembled to confirm that is what comes out -- `cmp` and
//! `cmovae` on x86-64, `cmp` and `csel` on `AArch64`, with no branch between
//! the clamp and the load -- and `docs/certification/SPECULATION.md` §2
//! records it.
//!
//! What the pure-Rust form cannot do is issue Arm's `CSDB`, which forbids the
//! processor to *predict the value* of a conditional select. The kernel's own
//! sites, which can issue it, do; these two are recorded as a residual.
//!
//! # The switch
//!
//! Built with `--cfg ferrix_mitigations_off` -- `cargo xtask --mitigations off`
//! -- the clamp is the identity and the check above it is all that is left.
//! That is the whole of the off setting's effect here.

/// `Some(index)` when `index < len`, clamped so that no mispredicted path can
/// use it to reach past `len`; `None` otherwise.
///
/// Use it in place of an `index < len` test whose index a program chose, and
/// index with what it returns.
#[inline]
#[must_use]
pub fn bounded(index: usize, len: usize) -> Option<usize> {
    if index >= len {
        return None;
    }
    Some(clamp(index, len))
}

/// `index` if `index < len`, else zero, computed without a branch.
#[cfg(not(ferrix_mitigations_off))]
#[inline(always)]
fn clamp(index: usize, len: usize) -> usize {
    let (index, len) = core::hint::black_box((index, len));
    // All ones when inside, all zeros when not.
    let mask = 0_usize.wrapping_sub(usize::from(index < len));
    index & mask
}

/// The off setting: the ordinary check alone.
#[cfg(ferrix_mitigations_off)]
#[inline(always)]
fn clamp(index: usize, _len: usize) -> usize {
    index
}

#[cfg(test)]
mod tests {
    use super::bounded;

    #[test]
    fn inside_is_itself_and_outside_is_refused() {
        assert_eq!(bounded(0, 1), Some(0));
        assert_eq!(bounded(6, 7), Some(6));
        assert_eq!(bounded(7, 7), None);
        assert_eq!(bounded(usize::MAX, 7), None);
        assert_eq!(bounded(0, 0), None);
        assert_eq!(bounded(usize::MAX - 1, usize::MAX), Some(usize::MAX - 1));
    }

    #[test]
    fn the_clamp_alone_zeroes_what_is_outside() {
        // The path a misprediction takes: the check skipped, the clamp run.
        assert_eq!(super::clamp(9, 4), 0);
        assert_eq!(super::clamp(usize::MAX, 4), 0);
        assert_eq!(super::clamp(3, 4), 3);
    }
}
