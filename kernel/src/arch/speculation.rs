//! Side-channel defences: the part every architecture shares.
//!
//! `docs/certification/SPECULATION.md` is the argument; this is the
//! bookkeeping. Each architecture decides, on the boot processor, which
//! defences its processor needs and offers (`<arch>/speculation.rs`), applies
//! them there and on every secondary as it starts, and reads back what it
//! wrote. What is common is kept here:
//!
//! * [`nospec_index`] and [`nospec_below`], the bounds checks a mispredicted
//!   branch cannot see past, built on each architecture's one-instruction
//!   clamp;
//! * the record of what each processor applied, which the boot check
//!   compares across processors;
//! * the decision a switch of address space rests on: whether the processor
//!   is about to run a different program's code than the one it last ran,
//!   which is when the branch predictors have to be emptied of it.
//!
//! # The switch
//!
//! [`HARDENED`] is false only in a kernel built with
//! `--cfg ferrix_mitigations_off` (`cargo xtask --mitigations off`), and then
//! every defence here and in the architectures is compiled out: the clamps are
//! the identity, nothing is written to the processor, no barrier is issued,
//! and the entry paths' extra instructions are assembled away. The reference
//! configuration is the other setting.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use ferrix_sched::MAX_CPUS;

use super::machine_speculation as machine;

/// Whether this kernel carries its side-channel defences: every build but one
/// made with `--mitigations off`.
pub(crate) const HARDENED: bool = !cfg!(ferrix_mitigations_off);

/// `Some(index)` when `index < len`, clamped so that no mispredicted path can
/// use it to reach past `len`; `None` otherwise.
///
/// For an index a program chose, before it indexes anything: a system call
/// number, a slot in a table. What makes it more than `index < len` is
/// that on the path a mispredicted comparison takes, the value it returns is
/// zero rather than whatever the program asked for (Spectre variant 1).
#[inline(always)]
pub(crate) fn nospec_index(index: usize, len: usize) -> Option<usize> {
    if index >= len {
        return None;
    }
    Some(machine::clamp_index(index, len))
}

/// [`nospec_index`] for a 64-bit value below `end`: a user address checked
/// against the top of the user half.
#[inline(always)]
pub(crate) fn nospec_below(value: u64, end: u64) -> Option<u64> {
    if value >= end {
        return None;
    }
    Some(machine::clamp_below(value, end))
}

/// A set of defences, as bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Defences(u32);

impl Defences {
    /// Nothing.
    pub(crate) const NONE: Defences = Defences(0);
    /// Program-chosen indices clamped after their bounds check: every
    /// architecture, always, in a hardened build.
    pub(crate) const CLAMPED_INDICES: Defences = Defences(1 << 0);
    /// x86-64: a fence after the conditional `swapgs` on interrupt entry.
    pub(crate) const SWAPGS_FENCE: Defences = Defences(1 << 1);
    /// x86-64: a program's registers cleared on entry, before any Rust runs.
    pub(crate) const ENTRY_REGISTERS_CLEARED: Defences = Defences(1 << 2);
    /// x86-64: Intel's enhanced IBRS, set once.
    pub(crate) const IBRS_ENHANCED: Defences = Defences(1 << 3);
    /// x86-64: AMD's automatic IBRS, set once in `EFER`.
    pub(crate) const IBRS_AUTOMATIC: Defences = Defences(1 << 4);
    /// x86-64: IBRS on a processor that says it may simply be left on.
    pub(crate) const IBRS_ALWAYS_ON: Defences = Defences(1 << 5);
    /// x86-64: single-thread indirect branch predictors.
    pub(crate) const STIBP: Defences = Defences(1 << 6);
    /// Speculative store bypass disabled: `SSBD` on x86-64, `PSTATE.SSBS`
    /// clear on `AArch64`, or firmware's workaround 2.
    pub(crate) const SSBD: Defences = Defences(1 << 7);
    /// The branch predictors emptied when the processor switches to another
    /// program's address space: `IBPB` on x86-64, firmware's workaround 1 on
    /// `AArch64`, `BPIALL` or `ICIALLU` on ARMv7-A.
    pub(crate) const SWITCH_BARRIER: Defences = Defences(1 << 8);
    /// x86-64: the return stack buffer refilled at the same switch.
    pub(crate) const RSB_FILL: Defences = Defences(1 << 9);
    /// x86-64: `VERW` clears the processor's buffers on every return to
    /// ring 3 (MDS).
    pub(crate) const BUFFERS_CLEARED: Defences = Defences(1 << 10);
    /// `AArch64`: the branch history overwritten on every entry from EL0.
    pub(crate) const BHB_LOOP: Defences = Defences(1 << 11);
    /// A defence was written and did not read back: the boot check fails.
    pub(crate) const READ_BACK_FAILED: Defences = Defences(1 << 30);

