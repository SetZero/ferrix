//! The global descriptor table and the task state segment.
//!
//! Long mode barely uses segmentation — every segment is flat and covers
//! everything — but it does not let you skip it either. Three things still
//! need the GDT:
//!
//! * the privilege level, which is a property of the code segment,
//! * `SYSCALL`/`SYSRET`, which do not take a selector but *compute* one from
//!   `IA32_STAR`, so the selectors have to sit in a particular order,
//! * the task state segment, which is the only way to say which stack the CPU
//!   should switch to when a fault arrives from user mode.
//!
//! The layout below is the one Linux uses, and the order is not a preference.
//! `SYSRET` loads `CS` from `STAR[63:48] + 16` and `SS` from `STAR[63:48] + 8`,
//! so user data has to precede user 64-bit code by exactly eight bytes.

use alloc::boxed::Box;
use core::cell::UnsafeCell;

use super::cpu;

/// Kernel code. `SYSCALL` loads this from `STAR[47:32]`.
pub(crate) const KERNEL_CODE: u16 = 0x08;
/// Kernel data.
pub(crate) const KERNEL_DATA: u16 = 0x10;
/// Unused 32-bit user code. Present only to put user data at the right offset
/// for `SYSRET`, which is also why it cannot simply be deleted.
const USER_CODE32: u16 = 0x18;
/// User data. `SYSRET` computes this as `STAR[63:48] + 8`.
pub(crate) const USER_DATA: u16 = 0x20;
/// User 64-bit code. `SYSRET` computes this as `STAR[63:48] + 16`.
pub(crate) const USER_CODE: u16 = 0x28;
/// The task state segment. Sixteen bytes, so it occupies two slots.
const TSS_SELECTOR: u16 = 0x30;

/// The base `SYSRET` computes user selectors from.
pub(crate) const SYSRET_BASE: u16 = USER_CODE32;

// `SYSRET` does not take a selector: it *computes* one, loading `CS` from
// `STAR[63:48] + 16` and `SS` from `STAR[63:48] + 8`. That makes the order of
// the three user entries part of the instruction's contract rather than a
// matter of taste, and getting it wrong returns to user mode with the wrong
// segment -- which faults immediately if you are lucky and does something far
// worse if you are not. Asserting it here means the layout cannot be shuffled
// without the build saying so.
const _: () = assert!(
    USER_DATA == SYSRET_BASE + 8,
    "SYSRET loads SS from STAR[63:48] + 8, so user data must sit there"
);
const _: () = assert!(
    USER_CODE == SYSRET_BASE + 16,
    "SYSRET loads CS from STAR[63:48] + 16, so user 64-bit code must sit there"
);

/// Descriptor bit: the segment is present.
const PRESENT: u64 = 1 << 47;
/// Descriptor bit: a code or data segment rather than a system one.
const USER_SEGMENT: u64 = 1 << 44;
/// Descriptor bit: executable, which is what makes a segment a code segment.
const EXECUTABLE: u64 = 1 << 43;
/// Descriptor bit: writable, for a data segment.
const WRITABLE: u64 = 1 << 41;
/// Descriptor bit: 64-bit code. Mutually exclusive with the 32-bit size bit.
const LONG_MODE: u64 = 1 << 53;
/// Descriptor field: the privilege level the segment runs at.
const fn dpl(level: u64) -> u64 {
    level << 45
}

/// System descriptor type 9: an available 64-bit task state segment.
const TSS_AVAILABLE: u64 = 0b1001 << 40;

/// Interrupt stack table slot used for the double-fault handler.
///
/// The one fault that has to be handled on a stack of its own: a double fault
/// usually means the kernel stack is unusable, and taking the handler on that
/// same stack turns it into a triple fault, which is a silent reset.
pub(crate) const DOUBLE_FAULT_IST: u16 = 1;

/// Bytes in each interrupt stack table stack.
const IST_STACK_SIZE: usize = 16 * 1024;

/// Bytes in a 64-bit task state segment.
const TSS_SIZE: usize = 104;

/// Offset of `RSP0`, the stack the CPU switches to on entry to ring 0.
const TSS_PRIVILEGE_STACK: usize = 4;
/// Offset of `IST1`. The seven interrupt stack table slots follow it.
const TSS_INTERRUPT_STACK: usize = 36;
/// Offset of the I/O permission bitmap pointer.
const TSS_IOMAP_BASE: usize = 102;

/// The task state segment.
///
/// Long mode ignores almost all of the 32-bit TSS: what remains is the
/// privilege-level stack pointers, the interrupt stack table, and the I/O
/// permission bitmap offset.
///
/// **Bytes rather than fields, deliberately.** `RSP0` sits at offset 4, which
/// puts a `u64` at a four-byte-aligned offset, so a struct describing this
/// layout has to be `packed` — and taking a reference to a field of a packed
/// struct is undefined behaviour in Rust *even when the reference is never
/// read*. Named offsets into a byte array say exactly the same thing and stay
/// sound.
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug)]
struct TaskStateSegment([u8; TSS_SIZE]);

impl TaskStateSegment {
    const fn new() -> TaskStateSegment {
        TaskStateSegment([0; TSS_SIZE])
    }

