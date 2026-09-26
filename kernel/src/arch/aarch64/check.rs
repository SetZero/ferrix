//! The boot check for what this architecture decides on its own from values
//! the machine hands it.
//!
//! Stage 3's trap check proves the entry path with two breakpoints and three
//! page faults, which is every syndrome an ordinary boot raises. The decoding
//! behind it has more to say: a misaligned program counter or stack pointer, a
//! trapped floating-point exception, a class the architecture has not
//! allocated. Each is required here to become the trap and the signal Linux
//! raises for it (`arch/arm64/kernel/traps.c` and `mm/fault.c`), from a frame
//! built with that syndrome, because a program that raises one is a program
//! the kernel's own checks do not carry. The rest is the same kind of thing:
//!
//! * the `console=` value a loader names a `ramoops` zone with, read or
//!   refused -- the Pixel 7's console, whose parser is the one part of it a
//!   machine with a PL011 can run;
//! * which driver a GIC firmware describes with version zero gets;
//! * rewinding a system call a signal interrupted, and pointing one whose
//!   restart is `restart_syscall`'s at that call;
//! * masking a line at the controller and letting it through again, which a
//!   device whose driver holds its interrupt relies on, read back from the
//!   distributor or this core's redistributor, a shared line and a private
//!   one;
//! * the refusals: an identifier the controller does not have, a `GICv2m`
//!   frame at address zero or naming a range that is not shared peripheral
//!   interrupts, and on a GICv3 a device ID past the ITS's device table.
//!
//! Every frame, line and request is made for the check and given back: the
//! lines were ones nothing had enabled, and are left that way.

use alloc::format;
use core::hint::black_box;

use ferrix_linux_abi::types::{SIGBUS, SIGFPE, SIGILL, SIGSEGV, SIGTRAP};

use super::trap::{TrapFrame, UserRegs, classify, fault_signal};
use crate::console::println;
use crate::trap::{PageFault, Trap};

/// Vector entry for a synchronous exception from EL1.
const FROM_KERNEL: u64 = 4;
/// Vector entry for an IRQ from EL1.
const IRQ_FROM_KERNEL: u64 = 5;
/// Vector entry for a synchronous exception from EL0.
const FROM_USER: u64 = 8;
/// Vector entry for an IRQ from EL0.
const IRQ_FROM_USER: u64 = 9;

/// `si_code` values the signal carries.
const SI_KERNEL: i32 = 0x80;
const SEGV_MAPERR: i32 = 1;
const SEGV_ACCERR: i32 = 2;
const BUS_ADRALN: i32 = 1;
const ILL_ILLOPC: i32 = 1;
const TRAP_BRKPT: i32 = 1;

/// Where the frames say the trap was taken and what it touched.
const PC: u64 = 0x0040_1002;
const SP: u64 = 0x7fff_0008;
const FAR: u64 = 0x0dea_d000;

/// Run the check, and say what it proved.
///
/// # Errors
///
/// What did not hold.
pub(crate) fn check() -> Result<(), &'static str> {
    let decoded = check_trap_decoding()?;
    check_frames_render()?;
    let zones = check_ramoops_zones()?;
    let versions = check_described_version()?;
    check_restart_rewind()?;
    let lines = check_masking()?;
    let refused = check_refusals()?;
    println!(
        "  machine  {decoded} exception syndromes decoded to the trap and signal Linux gives them, \
         {zones} console= values read as a ramoops zone or refused, {versions} GIC descriptions \
         given their driver, {lines} idle lines masked and let through at the controller, \
         {refused} requests refused as specified"
    );
    Ok(())
}

/// A frame for an exception taken at vector entry `kind` with exception
/// class `class` and syndrome `iss`.
const fn frame(kind: u64, class: u64, iss: u64) -> TrapFrame {
    TrapFrame {
        x: [0; 31],
        sp: SP,
        elr: PC,
        spsr: 0,
        esr: class << 26 | iss,
        far: FAR,
        kind,
        reserved: 0,
    }
}

/// One case: the frame, the trap it must classify as, and the signal a
/// program that took it must get -- `None` where the trap is not a fault
/// (a system call, an interrupt).
type Case = (TrapFrame, Trap, Option<(u32, i32, u64)>);

