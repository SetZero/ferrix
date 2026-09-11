//! Processors on `AArch64`.
//!
//! On an ACPI machine the MADT's GIC CPU interface entries *are* the processor
//! list: one per core, each carrying that core's `MPIDR_EL1` affinity, which
//! is the number PSCI takes to start it.
//!
//! # Starting one
//!
//! PSCI's `CPU_ON` starts a core at a physical address, with its MMU and
//! caches off and one argument in `x0`. Everything the kernel runs on — its
//! image, its stacks, its per-CPU records — is in the upper half and reachable
//! only through page tables, so the first instructions have to install those
//! tables and turn the MMU on while executing at an address that means
//! something only with it off. That is the problem the loader solves at boot,
//! and the answer is the same: an identity map of the few instructions that
//! straddle the switch.
//!
//! The identity map here is private to bring-up. It maps the page holding the
//! entry sequence at its own physical address, in a tree of its own that only
//! a starting core's `TTBR0_EL1` ever points at, and it is freed once every
//! core has left it.

use alloc::vec::Vec;

use ferrix_acpi::MadtEntry;
use ferrix_bootinfo::{BootView, PAGE_SIZE};
use ferrix_paging::MapFlags;

use super::cpu;
use crate::smp::Described;

/// The affinity fields of `MPIDR_EL1`: `Aff0` to `Aff2` in bits 0..24 and
/// `Aff3` in bits 32..40.
///
/// The rest of the register is not identity. Bit 31 reads as one, bit 30 says
/// whether this is a uniprocessor system, bit 24 whether `Aff0` numbers
/// threads — and the MADT stores the affinity fields alone, with everything
/// else zero. An unmasked register therefore never matches its own
/// processor's entry: on QEMU's `virt` it is `0x8000_0000` against `0x0`.
const MPIDR_AFFINITY: u64 = 0x0000_00FF_00FF_FFFF;

/// PSCI `CPU_ON`, in the 64-bit calling convention.
const PSCI_CPU_ON: u64 = 0xC400_0003;

/// FADT `ARM_BOOT_ARCH`: PSCI is implemented.
const BOOT_ARCH_PSCI: u16 = 1 << 0;
/// FADT `ARM_BOOT_ARCH`: PSCI is entered with `HVC` rather than `SMC`.
const BOOT_ARCH_HVC: u16 = 1 << 1;

