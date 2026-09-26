//! x86-64's side-channel defences.
//!
//! `docs/certification/SPECULATION.md` §3 argues the set; this applies it.
//! Decided once, on the boot processor, from `CPUID` and
//! `IA32_ARCH_CAPABILITIES`; applied there and on every secondary as it
//! starts; read back on each.
//!
//! | Hazard | Defence here | When |
//! |---|---|---|
//! | Spectre v1 | indices clamped (`cmp`/`sbb`), a fence after the conditional `swapgs`, a program's registers cleared on entry | always |
//! | Spectre v2, program to kernel | enhanced IBRS, AMD's automatic IBRS, or IBRS left on where the processor says it may be | whichever it offers |
//! | Spectre v2, program to program | `IBPB` and a return stack refill when a processor switches address space; `STIBP` where eIBRS does not already give it | where offered |
//! | Speculative store bypass | `SSBD`, through `IA32_SPEC_CTRL` or AMD's virtual register | unless `SSB_NO` |
//! | MDS | `VERW` on every return to ring 3 | Intel parts with `MD_CLEAR` and without `MDS_NO` |
//! | Meltdown, L1TF | none: KPTI is not built | reported, and excluded by the safety manual's AoU-11 |
//!
//! What a processor that offers none of the IBRS forms gets is a line in the
//! boot log saying so. Retpolines would be the software answer, and the
//! pinned stable compiler has them only through a deprecated target feature
//! that is scheduled to become an error, and not in the precompiled `core`
//! and `alloc` the kernel links -- §3 has the details.

use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use super::cpu;
use crate::arch::speculation::{Defences, HARDENED, record_this_cpu};
use crate::console::println;

/// `IA32_SPEC_CTRL`: IBRS, STIBP and SSBD.
const IA32_SPEC_CTRL: u32 = 0x48;
/// `IA32_PRED_CMD`: writing [`PRED_CMD_IBPB`] empties the indirect predictors.
const IA32_PRED_CMD: u32 = 0x49;
/// `IA32_ARCH_CAPABILITIES`: what the processor is not vulnerable to.
const IA32_ARCH_CAPABILITIES: u32 = 0x10A;
/// AMD's `VIRT_SPEC_CTRL`, where a hypervisor offers SSBD without
/// `IA32_SPEC_CTRL`.
const AMD_VIRT_SPEC_CTRL: u32 = 0xC001_011F;
/// `IA32_EFER`.
const IA32_EFER: u32 = 0xC000_0080;

/// `IA32_SPEC_CTRL.IBRS`.
const SPEC_CTRL_IBRS: u64 = 1 << 0;
/// `IA32_SPEC_CTRL.STIBP`.
const SPEC_CTRL_STIBP: u64 = 1 << 1;
/// `IA32_SPEC_CTRL.SSBD`, and the same bit of AMD's virtual register.
const SPEC_CTRL_SSBD: u64 = 1 << 2;
/// `IA32_PRED_CMD.IBPB`.
const PRED_CMD_IBPB: u64 = 1 << 0;
/// `EFER.AIBRSE`: AMD's automatic IBRS.
const EFER_AUTO_IBRS: u64 = 1 << 21;

/// `IA32_ARCH_CAPABILITIES.RDCL_NO`: not vulnerable to Meltdown.
const CAP_RDCL_NO: u64 = 1 << 0;
/// `IA32_ARCH_CAPABILITIES.IBRS_ALL`: enhanced IBRS.
const CAP_IBRS_ALL: u64 = 1 << 1;
/// `IA32_ARCH_CAPABILITIES.SSB_NO`: not vulnerable to store bypass.
const CAP_SSB_NO: u64 = 1 << 4;
/// `IA32_ARCH_CAPABILITIES.MDS_NO`: not vulnerable to MDS.
const CAP_MDS_NO: u64 = 1 << 5;

