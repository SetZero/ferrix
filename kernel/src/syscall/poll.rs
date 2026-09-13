//! Waiting for descriptors to become ready: `poll` and `ppoll`.
//!
//! # How a wait waits
//!
//! By asking again. Each descriptor's [`OpenFile::poll`] says what it is ready
//! for right now, and a call with nothing ready sleeps a few milliseconds and
//! asks again, until something is, the timeout passes, or the caller is ended.
//! That is not how Linux does it -- a file registers the waiter and wakes it
//! -- and it costs a wake-up per slice while a program waits. It is also all
//! that anything here needs yet: a regular file is always ready, the console
//! reports ready, and nothing that can be unready without a waker exists until
//! pipes do, which is when files learn to wake a waiter.
//!
//! [`OpenFile::poll`]: ferrix_vfs::OpenFile::poll

use alloc::vec;

use ferrix_linux_abi::errno::Errno;

use crate::sched;
use crate::syscall::fd;
use crate::syscall::process::Process;
use crate::syscall::time::TimeWidth;
use crate::syscall::uaccess;

/// There is data to read.
pub(crate) const POLLIN: u16 = 0x001;
/// Writing will not block.
pub(crate) const POLLOUT: u16 = 0x004;
/// An error condition, reported whether asked for or not.
pub(crate) const POLLERR: u16 = 0x008;
/// The other end hung up, reported whether asked for or not.
pub(crate) const POLLHUP: u16 = 0x010;
/// The descriptor is not open, reported whether asked for or not.
pub(crate) const POLLNVAL: u16 = 0x020;
/// Normal data to read: the same as `POLLIN` for everything here.
pub(crate) const POLLRDNORM: u16 = 0x040;
/// Normal data may be written: the same as `POLLOUT` for everything here.
pub(crate) const POLLWRNORM: u16 = 0x100;

/// Bytes in a `struct pollfd`: an `int` and two `short`s, on every
/// architecture.
const POLLFD_BYTES: usize = 8;

/// How long a wait with nothing ready sleeps before asking again.
const SLICE_NANOS: u64 = 5_000_000;

/// Nanoseconds in a second, and in a millisecond.
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const NANOS_PER_MILLI: u64 = 1_000_000;

/// The only `sigsetsize` the kernel accepts.
const SIGSET_SIZE: u64 = 8;

/// `poll`: `timeout` in milliseconds, negative for no limit.
///
/// # Errors
///
/// As [`sys_ppoll`].
pub(crate) fn sys_poll(
    process: &Process,
    fds: u64,
    nfds: u64,
    timeout: i32,
) -> Result<usize, Errno> {
    let deadline = u64::try_from(timeout)
        .ok()
        .map(|millis| now().saturating_add(millis.saturating_mul(NANOS_PER_MILLI)));
    wait(process, fds, nfds, deadline)
}

/// `ppoll`: `timeout` a `struct timespec` of `width`, or null for no limit.
///
/// The signal mask the caller asks to wait under is accepted and not applied:
/// nothing delivers a signal yet, so there is nothing for it to hold back.
///
/// # Errors
///
/// `EINVAL` for more descriptors than the caller may have open, a malformed
/// timeout or a wrong signal set size; `EFAULT` for a bad pointer; `EINTR` if
/// the caller is ended while it waits.
pub(crate) fn sys_ppoll(
    process: &Process,
    fds: u64,
    nfds: u64,
    timeout: u64,
    sigmask: u64,
    sigsetsize: u64,
    width: TimeWidth,
) -> Result<usize, Errno> {
    if sigmask != 0 && sigsetsize != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let deadline = if timeout == 0 {
        None
    } else {
        let nanos = read_timespec(process, timeout, width)?;
        Some(now().saturating_add(nanos))
    };
    wait(process, fds, nfds, deadline)
}

