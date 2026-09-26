//! `read` and `write`, and the calls that are those with a position or with
//! several buffers: `pread64`, `pwrite64`, `readv` and `writev`.
//!
//! Stage 7 answered these for descriptors 0, 1 and 2 by name. The descriptor
//! table replaced the *lookup* and not the calls, as stage 7 said it would:
//! each still copies from or to user memory in bounded pieces and reports the
//! count it managed. What a descriptor names is now a
//! `ferrix_vfs::OpenFile`, and the console is one of those like any other.
//!
//! # Through a bounce buffer, a page at a time
//!
//! A filesystem reads into and writes from kernel memory, and user memory is
//! reached only through `uaccess`, so every transfer goes through a buffer of
//! at most a page. A program is entitled to `read` a gigabyte in one call and
//! the kernel must not try to hold it.
//!
//! # Short counts, and the one case that needs undoing
//!
//! A transfer that fails part-way reports what it did rather than the error,
//! as Linux does: a program that wrote half its buffer has to be told so, or
//! it will write that half again. A read from a regular file that could not be
//! copied out to the program moves the offset back over the bytes the program
//! never got, because Linux copies straight into the program's buffer and so
//! never advances past a fault. A stream cannot be moved back: a console line
//! read into a buffer the program cannot receive is lost, which is also what
//! happens to a terminal's input on Linux.
//!
//! # `iovec` is native words
//!
//! The structure is two pointer-sized words, so it is eight bytes wide on
//! ARMv7-A and sixteen on the other two. Read as native words rather than
//! through a fixed layout for that reason: `libs/linux-abi`'s `Iovec` is the
//! 64-bit one, and using it here would read a 32-bit program's array at twice
//! the stride and hand the kernel a pointer assembled from two halves of
//! different segments.

use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_vfs::{OpenFile, Whence};

use crate::syscall::fd;
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// The most one piece of a transfer carries: a page.
const CHUNK: usize = 4096;

/// Linux's `IOV_MAX`: the most segments one `readv` or `writev` may carry.
const IOV_MAX: u64 = 1024;

/// Linux's `MAX_RW_COUNT`: the most one call transfers, which is `INT_MAX`
/// rounded down to a page. Larger requests are clamped rather than refused,
/// so that the count always fits a 32-bit return register as a non-negative
/// value.
const MAX_RW_COUNT: u64 = 0x7FFF_F000;

/// Where a transfer reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// At the description's offset, moving it: `read`, `write` and the
    /// vectored calls.
    Current,
    /// At this offset, leaving the description's alone: `pread64`, `pwrite64`.
    At(u64),
}

/// `read`.
pub(crate) fn sys_read(process: &Process, fd: i32, buf: u64, len: u64) -> Result<usize, Errno> {
    let file = fd::file(process, fd)?;
    count(read_into(process, &file, buf, len, Position::Current)?)
}

/// `write`.
///
/// A zero-length write is not a no-op: it still validates the descriptor,
/// which is why the lookup comes first.
pub(crate) fn sys_write(process: &Process, fd: i32, buf: u64, len: u64) -> Result<usize, Errno> {
    let file = fd::file(process, fd)?;
    count(write_from(process, &file, buf, len, Position::Current)?)
}

/// `pread64`. A negative offset is refused before the descriptor is looked
/// at, as on Linux.
pub(crate) fn sys_pread64(
    process: &Process,
    fd: i32,
    buf: u64,
    len: u64,
    offset: i64,
) -> Result<usize, Errno> {
    let offset = u64::try_from(offset).map_err(|_| Errno::EINVAL)?;
    let file = fd::file(process, fd)?;
    count(read_into(process, &file, buf, len, Position::At(offset))?)
}

/// `pwrite64`. A negative offset is refused before the descriptor is looked
/// at, as on Linux.
pub(crate) fn sys_pwrite64(
    process: &Process,
    fd: i32,
    buf: u64,
    len: u64,
    offset: i64,
) -> Result<usize, Errno> {
    let offset = u64::try_from(offset).map_err(|_| Errno::EINVAL)?;
    let file = fd::file(process, fd)?;
    count(write_from(process, &file, buf, len, Position::At(offset))?)
}

