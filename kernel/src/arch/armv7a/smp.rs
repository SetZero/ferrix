//! Processors on ARMv7-A.
//!
//! The device tree is the processor list here, as the MADT is on the other
//! two: `/cpus` has a node per core, whose `reg` is the affinity fields of
//! that core's `MPIDR` and whose `enable-method` says how it is started. On
//! every machine this kernel runs on the method is PSCI, and `CPU_ON` is
//! AArch64's call in the 32-bit convention.
//!
//! # Starting one
//!
//! AArch64's problem, in coprocessor 15's spelling. `CPU_ON` starts a core in
//! SVC mode at a physical address, with its MMU and caches off and one
//! argument in `r0`, and everything the kernel runs on is in the upper half.
//! So the entry sequence below installs the loader's long-descriptor regime —
//! `MAIR0` and `MAIR1`, `TTBCR` with `EAE`, `TTBR0` and `TTBR1` — through an
//! identity map of itself, loading every parameter before the MMU goes on,
//! and branches to Rust at a virtual address. What differs from AArch64 is
//! `TTBR1`, which holds the kernel's root plus sixteen, for the reason
//! `boot/src/arch/armv7a.rs` gives.
//!
//! One stack is all a secondary needs. The trap stubs store on the SVC stack
//! from whichever mode an exception arrives in, so no other mode has one —
//! which is also why the entry sequence checks that the core is in SVC mode
//! before anything else, and parks it if not: a core in any other mode would
//! take its first exception somewhere with no stack at all. A parked core
//! never reports in, and the boot fails naming that, rather than running on.
//!
//! # What QEMU cannot check
//!
//! A Cortex-A7 or A15 has to have `ACTLR.SMP` set before its caches and MMU
//! go on, or it is not coherent with the other cores. It is not set here.
//! From the non-secure world the write is permitted only if the secure world
//! allowed it, and an undefined instruction with the MMU off has nowhere to
//! go; PSCI firmware on real boards — TF-A, U-Boot's own — sets it before
//! entering the kernel, and QEMU does not model the bit at all. A board that
//! turns out not to is where this changes.

use alloc::vec::Vec;

use ferrix_bootinfo::{BootView, PAGE_SIZE};
use ferrix_fdt::PsciConduit;
use ferrix_paging::MapFlags;

use super::{cpu, gicv2};
use crate::smp::Described;

/// The affinity fields of `MPIDR`: `Aff0` to `Aff2`, bits 0..24.
///
/// For AArch64's reason: the rest of the register is not identity. Bit 31
/// reads as one on every core with the multiprocessing extensions and bit 30
/// says whether this is a uniprocessor, while the device tree stores the
/// affinity fields alone.
const MPIDR_AFFINITY: u64 = 0x00FF_FFFF;

/// PSCI `CPU_ON`, in the 32-bit calling convention.
const PSCI_CPU_ON: u32 = 0x8400_0003;

/// Where the kernel half's level-1 table starts within the root: entry 2,
/// eight bytes each. `boot/src/arch/armv7a.rs` says why.
const TTBR1_OFFSET: u64 = 16;

/// Where `TTBR0`'s half of the address space ends, with `TTBCR.T1SZ = 1`.
///
/// The identity map lives under `TTBR0`, so the entry sequence has to be
/// below this to be identity mapped at all. On QEMU's `virt` RAM starts at
/// 1 GiB and it is; the loader refuses a machine where it would not be.
const LOWER_HALF_END: u64 = 0x8000_0000;