/// What the processor offers and says about itself.
#[derive(Debug, Clone, Copy, Default)]
struct Offered {
    /// `GenuineIntel`.
    intel: bool,
    /// `AuthenticAMD` or `HygonGenuine`.
    amd: bool,
    /// `IA32_SPEC_CTRL` exists and takes IBRS.
    ibrs: bool,
    /// `IA32_PRED_CMD` takes IBPB.
    ibpb: bool,
    /// `IA32_SPEC_CTRL` takes STIBP.
    stibp: bool,
    /// AMD: IBRS may simply be left on, and protects when it is.
    ibrs_always_on: bool,
    /// `IA32_SPEC_CTRL` takes SSBD.
    ssbd: bool,
    /// AMD's virtual register takes SSBD.
    virt_ssbd: bool,
    /// AMD: not vulnerable to store bypass.
    amd_ssb_no: bool,
    /// AMD: automatic IBRS.
    auto_ibrs: bool,
    /// Intel: `VERW` clears the buffers MDS reads.
    md_clear: bool,
    /// `IA32_ARCH_CAPABILITIES`, or zero where it does not exist.
    capabilities: u64,
}

/// Whether `leaf` is at or below the highest leaf of its range.
fn has_leaf(leaf: u32) -> bool {
    __cpuid(leaf & 0x8000_0000).eax >= leaf
}

/// `CPUID` and `IA32_ARCH_CAPABILITIES`, read.
fn offered() -> Offered {
    let vendor = __cpuid(0);
    let is = |name: &[u8; 12]| {
        let bytes = [vendor.ebx, vendor.edx, vendor.ecx];
        bytes
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .eq(name.iter().copied())
    };
    let intel = is(b"GenuineIntel");
    let amd = is(b"AuthenticAMD") || is(b"HygonGenuine");

    let leaf7 = if has_leaf(7) {
        __cpuid_count(7, 0).edx
    } else {
        0
    };
    let amd8 = if has_leaf(0x8000_0008) {
        __cpuid(0x8000_0008).ebx
    } else {
        0
    };
    let amd21 = if has_leaf(0x8000_0021) {
        __cpuid(0x8000_0021).eax
    } else {
        0
    };
    let bit = |word: u32, n: u32| word & (1 << n) != 0;
    let capabilities = if bit(leaf7, 29) {
        // SAFETY: CPUID.7.EDX[29] says the register exists.
        unsafe { cpu::read_msr(IA32_ARCH_CAPABILITIES) }
    } else {
        0
    };
    Offered {
        intel,
        amd,
        ibrs: bit(leaf7, 26) || bit(amd8, 14),
        ibpb: bit(leaf7, 26) || bit(amd8, 12),
        stibp: bit(leaf7, 27) || bit(amd8, 15),
        ibrs_always_on: bit(amd8, 16),
        ssbd: bit(leaf7, 31) || bit(amd8, 24),
        virt_ssbd: bit(amd8, 25),
        amd_ssb_no: bit(amd8, 26),
        auto_ibrs: bit(amd21, 8),
        md_clear: bit(leaf7, 10),
        capabilities,
    }
}

/// What every processor applies: decided on the boot processor, before any
/// other has started.
#[derive(Debug, Clone, Copy, Default)]
struct Plan {
    /// Written to `IA32_SPEC_CTRL`, if not zero.
    spec_ctrl: u64,
    /// Set `EFER.AIBRSE`.
    auto_ibrs: bool,
    /// Write SSBD to AMD's virtual register.
    virt_ssbd: bool,
    /// Every defence the plan amounts to.
    defences: Defences,
}