/// Read a `struct timespec` of `width` as nanoseconds.
fn read_timespec(process: &Process, at: u64, width: TimeWidth) -> Result<u64, Errno> {
    let wide = width == TimeWidth::Wide || size_of::<usize>() == 8;
    let (seconds, nanos) = if wide {
        let mut bytes = [0_u8; 16];
        uaccess::copy_from_user(process.space(), at, &mut bytes).map_err(|_| Errno::EFAULT)?;
        let (seconds, nanos) = bytes.split_at(8);
        (
            i64::from_le_bytes(seconds.try_into().map_err(|_| Errno::EINVAL)?),
            i64::from_le_bytes(nanos.try_into().map_err(|_| Errno::EINVAL)?),
        )
    } else {
        let mut bytes = [0_u8; 8];
        uaccess::copy_from_user(process.space(), at, &mut bytes).map_err(|_| Errno::EFAULT)?;
        let (seconds, nanos) = bytes.split_at(4);
        (
            i64::from(i32::from_le_bytes(
                seconds.try_into().map_err(|_| Errno::EINVAL)?,
            )),
            i64::from(i32::from_le_bytes(
                nanos.try_into().map_err(|_| Errno::EINVAL)?,
            )),
        )
    };
    let seconds = u64::try_from(seconds).map_err(|_| Errno::EINVAL)?;
    let nanos = u64::try_from(nanos).map_err(|_| Errno::EINVAL)?;
    if nanos >= NANOS_PER_SECOND {
        return Err(Errno::EINVAL);
    }
    Ok(seconds
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add(nanos))
}

/// The counter's reading, in nanoseconds.
fn now() -> u64 {
    crate::timer::now_nanos()
}

/// Ask until something is ready or `deadline` passes, then write the answers
/// back and report how many descriptors had one.
fn wait(process: &Process, fds: u64, nfds: u64, deadline: Option<u64>) -> Result<usize, Errno> {
    let limit = u64::from(process.files().lock().limit());
    if nfds > limit {
        return Err(Errno::EINVAL);
    }
    let count = usize::try_from(nfds).map_err(|_| Errno::EINVAL)?;
    let mut entries = vec![0_u8; count.checked_mul(POLLFD_BYTES).ok_or(Errno::EINVAL)?];
    if !entries.is_empty() {
        uaccess::copy_from_user(process.space(), fds, &mut entries).map_err(|_| Errno::EFAULT)?;
    }
    loop {
        let ready = scan(process, &mut entries)?;
        let expired = deadline.is_some_and(|at| now() >= at);
        if ready > 0 || expired {
            if !entries.is_empty() {
                uaccess::copy_to_user(process.space(), fds, &entries).map_err(|_| Errno::EFAULT)?;
            }
            return Ok(ready);
        }
        // Ended, or a signal to deliver: `EINTR`, and the signal on the way out.
        if process.signal_pending() {
            return Err(Errno::EINTR);
        }
        let slice = now().saturating_add(SLICE_NANOS);
        sched::sleep_until(deadline.map_or(slice, |at| at.min(slice)));
    }
}

/// Fill in every entry's `revents` and count the entries that got one.
///
/// A negative descriptor is skipped, which is how a program leaves a slot in
/// its array unused; one that names nothing answers `POLLNVAL`. Errors and
/// hang-ups are reported whether they were asked for or not, as on Linux.
fn scan(process: &Process, entries: &mut [u8]) -> Result<usize, Errno> {
    let mut ready = 0;
    for entry in entries.chunks_exact_mut(POLLFD_BYTES) {
        let fd = i32::from_le_bytes(
            entry
                .get(..4)
                .ok_or(Errno::EINVAL)?
                .try_into()
                .map_err(|_| Errno::EINVAL)?,
        );
        let events = u16::from_le_bytes(
            entry
                .get(4..6)
                .ok_or(Errno::EINVAL)?
                .try_into()
                .map_err(|_| Errno::EINVAL)?,
        );
        let revents = if fd < 0 {
            0
        } else {
            match fd::file(process, fd) {
                Err(_) => POLLNVAL,
                Ok(file) => {
                    let readiness = file.poll();
                    let mut offered = 0;
                    if readiness.readable {
                        offered |= POLLIN | POLLRDNORM;
                    }
                    if readiness.writable {
                        offered |= POLLOUT | POLLWRNORM;
                    }
                    let mut revents = offered & events;
                    if readiness.hangup {
                        revents |= POLLHUP;
                    }
                    if readiness.error {
                        revents |= POLLERR;
                    }
                    revents
                }
            }
        };
        entry
            .get_mut(6..8)
            .ok_or(Errno::EINVAL)?
            .copy_from_slice(&revents.to_le_bytes());
        if revents != 0 {
            ready += 1;
        }
    }
    Ok(ready)
}
