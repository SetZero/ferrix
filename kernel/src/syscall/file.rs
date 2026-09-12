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

use ferrix_linux_abi::errno::Errno;

use crate::console;
use crate::syscall::process::Process;
use crate::syscall::uaccess::{self, UserError};

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
