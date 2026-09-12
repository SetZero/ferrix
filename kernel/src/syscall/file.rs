//! `write` and `writev`.
//!
//! The third of the three calls a static binary cannot survive losing — the
//! other two are TLS setup and `mprotect` — and the one that makes a program's
//! output visible, which is what the first milestone is for.
//!
//! # No file descriptor table yet
//!
//! Descriptors 1 and 2 go to the console and everything else is `EBADF`. That
//! is honest rather than a stub: there is no filesystem to open anything from,
//! so a table would map two numbers onto one device and nothing else.
//!
//! It is also not something a later stage has to unpick. When stage 8 brings
//! the VFS, the table replaces the *lookup* here — which descriptor names
//! which object — and not the call: `sys_write` will still copy from user
//! memory, still bound its chunks, and still report the count it managed. The
//! two hardcoded numbers are one `match` that becomes a lookup.

use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_sync::SpinLock;

use crate::arch;
use crate::console;
use crate::syscall::process::Process;
use crate::syscall::uaccess::{self, UserError};

/// Standard input.
const STDIN: u64 = 0;
/// Standard output.
const STDOUT: u64 = 1;
/// Standard error.
const STDERR: u64 = 2;

/// How much of a program's buffer is copied in at a time.
///
/// On the stack, so it is bounded by what a kernel stack can afford rather
/// than by what the program asked for. A program is entitled to `write` a
/// gigabyte in one call and the kernel must not try to hold it.
const CHUNK: usize = 256;

/// Linux's `IOV_MAX`: the most segments one `writev` may carry.
const IOV_MAX: u64 = 1024;

/// Everything the copy layer can refuse, as the program sees it.
fn refused(error: UserError) -> Errno {
    match error {
        // All three are `EFAULT` to a program. Which of its pages was wrong is
        // a fact about the kernel's view of its address space, and not
        // something the ABI has a way to say.
        UserError::NotUserRange | UserError::Overflow | UserError::Fault => Errno::EFAULT,
    }
}

/// Whether this descriptor is one of the two that go somewhere.
fn writable(fd: u64) -> Result<(), Errno> {
    if fd == STDOUT || fd == STDERR {
        Ok(())
    } else {
        Err(Errno::EBADF)
    }
}

/// `write`.
///
/// Reports the number of bytes written, which for the console is all of them
/// or an error. A short count is legal in the ABI and every correct caller
/// loops on it, but there is nothing here that can be short: the console does
/// not block and has no buffer to fill.
pub(crate) fn sys_write(process: &Process, fd: u64, buf: u64, len: u64) -> Result<usize, Errno> {
    writable(fd)?;
    if len == 0 {
        // Not a no-op in the ABI: a zero-length write still validates the
        // descriptor, which is why the check above comes first.
        return Ok(0);
    }
    let written = write_range(process, buf, len)?;
    usize::try_from(written).map_err(|_| Errno::EINVAL)
}

/// `writev`.
///
/// musl's buffered output goes through this rather than `write`, so a program
/// that prints with `printf` reaches here and not the simpler call.
///
/// The segment count is validated and the lengths summed *before* anything is
/// written, because the ABI says `EINVAL` for a total that overflows and a
/// program must not see half its output before being told no.
pub(crate) fn sys_writev(process: &Process, fd: u64, iov: u64, count: u64) -> Result<usize, Errno> {
    writable(fd)?;
    if count == 0 {
        return Ok(0);
    }
    if count > IOV_MAX {
        return Err(Errno::EINVAL);
    }

    // Two passes. The first reads every segment and checks the total, the
    // second writes. Reading the array twice costs little and means a
    // malformed later segment cannot leave earlier ones already printed.
    let mut total = 0_u64;
    for index in 0..count {
        let (_, len) = read_iovec(process, iov, index)?;
        total = total.checked_add(len).ok_or(Errno::EINVAL)?;
    }
    if i64::try_from(total).is_err() {
        // Linux refuses a total that will not fit in the return value rather
        // than reporting a negative count, which a caller would read as an
        // error number.
        return Err(Errno::EINVAL);
    }

    let mut written = 0_u64;
    for index in 0..count {
        let (base, len) = read_iovec(process, iov, index)?;
        if len == 0 {
            continue;
        }
        written = written
            .checked_add(write_range(process, base, len)?)
            .ok_or(Errno::EINVAL)?;
    }
    usize::try_from(written).map_err(|_| Errno::EINVAL)
}

/// Read one `struct iovec` out of the program's array.
///
/// The structure is two pointer-sized words, so it is eight bytes wide on
/// ARMv7-A and sixteen on the other two. Read as native words rather than
/// through a fixed layout for that reason: `libs/linux-abi`'s `Iovec` is the
/// 64-bit one, and using it here would read a 32-bit program's array at twice
/// the stride and hand the kernel a pointer assembled from two halves of
/// different segments.
fn read_iovec(process: &Process, iov: u64, index: u64) -> Result<(u64, u64), Errno> {
    let word = size_of::<usize>() as u64;
    let stride = word * 2;
    let at = iov
        .checked_add(index.checked_mul(stride).ok_or(Errno::EINVAL)?)
        .ok_or(Errno::EINVAL)?;
    let base = read_word(process, at)?;
    let len = read_word(process, at.checked_add(word).ok_or(Errno::EINVAL)?)?;
    Ok((base, len))
}