/// `readv`: fill the segments in order, stopping at the first that is not
/// filled, since a short read means there was no more to read.
pub(crate) fn sys_readv(
    process: &Process,
    fd: i32,
    iov: u64,
    entries: u64,
) -> Result<usize, Errno> {
    let file = fd::file(process, fd)?;
    let segments = read_iovecs(process, iov, entries)?;
    if segments.is_empty() {
        return if file.readable() {
            Ok(0)
        } else {
            Err(Errno::EBADF)
        };
    }
    // What reads go to, not the node that was opened: a FIFO or a terminal's
    // node made by mknod on tmpfs is no stream itself, but its pipe or its
    // terminal is.
    let stream = file.is_stream();
    let mut done = 0_u64;
    for (base, len) in segments {
        let got = match read_into(process, &file, base, len, Position::Current) {
            Ok(got) => got,
            Err(_) if done > 0 => break,
            Err(errno) => return Err(errno),
        };
        done = done.saturating_add(got);
        // A stream has given what it had: asking it again would wait.
        if got < len || (stream && got > 0) {
            break;
        }
    }
    count(done)
}

/// `writev`.
///
/// musl's buffered output goes through this rather than `write`, so a program
/// that prints with `printf` reaches here and not the simpler call.
///
/// The whole array is read and the lengths summed *before* anything is
/// written, because the ABI says `EINVAL` for a total that overflows and a
/// program must not see half its output before being told no.
pub(crate) fn sys_writev(
    process: &Process,
    fd: i32,
    iov: u64,
    entries: u64,
) -> Result<usize, Errno> {
    let file = fd::file(process, fd)?;
    let segments = read_iovecs(process, iov, entries)?;
    if segments.is_empty() {
        return if file.writable() {
            Ok(0)
        } else {
            Err(Errno::EBADF)
        };
    }
    let mut done = 0_u64;
    for (base, len) in segments {
        let wrote = match write_from(process, &file, base, len, Position::Current) {
            Ok(wrote) => wrote,
            Err(_) if done > 0 => break,
            Err(errno) => return Err(errno),
        };
        done = done.saturating_add(wrote);
        if wrote < len.min(MAX_RW_COUNT) {
            break;
        }
    }
    count(done)
}

/// A transferred count as a return value.
fn count(done: u64) -> Result<usize, Errno> {
    usize::try_from(done).map_err(|_| Errno::EINVAL)
}

/// A buffer for one piece of a transfer of `len` bytes.
fn bounce(len: u64) -> Result<Vec<u8>, Errno> {
    let size = usize::try_from(len.min(CHUNK as u64)).map_err(|_| Errno::EINVAL)?;
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(size).map_err(|_| Errno::ENOMEM)?;
    buffer.resize(size, 0);
    Ok(buffer)
}

/// Read from `file` into the program's `[buf, buf + len)`, reporting how much
/// arrived.
fn read_into(
    process: &Process,
    file: &OpenFile,
    buf: u64,
    len: u64,
    position: Position,
) -> Result<u64, Errno> {
    let read = |done: u64, slot: &mut [u8]| match position {
        Position::Current => file.read(slot),
        Position::At(offset) => file.read_at(offset.checked_add(done).ok_or(Errno::EINVAL)?, slot),
    };
    let len = len.min(MAX_RW_COUNT);
    if len == 0 {
        // Still asked of the file: a zero-length read of a directory is
        // `EISDIR` and of a stream by position is `ESPIPE`, not zero.
        return read(0, &mut []).map(|_| 0);
    }
    let mut buffer = bounce(len)?;
    // Asked of what reads go to, as `sys_readv` asks it.
    let stream = file.is_stream();
    let mut done = 0_u64;
    while done < len {
        let want = usize::try_from((len - done).min(CHUNK as u64)).map_err(|_| Errno::EINVAL)?;
        let slot = buffer.get_mut(..want).ok_or(Errno::EINVAL)?;
        let got = match read(done, slot) {
            Ok(got) => got,
            Err(_) if done > 0 => break,
            Err(errno) => return Err(errno),
        };
        let arrived = slot.get(..got).ok_or(Errno::EIO)?;
        let at = buf.checked_add(done).ok_or(Errno::EFAULT)?;
        if uaccess::copy_to_user(process.space(), at, arrived).is_err() {
            if stream {
                file.io().unread_stream(arrived);
            }
            unread(file, position, got);
            if done > 0 {
                break;
            }
            return Err(Errno::EFAULT);
        }
        done += got as u64;
        // Short means the end of the file, and a stream that gave anything
        // has given what it had: asking again would wait for more.
        if got < want || stream {
            break;
        }
    }
    Ok(done)
}