impl Plan {
    /// The plan for a processor that offers `offered`.
    fn for_processor(offered: &Offered) -> Plan {
        let mut plan = Plan {
            defences: Defences::CLAMPED_INDICES
                .with(Defences::SWAPGS_FENCE)
                .with(Defences::ENTRY_REGISTERS_CLEARED)
                .with(Defences::RSB_FILL),
            ..Plan::default()
        };
        let enhanced = offered.capabilities & CAP_IBRS_ALL != 0;
        if enhanced && offered.ibrs {
            plan.spec_ctrl |= SPEC_CTRL_IBRS;
            plan.defences = plan.defences.with(Defences::IBRS_ENHANCED);
        } else if offered.auto_ibrs {
            plan.auto_ibrs = true;
            plan.defences = plan.defences.with(Defences::IBRS_AUTOMATIC);
        } else if offered.ibrs_always_on && offered.ibrs {
            plan.spec_ctrl |= SPEC_CTRL_IBRS;
            plan.defences = plan.defences.with(Defences::IBRS_ALWAYS_ON);
        }
        // Enhanced IBRS covers the sibling thread as well; nothing else does.
        if offered.stibp && !enhanced {
            plan.spec_ctrl |= SPEC_CTRL_STIBP;
            plan.defences = plan.defences.with(Defences::STIBP);
        }
        if !store_bypass_immune(offered) {
            if offered.ssbd {
                plan.spec_ctrl |= SPEC_CTRL_SSBD;
                plan.defences = plan.defences.with(Defences::SSBD);
            } else if offered.virt_ssbd {
                plan.virt_ssbd = true;
                plan.defences = plan.defences.with(Defences::SSBD);
            }
        }
        if offered.ibpb {
            plan.defences = plan.defences.with(Defences::SWITCH_BARRIER);
        }
        if mds_exposed(offered) {
            plan.defences = plan.defences.with(Defences::BUFFERS_CLEARED);
        }
        plan
    }
}

/// Whether the processor says it cannot bypass a store.
const fn store_bypass_immune(offered: &Offered) -> bool {
    offered.amd_ssb_no || offered.capabilities & CAP_SSB_NO != 0
}

/// Whether the processor may leak its buffers to MDS and can clear them.
///
/// Only Intel parts are affected, and only those without `MDS_NO`; `VERW`
/// clears the buffers only where microcode says so with `MD_CLEAR`.
const fn mds_exposed(offered: &Offered) -> bool {
    offered.intel && offered.md_clear && offered.capabilities & CAP_MDS_NO == 0
}

/// Whether the processor may be exposed to Meltdown and L1TF: an Intel part
/// that does not say `RDCL_NO`. AMD's are not.
const fn meltdown_exposed(offered: &Offered) -> bool {
    offered.intel && offered.capabilities & CAP_RDCL_NO == 0
}

/// Whether `VERW` runs on every return to ring 3. Read by the exits in
/// `syscall` and `trap`, from assembly, as a byte.
pub(super) static CLEAR_CPU_BUFFERS: AtomicU8 = AtomicU8::new(0);

/// The memory operand `VERW` takes: a writable data segment's selector. The
/// buffer clearing is a side effect of the memory form only.
pub(super) static VERW_SELECTOR: u16 = super::gdt::KERNEL_DATA;

/// Whether the entry paths' extra instructions are assembled in: `1` or `0`,
/// for an assembler `.if`.
pub(super) const ENTRY_HARDENING: u8 = HARDENED as u8;

/// The plan, for the secondaries: `IA32_SPEC_CTRL`'s value.
static PLAN_SPEC_CTRL: AtomicU64 = AtomicU64::new(0);
/// The plan, for the secondaries: its defences.
static PLAN_DEFENCES: AtomicU32 = AtomicU32::new(0);
/// The plan, for the secondaries: `EFER.AIBRSE`.
static PLAN_AUTO_IBRS: AtomicBool = AtomicBool::new(false);
/// The plan, for the secondaries: AMD's virtual SSBD.
static PLAN_VIRT_SSBD: AtomicBool = AtomicBool::new(false);
/// Whether a switch of address space issues `IBPB`.
static SWITCH_IBPB: AtomicBool = AtomicBool::new(false);