    /// Every defence, with the name a boot log gives it.
    const NAMES: [(Defences, &'static str); 12] = [
        (Defences::CLAMPED_INDICES, "clamped indices"),
        (Defences::SWAPGS_FENCE, "SWAPGS fence"),
        (Defences::ENTRY_REGISTERS_CLEARED, "entry registers cleared"),
        (Defences::IBRS_ENHANCED, "eIBRS"),
        (Defences::IBRS_AUTOMATIC, "AutoIBRS"),
        (Defences::IBRS_ALWAYS_ON, "IBRS always-on"),
        (Defences::STIBP, "STIBP"),
        (Defences::SSBD, "SSBD"),
        (Defences::SWITCH_BARRIER, "predictor barrier on switch"),
        (Defences::RSB_FILL, "RSB fill on switch"),
        (Defences::BUFFERS_CLEARED, "VERW on exit"),
        (Defences::BHB_LOOP, "BHB loop on entry"),
    ];

    /// Whether every defence in `other` is in this set.
    pub(crate) const fn contains(self, other: Defences) -> bool {
        self.0 & other.0 == other.0
    }

    /// This set with `other` added.
    #[must_use]
    pub(crate) const fn with(self, other: Defences) -> Defences {
        Defences(self.0 | other.0)
    }

    /// This set, written as the boot log writes it: the names, comma
    /// separated, or `none`.
    pub(crate) fn names(self) -> impl core::fmt::Display {
        Names(self)
    }

    /// The bits, for a record kept in an atomic.
    pub(crate) const fn bits(self) -> u32 {
        self.0
    }

    /// The set [`Defences::bits`] made.
    pub(crate) const fn from_bits(bits: u32) -> Defences {
        Defences(bits)
    }
}

/// [`Defences::names`].
struct Names(Defences);

impl core::fmt::Display for Names {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;
        for (defence, name) in Defences::NAMES {
            if self.0.contains(defence) {
                if !first {
                    f.write_str(", ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        if first {
            f.write_str("none")?;
        }
        Ok(())
    }
}

/// Set in a processor's record once it has written one: a record of zero is
/// a processor that never got here.
const RECORDED: u32 = 1 << 31;

/// What each processor applied, by logical number, [`RECORDED`] included.
static APPLIED: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];

/// The user root each processor last ran a program in, by logical number:
/// zero before the first.
static LAST_ROOT: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// How many switch barriers have been issued, on every processor together.
static SWITCH_BARRIERS: AtomicU64 = AtomicU64::new(0);

/// The running processor's logical number, or zero for the boot processor
/// before it has a per-CPU record -- which it is, being processor zero.
fn this_cpu() -> usize {
    crate::smp::this_cpu().map_or(0, |cpu| cpu.logical)
}

/// Record what the running processor applied.
pub(crate) fn record_this_cpu(applied: Defences) {
    if let Some(slot) = APPLIED.get(this_cpu()) {
        slot.store(applied.bits() | RECORDED, Ordering::Release);
    }
}

/// What processor `logical` recorded, or `None` if it recorded nothing.
pub(crate) fn applied_by(logical: usize) -> Option<Defences> {
    let bits = APPLIED.get(logical)?.load(Ordering::Acquire);
    (bits & RECORDED != 0).then_some(Defences(bits & !RECORDED))
}

/// How many switch barriers have been issued so far.
pub(crate) fn switch_barriers() -> u64 {
    SWITCH_BARRIERS.load(Ordering::Relaxed)
}

/// Called by an architecture's `install_user_root` after it has written
/// `root`: issue the switch barrier if the processor last ran a program in a
/// different address space.
///
/// Keyed on the root table's address, per processor, so that a thread of the
/// same process coming back after the idle loop or a kernel thread costs
/// nothing -- which is most switches. The one way two programs can share a
/// root address is a root freed and reused, and [`forget_root`] closes that:
/// a new space's root is wiped from every processor's record before it is
/// ever installed.
pub(crate) fn entered_space(root: u64) {
    if !HARDENED {
        return;
    }
    let Some(last) = LAST_ROOT.get(this_cpu()) else {
        return;
    };
    if last.swap(root, Ordering::Relaxed) != root && machine::switch_barrier() {
        let _ = SWITCH_BARRIERS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Called by an architecture's `prepare_user_root` for a root that is about to
/// belong to a new address space: no processor may treat it as the space it
/// last ran.
pub(crate) fn forget_root(root: u64) {
    if !HARDENED {
        return;
    }
    for last in LAST_ROOT.iter().take(crate::smp::count()) {
        let _ = last.compare_exchange(root, 0, Ordering::Relaxed, Ordering::Relaxed);
    }
}