/// Move the offset back over bytes a read took but could not deliver.
///
/// Only for a read at the current offset, and only where there is an offset:
/// a stream's `seek` is `ESPIPE`, and those bytes are gone.
fn unread(file: &OpenFile, position: Position, got: usize) {
    if position != Position::Current || got == 0 {
        return;
    }
    if let Ok(back) = i64::try_from(got) {
        let _ = file.seek(-back, Whence::Current);
    }
}

/// Write the program's `[buf, buf + len)` to `file`, reporting how much went.
fn write_from(
    process: &Process,
    file: &OpenFile,
    buf: u64,
    len: u64,
    position: Position,
) -> Result<u64, Errno> {
    let write = |done: u64, data: &[u8]| match position {
        Position::Current => file.write(data),
        Position::At(offset) => file.write_at(offset.checked_add(done).ok_or(Errno::EINVAL)?, data),
    };
    let len = len.min(MAX_RW_COUNT);
    if len == 0 {
        return empty_write(file, position);
    }
    let mut buffer = bounce(len)?;
    let mut done = 0_u64;
    while done < len {
        let want = usize::try_from((len - done).min(CHUNK as u64)).map_err(|_| Errno::EINVAL)?;
        let slot = buffer.get_mut(..want).ok_or(Errno::EINVAL)?;
        let at = buf.checked_add(done).ok_or(Errno::EFAULT)?;
        if uaccess::copy_from_user(process.space(), at, slot).is_err() {
            if done > 0 {
                break;
            }
            return Err(Errno::EFAULT);
        }
        let wrote = match write(done, slot) {
            Ok(wrote) => wrote,
            Err(_) if done > 0 => break,
            Err(errno) => return Err(errno),
        };
        done += wrote as u64;
        if wrote < want {
            break;
        }
    }
    Ok(done)
}

/// A write of nothing: the checks a real write makes, and no call into the
/// file, because under `O_APPEND` even an empty write would move the offset to
/// the end, which Linux's does not.
fn empty_write(file: &OpenFile, position: Position) -> Result<u64, Errno> {
    if !file.writable() {
        return Err(Errno::EBADF);
    }
    if matches!(position, Position::At(_)) && !file.takes_offsets() {
        return Err(Errno::ESPIPE);
    }
    Ok(0)
}

/// Read and check a program's `iovec` array: at most `IOV_MAX` entries, and a
/// total that fits a non-negative return value.
fn read_iovecs(process: &Process, iov: u64, entries: u64) -> Result<Vec<(u64, u64)>, Errno> {
    if entries > IOV_MAX {
        return Err(Errno::EINVAL);
    }
    let entries = usize::try_from(entries).map_err(|_| Errno::EINVAL)?;
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(entries)
        .map_err(|_| Errno::ENOMEM)?;
    let mut total = 0_u64;
    for index in 0..entries as u64 {
        let (base, len) = read_iovec(process, iov, index)?;
        total = total.checked_add(len).ok_or(Errno::EINVAL)?;
        segments.push((base, len));
    }
    // Linux refuses a total that will not fit in the return value rather than
    // reporting a negative count, which a caller would read as an error
    // number. Measured against this architecture's word, not 64 bits.
    if isize::try_from(total).is_err() {
        return Err(Errno::EINVAL);
    }
    Ok(segments)
}

/// Read one `struct iovec` out of the program's array, as native words.
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
    uaccess::copy_from_user(process.space(), at, slot).map_err(|_| Errno::EFAULT)?;
    Ok(u64::from_le_bytes(bytes))
}
