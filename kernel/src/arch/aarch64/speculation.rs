//! `AArch64`'s side-channel defences.
//!
//! `docs/certification/SPECULATION.md` §4 argues the set; this applies it.
//! Decided on the boot processor from its ID registers and what firmware
//! says through SMCCC, applied there and on every secondary as it starts.
//!
//! | Hazard | Defence here | When |
//! |---|---|---|
//! | Spectre v1 | indices clamped with `csel` and `csdb` | always |
//! | Spectre v2 | firmware's `ARCH_WORKAROUND_1` when the processor switches address space | the core lacks `CSV2` and firmware offers it |
//! | Spectre-BHB | the branch history overwritten by a loop on every entry from EL0 | the core is on Arm's list, and lacks `ECBHB` |
//! | Speculative store bypass | `PSTATE.SSBS` clear in EL1 (`SCTLR_EL1.DSSBS`) and for a program's first instruction; firmware's `ARCH_WORKAROUND_2` without `SSBS` | the core has `SSBS`, or firmware offers the workaround |
//! | Meltdown | none: KPTI is not built | reported, and excluded by AoU-11 |
//!
//! The reference machine's Cortex-A72 needs the BHB loop (eight branches),
//! has neither `SSBS` nor `CSV2`, and under QEMU has no firmware to offer
//! either workaround: its boot log says so. Arm lists it as unaffected by
//! Meltdown.

use core::arch::asm;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

use ferrix_bootinfo::BootView;

use super::cpu;
use crate::arch::speculation::{Defences, HARDENED, applied_by, record_this_cpu};
use crate::console::println;

/// `SCTLR_EL1.DSSBS`: the value `PSTATE.SSBS` takes on an exception to EL1.
const SCTLR_DSSBS: u64 = 1 << 44;

/// SMCCC and PSCI function numbers.
mod smccc {
    /// `PSCI_VERSION`.
    pub(super) const PSCI_VERSION: u64 = 0x8400_0000;
    /// `PSCI_FEATURES`.
    pub(super) const PSCI_FEATURES: u64 = 0x8400_000A;
    /// `SMCCC_VERSION`.
    pub(super) const VERSION: u64 = 0x8000_0000;
    /// `SMCCC_ARCH_FEATURES`.
    pub(super) const ARCH_FEATURES: u64 = 0x8000_0001;
    /// `SMCCC_ARCH_WORKAROUND_1`: invalidate the branch predictor.
    pub(super) const WORKAROUND_1: u64 = 0x8000_8000;
    /// `SMCCC_ARCH_WORKAROUND_2`: turn the store bypass mitigation on or off.
    pub(super) const WORKAROUND_2: u64 = 0x8000_7FFF;
}

/// How firmware is reached, for the workarounds: none, `hvc` or `smc`.
static CONDUIT: AtomicU8 = AtomicU8::new(CONDUIT_NONE);
/// No firmware workaround is to be called.
const CONDUIT_NONE: u8 = 0;
/// Through `hvc`.
const CONDUIT_HVC: u8 = 1;
/// Through `smc`.
const CONDUIT_SMC: u8 = 2;

/// Whether a switch of address space calls `ARCH_WORKAROUND_1`.
static SWITCH_WORKAROUND: AtomicU8 = AtomicU8::new(0);
/// Whether each processor calls `ARCH_WORKAROUND_2` to turn the store bypass
/// mitigation on.
static FIRMWARE_SSBD: AtomicU8 = AtomicU8::new(0);
/// The boot processor's plan, which each secondary adjusts to its own core.
static PLAN_DEFENCES: AtomicU32 = AtomicU32::new(0);

/// How many branches the entry loop takes: the largest any processor needs,
/// zero for none. Read from assembly by every vector entry from EL0.
pub(super) static BHB_LOOPS: AtomicU64 = AtomicU64::new(0);

/// Whether the entry paths' extra instructions are assembled in: `1` or `0`,
/// for an assembler `.if`.
pub(super) const ENTRY_HARDENING: u8 = HARDENED as u8;

/// What the ID registers say about this core.
#[derive(Debug, Clone, Copy)]
struct Core {
    /// `MIDR_EL1`.
    midr: u64,
    /// `ID_AA64PFR0_EL1.CSV2` is set: branch targets cannot be trained
    /// across contexts.
    csv2: bool,
    /// `ID_AA64PFR0_EL1.CSV3` is set: not vulnerable to Meltdown.
    csv3: bool,
    /// `ID_AA64PFR1_EL1.SSBS`: 0, 1 (the bit exists), or 2 (and `MSR SSBS`).
    ssbs: u64,
    /// `ID_AA64MMFR1_EL1.ECBHB`: the branch history is not shared.
    ecbhb: bool,
}

