//! The terminal requests a program makes of the console.
//!
//! # What `sh -i` asks, measured
//!
//! With `strace`, on the host, against the same static busybox. On a
//! terminal, the shell asks `TCGETS` on descriptor 0 and, when that succeeds,
//! puts the terminal into raw mode with `TCSETS`, asks `TIOCGWINSZ` for the
//! width, and reads one byte at a time, doing its own echo and line editing.
//! For job control it asks `TIOCGPGRP` and requires the answer to be its own
//! group, then makes each job's group the foreground with `TIOCSPGRP` and
//! takes the terminal back afterwards.
//!
//! These used to be refused, because the console echoed and edited every line
//! itself and a shell doing the same doubled every character. They answer now
//! because `crate::fs::terminal` honours `ICANON` and `ECHO`: raw mode really
//! is raw.
//!
//! # The controlling terminal
//!
//! Linux gives a session leader with no controlling terminal the first
//! terminal it opens. The first process here does not open the console -- the
//! kernel hands it descriptors 0, 1 and 2 -- so the console is given instead
//! to a session leader the first time it asks about job control, provided no
//! live session holds it already. There is one terminal, so a process's
//! controlling terminal is the console exactly when the console's session is
//! the process's.
//!
//! Every request that needs a controlling terminal answers `ENOTTY` to a
//! process whose session does not hold the console, as Linux does.

use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    FIONREAD, TCFLSH, TCGETS, TCIFLUSH, TCIOFLUSH, TCION, TCOFLUSH, TCSETS, TCSETSF, TCSETSW,
    TCXONC, TERMIOS_BYTES, TIOCGPGRP, TIOCGSID, TIOCGWINSZ, TIOCNOTTY, TIOCSCTTY, TIOCSPGRP,
    TIOCSWINSZ,
};
use ferrix_vfs::OpenFile;

use crate::fs::terminal::{self, Terminal, Termios, Winsize};
use crate::syscall::process::Process;
use crate::syscall::{registry, uaccess};

/// `ioctl` on a descriptor that names the console.
///
/// The caller, `crate::syscall::fd::sys_ioctl`, has resolved the descriptor
/// and checked that it is the console. Every request not listed is `ENOTTY`,
/// which is what Linux's terminal layer answers for one it does not know.
pub(crate) fn ioctl(
    process: &Process,
    file: &Arc<OpenFile>,
    request: u32,
    arg: u64,
) -> Result<usize, Errno> {
    let _ = file;
    match request {
        TCGETS => {
            let termios = terminal::with(|terminal| terminal.discipline.termios());
            put(process, arg, &termios.to_bytes())
        }
        TCSETS | TCSETSW | TCSETSF => {
            let mut bytes = [0_u8; TERMIOS_BYTES];
            get(process, arg, &mut bytes)?;
            let termios = Termios::from_bytes(&bytes);
            // Nothing is buffered on the way out, so "once output has
            // drained" is now, and `TCSETSW` is `TCSETS`.
            terminal::with(|terminal| {
                if request == TCSETSF {
                    terminal.discipline.flush_input();
                }
                terminal.discipline.set_termios(termios);
            });
            Ok(0)
        }
        TIOCGWINSZ => {
            let size = terminal::with(|terminal| terminal.winsize);
            put(process, arg, &size.to_bytes())
        }
        TIOCSWINSZ => {
            let mut bytes = [0_u8; 8];
            get(process, arg, &mut bytes)?;
            // Linux raises `SIGWINCH` on the foreground group when the size
            // changes; nothing raises it here yet.
            terminal::with(|terminal| terminal.winsize = Winsize::from_bytes(bytes));
            Ok(0)
        }
        FIONREAD => {
            let count = i32::try_from(terminal::available()).unwrap_or(i32::MAX);
            put(process, arg, &count.to_le_bytes())
        }
        TCFLSH => match arg {
            TCIFLUSH | TCIOFLUSH => {
                terminal::with(|terminal| terminal.discipline.flush_input());
                Ok(0)
            }
            // Output is never queued, so there is none to discard.
            TCOFLUSH => Ok(0),
            _ => Err(Errno::EINVAL),
        },
        // Output is never held back, so suspending and restarting it are both
        // already true.
        TCXONC if arg <= TCION => Ok(0),
        TCXONC => Err(Errno::EINVAL),
        TIOCSCTTY | TIOCNOTTY | TIOCGPGRP | TIOCSPGRP | TIOCGSID => {
            job_control(process, request, arg)
        }
        _ => Err(Errno::ENOTTY),
    }
}

