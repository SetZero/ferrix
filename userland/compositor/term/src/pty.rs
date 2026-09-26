//! The pseudoterminal, and the program on the other end of it.
//!
//! `/dev/ptmx` gives the master; `TIOCGPTN` says which pair it is;
//! `TIOCSPTLCK` unlocks it; `/dev/pts/<n>` is the slave. The child gets a
//! session of its own, the slave for its controlling terminal and for all
//! three of its standard descriptors, and then it is whatever program the
//! terminal was asked to run. That is `forkpty` written out, and it is
//! written out because Ferrix's C library does not have one.

use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::RawFd;

/// The type an `ioctl` request is on this target: `unsigned long` on most
/// of Linux and `int` on musl's 32-bit and on the BSDs, which `libc` calls
/// `Ioctl`.
type Request = libc::Ioctl;

/// `TIOCGPTN`: which pair a master is, `_IOR('T', 0x30, unsigned int)`.
const TIOCGPTN: Request = 0x8004_5430_u32 as Request;
/// `TIOCSPTLCK`: lock or unlock the slave, `_IOW('T', 0x31, int)`.
const TIOCSPTLCK: Request = 0x4004_5431_u32 as Request;
/// `TIOCSCTTY`: make this terminal the caller's controlling terminal.
const TIOCSCTTY: Request = 0x540E;
/// `TIOCSWINSZ`: say how large the terminal is.
const TIOCSWINSZ: Request = 0x5414;

/// `struct winsize`, which `TIOCSWINSZ` takes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Winsize {
    rows: u16,
    columns: u16,
    x_pixels: u16,
    y_pixels: u16,
}

/// A pseudoterminal with a program on it.
#[derive(Debug)]
pub struct Pty {
    master: RawFd,
    /// The child's process id, for waiting on it.
    child: libc::pid_t,
    /// Whether the child has been reaped.
    finished: bool,
}

impl Pty {
    /// Open a pair, start `program` on the slave, and keep the master.
    ///
    /// # Errors
    ///
    /// Whatever the pseudoterminal or the fork said.
    pub fn start(program: &str, arguments: &[String], size: (u16, u16)) -> io::Result<Self> {
        let master = open_master()?;
        let number = ptn(master)?;
        unlock(master)?;
        let slave = open_slave(number)?;
        set_size(master, size);

        // SAFETY: fork has no safe wrapper. The child calls only
        // async-signal-safe calls -- `setsid`, `ioctl`, `dup2`, `close` and
        // `execve` -- and never returns.
        let child = unsafe { libc::fork() };
        match child {
            -1 => {
                close(master);
                close(slave);
                Err(io::Error::last_os_error())
            }
            0 => {
                child_side(master, slave, program, arguments);
                // `child_side` execs or exits; this is unreachable, and an
                // exit here is what a child that could not exec must do.
                std::process::exit(127);
            }
            child => {
                close(slave);
                set_nonblocking(master)?;
                Ok(Self {
                    master,
                    child,
                    finished: false,
                })
            }
        }
    }

