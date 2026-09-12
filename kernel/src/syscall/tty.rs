//! The terminal requests a program makes of the console.
//!
//! Only what an interactive shell asks, and answered for what the console is
//! today: a serial port with a canonical line discipline in front of it and
//! no terminal settings behind it.
//!
//! # What `sh -i` asks, measured
//!
//! With `strace`, on the host, against the same static busybox. On a
//! terminal, the shell asks `TCGETS` on descriptor 0 and, when that succeeds,
//! puts the terminal into raw mode with `TCSETS`, asks `TIOCGWINSZ` for the
//! width, and reads one byte at a time, doing its own echo and line editing.
//! With its input on a pipe, `TCGETS` and `TIOCGWINSZ` are both `ENOTTY`; the
//! shell says `can't access tty; job control turned off`, reads whole lines,
//! and otherwise works -- `printf` included, which asks `TIOCGWINSZ` of
//! descriptor 1 and carries on when refused.
//!
//! # Why both are refused
//!
//! Because the second behaviour is the one the console can support. A
//! successful `TCGETS` would tell the shell it may switch echo and canonical
//! mode off, and nothing here can: `crate::fs::console` echoes and edits the
//! line itself, so a shell doing the same would echo every character twice and
//! edit a line that had already been edited. Answering `TIOCGWINSZ` while
//! refusing `TCGETS` would describe a terminal that is not one.
//!
//! Both start answering together, when the console has termios to report and
//! a line discipline that honours `TCSETS` -- stage 15's tty layer.

use alloc::sync::Arc;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{TCGETS, TIOCGWINSZ};
use ferrix_vfs::OpenFile;

use crate::syscall::process::Process;

/// `ioctl` on a descriptor that names the console.
///
/// The caller, `crate::syscall::fd::sys_ioctl`, has resolved the descriptor
/// and checked that it is the console.
pub(crate) fn ioctl(
    process: &Process,
    file: &Arc<OpenFile>,
    request: u32,
    arg: u64,
) -> Result<usize, Errno> {
    let _ = (process, file, arg);
    match request {
        // The two a shell asks. Refused on purpose: see the module
        // documentation for why that is what keeps `sh -i` working.
        TCGETS | TIOCGWINSZ => Err(Errno::ENOTTY),
        // Everything else a terminal answers, which nothing has asked yet.
        _ => Err(Errno::ENOTTY),
    }
}