    /// Write a little-endian `u64` at `offset`.
    ///
    /// Silently does nothing if the offset is out of range, which cannot happen
    /// — every caller passes one of the constants above — but is the shape that
    /// avoids a panic in a kernel.
    fn write_u64(&mut self, offset: usize, value: u64) {
        if let Some(slot) = self.0.get_mut(offset..offset + 8) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
    }

    /// Write a little-endian `u16` at `offset`.
    fn write_u16(&mut self, offset: usize, value: u16) {
        if let Some(slot) = self.0.get_mut(offset..offset + 2) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
    }

    /// Point the I/O permission bitmap past the end of the segment, which is
    /// how a TSS says that ring 3 may touch no port at all.
    fn deny_all_ports(&mut self) {
        self.write_u16(TSS_IOMAP_BASE, TSS_SIZE as u16);
    }

    /// Set one of the seven interrupt stack table entries, numbered from one
    /// as the gate's IST field numbers them.
    fn set_interrupt_stack(&mut self, slot: u16, stack_top: u64) {
        if slot == 0 || slot > 7 {
            return;
        }
        let offset = TSS_INTERRUPT_STACK + (slot as usize - 1) * 8;
        self.write_u64(offset, stack_top);
    }

    /// Set `RSP0`.
    fn set_privilege_stack(&mut self, stack_top: u64) {
        self.write_u64(TSS_PRIVILEGE_STACK, stack_top);
    }
}

/// One processor's descriptor tables.
///
/// **Per processor, and it has to be.** A TSS holds its processor's stack
/// pointers, and loading one marks its descriptor busy — so a second processor
/// loading the same descriptor takes a general protection fault. The GDT holds
/// that descriptor, so it is per processor too.
#[repr(C, align(16))]
struct Tables {
    /// Seven eight-byte slots: the six selectors above plus the second half of
    /// the sixteen-byte TSS descriptor.
    gdt: [u64; 8],
    tss: TaskStateSegment,
}

impl Tables {
    const fn new() -> Tables {
        Tables {
            gdt: [0; 8],
            tss: TaskStateSegment::new(),
        }
    }
}

/// The boot processor's tables.
struct Global(UnsafeCell<Tables>);

// SAFETY: written once by `init` on the boot CPU before interrupts are enabled,
// and read by the CPU alone thereafter. Every other processor has its own, from
// `init_secondary`.
unsafe impl Sync for Global {}

/// The boot processor's tables: static, because they are loaded before there
/// is an allocator.
static BOOT_TABLES: Global = Global(UnsafeCell::new(Tables::new()));

/// The boot processor's double-fault stack, static for the same reason.
///
/// Every other processor's comes from the vmap arena, guard pages and all.
#[repr(C, align(16))]
struct BootStack(UnsafeCell<[u8; IST_STACK_SIZE]>);

// SAFETY: nothing in the kernel reads or writes this: the CPU switches to it
// on a double fault, which is its whole purpose.
unsafe impl Sync for BootStack {}

static BOOT_DOUBLE_FAULT_STACK: BootStack = BootStack(UnsafeCell::new([0; IST_STACK_SIZE]));

/// The operand `lgdt` takes: a limit and a base.
#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// Build the boot processor's GDT and TSS and load them.
///
/// # Safety
///
/// Must be called exactly once, on the boot CPU, before any user mode entry and
/// before interrupts are enabled.
pub(crate) unsafe fn init() {
    // SAFETY: single-threaded early boot, and this is the only writer.
    let tables = unsafe { &mut *BOOT_TABLES.0.get() };

    // The x86 stack grows down and `push` decrements first, so the top is one
    // past the last byte. Sixteen-byte aligned because the ABI says so and
    // because a misaligned stack breaks `movaps` in any handler that touches
    // floating point.
    let stack_top = BOOT_DOUBLE_FAULT_STACK.0.get() as u64 + IST_STACK_SIZE as u64;
    // SAFETY: the boot processor's own tables, loaded once, and a stack the
    // CPU alone uses.
    unsafe { load(tables, stack_top & !0xF) };
}