/// Every processor the device tree says can be started, and which one this is.
///
/// Counted: the processor running this, whatever its node says, and every
/// other whose `enable-method` is PSCI. A core started some other way — a
/// spin table — is one this kernel cannot start, and counting it would have
/// the exit test wait for a processor that will never answer, which is the
/// rule ACPI's enabled flag stands for on the other two. `status = "disabled"`
/// is counted: for a processor it means stopped, not absent.
///
/// # Errors
///
/// If there is no device tree to read.
pub(crate) fn describe_cpus(view: &BootView<'_>) -> Result<Described, &'static str> {
    let tree = crate::fdt::open(view)?;
    let boot = hardware_id();
    let ids: Vec<u64> = tree
        .cpus()
        .map(|cpu| (cpu.id & MPIDR_AFFINITY, cpu.enable_method()))
        .filter(|&(id, method)| id == boot || method == Some("psci"))
        .map(|(id, _)| id)
        .collect();

    Ok(Described {
        id_name: "MPIDR",
        boot,
        ids,
    })
}

/// This processor's hardware identifier: the affinity fields of its `MPIDR`.
pub(crate) fn hardware_id() -> u64 {
    u64::from(cpu::read_mpidr()) & MPIDR_AFFINITY
}

// The entry sequence. `r0` is the physical address of a `StartBlock`, and the
// MMU and caches are off, so every load here is of memory rather than of this
// core's cache — which is why the block is cleaned to the point of coherency
// before the core is started.
//
// All nine words are loaded in the first instruction, *before* the MMU goes
// on: after it, `r0` is a physical address with nothing mapped at it. The
// instruction cache and the branch predictor are invalidated before the MMU
// goes on, as the loader does, because whatever they hold predates this
// kernel.
core::arch::global_asm!(
    r#"
.arm
.section .text
.balign 32
.globl ferrix_secondary_entry
ferrix_secondary_entry:
    mrs   r6, cpsr
    and   r6, r6, #0x1f
    cmp   r6, #0x13
    bne   ferrix_secondary_parked
    ldm   r0, {{r1-r5, r8-r10, r12}}
    mov   r6, #0
    mcr   p15, 0, r6, c7, c5, 0
    mcr   p15, 0, r6, c7, c5, 6
    mcr   p15, 0, r1, c10, c2, 0
    mcr   p15, 0, r2, c10, c2, 1
    mcr   p15, 0, r3, c2, c0, 2
    mcrr  p15, 0, r4, r6, c2
    mcrr  p15, 1, r5, r6, c2
    isb
    mcr   p15, 0, r6, c8, c7, 0
    dsb
    isb
    mcr   p15, 0, r8, c1, c0, 0
    isb
    mov   sp, r9
    mov   r0, r12
    mov   r11, #0
    mov   lr, #0
    bx    r10
ferrix_secondary_parked:
    wfi
    b     ferrix_secondary_parked
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
/// Field order is the `ldm`'s: registers load in ascending order from
/// ascending addresses, so the order here is the order of `r1`–`r5`, `r8`–`r10`
/// and `r12` in the entry sequence.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct StartBlock {
    /// Memory attributes, copied from this core: `r1`.
    mair0: u32,
    /// The other half of them: `r2`.
    mair1: u32,
    /// Translation control, copied from this core with the lower half's walk
    /// enabled, since it is the identity map's: `r3`.
    ttbcr: u32,
    /// The identity map's root: `r4`.
    ttbr0: u32,
    /// The kernel's root plus [`TTBR1_OFFSET`]: `r5`.
    ttbr1: u32,
    /// System control, copied from this core — MMU and caches on, vectors at
    /// `VBAR`: `r8`.
    sctlr: u32,
    /// The new core's SVC stack: `r9`.
    stack_top: u32,
    /// Where to go once the MMU is on: [`secondary_start`], by virtual
    /// address: `r10`.
    entry: u32,
    /// Its argument, the new core's per-CPU record: `r12`.
    argument: u32,
    /// Padding to a multiple of eight bytes; not loaded.
    reserved: u32,
}

const _: () = assert!(
    size_of::<StartBlock>() == 40,
    "the entry sequence loads nine words from the start block"
);

/// A physical or virtual address as the 32-bit register it has to fit.
///
/// Every address here is below 4 GiB by construction — the RAM of every
/// machine this kernel runs on, and the whole of its address space — so this
/// fails only if that stops being true, and says so rather than truncating.
fn register(value: u64, what: &'static str) -> Result<u32, &'static str> {
    u32::try_from(value).map_err(|_| what)
}

/// Starts secondary cores, one at a time.
#[derive(Debug)]
pub(crate) struct CpuStarter {
    /// How to reach PSCI.
    conduit: PsciConduit,
    /// Where PSCI starts a core: the entry sequence's physical address.
    entry_phys: u32,
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
    /// Build the identity map and the start block's template, from this
    /// core's own registers.
    pub(crate) fn new(view: &BootView<'_>) -> Result<CpuStarter, &'static str> {
        let conduit = super::psci_conduit().ok_or(
            "the device tree names no PSCI conduit, and PSCI is how this kernel starts a core",
        )?;

        let info = view.raw();
        let physical = |virt: u64| virt - info.kernel_virt + info.kernel_phys;
        let entry_phys = physical((&raw const ferrix_secondary_entry).addr() as u64);
        let end_phys = physical((&raw const ferrix_secondary_entry_end).addr() as u64);
        let identity_base = entry_phys - entry_phys % PAGE_SIZE;
        let identity_len = (end_phys - identity_base).next_multiple_of(PAGE_SIZE);
        if identity_base + identity_len > LOWER_HALF_END {
            return Err("the secondary entry is above 2 GiB, where TTBR0 cannot identity map it");
        }

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
            entry_phys: register(entry_phys, "the secondary entry is above 4 GiB")?,
            identity_base,
            identity_len,
            identity_root,
            block,
            template: StartBlock {
                mair0: cpu::read_mair0(),
                mair1: cpu::read_mair1(),
                ttbcr: cpu::read_ttbcr() & !cpu::TTBCR_EPD0,
                ttbr0: register(identity_root, "the identity map's root is above 4 GiB")?,
                ttbr1: register(
                    crate::mm::root_table() + TTBR1_OFFSET,
                    "the kernel's root table is above 4 GiB",
                )?,
                sctlr: cpu::read_sctlr(),
                stack_top: 0,
                entry: secondary_start as extern "C" fn(u32) -> ! as usize as u32,
                argument: 0,
                reserved: 0,
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
            stack_top: register(stack_top, "a secondary stack is above 4 GiB")?,
            argument: register(argument, "a per-CPU record is above 4 GiB")?,
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

        let target = register(hardware_id, "an MPIDR wider than thirty-two bits")?;
        let context = register(self.block, "the start block is above 4 GiB")?;
        match psci(self.conduit, PSCI_CPU_ON, target, self.entry_phys, context) {
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

/// Make a PSCI call and return its status.
fn psci(conduit: PsciConduit, function: u32, a: u32, b: u32, c: u32) -> i32 {
    // SAFETY: the only function this module calls is `CPU_ON`, whose entry
    // point is the sequence above and whose argument is a start block this
    // module wrote.
    let status = unsafe { cpu::psci_call(conduit, function, a, b, c) };
    status as i32
}

/// Where a secondary core arrives once the entry sequence has its MMU on.
///
/// In SVC mode, in the upper half, on its own stack, with every exception
/// masked and the identity map still installed.
extern "C" fn secondary_start(record: u32) -> ! {
    // SAFETY: nothing from here on executes or reads through the lower half.
    // The identity map was for the instructions before the branch here, and
    // this also invalidates what the walk through it left in this core's TLB.
    unsafe { cpu::disable_ttbr0() };
    // SAFETY: once on this core, before anything on it can fault, with every
    // exception masked. `trap::init` writes only this core's own `VBAR` and
    // `SCTLR` bits; the table it points them at is code every core shares.
    unsafe { super::trap::init() };
    gicv2::init_this_cpu();
    crate::smp::secondary_main(u64::from(record))
}