/// Every processor firmware says can be started, and which one this is.
///
/// # Errors
///
/// If the ACPI tables or the MADT cannot be read.
pub(crate) fn describe_cpus(view: &BootView<'_>) -> Result<Described, &'static str> {
    let firmware =
        crate::acpi::Firmware::open(view).map_err(|_| "the machine has no readable ACPI tables")?;
    let acpi = firmware.acpi();
    let madt = acpi.madt().map_err(|_| "the machine has no MADT")?;

    let mut ids = Vec::new();
    for entry in madt.entries() {
        // As on x86-64, a processor whose enabled flag is clear is one that
        // cannot be started now, and is not counted.
        if let MadtEntry::Gicc(gicc) = entry
            && gicc.is_enabled()
        {
            ids.push(gicc.mpidr & MPIDR_AFFINITY);
        }
    }

    Ok(Described {
        id_name: "MPIDR",
        boot: hardware_id(),
        ids,
    })
}

/// This processor's hardware identifier: the affinity fields of its
/// `MPIDR_EL1`.
pub(crate) fn hardware_id() -> u64 {
    cpu::read_mpidr() & MPIDR_AFFINITY
}

// The entry sequence. `x0` is the physical address of a `StartBlock`, and the
// MMU and caches are off, so every load here is of memory rather than of this
// core's cache — which is why the block is cleaned to the point of coherency
// before the core is started.
//
// All eight words are loaded *before* the MMU goes on. After it, `x0` is a
// physical address with nothing mapped at it: the identity map covers this
// code and nothing else.
core::arch::global_asm!(
    r#"
.section .text
.balign 64
.globl ferrix_secondary_entry
ferrix_secondary_entry:
    msr  spsel, #1
    ldp  x1, x2, [x0, #0]
    ldp  x3, x4, [x0, #16]
    ldp  x5, x6, [x0, #32]
    ldp  x7, x8, [x0, #48]
    msr  mair_el1, x1
    msr  tcr_el1, x2
    msr  ttbr0_el1, x3
    msr  ttbr1_el1, x4
    isb
    tlbi vmalle1
    dsb  nsh
    isb
    msr  sctlr_el1, x5
    isb
    mov  sp, x6
    mov  x0, x8
    mov  x29, xzr
    mov  x30, xzr
    br   x7
.globl ferrix_secondary_entry_end
ferrix_secondary_entry_end:
"#
);

unsafe extern "C" {
    /// Where PSCI starts a secondary core, by its physical address.
    static ferrix_secondary_entry: [u8; 0];
    /// One past the entry sequence's last instruction.
    static ferrix_secondary_entry_end: [u8; 0];
}

/// What a starting core reads, with its MMU off, to get its MMU on.
///
/// Field order is the assembly's, which loads these in pairs at fixed
/// offsets.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct StartBlock {
    /// Memory attributes, copied from this core.
    mair: u64,
    /// Translation control, copied from this core with the lower half's walk
    /// enabled — it is the identity map's.
    tcr: u64,
    /// The identity map's root.
    ttbr0: u64,
    /// The kernel's root.
    ttbr1: u64,
    /// System control, copied from this core: MMU, caches and alignment.
    sctlr: u64,
    /// The new core's kernel stack.
    stack_top: u64,
    /// Where to go once the MMU is on: [`secondary_start`], by virtual address.
    entry: u64,
    /// Its argument: the new core's per-CPU record.
    argument: u64,
}

const _: () = assert!(
    size_of::<StartBlock>() == 64,
    "the entry sequence loads exactly eight words from the start block"
);

/// How PSCI is reached on this machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Conduit {
    /// A hypervisor call: the machine has a hypervisor, or PSCI lives in one.
    Hvc,
    /// A secure monitor call: PSCI lives in EL3 firmware.
    Smc,
}

/// Starts secondary cores, one at a time.
#[derive(Debug)]
pub(crate) struct CpuStarter {
    /// How to reach PSCI.
    conduit: Conduit,
    /// Where PSCI starts a core: the entry sequence's physical address.
    entry_phys: u64,
    /// First byte of the identity-mapped span.
    identity_base: u64,
    /// Bytes of it.
    identity_len: u64,
    /// Root of the identity tree.
    identity_root: u64,
    /// The frame the start block is written to, by physical address.
    block: u64,
    /// Everything in the start block that is the same for every core.
    template: StartBlock,
}

impl CpuStarter {
    /// Build the identity map and the start block's template.
    ///
    /// Runs on the boot core, whose registers the template copies: a
    /// secondary that runs with different memory attributes or translation
    /// control from its siblings is one whose view of shared memory differs
    /// from theirs.
    pub(crate) fn new(view: &BootView<'_>) -> Result<CpuStarter, &'static str> {
        let conduit = psci_conduit(view)?;

        let info = view.raw();
        let physical = |virt: u64| virt - info.kernel_virt + info.kernel_phys;
        let entry_phys = physical((&raw const ferrix_secondary_entry) as u64);
        let end_phys = physical((&raw const ferrix_secondary_entry_end) as u64);
        let identity_base = entry_phys - entry_phys % PAGE_SIZE;
        let identity_len = (end_phys - identity_base).next_multiple_of(PAGE_SIZE);

        let root_frame = crate::mm::allocate_frames(0)
            .ok_or("no frame for the secondary cores' identity map")?;
        crate::mm::zero_frame(root_frame);
        let identity_root = root_frame * PAGE_SIZE;
        // Not global: these entries belong to one short-lived tree, and a
        // global one would survive in a TLB past the switch that retires it.
        let flags = MapFlags {
            global: false,
            ..MapFlags::KERNEL_CODE
        };
        crate::mm::map_in(
            identity_root,
            identity_base,
            identity_base,
            identity_len,
            flags,
        )
        .map_err(|_| "could not identity map the secondary entry sequence")?;

        let block = crate::mm::allocate_frames(0)
            .ok_or("no frame for the secondary start block")?
            * PAGE_SIZE;

        Ok(CpuStarter {
            conduit,
            entry_phys,
            identity_base,
            identity_len,
            identity_root,
            block,
            template: StartBlock {
                mair: cpu::read_mair(),
                tcr: cpu::read_tcr() & !cpu::TCR_EPD0,
                ttbr0: identity_root,
                ttbr1: crate::mm::root_table(),
                sctlr: cpu::read_sctlr(),
                stack_top: 0,
                entry: secondary_start as extern "C" fn(u64) -> ! as usize as u64,
                argument: 0,
            },
        })
    }

    /// Start the core `hardware_id` on `stack_top`, handing it `argument`.
    ///
    /// Returns once PSCI has accepted the request, not once the core is
    /// running: the caller waits for that, and must before starting another,
    /// because every core reads the same start block.
    pub(crate) fn start(
        &mut self,
        hardware_id: u64,
        stack_top: u64,
        argument: u64,
    ) -> Result<(), &'static str> {
        let block = StartBlock {
            stack_top,
            argument,
            ..self.template
        };
        let at = crate::mm::direct_map(self.block);
        // SAFETY: `self.block` is a frame this starter allocated and nothing
        // else refers to; the direct map makes it writable, and a frame is
        // aligned for anything.
        unsafe { (at as *mut StartBlock).write(block) };
        // The core that reads this has its caches off, so it sees memory —
        // not this core's cache, where the write above still is.
        cpu::clean_to_poc(at, size_of::<StartBlock>() as u64);

        match psci(
            self.conduit,
            PSCI_CPU_ON,
            hardware_id,
            self.entry_phys,
            self.block,
        ) {
            0 => Ok(()),
            -2 => Err("PSCI refused the start request's parameters"),
            -4 => Err("PSCI says the core is already on"),
            -5 => Err("PSCI says the core is already being started"),
            -9 => Err("PSCI refused the entry point's address"),
            _ => Err("PSCI could not start the core"),
        }
    }

    /// Take down the identity map and the start block.
    ///
    /// Only once every core that was started has reported in: each one leaves
    /// the identity map before it does.
    pub(crate) fn finish(self) -> Result<(), &'static str> {
        crate::mm::unmap_in(self.identity_root, self.identity_base, self.identity_len)
            .map_err(|_| "could not take down the secondary cores' identity map")?;
        crate::mm::deallocate_frames(self.identity_root / PAGE_SIZE, 0);
        crate::mm::deallocate_frames(self.block / PAGE_SIZE, 0);
        Ok(())
    }
}

/// How firmware says PSCI is reached.
fn psci_conduit(view: &BootView<'_>) -> Result<Conduit, &'static str> {
    let firmware =
        crate::acpi::Firmware::open(view).map_err(|_| "the machine has no readable ACPI tables")?;
    let flags = firmware
        .acpi()
        .fadt()
        .ok()
        .and_then(|fadt| fadt.arm_boot_arch())
        .unwrap_or(0);

    if flags & BOOT_ARCH_PSCI == 0 {
        return Err("firmware does not say PSCI is implemented, and the parking protocol is not");
    }
    Ok(if flags & BOOT_ARCH_HVC != 0 {
        Conduit::Hvc
    } else {
        Conduit::Smc
    })
}

/// Make a PSCI call and return its status.
fn psci(conduit: Conduit, function: u64, a: u64, b: u64, c: u64) -> i32 {
    let status = match conduit {
        // SAFETY: the only function this module calls is `CPU_ON`, whose
        // entry point is the sequence above and whose argument is a start
        // block this module wrote.
        Conduit::Hvc => unsafe { cpu::hvc_call(function, a, b, c) },
        // SAFETY: as above.
        Conduit::Smc => unsafe { cpu::smc_call(function, a, b, c) },
    };
    // PSCI returns a 32-bit signed status in the low half of `x0`.
    status as u32 as i32
}

/// Where a secondary core arrives once the entry sequence has its MMU on.
///
/// Running in the upper half on its own stack, with every exception masked
/// and the identity map still installed.
extern "C" fn secondary_start(record: u64) -> ! {
    // SAFETY: nothing from here on executes or reads through the lower half.
    // The identity map was for the instructions before the branch here.
    unsafe { cpu::disable_ttbr0() };
    // Disabling the walk does not drop what it already cached: the TLB
    // invalidation does.
    cpu::flush_tlb();
    // SAFETY: once on this core, before anything on it can fault, with every
    // exception masked.
    unsafe { super::trap::init() };
    super::gic::init_this_cpu();
    crate::smp::secondary_main(record)
}