/// One pointer-sized little-endian word from the program's memory.
fn read_word(process: &Process, at: u64) -> Result<u64, Errno> {
    let mut bytes = [0_u8; 8];
    let width = size_of::<usize>();
    let slot = bytes.get_mut(..width).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), at, slot).map_err(refused)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Copy a range out of the program and put it on the console.
fn write_range(process: &Process, buf: u64, len: u64) -> Result<u64, Errno> {
    let mut done = 0_u64;
    let mut chunk = [0_u8; CHUNK];
    while done < len {
        let remaining = len - done;
        let take = usize::try_from(remaining.min(CHUNK as u64)).map_err(|_| Errno::EINVAL)?;
        let at = buf.checked_add(done).ok_or(Errno::EFAULT)?;
        let slot = chunk.get_mut(..take).ok_or(Errno::EINVAL)?;

        uaccess::copy_from_user(process.space(), at, slot).map_err(refused)?;
        console::write_bytes(slot);
        done += take as u64;
    }
    Ok(done)
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Bytes a finished line left behind because the reader asked for fewer.
///
/// One buffer for the machine, because there is one console. A program that
/// reads a line in two calls must get the second half on the second call, not
/// a fresh wait for the keyboard — which is the whole of canonical mode's
/// contract, and the part a naive `read` gets wrong first.
static PENDING: SpinLock<Vec<u8>> = SpinLock::new(Vec::new());

/// Carriage return, which a terminal in raw mode sends for the Enter key.
const CR: u8 = b'\r';
/// Delete, which most terminals send for Backspace.
const DEL: u8 = 0x7F;
/// Backspace, which the rest send.
const BS: u8 = 0x08;
/// Ctrl-D: end of file, when it arrives on an empty line.
const EOT: u8 = 0x04;

/// `read`.
///
/// Descriptor 0 only, and canonical: nothing is returned until a whole line
/// has been typed, the line is echoed as it is typed, Enter ends it, Backspace
/// edits it, and Ctrl-D on an empty line is end of file.
///
/// # This is a line discipline, and it is standing in for one
///
/// QEMU puts the host terminal in raw mode for `-serial stdio`, so nothing
/// echoes what is typed and Enter arrives as a carriage return. On Linux the
/// tty layer's `ECHO` and `ICRNL` fix both, between the keyboard and the
/// program. Ferrix has no tty layer yet, so the four rules above are here —
/// and they move there when stage 15 brings ttys, at which point this becomes
/// a read from a device like any other.
pub(crate) fn sys_read(process: &Process, fd: u64, buf: u64, len: u64) -> Result<usize, Errno> {
    if fd != STDIN {
        return Err(Errno::EBADF);
    }
    if len == 0 {
        return Ok(0);
    }

    if PENDING.lock().is_empty() {
        let line = read_line();
        if line.is_empty() {
            // Ctrl-D on an empty line: end of file, which is a count of zero.
            return Ok(0);
        }
        PENDING.lock().extend_from_slice(&line);
    }

    let mut pending = PENDING.lock();
    let take = usize::try_from(len)
        .unwrap_or(usize::MAX)
        .min(pending.len());
    let chunk = pending.get(..take).ok_or(Errno::EINVAL)?;
    uaccess::copy_to_user(process.space(), buf, chunk).map_err(refused)?;
    let _ = pending.drain(..take);
    Ok(take)
}

/// How long a console read sleeps between looks for a keystroke.
///
/// Two milliseconds is shorter than anyone types, and long enough that a shell
/// waiting at its prompt costs a processor nothing measurable.
const CONSOLE_POLL_NANOS: u64 = 2_000_000;

/// Collect one line from the keyboard, echoing it, until Enter or Ctrl-D.
///
/// The lock on [`PENDING`] is not held here: the wait may last minutes, and a
/// lock held across it would be a lock nothing else could take.
fn read_line() -> Vec<u8> {
    let mut line = Vec::new();
    loop {
        let Some(byte) = arch::read_console_byte() else {
            // Nothing typed yet. A program reading the console is a task like
            // any other, so it sleeps between looks rather than spinning a
            // processor away from everything else -- and a program killed while
            // it waits stops waiting, and reads end of file.
            let killed =
                crate::syscall::process::current().is_some_and(|process| process.is_terminated());
            if killed {
                return Vec::new();
            }
            crate::sched::sleep_for(CONSOLE_POLL_NANOS);
            continue;
        };
        match byte {
            CR | b'\n' => {
                console::write_bytes(b"\n");
                line.push(b'\n');
                return line;
            }
            DEL | BS => {
                if line.pop().is_some() {
                    // Back over the character, blank it, back again.
                    console::write_bytes(b"\x08 \x08");
                }
            }
            EOT if line.is_empty() => return line,
            EOT => {}
            other => {
                console::write_bytes(&[other]);
                line.push(other);
            }
        }
    }
}
