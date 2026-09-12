//! Single x86-64 instructions with no spelling in Rust.
//!
//! Every function here is one instruction. They are on the assembly allow-list
//! as "CPU primitives" (`docs/ASSEMBLY.md`) because reading a control register
//! or touching the I/O address space is not something Rust has syntax for —
//! not because assembly is convenient.

use core::arch::asm;

/// Write a byte to an I/O port.
///
/// # Safety
///
/// I/O ports are device registers: the caller must know what device is behind
/// `port` and that writing `value` to it is intended.
pub(crate) unsafe fn outb(port: u16, value: u8) {
    // SAFETY: the caller guarantees the port and the value.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// Read a byte from an I/O port.
///
/// # Safety
///
/// As [`outb`]: the caller must know what device is behind `port`. Reading a
/// device register can have side effects — on a 16550, reading the receive
/// buffer consumes a character.
pub(crate) unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: the caller guarantees the port.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Write a 32-bit word to an I/O port.
///
/// # Safety
///
/// As [`outb`].
pub(crate) unsafe fn outl(port: u16, value: u32) {
    // SAFETY: the caller guarantees the port and the value.
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}

/// Halt until the next interrupt.
pub(crate) fn hlt() {
    // SAFETY: `hlt` at ring 0 stops the CPU until an interrupt arrives and has
    // no other effect.
    unsafe {
        asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

/// Mask interrupts on this CPU.
pub(crate) fn disable_interrupts() {
    // SAFETY: `cli` only clears the interrupt flag.
    unsafe {
        asm!("cli", options(nomem, nostack));
    }
}

/// `CR4.PGE` — the bit that makes the global bit in a page table entry mean
/// anything. The loader sets it; see `boot/src/arch/x86_64.rs`.
const CR4_PGE: u64 = 1 << 7;

/// Invalidate the whole `TLB`, **including global entries**.
///
/// # Why this is not just a `CR3` reload
///
/// Reloading `CR3` is the obvious way to flush a `TLB` and it is the wrong one
/// here, because it leaves global entries exactly where they were — the
/// architecture says so, and that is what the global bit is *for*: a
/// translation that survives an address space switch.
///
/// Nearly every mapping this kernel makes is global. Kernel text, the direct
/// map and every device window carry `MapFlags::global`, because they are the
/// same in every address space and marking them so is what stops a process
/// switch from throwing away the kernel's own translations. So a flush that
/// spared global entries spared essentially everything, and `flush_tlb` was a
/// no-op wearing a descriptive name.
///
/// **This was not theoretical.** Under emulation it never showed, because a
/// `TLB` that does not really exist cannot hold a stale entry; under hardware
/// virtualisation it produced three different failures with one cause. The
/// `vmap` arena reuses address space, so a device window unmapped in stage 2
/// handed its address to the `HPET` in stage 3 — which then read the kernel
/// image through the old translation and reported a period no `HPET` can
/// have. The local `APIC` read its calibration the same way. And stage 4's
/// shootdown check, which exists precisely to catch a processor holding a
/// translation it was told to drop, caught this one.
///
/// Clearing and restoring `CR4.PGE` is the architecturally defined way to
/// invalidate global entries: a write to `CR4` that changes `PGE` flushes the
/// whole `TLB`, global entries included. `invlpg` per page would also do it
/// and is what a grown kernel uses for a small range; this is the blunt
/// instrument, and the right one while the kernel still counts its mappings in
/// the hundreds.
pub(crate) fn flush_tlb_including_global() {
    // **Masked throughout, and not for atomicity of the flush.** The hazard is
    // re-entry: an interrupt taken between the two writes would run with `PGE`
    // clear, and a handler that flushed the `TLB` itself would read that
    // `CR4`, clear an already-clear bit — changing nothing, so flushing
    // nothing — and restore a `CR4` with `PGE` still clear. Global pages would
    // then be off for the rest of the boot, silently.
    let flags = read_rflags();
    disable_interrupts();

    let cr4 = read_cr4();
    // SAFETY: `cr4` was just read, so this is that value with one bit cleared.
    // Clearing `PGE` alters no mapping; its only effect is to invalidate the
    // whole `TLB`, global entries included, which is what this exists for. The
    // bit is restored immediately below, with interrupts masked in between.
    unsafe { write_cr4(cr4 & !CR4_PGE) };
    // SAFETY: the value read a moment ago, put back unchanged, before anything
    // could observe `PGE` clear.
    unsafe { write_cr4(cr4) };

    // And `CR3`, for the non-global entries: toggling `PGE` covers them too,
    // but this is the operation the rest of the kernel means by "flush" and
    // leaving it out would make the correctness of this function depend on a
    // footnote rather than on two instructions that plainly do it.
    let cr3: u64;
    // SAFETY: reading CR3 has no side effects.
    unsafe {
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    }
    // SAFETY: writing back the value just read changes no mapping; its only
    // effect is to flush the TLB, which is the point.
    unsafe {
        asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }

    if flags & RFLAGS_INTERRUPT != 0 {
        enable_interrupts();
    }
}

/// `RFLAGS.IF` — interrupts are unmasked.
pub(crate) const RFLAGS_INTERRUPT: u64 = 1 << 9;

/// The flags register.
///
/// Read for one bit: `IF`, which says whether interrupts are unmasked. A lock
/// that masks interrupts has to restore the state it found rather than
/// unconditionally unmasking, or taking one inside another silently enables
/// interrupts halfway out of the outer critical section.
pub(crate) fn read_rflags() -> u64 {
    let flags: u64;
    // SAFETY: `pushfq` and `pop` read the flags register through the stack and
    // leave it as they found it. `nostack` is deliberately *not* claimed: this
    // is the one primitive here that uses the stack.
    unsafe {
        asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
    }
    flags
}

/// Write a model-specific register.
///
/// # Safety
///
/// `msr` must be a register this CPU implements and `value` one it accepts. An
/// invalid one is a general protection fault; a valid but wrong one changes how
/// the CPU behaves.
pub(crate) unsafe fn write_msr(msr: u32, value: u64) {
    // SAFETY: the caller guarantees the register and the value.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Read a model-specific register.
///
/// # Safety
///
/// `msr` must be a register this CPU implements; reading one it does not is a
/// general protection fault.
pub(crate) unsafe fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: the caller guarantees the register exists.
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Control register 0: protection, paging, write protection, caching.
pub(crate) fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: reading CR0 has no side effects.
    unsafe {
        asm!("mov {}, cr0", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Control register 4: the paging and instruction-set extensions in force.
pub(crate) fn read_cr4() -> u64 {
    let value: u64;
    // SAFETY: reading CR4 has no side effects.
    unsafe {
        asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Write control register 4.
///
/// # Safety
///
/// Every bit set must be a feature this processor has and a mode it can be in
/// now: `PCIDE` outside long mode, or `LA57` under a four-level table, is a
/// fault at best.
pub(crate) unsafe fn write_cr4(value: u64) {
    // SAFETY: the caller guarantees the value.
    unsafe {
        asm!("mov cr4, {}", in(reg) value, options(nostack, preserves_flags));
    }
}

/// Switch to the page tables rooted at `root`.
///
/// # Safety
///
/// `root` must map everything this processor touches next — the code it is
/// executing, its stack — at the addresses it is using them at.
pub(crate) unsafe fn write_cr3(root: u64) {
    // SAFETY: the caller guarantees the new tables map what runs next.
    unsafe {
        asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags));
    }
}

/// The word at offset zero from `GS`'s base.
///
/// # Safety
///
/// `GS`'s base must point at readable memory. Until the kernel writes it, it
/// points wherever firmware left it.
pub(crate) unsafe fn read_gs_word() -> u64 {
    let word: u64;
    // SAFETY: the caller guarantees the base is readable.
    unsafe {
        asm!(
            "mov {}, qword ptr gs:[0]",
            out(reg) word,
            options(readonly, nostack, preserves_flags),
        );
    }
    word
}

/// QEMU's `isa-debug-exit` device.
///
/// Writing to it ends the emulator with `(value << 1) | 1`, which is how the
/// boot test distinguishes a kernel that finished from one that was killed on a
/// timeout. On real hardware the port is unused and the write does nothing,
/// which is why [`super::shutdown`] halts afterwards rather than assuming.
const DEBUG_EXIT_PORT: u16 = 0xF4;

/// The value that makes QEMU exit 33.
const DEBUG_EXIT_SUCCESS: u32 = 0x10;

/// Ask QEMU to exit successfully. Does nothing on real hardware.
pub(crate) fn debug_exit() {
    // SAFETY: on QEMU this port is the debug-exit device and this is exactly
    // what it is for; on hardware, port 0xF4 is unassigned and a write to an
    // unassigned port is discarded.
    unsafe { outl(DEBUG_EXIT_PORT, DEBUG_EXIT_SUCCESS) };
}

// ---------------------------------------------------------------------------
// Descriptor tables
// ---------------------------------------------------------------------------

/// Load the global descriptor table.
///
/// # Safety
///
/// `pointer` must be the address of a `limit`/`base` pair describing a valid
/// GDT that stays alive for as long as it is loaded. The CPU keeps using it for
/// every privilege transition, so a GDT on a stack that goes away is a fault
/// with no obvious cause.
pub(crate) unsafe fn load_gdt(pointer: u64) {
    // SAFETY: the caller guarantees the operand describes a live, valid table.
    unsafe {
        asm!("lgdt [{}]", in(reg) pointer, options(readonly, nostack, preserves_flags));
    }
}

/// Load the interrupt descriptor table.
///
/// # Safety
///
/// As [`load_gdt`], for an IDT whose every present gate points at real code.
pub(crate) unsafe fn load_idt(pointer: u64) {
    // SAFETY: the caller guarantees the operand describes a live, valid table.
    unsafe {
        asm!("lidt [{}]", in(reg) pointer, options(readonly, nostack, preserves_flags));
    }
}

/// Load the task register.
///
/// # Safety
///
/// `selector` must name an available 64-bit TSS descriptor in the current GDT.
/// Loading one that is already busy, or that is not a TSS at all, is a general
/// protection fault.
pub(crate) unsafe fn load_tss(selector: u16) {
    // SAFETY: the caller guarantees the selector names an available TSS.
    unsafe {
        asm!("ltr {0:x}", in(reg) selector, options(nostack, preserves_flags));
    }
}

/// Reload every segment register from the new GDT.
///
/// Necessary because `lgdt` does not touch the hidden descriptor caches: after
/// it, the CPU is still running on the *old* segment descriptors, and the first
/// interrupt to reload `CS` from the new table would fault.
///
/// # Safety
///
/// `code` and `data` must name a 64-bit code segment and a writable data
/// segment in the currently loaded GDT.
pub(crate) unsafe fn reload_segments(code: u16, data: u16) {
    // SAFETY: the caller guarantees both selectors are valid in the live GDT.
    // The far return is the only way to reload CS: there is no `mov cs`.
    unsafe {
        asm!(
            "push {code}",
            "lea {scratch}, [rip + 55f]",
            "push {scratch}",
            "retfq",
            "55:",
            "mov ds, {data:e}",
            "mov es, {data:e}",
            "mov ss, {data:e}",
            code = in(reg) u64::from(code),
            data = in(reg) u32::from(data),
            scratch = lateout(reg) _,
            options(preserves_flags),
        );
    }
}

/// The linear address a page fault was taken on.
pub(crate) fn read_cr2() -> u64 {
    let address: u64;
    // SAFETY: reading CR2 has no side effects. It is only meaningful inside a
    // page fault handler, before another fault overwrites it.
    unsafe {
        asm!("mov {}, cr2", out(reg) address, options(nomem, nostack, preserves_flags));
    }
    address
}

/// The current stack pointer.
///
/// Used to give the task state segment a valid `RSP0` at boot, so that the
/// first trap arriving from ring 3 has somewhere to land even before the
/// scheduler starts replacing it per task.
pub(crate) fn read_stack_pointer() -> u64 {
    let stack: u64;
    // SAFETY: reading `rsp` has no side effects.
    unsafe {
        asm!("mov {}, rsp", out(reg) stack, options(nomem, nostack, preserves_flags));
    }
    stack
}

/// Unmask interrupts on this CPU.
///
/// The counterpart to [`disable_interrupts`], and the moment the kernel stops
/// being the only thing that decides when it runs.
pub(crate) fn enable_interrupts() {
    // SAFETY: `sti` only sets the interrupt flag. Every vector has a gate by
    // the time anything calls this — `init_traps` runs long before.
    unsafe {
        asm!("sti", options(nomem, nostack));
    }
}

/// Unmask interrupts and halt until one arrives, with no gap between the two.
///
/// `sti` takes effect only after the instruction that follows it, so an
/// interrupt that became pending while they were masked is delivered to the
/// `hlt` — waking it — rather than slipping in between and leaving the
/// processor asleep with the reason it should be awake already handled.
pub(crate) fn enable_interrupts_and_halt() {
    // SAFETY: `sti` and `hlt` change only the interrupt flag and whether the
    // processor is running; every vector has a gate by the time this is used.
    unsafe {
        asm!("sti", "hlt", options(nomem, nostack));
    }
}

/// The time-stamp counter.
///
/// Counts core clock cycles on every CPU since the Pentium, and at a constant
/// rate independent of frequency scaling on anything since Nehalem. Nothing
/// reports that rate, which is why [`super::clock`] measures it.
pub(crate) fn rdtsc() -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: `rdtsc` reads a counter into edx:eax and has no other effect.
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}