    /// Read whatever the program has written, or nothing.
    ///
    /// # Errors
    ///
    /// Whatever the read said, other than "nothing yet".
    pub fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        // SAFETY: a read of a descriptor this holds into a buffer of the
        // length given.
        let read = unsafe {
            libc::read(
                self.master,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                buffer.len(),
            )
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(0);
            }
            return Err(error);
        }
        Ok(read.unsigned_abs())
    }

    /// Type at the program.
    ///
    /// # Errors
    ///
    /// Whatever the write said.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut at = 0;
        while at < bytes.len() {
            let rest = bytes.get(at..).unwrap_or(&[]);
            // SAFETY: a write of a descriptor this holds from a buffer of
            // the length given.
            let written = unsafe {
                libc::write(
                    self.master,
                    rest.as_ptr().cast::<libc::c_void>(),
                    rest.len(),
                )
            };
            if written < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    return Ok(());
                }
                return Err(error);
            }
            at += written.unsigned_abs();
        }
        Ok(())
    }

    /// Write as much of `bytes` as the pseudoterminal takes now, and say how
    /// much that was.
    ///
    /// [`Pty::write`] drops what does not fit, which is right for a key --
    /// a program that has stopped reading has stopped reading keys too --
    /// and wrong for a paste, which is larger than the line discipline's
    /// buffer and has to arrive whole. The rest is the caller's to offer
    /// again once the program has read some.
    ///
    /// # Errors
    ///
    /// Whatever the write said, other than "not now".
    pub fn write_some(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        // SAFETY: a write of a descriptor this holds from a buffer of the
        // length given.
        let written = unsafe {
            libc::write(
                self.master,
                bytes.as_ptr().cast::<libc::c_void>(),
                bytes.len(),
            )
        };
        if written < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(0);
            }
            return Err(error);
        }
        Ok(written.unsigned_abs())
    }

    /// Say how large the terminal is now, which raises `SIGWINCH` on the
    /// program if it changed.
    pub fn resize(&mut self, size: (u16, u16)) {
        set_size(self.master, size);
    }

    /// Whether the program has finished, reaping it if it has.
    pub fn done(&mut self) -> bool {
        if self.finished {
            return true;
        }
        let mut status = 0;
        // SAFETY: a wait for this process's own child, which does not block.
        let waited = unsafe { libc::waitpid(self.child, &raw mut status, libc::WNOHANG) };
        if waited == self.child || waited < 0 {
            self.finished = true;
        }
        self.finished
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        close(self.master);
    }
}

/// What the child does: a session of its own, the slave for its terminal and
/// its three descriptors, and then the program.
fn child_side(master: RawFd, slave: RawFd, program: &str, arguments: &[String]) {
    close(master);
    // A session of its own, so that the slave can become its controlling
    // terminal: `setsid` then `TIOCSCTTY` is what every terminal emulator's
    // child does.
    //
    // SAFETY: a call the child of a fork may make.
    let _ = unsafe { libc::setsid() };
    // SAFETY: the slave this process just opened.
    let _ = unsafe { libc::ioctl(slave, TIOCSCTTY, 0) };
    for target in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        // SAFETY: two descriptors of this process's own.
        let _ = unsafe { libc::dup2(slave, target) };
    }
    if slave > libc::STDERR_FILENO {
        close(slave);
    }
    let Ok(path) = CString::new(program) else {
        return;
    };
    let mut argv: Vec<CString> = vec![path.clone()];
    argv.extend(
        arguments
            .iter()
            .filter_map(|argument| CString::new(argument.as_str()).ok()),
    );
    let mut pointers: Vec<*const libc::c_char> = argv.iter().map(|word| word.as_ptr()).collect();
    pointers.push(core::ptr::null());
    let environment: Vec<CString> = environment(std::env::vars_os())
        .into_iter()
        .filter_map(|variable| CString::new(variable).ok())
        .collect();
    let mut variables: Vec<*const libc::c_char> = environment
        .iter()
        .map(|variable| variable.as_ptr())
        .collect();
    variables.push(core::ptr::null());
    // SAFETY: the pointers are to NUL-terminated strings held alive until
    // the call, and both arrays end with a null.
    unsafe {
        let _ = libc::execve(path.as_ptr(), pointers.as_ptr(), variables.as_ptr());
    }
}

/// Where a program is looked for when nothing has said: the directories the
/// image puts programs in.
const DEFAULT_PATH: &str = "/bin:/usr/bin:/sbin:/usr/sbin";

