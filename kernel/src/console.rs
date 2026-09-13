//! The kernel's early console.
//!
//! This is one of the two output devices the kernel drives itself, and
//! `docs/ARCHITECTURE.md` §1 names both as the exceptions they are: every other
//! device is driven from userspace. The other is the framebuffer a panic is
//! drawn on (`panic/screen.rs`), which draws what this module keeps of its
//! recent output. It exists because a panic before `devmgr` starts has to say
//! something, and because a boot test with no serial output cannot tell a
//! kernel that hung from one that never ran.
//!
//! The port itself is architecture-specific — a 16550 behind x86-64 I/O ports,
//! a PL011 behind `MMIO` on `AArch64` — so the bytes go out through
//! `arch::console`.
//!
//! # One line at a time
//!
//! The port is behind a lock, taken once per [`println!`], so two CPUs
//! printing at once produce two whole lines rather than one line of both.
//!
//! The lock is also the one way the console can make a failure *worse*: a CPU
//! that faults while holding it — in the middle of formatting, say — would
//! report the fault by printing, wait for its own lock, and turn a
//! `FERRIX-PANIC` line into a boot test that times out saying nothing. So once
//! [`begin_panic`] has been called, a writer waits a bounded time for the lock
//! and then writes without it. A panic report interleaved with another CPU's
//! line is legible; one that never appears is not.

use core::fmt::{self, Write};
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use ferrix_sync::IrqSpinLock;

/// Whether [`crate::arch::init_console`] has run.
static READY: AtomicBool = AtomicBool::new(false);

/// Whether something is reporting a failure the kernel will not survive.
static PANICKING: AtomicBool = AtomicBool::new(false);

/// The port, one writer at a time.
///
/// Interrupt-masking so that a handler that prints cannot interrupt a line
/// being printed on its own CPU and wait for it.
static PORT: IrqSpinLock<Port, crate::arch::Irq> = IrqSpinLock::new(Port);

/// Attempts at the lock a panicking writer makes before writing without it.
///
/// Long enough for another CPU to finish any line it is part way through, and
/// short enough that a lock nobody will ever release costs a moment rather
/// than the boot test's timeout.
const PANIC_SPINS: u32 = 10_000_000;

/// How much recent console output is kept for a failure report.
///
/// A little more than the largest QR code carries, which is the most any
/// reader of it can use.
const RECENT_BYTES: usize = 4096;

/// The last [`RECENT_BYTES`] written to the port, as a ring.
///
/// Atomics rather than a lock, because the moment this is read is a panic,
/// possibly on a processor holding the port's lock or part way through a
/// line. A racing writer can leave a byte stale; nothing can leave the ring
/// unreadable.
static RECENT: [AtomicU8; RECENT_BYTES] = [const { AtomicU8::new(0) }; RECENT_BYTES];

/// How many bytes have ever gone into [`RECENT`]. The next one goes at this
/// count modulo the ring's length.
static RECENT_WRITTEN: AtomicUsize = AtomicUsize::new(0);

/// Record that the port is configured and may be written.
///
/// # Safety
///
/// The caller must have configured the port `arch::console::write_byte` writes
/// to, including any mapping it needs.
pub(crate) unsafe fn mark_ready() {
    READY.store(true, Ordering::Release);
}

/// Say that what is printed from here on is a failure report.
///
/// Called by the panic handler and by the trap path's fatal reports, before
/// they print anything.
pub(crate) fn begin_panic() {
    PANICKING.store(true, Ordering::Relaxed);
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
            remember(byte);
        }
        Ok(())
    }
}

/// Keep `byte` in the recent-output ring.
fn remember(byte: u8) {
    let at = RECENT_WRITTEN.fetch_add(1, Ordering::Relaxed);
    if let Some(slot) = RECENT.get(at % RECENT_BYTES) {
        slot.store(byte, Ordering::Relaxed);
    }
}

/// Copy the most recent console output into `out`, oldest byte first, and
/// return how many bytes were copied: at most `out.len()`, and never more
/// than the ring has kept.
pub(crate) fn recent(out: &mut [u8]) -> usize {
    let written = RECENT_WRITTEN.load(Ordering::Relaxed);
    let count = written.min(RECENT_BYTES).min(out.len());
    let start = written.saturating_sub(count);
    for (offset, slot) in out.iter_mut().take(count).enumerate() {
        *slot = RECENT
            .get(start.wrapping_add(offset) % RECENT_BYTES)
            .map_or(b'?', |byte| byte.load(Ordering::Relaxed));
    }
    count
}

/// Write formatted output to the console, if there is one yet.
pub(crate) fn write(arguments: fmt::Arguments<'_>) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    if !PANICKING.load(Ordering::Relaxed) {
        let _ = PORT.lock().write_fmt(arguments);
        return;
    }

    for _ in 0..PANIC_SPINS {
        if let Some(mut port) = PORT.try_lock() {
            let _ = port.write_fmt(arguments);
            return;
        }
        spin_loop();
    }
    // Whoever holds the lock is not going to release it — quite possibly
    // because it is this CPU, part way through the line that failed.
    let _ = Port.write_fmt(arguments);
}

/// Write raw bytes to the console.
///
/// What `write(2)` to file descriptor 1 or 2 ends up calling. Separate from
/// [`write`] because a program's output is bytes, not `format_args!`: it need
/// not be valid UTF-8, and a `\0` or a stray `\xFF` in the middle of it is
/// the program's business rather than something to refuse.
///
/// The newline translation is kept, and it is standing in for something: a
/// terminal line discipline's `ONLCR`, which is what turns a bare `\n` into
/// CRLF on a real Linux tty. Ferrix has no tty layer yet, and without the
/// translation every program's output would climb the screen in a staircase.
/// When stage 15 brings ttys, this moves there and this function stops
/// translating.
pub(crate) fn write_bytes(bytes: &[u8]) {
    emit(bytes, true);
}

/// Write raw bytes to the console exactly as they are: a terminal whose
/// program turned `ONLCR` or `OPOST` off. See `crate::fs::terminal`.
pub(crate) fn write_raw(bytes: &[u8]) {
    emit(bytes, false);
}

/// Put `bytes` on the port under its lock, a bare newline as CRLF if asked.
fn emit(bytes: &[u8], crlf: bool) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let mut port = PORT.lock();
    let _ = port.write_str("");
    for &byte in bytes {
        if crlf && byte == b'\n' {
            crate::arch::console::write_byte(b'\r');
        }
        crate::arch::console::write_byte(byte);
    }
}

/// Print to the kernel console, with a newline.
macro_rules! println {
    () => { $crate::console::write(format_args!("\n")) };
    ($($argument:tt)*) => {
        $crate::console::write(format_args!("{}\n", format_args!($($argument)*)))
    };
}

pub(crate) use println;