/// The exception classes, and what each becomes. Returns how many.
fn check_trap_decoding() -> Result<usize, &'static str> {
    /// `ISS` of a data abort: the write bit.
    const WRITE: u64 = 1 << 6;
    /// Fault status: a translation fault at level 3.
    const TRANSLATION: u64 = 0b00_0111;
    /// Fault status: a permission fault at level 3.
    const PERMISSION: u64 = 0b00_1111;
    /// Fault status: an access-flag fault at level 1.
    const ACCESS_FLAG: u64 = 0b00_1001;

    let fault = |address, write, execute, user, present| {
        Trap::PageFault(PageFault {
            address,
            write,
            execute,
            user,
            present,
        })
    };
    let named = |class: u64, name| Trap::Fault {
        name,
        code: class << 26,
    };
    let cases: [Case; 14] = [
        (
            frame(FROM_USER, 0b11_1100, 0),
            Trap::Breakpoint,
            Some((SIGTRAP, TRAP_BRKPT, PC)),
        ),
        (frame(FROM_USER, 0b01_0101, 0), Trap::SystemCall, None),
        (
            frame(FROM_USER, 0, 0),
            Trap::IllegalInstruction,
            Some((SIGILL, ILL_ILLOPC, PC)),
        ),
        (
            frame(FROM_USER, 0b10_0100, WRITE | TRANSLATION),
            fault(FAR, true, false, true, false),
            Some((SIGSEGV, SEGV_MAPERR, FAR)),
        ),
        (
            frame(FROM_KERNEL, 0b10_0101, PERMISSION),
            fault(FAR, false, false, false, true),
            Some((SIGSEGV, SEGV_ACCERR, FAR)),
        ),
        // The write bit's position means something else in an instruction
        // abort, and must not make it a write.
        (
            frame(FROM_USER, 0b10_0000, WRITE | ACCESS_FLAG),
            fault(FAR, false, true, true, false),
            Some((SIGSEGV, SEGV_MAPERR, FAR)),
        ),
        (
            frame(FROM_KERNEL, 0b10_0001, TRANSLATION),
            fault(FAR, false, true, false, false),
            Some((SIGSEGV, SEGV_MAPERR, FAR)),
        ),
        (
            frame(FROM_USER, 0b10_0010, 0),
            named(0b10_0010, "misaligned program counter"),
            Some((SIGBUS, BUS_ADRALN, PC)),
        ),
        (
            frame(FROM_USER, 0b10_0110, 0),
            named(0b10_0110, "stack pointer alignment fault"),
            Some((SIGBUS, BUS_ADRALN, SP)),
        ),
        (
            frame(FROM_USER, 0b10_1100, 0),
            named(0b10_1100, "floating point exception"),
            Some((SIGFPE, SI_KERNEL, PC)),
        ),
        (
            frame(FROM_USER, 0b00_0001, 0),
            named(0b00_0001, "trapped WFI or WFE"),
            Some((SIGILL, ILL_ILLOPC, PC)),
        ),
        // A class the architecture has not allocated.
        (
            frame(FROM_USER, 0b11_1111, 0),
            named(0b11_1111, "exception"),
            Some((SIGILL, ILL_ILLOPC, PC)),
        ),
        (frame(IRQ_FROM_USER, 0, 0), Trap::Interrupt(9), None),
        (frame(IRQ_FROM_KERNEL, 0, 0), Trap::Interrupt(5), None),
    ];
    // Through `black_box`, so that the decoder runs on each frame as it would
    // on a trap's, rather than being folded away against constant input.
    for (frame, trap, signal) in &cases {
        let frame = &black_box(*frame);
        if classify(frame) != *trap {
            return Err("an exception syndrome was classified as the wrong trap");
        }
        if let Some(signal) = signal
            && fault_signal(frame, trap) != *signal
        {
            return Err("a program's fault was given a signal Linux would not give it");
        }
    }
    if !frame(FROM_USER, 0, 0).came_from_user() || frame(FROM_KERNEL, 0, 0).came_from_user() {
        return Err("a trap frame misreported whether EL0 took the exception");
    }
    Ok(cases.len())
}

/// A frame, and a program's registers made of one, render for a report:
/// each type carries `Debug` so that it can, and a rendering without the
/// registers would be no use to one.
fn check_frames_render() -> Result<(), &'static str> {
    let frame = frame(FROM_USER, 0b10_0100, 0);
    let text = format!("{frame:?} {:?}", UserRegs(frame));
    if !text.contains("esr") || !text.contains("UserRegs") {
        return Err("a trap frame's rendering left out its registers");
    }
    Ok(())
}

/// The loader's `console=ramoops,<address>,<size>`, read or refused.
/// Returns how many values were tried.
fn check_ramoops_zones() -> Result<usize, &'static str> {
    use super::console::ramoops_zone;

    let read = [(
        "ramoops,0x9ff00000,0x40000",
        Some((0x9ff0_0000_u64, 0x4_0000_u64)),
    )];
    let refused = [
        // The base must be a page's, which the mapping needs.
        "ramoops,0x9ff00800,0x40000",
        // The zone must hold its header and a byte.
        "ramoops,0x9ff00000,0xc",
        "ramoops,0x9ff00000",
        "ramoops,0x9ff00000,0x40000,0x1",
        // Both numbers are hexadecimal with a prefix, as the loader writes them.
        "ramoops,9ff00000,0x40000",
        "ttyAMA0",
    ];
    for (value, zone) in read {
        if ramoops_zone(black_box(value)) != zone {
            return Err("a console= value naming a ramoops zone was not read as that zone");
        }
    }
    if refused
        .iter()
        .any(|value| ramoops_zone(black_box(value)).is_some())
    {
        return Err("a console= value that names no usable ramoops zone was taken as one");
    }
    Ok(read.len() + refused.len())
}