/// Decide, apply on the boot processor, and say what was done.
///
/// Before any secondary starts, which each read the plan, and before the
/// first program.
pub(crate) fn init() {
    if !HARDENED {
        println!("  cpu      speculation defences off: built with --mitigations off");
        record_this_cpu(Defences::NONE);
        return;
    }
    let offered = offered();
    let plan = Plan::for_processor(&offered);
    PLAN_SPEC_CTRL.store(plan.spec_ctrl, Ordering::Relaxed);
    PLAN_AUTO_IBRS.store(plan.auto_ibrs, Ordering::Relaxed);
    PLAN_VIRT_SSBD.store(plan.virt_ssbd, Ordering::Relaxed);
    SWITCH_IBPB.store(
        plan.defences.contains(Defences::SWITCH_BARRIER),
        Ordering::Relaxed,
    );
    CLEAR_CPU_BUFFERS.store(
        u8::from(plan.defences.contains(Defences::BUFFERS_CLEARED)),
        Ordering::Relaxed,
    );
    PLAN_DEFENCES.store(plan.defences.bits(), Ordering::Release);

    let applied = apply(&plan);
    record_this_cpu(applied);
    println!("  cpu      speculation defences: {}", applied.names());
    report_exposure(&offered, plan.defences);
}

/// Say what the plan leaves uncovered, and why.
fn report_exposure(offered: &Offered, defences: Defences) {
    let v2 = if defences.contains(Defences::IBRS_ENHANCED)
        || defences.contains(Defences::IBRS_AUTOMATIC)
        || defences.contains(Defences::IBRS_ALWAYS_ON)
    {
        "covered"
    } else {
        "NOT covered: no eIBRS, AutoIBRS or always-on IBRS offered (AoU-11)"
    };
    let meltdown = if meltdown_exposed(offered) {
        "EXPOSED: no RDCL_NO, and KPTI is not built (AoU-11)"
    } else if offered.intel {
        "not affected (RDCL_NO)"
    } else if offered.amd {
        "not affected (AMD)"
    } else {
        "not known to be affected"
    };
    let bypass = if store_bypass_immune(offered) {
        "not affected"
    } else if defences.contains(Defences::SSBD) {
        "covered"
    } else {
        "NOT covered: no SSBD offered (AoU-11)"
    };
    let between = if defences.contains(Defences::SWITCH_BARRIER) {
        "covered"
    } else {
        "NOT covered: no IBPB offered (AoU-11)"
    };
    println!(
        "  cpu      speculation exposure: Spectre v2 into ring 0 {v2}; between programs \
         {between}; store bypass {bypass}; Meltdown {meltdown}"
    );
}

/// Apply the plan on this processor, read it back, and say what took.
fn apply(plan: &Plan) -> Defences {
    let mut held = true;
    if plan.spec_ctrl != 0 {
        // SAFETY: each bit is in the plan only because CPUID said the
        // register exists and takes it.
        unsafe { cpu::write_msr(IA32_SPEC_CTRL, plan.spec_ctrl) };
        // SAFETY: as above, the register exists.
        let read = unsafe { cpu::read_msr(IA32_SPEC_CTRL) };
        held &= read & plan.spec_ctrl == plan.spec_ctrl;
    }
    if plan.auto_ibrs {
        // SAFETY: `IA32_EFER` exists on every processor with long mode.
        let efer = unsafe { cpu::read_msr(IA32_EFER) };
        // SAFETY: CPUID 0x8000_0021.EAX[8] says the bit is implemented.
        unsafe { cpu::write_msr(IA32_EFER, efer | EFER_AUTO_IBRS) };
        // SAFETY: as above.
        held &= unsafe { cpu::read_msr(IA32_EFER) } & EFER_AUTO_IBRS != 0;
    }
    if plan.virt_ssbd {
        // SAFETY: CPUID 0x8000_0008.EBX[25] says the register exists.
        unsafe { cpu::write_msr(AMD_VIRT_SPEC_CTRL, SPEC_CTRL_SSBD) };
        // SAFETY: as above.
        held &= unsafe { cpu::read_msr(AMD_VIRT_SPEC_CTRL) } & SPEC_CTRL_SSBD != 0;
    }
    if held {
        plan.defences
    } else {
        plan.defences.with(Defences::READ_BACK_FAILED)
    }
}

