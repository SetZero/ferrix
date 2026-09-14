//! `pipe`, `pipe2` and `sendfile`.
//!
//! The pipe itself is `crate::fs::pipe`. What is here is making one into two
//! descriptors, and `sendfile`, which moves bytes from one descriptor to
//! another without the program ever holding them.
//!
//! # Two descriptors, or none
//!
//! `pipe2` writes both numbers into the program's memory once both are
//! installed. If that write faults the program cannot learn the numbers, so
//! both are closed again before `EFAULT` is returned, and a failed call leaves
//! the table as it found it.
//!
//! # `sendfile` through a kernel buffer
//!
//! Linux splices pages from one file to the other. Nothing here has a page
//! cache to splice from, so the bytes go through a buffer of at most
//! [`SENDFILE_CHUNK`]: `read` and `write` through a bounce buffer, with no copy
//! to the program in between. A short write to the output puts the input's
//! position back over what the output did not take, where the input has a
//! position. A pipe has none, so from a pipe the bytes a short write left
//! behind are lost where Linux's splice would have left them queued; it is
//! read once per call, so that one short write is all that can lose.
//!
//! ARMv7-A numbers only `sendfile64`, whose offset is a 64-bit `loff_t` like
//! the 64-bit architectures' `off_t`, so every offset read here is eight bytes.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{O_CLOEXEC, O_NONBLOCK};
use ferrix_vfs::{FileType, OpenFile, Whence};

use crate::fs;
use crate::syscall::fd;
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// The most one `sendfile` holds in the kernel at once: sixteen pages.
const SENDFILE_CHUNK: usize = 16 * 4096;

/// Linux's `MAX_RW_COUNT`, which `sendfile` clamps to as `read` and `write`
/// do, so that the count always fits a 32-bit return register.
const MAX_RW_COUNT: u64 = 0x7FFF_F000;

/// `pipe2`, and `pipe` as `pipe2` with no flags.
///
/// Only `O_CLOEXEC` and `O_NONBLOCK` are taken, both decoded through the
/// architecture's `open` table, where all three architectures agree on them:
/// neither `arch/arm/include/uapi/asm/fcntl.h` nor its arm64 counterpart
/// overrides the generic header's values, and x86-64 has no header of its own.
/// Linux also takes `O_DIRECT`, for a pipe that keeps writes apart as packets,
/// and `O_NOTIFICATION_PIPE`; there are no such pipes here, so both are
/// `EINVAL` with every other bit.
pub(crate) fn sys_pipe2(process: &Process, fds: u64, raw: u32) -> Result<usize, Errno> {
    if raw & !(O_CLOEXEC | O_NONBLOCK) != 0 {
        return Err(Errno::EINVAL);
    }
    let (flags, cloexec) = fd::decode_open_flags(raw);
    let owner = crate::syscall::path::creator_ids(process);
    let (reader, writer) = fs::pipe::new_pipe(flags.nonblock, owner)?;
    let (read_fd, write_fd) = install(process, reader, writer, cloexec)?;

    let pair: Vec<u8> = read_fd
        .to_le_bytes()
        .into_iter()
        .chain(write_fd.to_le_bytes())
        .collect();
    if uaccess::copy_to_user(process.space(), fds, &pair).is_err() {
        // Taken out under the lock, dropped after it: see `fd`.
        let taken = {
            let mut files = process.files().lock();
            (files.remove(read_fd), files.remove(write_fd))
        };
        drop(taken);
        return Err(Errno::EFAULT);
    }
    Ok(0)
}

/// Put both ends in the table in one hold of its lock, or neither.
///
/// An end a full table refuses is dropped inside the lock, which for a pipe
/// end is bounded: it closes a pipe and wakes its queues, and holds no dentry
/// in any tree.
fn install(
    process: &Process,
    reader: Arc<OpenFile>,
    writer: Arc<OpenFile>,
    cloexec: bool,
) -> Result<(i32, i32), Errno> {
    let mut files = process.files().lock();
    let read_fd = files.insert(reader, cloexec)?;
    match files.insert(writer, cloexec) {
        Ok(write_fd) => Ok((read_fd, write_fd)),
        Err(errno) => {
            let displaced = files.remove(read_fd);
            drop(files);
            drop(displaced);
            Err(errno)
        }
    }
}