/// Point this processor's `RSP0` at `top`.
///
/// `RSP0` is the stack an interrupt or exception from ring 3 switches to, so
/// this has to follow whichever task is running: land a page fault on the
/// previous task's stack and two tasks share one, which corrupts quietly and
/// at a distance.
///
/// # Finding the TSS
///
/// By asking the processor, rather than by remembering. `STR` gives this
/// processor's task register and `SGDT` gives its GDT, so the descriptor --
/// and the base address inside it -- can be read back from the hardware that
/// is actually using them.
///
/// The alternative was to record each processor's `Tables` pointer somewhere
/// per-processor, and the ordering makes that worse than it sounds: the GDT is
/// loaded in `init_traps`, long before `smp` exists to hold anything per
/// processor. A cached pointer would have to be filled in later by code that
/// remembered to, and would be wrong rather than absent if it were not.
///
/// # Safety
///
/// A TSS must be loaded, which [`init`] or [`init_secondary`] has done by the
/// time any task runs, and `top` must be the top of a stack this processor
/// alone uses.
pub(crate) unsafe fn set_privilege_stack(top: u64) {
    // SAFETY: reads the task register; no memory is touched.
    let selector = unsafe { cpu::read_task_register() };
    // SAFETY: writes ten bytes of GDTR into a local.
    let (gdt_base, gdt_limit) = unsafe { cpu::read_gdt() };

    let index = usize::from(selector & !0x7);
    // A 64-bit TSS descriptor is sixteen bytes, so both halves must be inside
    // the table. A limit that says otherwise means the GDT is not the one this
    // code built, and writing into it would be writing somewhere arbitrary.
    if index + 16 > usize::from(gdt_limit) + 1 {
        return;
    }

    let descriptor = (gdt_base as usize + index) as *const u64;
    // SAFETY: `index` is inside the GDT the processor is using, which this
    // module built and which lives for the life of the processor.
    let low = unsafe { descriptor.read() };
    // SAFETY: the second half of the same descriptor, whose sixteen bytes were
    // bounds-checked above.
    let upper = unsafe { descriptor.add(1) };
    // SAFETY: as above; the pointer is inside the table.
    let high = unsafe { upper.read() };

    // The base is scattered across the descriptor in three pieces below 32
    // bits and one above, an arrangement inherited from the 286 and preserved
    // through two widenings.
    let base =
        ((low >> 16) & 0x00FF_FFFF) | (((low >> 56) & 0xFF) << 24) | ((high & 0xFFFF_FFFF) << 32);

    let rsp0 = (base as usize + TSS_PRIVILEGE_STACK) as *mut u64;
    // SAFETY: the TSS this processor has loaded, at the offset long mode puts
    // `RSP0`. Unaligned because the 32-bit TSS layout put a `u32` before it,
    // which is why `TaskStateSegment` is a byte array in the first place.
    unsafe { rsp0.write_unaligned(top) };
}

/// Build and load this secondary processor's own GDT and TSS.
///
/// # Safety
///
/// Must be called once, on the secondary processor itself, before it enables
/// interrupts.
pub(crate) unsafe fn init_secondary() -> Result<(), &'static str> {
    let stack = crate::vmap::allocate_stack()
        .map_err(|_| "no double-fault stack for a secondary processor")?;
    // Leaked: the processor uses these for the rest of its life.
    let tables: &'static mut Tables = Box::leak(Box::new(Tables::new()));
    // SAFETY: fresh tables that nothing else refers to, and a fresh stack.
    unsafe { load(tables, stack.top) };
    Ok(())
}

/// Fill `tables` for this processor and make it use them.
///
/// # Safety
///
/// `tables` must belong to this processor alone and live as long as it runs,
/// and `double_fault_top` must be the top of a stack nothing else uses.
unsafe fn load(tables: &'static mut Tables, double_fault_top: u64) {
    tables
        .tss
        .set_interrupt_stack(DOUBLE_FAULT_IST, double_fault_top);
    tables.tss.deny_all_ports();

    let tss_address = (&raw const tables.tss) as u64;
    let (low, high) = tss_descriptor(tss_address);

    tables.gdt = [
        0,
        USER_SEGMENT | PRESENT | EXECUTABLE | LONG_MODE,
        USER_SEGMENT | PRESENT | WRITABLE,
        USER_SEGMENT | PRESENT | EXECUTABLE | dpl(3),
        USER_SEGMENT | PRESENT | WRITABLE | dpl(3),
        USER_SEGMENT | PRESENT | EXECUTABLE | LONG_MODE | dpl(3),
        low,
        high,
    ];

    let pointer = DescriptorTablePointer {
        limit: (size_of_val(&tables.gdt) - 1) as u16,
        base: (&raw const tables.gdt) as u64,
    };

    // SAFETY: `pointer` describes the table just built, whose selectors match
    // the constants the reload below and every gate use.
    unsafe { cpu::load_gdt(&raw const pointer as u64) };
    // SAFETY: the GDT is loaded and holds a flat kernel code and data segment
    // at these selectors.
    unsafe { cpu::reload_segments(KERNEL_CODE, KERNEL_DATA) };
    // SAFETY: slot 0x30 of the GDT just built is an available 64-bit TSS
    // descriptor for `tables.tss`.
    unsafe { cpu::load_tss(TSS_SELECTOR) };

    // Give `RSP0` the stack the kernel is running on right now. Nothing enters
    // user mode yet, so nothing reads it yet -- but a TSS whose `RSP0` is zero
    // is one where the first trap from ring 3 pushes onto address zero, and it
    // costs nothing to make that impossible from the start. The scheduler
    // replaces it per task from stage 5.
    tables
        .tss
        .set_privilege_stack(cpu::read_stack_pointer() & !0xF);
}

/// Split a TSS base address into the two halves of a system descriptor.
fn tss_descriptor(base: u64) -> (u64, u64) {
    let limit = (TSS_SIZE - 1) as u64;

    let low = limit
        | ((base & 0x00FF_FFFF) << 16)
        | TSS_AVAILABLE
        | PRESENT
        | (((base >> 24) & 0xFF) << 56);
    let high = base >> 32;
    (low, high)
}