impl Core {
    /// This core's registers.
    fn read() -> Core {
        let pfr0 = cpu::read_id_aa64pfr0();
        Core {
            midr: read_midr(),
            csv2: (pfr0 >> 56) & 0xF != 0,
            csv3: (pfr0 >> 60) & 0xF != 0,
            ssbs: (read_id_aa64pfr1() >> 4) & 0xF,
            ecbhb: (read_id_aa64mmfr1() >> 60) & 0xF != 0,
        }
    }

    /// Arm's part number, if Arm designed the core.
    const fn arm_part(self) -> Option<u64> {
        if (self.midr >> 24) & 0xFF == 0x41 {
            Some((self.midr >> 4) & 0xFFF)
        } else {
            None
        }
    }

    /// The branches the BHB loop must take on this core: Arm's figures for
    /// each part it lists, zero where the history is not shared or the part is
    /// not listed. Linux's `spectre_bhb_loop_affected`.
    fn bhb_loops(self) -> u64 {
        if self.ecbhb {
            return 0;
        }
        match self.arm_part() {
            // Cortex-X3, Neoverse V2.
            Some(0xD4E | 0xD4F) => 132,
            // Cortex-A715, Cortex-A720.
            Some(0xD4D | 0xD81) => 38,
            // Cortex-A78, A78AE, A78C, X1, A710, X2, Neoverse N2, V1.
            Some(0xD41 | 0xD42 | 0xD4B | 0xD44 | 0xD47 | 0xD48 | 0xD49 | 0xD40) => 32,
            // Cortex-A76, A77, A76AE, Neoverse N1.
            Some(0xD0B..=0xD0E) => 24,
            // Cortex-A57, A72.
            Some(0xD07 | 0xD08) => 8,
            _ => 0,
        }
    }

    /// Whether the core is one Arm lists as not vulnerable to Meltdown, or
    /// says so itself. Linux's `kpti_safe_list`, for Arm's own parts: every one
    /// but the Cortex-A75.
    fn meltdown_safe(self) -> bool {
        self.csv3 || matches!(self.arm_part(), Some(part) if part != 0xD0A)
    }

    /// Whether the core is one of Arm's in-order designs, which speculate too
    /// little to bypass a store: A35, A53, A55, A510, A520.
    fn in_order(self) -> bool {
        matches!(self.arm_part(), Some(0xD04 | 0xD03 | 0xD05 | 0xD46 | 0xD80))
    }
}

/// What firmware offers, through SMCCC.
#[derive(Debug, Clone, Copy, Default)]
struct Firmware {
    /// `ARCH_WORKAROUND_1` is implemented and needed on this core.
    workaround_1: bool,
    /// `ARCH_WORKAROUND_2` is implemented, needed, and can be turned on.
    workaround_2: bool,
    /// `ARCH_WORKAROUND_2` says this core does not need it.
    ssb_not_required: bool,
}

/// Ask firmware, through `conduit`, what it offers.
///
/// Only as far as PSCI says it is safe to: an SMCCC call that firmware does
/// not implement is an undefined instruction on a machine with no EL2 behind
/// `hvc`, so SMCCC is asked for only once `PSCI_FEATURES` has said it exists.
fn ask_firmware(conduit: u8) -> Firmware {
    let call = |function, argument| firmware_call(conduit, function, argument) as u32 as i32;
    let psci = call(smccc::PSCI_VERSION, 0);
    if psci < 0x1_0000 || call(smccc::PSCI_FEATURES, smccc::VERSION) < 0 {
        return Firmware::default();
    }
    if call(smccc::VERSION, 0) < 0x1_0001 {
        return Firmware::default();
    }
    let second = call(smccc::ARCH_FEATURES, smccc::WORKAROUND_2);
    Firmware {
        workaround_1: call(smccc::ARCH_FEATURES, smccc::WORKAROUND_1) == 0,
        workaround_2: second == 0,
        ssb_not_required: second == 1,
    }
}

/// One SMCCC call through `conduit`; `u64::MAX` (an error) with none.
fn firmware_call(conduit: u8, function: u64, argument: u64) -> u64 {
    match conduit {
        // SAFETY: every function this module calls is a query or a
        // workaround SMCCC defines to change no state a caller relies on,
        // asked for only after PSCI said SMCCC is there to answer.
        CONDUIT_HVC => unsafe { cpu::hvc_call(function, argument, 0, 0) },
        // SAFETY: as above.
        CONDUIT_SMC => unsafe { cpu::smc_call(function, argument, 0, 0) },
        _ => u64::MAX,
    }
}