/// `sendfile` and `sendfile64`: copy up to `count` bytes from `in_fd` to
/// `out_fd`.
///
/// With an offset pointer the input is read from that offset, the offset is
/// written back past what was sent, and the input's own position is left
/// alone. Without one, the input's position moves.
pub(crate) fn sys_sendfile(
    process: &Process,
    out_fd: i32,
    in_fd: i32,
    offset_at: u64,
    count: u64,
) -> Result<usize, Errno> {
    let start = if offset_at == 0 {
        None
    } else {
        let mut bytes = [0_u8; 8];
        uaccess::copy_from_user(process.space(), offset_at, &mut bytes)
            .map_err(|_| Errno::EFAULT)?;
        Some(i64::from_le_bytes(bytes))
    };
    let (sent, end) = transfer(process, out_fd, in_fd, start, count)?;
    if let Some(end) = end {
        uaccess::copy_to_user(process.space(), offset_at, &end.to_le_bytes())
            .map_err(|_| Errno::EFAULT)?;
    }
    Ok(sent)
}

/// Check both descriptors in Linux's order, then copy. Answers what was sent,
/// and where the offset ended for a call that gave one.
///
/// `EBADF` for an input not open for reading or an output not open for
/// writing, as `sendfile(2)` documents; `ESPIPE` for an offset on an input
/// with no position; `EINVAL` for a negative offset, and for the two
/// combinations a copy cannot honour: a directory as input, and an output
/// under `O_APPEND`, which Linux's splice refuses.
fn transfer(
    process: &Process,
    out_fd: i32,
    in_fd: i32,
    start: Option<i64>,
    count: u64,
) -> Result<(usize, Option<u64>), Errno> {
    let input = fd::file(process, in_fd)?;
    if !input.readable() {
        return Err(Errno::EBADF);
    }
    if start.is_some() && input.is_stream() {
        return Err(Errno::ESPIPE);
    }
    let mut position = start
        .map(|at| u64::try_from(at).map_err(|_| Errno::EINVAL))
        .transpose()?;
    let output = fd::file(process, out_fd)?;
    if !output.writable() {
        return Err(Errno::EBADF);
    }
    if output.status().append || input.kind() == FileType::Directory {
        return Err(Errno::EINVAL);
    }
    let count = usize::try_from(count.min(MAX_RW_COUNT)).unwrap_or(SENDFILE_CHUNK);
    let sent = copy(&input, &output, position.as_mut(), count)?;
    Ok((sent, position))
}

/// Move up to `count` bytes, a chunk at a time, stopping at end of file, at a
/// short write, or after one chunk from a stream.
fn copy(
    input: &OpenFile,
    output: &OpenFile,
    mut position: Option<&mut u64>,
    count: usize,
) -> Result<usize, Errno> {
    let mut buffer = vec![0_u8; count.min(SENDFILE_CHUNK)];
    let mut sent = 0;
    while sent < count {
        let want = (count - sent).min(buffer.len());
        let slot = buffer.get_mut(..want).ok_or(Errno::EIO)?;
        let read = match position.as_deref() {
            Some(&at) => input.read_at(at, slot),
            None => input.read(slot),
        };
        let got = match read {
            Ok(0) => break,
            Ok(got) => got,
            Err(errno) => return partial(sent, errno),
        };
        let chunk = buffer.get(..got).ok_or(Errno::EIO)?;
        let wrote = match output.write(chunk) {
            Ok(wrote) => wrote,
            Err(errno) => {
                give_back(input, position.is_some(), got);
                return partial(sent, errno);
            }
        };
        sent += wrote;
        if let Some(at) = position.as_deref_mut() {
            *at = at.saturating_add(wrote as u64);
        }
        if wrote < got {
            give_back(input, position.is_some(), got - wrote);
            break;
        }
        if input.is_stream() {
            break;
        }
    }
    Ok(sent)
}

/// Put the input's position back over `unread` bytes it read and nobody
/// wrote. Only a call without an offset moved the position, and only an input
/// with a position can be put back.
fn give_back(input: &OpenFile, positioned: bool, unread: usize) {
    if positioned || input.is_stream() {
        return;
    }
    if let Ok(back) = i64::try_from(unread) {
        let _ = input.seek(-back, Whence::Current);
    }
}

/// A count so far, or `errno` if nothing was done.
fn partial(done: usize, errno: Errno) -> Result<usize, Errno> {
    if done > 0 { Ok(done) } else { Err(errno) }
}