/// The environment the program on the terminal starts with: the terminal's
/// own, as every terminal passes on, with `TERM` saying what this terminal
/// is and a `PATH` when the terminal was given none.
///
/// This used to be `TERM` and nothing else. The shell looked commands up
/// all the same -- zinc falls back to `/bin:/usr/bin` for its own lookups --
/// but what it started had no `PATH` to search, so `rustc` could not find
/// the `cc` it links with; and no `WAYLAND_DISPLAY` or `XDG_RUNTIME_DIR`, so
/// a Wayland program started from the shell could not find the compositor
/// the shell's own window is on.
fn environment(inherited: impl Iterator<Item = (OsString, OsString)>) -> Vec<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt as _;

    let mut variables: Vec<Vec<u8>> = Vec::new();
    let mut path = false;
    for (name, value) in inherited {
        // `TERM` is this terminal's to say: `xterm` is the name whose
        // escape sequences it understands, whatever it was started under.
        if name == "TERM" {
            continue;
        }
        path |= name == "PATH";
        let mut variable = name.as_bytes().to_vec();
        variable.push(b'=');
        variable.extend_from_slice(value.as_bytes());
        variables.push(variable);
    }
    variables.push(b"TERM=xterm".to_vec());
    if !path {
        variables.push(format!("PATH={DEFAULT_PATH}").into_bytes());
    }
    variables
}

/// Open `/dev/ptmx`.
fn open_master() -> io::Result<RawFd> {
    let path = c"/dev/ptmx";
    // SAFETY: a NUL-terminated path and constant flags.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Which pair a master is.
fn ptn(master: RawFd) -> io::Result<u32> {
    let mut number: libc::c_uint = 0;
    // SAFETY: `TIOCGPTN` writes one `unsigned int` at the pointer.
    let answer = unsafe { libc::ioctl(master, TIOCGPTN, &raw mut number) };
    if answer < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(number)
}

/// Unlock the slave, which `openpty` does before it opens it.
fn unlock(master: RawFd) -> io::Result<()> {
    let mut zero: libc::c_int = 0;
    // SAFETY: `TIOCSPTLCK` reads one `int` at the pointer.
    let answer = unsafe { libc::ioctl(master, TIOCSPTLCK, &raw mut zero) };
    if answer < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Open the slave of pair `number`.
fn open_slave(number: u32) -> io::Result<RawFd> {
    let path = CString::new(format!("/dev/pts/{number}"))
        .map_err(|_| io::Error::other("a slave path with a NUL in it"))?;
    // SAFETY: a NUL-terminated path held across the call, and constant
    // flags. `O_NOCTTY`: the terminal becomes the child's controlling
    // terminal through `TIOCSCTTY` after `setsid`, not through this open.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Say how large the terminal is.
fn set_size(fd: RawFd, (columns, rows): (u16, u16)) {
    let size = Winsize {
        rows,
        columns,
        x_pixels: 0,
        y_pixels: 0,
    };
    // SAFETY: `TIOCSWINSZ` reads one `struct winsize` at the pointer.
    let _ = unsafe { libc::ioctl(fd, TIOCSWINSZ, &raw const size) };
}

/// Read without waiting: the terminal has a window to keep drawing.
pub(crate) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: both calls take a descriptor this holds.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    let answer = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if answer < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Close a descriptor, ignoring what it says: nothing can be done about a
/// close that fails.
fn close(fd: RawFd) {
    // SAFETY: a descriptor this opened.
    let _ = unsafe { libc::close(fd) };
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    fn pairs(list: &[(&str, &str)]) -> impl Iterator<Item = (OsString, OsString)> {
        list.iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn strings(list: &[Vec<u8>]) -> Vec<String> {
        list.iter()
            .map(|variable| String::from_utf8_lossy(variable).into_owned())
            .collect()
    }

    #[test]
    fn the_program_gets_the_terminals_environment_with_its_own_term() {
        let got = super::environment(pairs(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("TERM", "linux"),
            ("PATH", "/opt/bin"),
        ]));
        assert_eq!(
            strings(&got),
            ["WAYLAND_DISPLAY=wayland-1", "PATH=/opt/bin", "TERM=xterm"]
        );
    }

    #[test]
    fn a_terminal_given_no_path_gives_its_program_one() {
        let got = super::environment(pairs(&[("HOME", "/")]));
        assert_eq!(
            strings(&got),
            [
                "HOME=/",
                "TERM=xterm",
                &format!("PATH={}", super::DEFAULT_PATH)
            ]
        );
    }
}