/// Decide, apply on the boot processor, and say what was done.
pub(crate) fn init(view: &BootView<'_>) {
    if !HARDENED {
        println!("  cpu      speculation defences off: built with --mitigations off");
        record_this_cpu(Defences::NONE);
        return;
    }
    let core = Core::read();
    let conduit = match super::smp::psci_conduit(view) {
        Ok(super::smp::Conduit::Hvc) => CONDUIT_HVC,
        Ok(super::smp::Conduit::Smc) => CONDUIT_SMC,
        Err(_) => CONDUIT_NONE,
    };
    let firmware = ask_firmware(conduit);
    CONDUIT.store(conduit, Ordering::Relaxed);

    let mut plan = Defences::CLAMPED_INDICES;
    if !core.csv2 && firmware.workaround_1 {
        SWITCH_WORKAROUND.store(1, Ordering::Relaxed);
        plan = plan.with(Defences::SWITCH_BARRIER);
    }
    if core.bhb_loops() > 0 {
        plan = plan.with(Defences::BHB_LOOP);
    }
    if core.ssbs > 0 {
        plan = plan.with(Defences::SSBD);
    } else if firmware.workaround_2 {
        FIRMWARE_SSBD.store(1, Ordering::Relaxed);
        plan = plan.with(Defences::SSBD);
    }
    PLAN_DEFENCES.store(plan.bits(), Ordering::Release);

    let applied = apply(plan, core);
    record_this_cpu(applied);
    println!("  cpu      speculation defences: {}", applied.names());
    report_exposure(core, &firmware, plan);
}

/// Say what the plan leaves uncovered, and why.
fn report_exposure(core: Core, firmware: &Firmware, plan: Defences) {
    let v2 = if core.csv2 {
        "not affected (CSV2)"
    } else if plan.contains(Defences::SWITCH_BARRIER) {
        "covered between programs"
    } else {
        "NOT covered: no CSV2, and firmware offers no ARCH_WORKAROUND_1 (AoU-11)"
    };
    let bhb = if plan.contains(Defences::BHB_LOOP) {
        "covered"
    } else if core.ecbhb {
        "not affected (ECBHB)"
    } else {
        "not affected (not on Arm's list)"
    };
    let bypass = if plan.contains(Defences::SSBD) {
        "covered"
    } else if firmware.ssb_not_required || core.in_order() {
        "not affected"
    } else {
        "NOT covered: no SSBS, and firmware offers no ARCH_WORKAROUND_2 (AoU-11)"
    };
    let meltdown = if core.meltdown_safe() {
        "not affected"
    } else {
        "EXPOSED: no CSV3 on a core Arm does not list as safe, and KPTI is not built (AoU-11)"
    };
    println!(
        "  cpu      speculation exposure: Spectre v2 {v2}; Spectre-BHB {bhb}; store bypass \
         {bypass}; Meltdown {meltdown}"
    );
}

/// Apply `plan` on this core, read back what can be, and say what took.
fn apply(plan: Defences, core: Core) -> Defences {
    let mut held = true;
    let _ = BHB_LOOPS.fetch_max(core.bhb_loops(), Ordering::Relaxed);
    if plan.contains(Defences::SSBD) && core.ssbs > 0 {
        let sctlr = cpu::read_sctlr();
        // SAFETY: clearing DSSBS changes only the value `PSTATE.SSBS` takes
        // on an exception to EL1, on a core whose ID register says it exists.
        unsafe { write_sctlr(sctlr & !SCTLR_DSSBS) };
        held &= cpu::read_sctlr() & SCTLR_DSSBS == 0;
        if core.ssbs >= 2 {
            clear_ssbs();
        }
    }
    if plan.contains(Defences::SSBD) && FIRMWARE_SSBD.load(Ordering::Relaxed) != 0 {
        let _ = firmware_call(CONDUIT.load(Ordering::Relaxed), smccc::WORKAROUND_2, 1);
    }
    if held {
        plan
    } else {
        plan.with(Defences::READ_BACK_FAILED)
    }
}

/// Apply the boot processor's plan on a secondary, as it starts, adjusted to
/// this core.
///
/// The cores of one machine need not be alike: a Pixel 7 boots on a
/// Cortex-A55, which needs no branch history loop, and starts two A78s and two
/// X1s, which need 32 branches of it. So what depends on the core -- the loop
/// and `SSBS` -- is decided here, per core, in both directions: a little core
/// may drop what the boot processor has, and a big one add what it lacks. What
/// depends on firmware stays the boot processor's decision, since firmware is
/// the same for every core.
pub(crate) fn apply_this_cpu() {
    if !HARDENED {
        record_this_cpu(Defences::NONE);
        return;
    }
    let core = Core::read();
    let plan = for_core(
        Defences::from_bits(PLAN_DEFENCES.load(Ordering::Acquire)),
        core,
    );
    record_this_cpu(apply(plan, core));
}