/// The requests about sessions and process groups.
///
/// The live processes are listed before the terminal is locked and dropped
/// after it is released: dropping the last reference to a process frees its
/// memory, which is not something to do under a spin lock.
fn job_control(process: &Process, request: u32, arg: u64) -> Result<usize, Errno> {
    let live = registry::live();
    let answer = match request {
        TIOCSCTTY => terminal::with(|terminal| take_controlling(process, terminal, &live, arg)),
        TIOCNOTTY => terminal::with(|terminal| {
            if terminal.session != process.sid() || terminal.session == 0 {
                return Err(Errno::ENOTTY);
            }
            // A session leader giving it up takes its session with it. Linux
            // also raises `SIGHUP` and `SIGCONT` on the foreground group.
            if process.pid() == process.sid() {
                terminal.session = 0;
                terminal.foreground = 0;
            }
            Ok(0)
        }),
        TIOCGPGRP => terminal::with(|terminal| controlling(process, terminal, &live))
            .and_then(|(_, foreground)| put_int(process, arg, foreground)),
        TIOCGSID => terminal::with(|terminal| controlling(process, terminal, &live))
            .and_then(|(session, _)| put_int(process, arg, session)),
        TIOCSPGRP => set_foreground(process, &live, arg),
        _ => Err(Errno::ENOTTY),
    };
    drop(live);
    answer
}

/// The console's session and foreground group, if the console is `process`'s
/// controlling terminal -- giving it to `process` first if it is a session
/// leader and nobody else holds it. See the module documentation.
fn controlling(
    process: &Process,
    terminal: &mut Terminal,
    live: &[Arc<Process>],
) -> Result<(u32, u32), Errno> {
    let sid = process.sid();
    if sid != 0 && terminal.session == sid {
        return Ok((terminal.session, terminal.foreground));
    }
    if sid != 0 && process.pid() == sid && session_is_gone(terminal.session, live) {
        terminal.session = sid;
        terminal.foreground = process.pgid();
        return Ok((terminal.session, terminal.foreground));
    }
    Err(Errno::ENOTTY)
}

/// `TIOCSCTTY`: Linux's rules. A session leader only; nothing to do if its
/// session already has the console; `EPERM` if another live session does,
/// unless `arg` is 1 and the caller may steal it -- which everyone may, since
/// everything runs as root.
fn take_controlling(
    process: &Process,
    terminal: &mut Terminal,
    live: &[Arc<Process>],
    arg: u64,
) -> Result<usize, Errno> {
    let sid = process.sid();
    if process.pid() != sid || sid == 0 {
        return Err(Errno::EPERM);
    }
    if terminal.session == sid {
        return Ok(0);
    }
    if !session_is_gone(terminal.session, live) && arg != 1 {
        return Err(Errno::EPERM);
    }
    terminal.session = sid;
    terminal.foreground = process.pgid();
    Ok(0)
}

/// `TIOCSPGRP`, in Linux's order: `ENOTTY` if the console is not the caller's
/// terminal, `EFAULT`, `EINVAL` for a negative group, `ESRCH` for a group
/// nobody is in, `EPERM` for one in another session.
///
/// A process in a background group changing the foreground would be sent
/// `SIGTTOU` on Linux unless it ignores it -- which a shell's children do not
/// yet, and nothing delivers a signal, so the change is simply allowed.
fn set_foreground(process: &Process, live: &[Arc<Process>], arg: u64) -> Result<usize, Errno> {
    let (session, _) = terminal::with(|terminal| controlling(process, terminal, live))?;
    let mut bytes = [0_u8; 4];
    get(process, arg, &mut bytes)?;
    let group = u32::try_from(i32::from_le_bytes(bytes)).map_err(|_| Errno::EINVAL)?;
    let members: Vec<u32> = live
        .iter()
        .filter(|other| other.pgid() == group)
        .map(|other| other.sid())
        .chain((process.pgid() == group).then(|| process.sid()))
        .collect();
    if members.is_empty() {
        return Err(Errno::ESRCH);
    }
    if !members.contains(&session) {
        return Err(Errno::EPERM);
    }
    terminal::with(|terminal| {
        if terminal.session == session {
            terminal.foreground = group;
        }
    });
    Ok(0)
}

/// Whether no live process is in session `sid`, so the console it held is
/// free. Zero is nobody's session.
fn session_is_gone(sid: u32, live: &[Arc<Process>]) -> bool {
    sid == 0 || !live.iter().any(|process| process.sid() == sid)
}

/// Raise `signal` on the console's foreground process group: what `ISIG`
/// asks for when the interrupt, quit or suspend character is typed.
///
/// Raise `signal` on the console's foreground process group: what the
/// interrupt, quit and suspend characters do.
///
/// Every live process in the group is sent it, as the kernel's own, which is
/// what Linux's line discipline does; a shell that catches `SIGINT` carries on
/// and a program that does not ends. With no foreground group, nothing is
/// raised and the keystroke is simply consumed.
pub(crate) fn signal_foreground_group(signal: u32) {
    let foreground = terminal::with(|terminal| terminal.foreground);
    if foreground == 0 {
        return;
    }
    for target in registry::live() {
        if target.pgid() == foreground {
            crate::syscall::kill::send(&target, signal, crate::syscall::signal::Origin::Kernel);
        }
    }
}

/// Copy a request's structure out of the program.
fn get(process: &Process, at: u64, bytes: &mut [u8]) -> Result<(), Errno> {
    uaccess::copy_from_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)
}

/// Copy a request's answer into the program.
fn put(process: &Process, at: u64, bytes: &[u8]) -> Result<usize, Errno> {
    uaccess::copy_to_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// Copy a `pid_t` answer into the program.
fn put_int(process: &Process, at: u64, value: u32) -> Result<usize, Errno> {
    put(process, at, &value.to_le_bytes())
}