/// An idle shared line and an idle private one are masked and let through,
/// each read back. Returns how many lines.
fn check_masking() -> Result<usize, &'static str> {
    use super::gic;

    let mut lines = 0;
    for private in [false, true] {
        let Some(line) = gic::idle_line_for_check(private) else {
            continue;
        };
        super::unmask_interrupt(line)?;
        let through = gic::enabled_for_check(line);
        super::mask_interrupt(line)?;
        if !through || gic::enabled_for_check(line) {
            return Err("a line the controller was told to mask or let through did not change");
        }
        lines += 1;
    }
    if lines == 0 {
        return Err("the controller has no idle line to mask");
    }
    Ok(lines)
}

/// What the interrupt controller must refuse. Returns how many requests.
fn check_refusals() -> Result<usize, &'static str> {
    // 1020 to 1023 are the controller's answers, not lines: 1023 is
    // "spurious".
    let special = super::gic::FIRST_SPECIAL_ID;
    if super::mask_interrupt(special).is_ok() || super::unmask_interrupt(special + 3).is_ok() {
        return Err("a special interrupt identifier was masked or let through as a line");
    }
    // A `GICv2m` frame at address zero, or one whose range firmware states
    // past the shared peripheral interrupts, is refused before anything is
    // recorded, so the frame the machine has is untouched.
    if crate::arch::gicv2::init_msi_frame(0, None).is_ok()
        || crate::arch::gicv2::init_msi_frame(0x0802_0000, Some((1024, 64))).is_ok()
    {
        return Err("a GICv2m frame at address zero or past the SPIs was taken");
    }
    let mut refused = 4;
    // A GICv3's ITS translates by device ID, and one past its device table
    // has nowhere to go. A GICv2m frame takes no device ID, so the request
    // would be granted there, and is not made.
    if super::gic::is_v3() && super::msi_doorbell().is_some() {
        if super::msi_allocate(u32::MAX).is_ok() {
            return Err("the ITS mapped a device ID past its device table");
        }
        refused += 1;
    }
    Ok(refused)
}

/// Which driver each description firmware can give gets: the version it
/// states, or for version zero the one its register blocks imply. Returns
/// how many descriptions.
///
/// # Errors
///
/// A description given the wrong driver.
fn check_described_version() -> Result<usize, &'static str> {
    let layout = |version, cpu_interface, redistributors| super::gic::Layout {
        distributor: 0x0800_0000,
        cpu_interface,
        redistributors,
        version,
        msi_frame: None,
        its: None,
    };
    let cases = [
        (layout(0, 0x0801_0000, None), 2),
        (layout(0, 0, Some((0x080A_0000, 0x00F6_0000))), 3),
        (layout(2, 0x0801_0000, None), 2),
        (layout(3, 0, Some((0x080A_0000, 0x00F6_0000))), 3),
        (layout(4, 0, Some((0x080A_0000, 0x00F6_0000))), 4),
        // Neither register block: nothing to probe, and nothing is chosen.
        (layout(0, 0, None), 0),
    ];
    if cases
        .iter()
        .any(|(layout, version)| super::gic::described_version(black_box(layout)) != *version)
    {
        return Err("a GIC firmware described was given the wrong driver");
    }
    Ok(cases.len())
}

/// A system call a signal interrupted is rewound to run again when the
/// handler returns, and one whose restart is `restart_syscall`'s -- a
/// `clock_nanosleep` a stop interrupted -- is pointed at that call instead:
/// the program's own number in `x8` is kept for the first and replaced for
/// the second, `x0` gets its argument back, and the return address steps
/// back over the `svc`. Linux's `arch_do_signal_or_restart`.
fn check_restart_rewind() -> Result<(), &'static str> {
    use ferrix_linux_abi::nr::aarch64::{CLOCK_NANOSLEEP, RESTART_SYSCALL};

    for restart_block in [false, true] {
        let mut trap = frame(FROM_USER, 0b01_0101, 0);
        trap.x[0] = (-4_i64) as u64;
        trap.x[8] = CLOCK_NANOSLEEP as u64;
        let mut context = super::UserContext::from_trap(&trap);
        context.rewind_syscall(CLOCK_NANOSLEEP as u64, 7, black_box(restart_block));
        context.store_trap(&mut trap);
        let number = if restart_block {
            RESTART_SYSCALL
        } else {
            CLOCK_NANOSLEEP
        };
        if trap.x.first() != Some(&7)
            || trap.x.get(8) != Some(&(number as u64))
            || trap.elr != PC - 4
        {
            return Err("an interrupted system call was not rewound to run again as it must");
        }
    }
    Ok(())
}
