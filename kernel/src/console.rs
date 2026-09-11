//! The kernel's early console.
//!
//! This is the one device driver inside the kernel, and `docs/ARCHITECTURE.md`
//! §1 names it as the exception it is: every other device is driven from
//! userspace. It exists because a panic before `devmgr` starts has to say
//! something, and because a boot test with no serial output cannot tell a
//! kernel that hung from one that never ran.
//!
//! The port itself is architecture-specific — a 16550 behind x86-64 I/O ports,
//! a PL011 behind `MMIO` on `AArch64` — so the bytes go out through
//! `arch::console`.

use core::cell::UnsafeCell;
use core::fmt::{self, Write};

/// Whether [`crate::arch::init_console`] has run.
struct Ready(UnsafeCell<bool>);

// SAFETY: early boot is single-threaded — no other CPU has been started and
// interrupts are masked — so there is never a second accessor. This becomes a
// per-CPU lock in stage 4, when there is more than one CPU to lock against.
unsafe impl Sync for Ready {}

static READY: Ready = Ready(UnsafeCell::new(false));

/// Record that the port is configured and may be written.
///
/// # Safety
///
/// The caller must have configured the port `arch::console::write_byte` writes
/// to, including any mapping it needs.
pub(crate) unsafe fn mark_ready() {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    unsafe { *READY.0.get() = true };
}

/// True once the console can be written.
fn is_ready() -> bool {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    unsafe { *READY.0.get() }
}

/// Somewhere for `format_args!` to go.
struct Port;

impl Write for Port {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            // A serial terminal wants CRLF; a bare newline leaves the cursor
            // where it was and the boot log becomes a staircase.
            if byte == b'\n' {
                crate::arch::console::write_byte(b'\r');
            }
            crate::arch::console::write_byte(byte);
        }
        Ok(())
    }
}

/// Write formatted output to the console, if there is one yet.
pub(crate) fn write(arguments: fmt::Arguments<'_>) {
    if !is_ready() {
        return;
    }
    let _ = Port.write_fmt(arguments);
}

/// Print to the kernel console, with a newline.
macro_rules! println {
    () => { $crate::console::write(format_args!("\n")) };
    ($($argument:tt)*) => {
        $crate::console::write(format_args!("{}\n", format_args!($($argument)*)))
    };
}

pub(crate) use println;