/// Apply the boot processor's plan on a secondary, as it starts.
pub(crate) fn apply_this_cpu() {
    if !HARDENED {
        record_this_cpu(Defences::NONE);
        return;
    }
    let defences = Defences::from_bits(PLAN_DEFENCES.load(Ordering::Acquire));
    let plan = Plan {
        spec_ctrl: PLAN_SPEC_CTRL.load(Ordering::Relaxed),
        auto_ibrs: PLAN_AUTO_IBRS.load(Ordering::Relaxed),
        virt_ssbd: PLAN_VIRT_SSBD.load(Ordering::Relaxed),
        defences,
    };
    record_this_cpu(apply(&plan));
}

/// Issue the switch barrier: `IBPB`, where the processor offers it, and a
/// return stack refill either way. Answers whether anything was issued,
/// which in a hardened build is always.
///
/// Called with interrupts masked, from `install_user_root`, when the
/// processor is about to run a program other than the one it last ran.
pub(crate) fn switch_barrier() -> bool {
    if SWITCH_IBPB.load(Ordering::Relaxed) {
        // SAFETY: `SWITCH_IBPB` is set only when CPUID said `IA32_PRED_CMD`
        // takes IBPB; the write empties predictors and changes nothing else.
        unsafe { cpu::write_msr(IA32_PRED_CMD, PRED_CMD_IBPB) };
    }
    fill_return_stack();
    HARDENED
}

/// Overwrite the return stack buffer with thirty-two harmless entries.
///
/// Each `call` pushes its return address -- the `int3` after it, which a
/// mispredicted `ret` would land on and stop -- onto both the stack and the
/// predictor; the `add` takes them back off the stack and leaves them in the
/// predictor, where they displace whatever the last program left. The
/// `lfence` keeps anything after from running before the refill has.
fn fill_return_stack() {
    if !HARDENED {
        return;
    }
    // SAFETY: thirty-two calls to the next instruction and the stack put
    // back as it was; no register but the stack pointer is touched, and that
    // is restored. The kernel's target has no red zone to overwrite.
    unsafe {
        asm!(
            ".rept 32",
            "call 2f",
            "int3",
            "2:",
            ".endr",
            "add rsp, 256",
            "lfence",
            options(preserves_flags),
        );
    }
}

/// `index` if `index < len`, else zero, without a branch: `cmp` sets the
/// carry exactly when `index < len`, and `sbb` of a register from itself
/// turns the carry into all ones or all zeros.
#[inline(always)]
pub(crate) fn clamp_index(index: usize, len: usize) -> usize {
    if !HARDENED {
        return index;
    }
    let mask: usize;
    // SAFETY: two register instructions; no memory, no stack.
    unsafe {
        asm!(
            "cmp {index}, {len}",
            "sbb {mask}, {mask}",
            index = in(reg) index,
            len = in(reg) len,
            mask = out(reg) mask,
            options(pure, nomem, nostack),
        );
    }
    index & mask
}

/// [`clamp_index`], for a 64-bit value: the same thing on this machine.
#[inline(always)]
pub(crate) fn clamp_below(value: u64, end: u64) -> u64 {
    clamp_index(value as usize, end as usize) as u64
}

/// Run `VERW` once, as every return to ring 3 does on an MDS-exposed
/// processor: for the boot check, which shows the operand is one the
/// instruction accepts wherever that path is not taken.
pub(crate) fn clear_cpu_buffers() {
    // SAFETY: `VERW` of a readable selector only sets `ZF` and, on a
    // processor with `MD_CLEAR`, clears buffers; the operand is a static.
    unsafe {
        asm!(
            "verw word ptr [{selector}]",
            selector = in(reg) &raw const VERW_SELECTOR,
            options(nostack),
        );
    }
}

/// What the boot check adds on this architecture: run `VERW` once, so a
/// machine whose exit path never runs it -- every one without MDS -- still
/// shows the instruction takes its operand.
///
/// # Errors
///
/// None: a `VERW` that did not take its operand would fault, not return.
pub(crate) fn check() -> Result<(), &'static str> {
    if HARDENED {
        clear_cpu_buffers();
    }
    Ok(())
}