/// `plan` with what depends on the core decided for `core`.
fn for_core(plan: Defences, core: Core) -> Defences {
    let without =
        |plan: Defences, defence: Defences| Defences::from_bits(plan.bits() & !defence.bits());
    let plan = if core.bhb_loops() > 0 {
        plan.with(Defences::BHB_LOOP)
    } else {
        without(plan, Defences::BHB_LOOP)
    };
    if core.ssbs > 0 || FIRMWARE_SSBD.load(Ordering::Relaxed) != 0 {
        plan.with(Defences::SSBD)
    } else {
        without(plan, Defences::SSBD)
    }
}

/// Issue the switch barrier: firmware's `ARCH_WORKAROUND_1`, which
/// invalidates the branch predictor, where the plan has it. Answers whether
/// it was issued.
pub(crate) fn switch_barrier() -> bool {
    if SWITCH_WORKAROUND.load(Ordering::Relaxed) == 0 {
        return false;
    }
    let _ = firmware_call(CONDUIT.load(Ordering::Relaxed), smccc::WORKAROUND_1, 0);
    true
}

/// What the boot check adds on this architecture: that the entry loop runs
/// exactly when some processor recorded that it needs it.
///
/// The count is one for the whole machine -- every vector entry reads the same
/// word -- so it is held to every processor's record, not to the boot
/// processor's plan, which on a machine of mixed cores says nothing about the
/// big ones.
///
/// # Errors
///
/// A processor that needs the loop and no count, or a count that no processor
/// asked for.
pub(crate) fn check() -> Result<(), &'static str> {
    let needed = (0..crate::smp::count()).any(|logical| {
        applied_by(logical).is_some_and(|applied| applied.contains(Defences::BHB_LOOP))
    });
    if needed != (BHB_LOOPS.load(Ordering::Relaxed) != 0) {
        return Err("the branch history loop's count disagrees with what the processors recorded");
    }
    Ok(())
}

/// `index` if `index < len`, else zero: `csel` on the flags `cmp` set, and
/// `csdb` so that no later instruction may use a *predicted* result of the
/// select.
#[inline(always)]
pub(crate) fn clamp_index(index: usize, len: usize) -> usize {
    if !HARDENED {
        return index;
    }
    let clamped: usize;
    // SAFETY: three register instructions; no memory, no stack.
    unsafe {
        asm!(
            "cmp {index}, {len}",
            "csel {clamped}, {index}, xzr, lo",
            "csdb",
            index = in(reg) index,
            len = in(reg) len,
            clamped = lateout(reg) clamped,
            options(pure, nomem, nostack),
        );
    }
    clamped
}

/// [`clamp_index`], for a 64-bit value: the same thing on this machine.
#[inline(always)]
pub(crate) fn clamp_below(value: u64, end: u64) -> u64 {
    clamp_index(value as usize, end as usize) as u64
}

/// `MIDR_EL1`: who designed the core, and which part it is.
fn read_midr() -> u64 {
    let value: u64;
    // SAFETY: an ID register read, legal at EL1 on every core.
    unsafe { asm!("mrs {}, midr_el1", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// `ID_AA64PFR1_EL1`, whose `SSBS` field says whether the store bypass bit
/// exists.
fn read_id_aa64pfr1() -> u64 {
    let value: u64;
    // SAFETY: as `read_midr`.
    unsafe {
        asm!("mrs {}, id_aa64pfr1_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// `ID_AA64MMFR1_EL1`, whose `ECBHB` field says whether the branch history
/// is shared across contexts.
fn read_id_aa64mmfr1() -> u64 {
    let value: u64;
    // SAFETY: as `read_midr`.
    unsafe {
        asm!("mrs {}, id_aa64mmfr1_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Write `SCTLR_EL1`, and synchronise.
///
/// # Safety
///
/// `value` must be the register's current value with only bits the caller
/// has argued changed.
unsafe fn write_sctlr(value: u64) {
    // SAFETY: the caller guarantees the value.
    unsafe { asm!("msr sctlr_el1, {}", "isb", in(reg) value, options(nostack, preserves_flags)) };
}

/// Clear `PSTATE.SSBS` now, on a core with `MSR SSBS`: the running context
/// took its value before `DSSBS` was cleared. Spelt as the register's
/// encoding, `S3_3_C4_C2_6`, which the assembler takes without a feature
/// flag.
fn clear_ssbs() {
    // SAFETY: writes one `PSTATE` bit that governs only speculation, on a
    // core whose ID register says the instruction exists.
    unsafe {
        asm!(
            "msr s3_3_c4_c2_6, xzr",
            options(nomem, nostack, preserves_flags)
        );
    };
}
