//! Waiting for descriptors to become ready: `poll`, `ppoll`, `select` and
//! `pselect6`.
//!
//! # How a wait waits
//!
//! By asking again. Each descriptor's [`OpenFile::poll`] says what it is ready
//! for right now, and a call with nothing ready sleeps a few milliseconds and
//! asks again, until something is, the timeout passes, or the caller is ended.
//! That is not how Linux does it -- a file registers the waiter and wakes it
//! -- and it costs a wake-up per slice while a program waits. It is also all
//! that anything here needs yet: a regular file is always ready, the console
//! drains its UART when asked, and nothing that can be unready without a waker
//! exists until pipes do, which is when files learn to wake a waiter.
//!
//! # Two encodings of one question
//!
//! `poll` asks with an array of `struct pollfd`; `select` with three bitmaps.
//! Both are answered from the same [`OpenFile::poll`] and the same wait loop,
//! [`wait_for`], and differ only in how the question is read and the answer
//! written. `select`'s mapping is Linux's: a descriptor is in the read set if
//! it is readable, has hung up or has an error, in the write set if it is
//! writable or has an error, and never in the exception set, which is for
//! out-of-band data nothing here has.
//!
//! # The signal mask a wait waits under
//!
//! `ppoll` and `pselect6` take a signal mask to install for the wait and
//! restore after it. It is installed, and restored. Nothing delivers a signal
//! yet, so no program can see the difference until something does; applying
//! it now means delivery finds the mask where it looks.
//!
//! [`OpenFile::poll`]: ferrix_vfs::OpenFile::poll

use alloc::vec;
use alloc::vec::Vec;

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

/// Bytes in a native word: an `fd_set` is an array of `unsigned long`, and
/// `pselect6`'s signal argument is two of them.
const WORD_BYTES: usize = size_of::<usize>();
/// Bits in one.
const WORD_BITS: usize = WORD_BYTES * 8;

/// How long a wait with nothing ready sleeps before asking again.
const SLICE_NANOS: u64 = 5_000_000;

/// Nanoseconds in a second, in a millisecond and in a microsecond.
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const NANOS_PER_MILLI: u64 = 1_000_000;
const NANOS_PER_MICRO: u64 = 1_000;
/// Microseconds in a second.
const MICROS_PER_SECOND: u64 = 1_000_000;

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

/// `ppoll`: `timeout` a `struct timespec` of `width`, or null for no limit,
/// waited under the signal mask at `sigmask` if there is one.
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
    let deadline = if timeout == 0 {
        None
    } else {
        let nanos = read_timespec(process, timeout, width)?;
        Some(now().saturating_add(nanos))
    };
    let mask = read_sigset(process, sigmask, sigsetsize)?;
    with_sigmask(process, mask, || wait(process, fds, nfds, deadline))
}

/// `select`, and ARMv7-A's `_newselect`: three sets of `nfds` descriptors, and
/// `timeout` a `struct timeval` of native words, or null for no limit.
///
/// The time left is written back into `timeout`, as Linux does on every
/// architecture unless the timeout was zero; a failure to write it is
/// ignored, as there too.
///
/// # Errors
///
/// As [`sys_pselect6`], except that a negative microsecond count is what makes
/// a timeout malformed: one of a million or more carries into the seconds.
pub(crate) fn sys_select(
    process: &Process,
    nfds: i32,
    sets: [u64; 3],
    timeout: u64,
) -> Result<usize, Errno> {
    let limit = if timeout == 0 {
        None
    } else {
        Some(read_timeval(process, timeout)?)
    };
    let deadline = limit.map(|nanos| now().saturating_add(nanos));
    let answer = select(process, nfds, sets, deadline);
    if let (Some(nanos), Some(deadline)) = (limit, deadline)
        && nanos != 0
    {
        write_timeval(process, timeout, remaining(deadline));
    }
    answer
}

/// `pselect6` and ARMv7-A's `pselect6_time64`: as `select`, with `timeout` a
/// `struct timespec` of `width`, and `sigmask` null or the address of a
/// `{ const sigset_t *set; size_t size; }` pair naming the mask to wait under.
///
/// # Errors
///
/// `EINVAL` for a negative `nfds`, a malformed timeout or a wrong signal set
/// size; `EFAULT` for a bad pointer; `EBADF` for a set bit naming a descriptor
/// that is not open; `EINTR` if the caller is ended while it waits.
pub(crate) fn sys_pselect6(
    process: &Process,
    nfds: i32,
    sets: [u64; 3],
    timeout: u64,
    sigmask: u64,
    width: TimeWidth,
) -> Result<usize, Errno> {
    let limit = if timeout == 0 {
        None
    } else {
        Some(read_timespec(process, timeout, width)?)
    };
    let mask = if sigmask == 0 {
        None
    } else {
        let mut pair = [0_u8; 16];
        let pair = pair.get_mut(..WORD_BYTES * 2).ok_or(Errno::EINVAL)?;
        uaccess::copy_from_user(process.space(), sigmask, pair).map_err(|_| Errno::EFAULT)?;
        read_sigset(process, word(pair, 0), word(pair, WORD_BYTES))?
    };
    let deadline = limit.map(|nanos| now().saturating_add(nanos));
    let answer = with_sigmask(process, mask, || select(process, nfds, sets, deadline));
    if let (Some(nanos), Some(deadline)) = (limit, deadline)
        && nanos != 0
    {
        write_timespec(process, timeout, width, remaining(deadline));
    }
    answer
}

/// The signal set at `at`, if `at` is not null.
///
/// The size is only checked when there is a set, as on Linux.
fn read_sigset(process: &Process, at: u64, size: u64) -> Result<Option<u64>, Errno> {
    if at == 0 {
        return Ok(None);
    }
    if size != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let mut bytes = [0_u8; 8];
    uaccess::copy_from_user(process.space(), at, &mut bytes).map_err(|_| Errno::EFAULT)?;
    Ok(Some(u64::from_le_bytes(bytes)))
}

/// Run `body` with the blocked mask replaced by `mask`, if there is one, and
/// put the caller's back afterwards.
fn with_sigmask<R>(process: &Process, mask: Option<u64>, body: impl FnOnce() -> R) -> R {
    let saved = mask.map(|mask| process.with_signals(|signals| signals.replace_blocked(mask)));
    let answer = body();
    if let Some(saved) = saved {
        let _ = process.with_signals(|signals| signals.replace_blocked(saved));
    }
    answer
}

/// Read a `struct timespec` of `width` as nanoseconds.
fn read_timespec(process: &Process, at: u64, width: TimeWidth) -> Result<u64, Errno> {
    let (seconds, nanos) = read_pair(process, at, width)?;
    let seconds = u64::try_from(seconds).map_err(|_| Errno::EINVAL)?;
    let nanos = u64::try_from(nanos).map_err(|_| Errno::EINVAL)?;
    if nanos >= NANOS_PER_SECOND {
        return Err(Errno::EINVAL);
    }
    Ok(seconds
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add(nanos))
}

/// Read a `struct timeval` of native words as nanoseconds.
fn read_timeval(process: &Process, at: u64) -> Result<u64, Errno> {
    let (seconds, micros) = read_pair(process, at, TimeWidth::Native)?;
    let seconds = u64::try_from(seconds).map_err(|_| Errno::EINVAL)?;
    let micros = u64::try_from(micros).map_err(|_| Errno::EINVAL)?;
    Ok(seconds
        .saturating_add(micros / MICROS_PER_SECOND)
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add((micros % MICROS_PER_SECOND) * NANOS_PER_MICRO))
}

/// Whether a time structure of `width` is two 64-bit fields here.
fn is_wide(width: TimeWidth) -> bool {
    width == TimeWidth::Wide || WORD_BYTES == 8
}

/// Read two signed fields of `width`: a `timespec` or a `timeval`.
fn read_pair(process: &Process, at: u64, width: TimeWidth) -> Result<(i64, i64), Errno> {
    if is_wide(width) {
        let mut bytes = [0_u8; 16];
        uaccess::copy_from_user(process.space(), at, &mut bytes).map_err(|_| Errno::EFAULT)?;
        let (seconds, rest) = bytes.split_at(8);
        Ok((
            i64::from_le_bytes(seconds.try_into().map_err(|_| Errno::EINVAL)?),
            i64::from_le_bytes(rest.try_into().map_err(|_| Errno::EINVAL)?),
        ))
    } else {
        let mut bytes = [0_u8; 8];
        uaccess::copy_from_user(process.space(), at, &mut bytes).map_err(|_| Errno::EFAULT)?;
        let (seconds, rest) = bytes.split_at(4);
        Ok((
            i64::from(i32::from_le_bytes(
                seconds.try_into().map_err(|_| Errno::EINVAL)?,
            )),
            i64::from(i32::from_le_bytes(
                rest.try_into().map_err(|_| Errno::EINVAL)?,
            )),
        ))
    }
}

/// Write two fields of `width` at `at`, ignoring a fault.
fn write_pair(process: &Process, at: u64, width: TimeWidth, first: u64, second: u64) {
    let mut bytes = [0_u8; 16];
    let used = if is_wide(width) {
        for (slot, byte) in bytes
            .iter_mut()
            .zip(first.to_le_bytes().into_iter().chain(second.to_le_bytes()))
        {
            *slot = byte;
        }
        16
    } else {
        let narrow = |value: u64| u32::try_from(value).unwrap_or(i32::MAX as u32);
        let fields = narrow(first)
            .to_le_bytes()
            .into_iter()
            .chain(narrow(second).to_le_bytes());
        for (slot, byte) in bytes.iter_mut().zip(fields) {
            *slot = byte;
        }
        8
    };
    if let Some(bytes) = bytes.get(..used) {
        let _ = uaccess::copy_to_user(process.space(), at, bytes);
    }
}

/// Write `nanos` back as a `struct timespec` of `width`.
fn write_timespec(process: &Process, at: u64, width: TimeWidth, nanos: u64) {
    write_pair(
        process,
        at,
        width,
        nanos / NANOS_PER_SECOND,
        nanos % NANOS_PER_SECOND,
    );
}

/// Write `nanos` back as a `struct timeval` of native words.
fn write_timeval(process: &Process, at: u64, nanos: u64) {
    write_pair(
        process,
        at,
        TimeWidth::Native,
        nanos / NANOS_PER_SECOND,
        nanos % NANOS_PER_SECOND / NANOS_PER_MICRO,
    );
}

/// How long is left until `deadline`, and zero once it has passed.
fn remaining(deadline: u64) -> u64 {
    deadline.saturating_sub(now())
}

/// The native word at `offset`, zero-extended.
fn word(bytes: &[u8], offset: usize) -> u64 {
    let mut eight = [0_u8; 8];
    for (slot, byte) in eight
        .iter_mut()
        .zip(bytes.iter().skip(offset).take(WORD_BYTES))
    {
        *slot = *byte;
    }
    u64::from_le_bytes(eight)
}

/// The counter's reading, in nanoseconds.
fn now() -> u64 {
    crate::timer::now_nanos()
}

/// Ask `scan` until it counts something ready or `deadline` passes, and
/// return its last count.
fn wait_for(
    process: &Process,
    deadline: Option<u64>,
    mut scan: impl FnMut() -> Result<usize, Errno>,
) -> Result<usize, Errno> {
    loop {
        let ready = scan()?;
        let expired = deadline.is_some_and(|at| now() >= at);
        if ready > 0 || expired {
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

/// `poll`'s wait: read the array, wait, write the answers back and report how
/// many descriptors had one.
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
    let ready = wait_for(process, deadline, || scan(process, &mut entries))?;
    if !entries.is_empty() {
        uaccess::copy_to_user(process.space(), fds, &entries).map_err(|_| Errno::EFAULT)?;
    }
    Ok(ready)
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

/// `select`'s wait: read the three sets, refuse a closed descriptor in any of
/// them, wait, and write the answers back.
///
/// `nfds` beyond the descriptor table's limit is cut to the limit, as Linux
/// cuts it to its table's size, and only the words that cover `nfds` bits are
/// read and written: bits past `nfds` in the last word are ignored going in
/// and cleared coming out. The sets are written back when the call succeeds,
/// including with zero on a timeout, and left alone when it fails.
fn select(
    process: &Process,
    nfds: i32,
    sets: [u64; 3],
    deadline: Option<u64>,
) -> Result<usize, Errno> {
    let asked = usize::try_from(nfds).map_err(|_| Errno::EINVAL)?;
    let table = usize::try_from(process.files().lock().limit()).map_err(|_| Errno::EINVAL)?;
    let count = asked.min(table);
    let bytes = count.div_ceil(WORD_BITS) * WORD_BYTES;

    let mut wanted: [Option<Vec<u8>>; 3] = [None, None, None];
    for (slot, &at) in wanted.iter_mut().zip(&sets) {
        if at == 0 {
            continue;
        }
        let mut set = vec![0_u8; bytes];
        if bytes > 0 {
            uaccess::copy_from_user(process.space(), at, &mut set).map_err(|_| Errno::EFAULT)?;
        }
        *slot = Some(set);
    }

    for number in 0..count {
        let asked_about = wanted.iter().flatten().any(|set| bit(set, number));
        let fd = i32::try_from(number).map_err(|_| Errno::EINVAL)?;
        if asked_about && fd::file(process, fd).is_err() {
            return Err(Errno::EBADF);
        }
    }

    let mut answer = [vec![0_u8; bytes], vec![0_u8; bytes], vec![0_u8; bytes]];
    let ready = wait_for(process, deadline, || {
        scan_sets(process, count, &wanted, &mut answer)
    })?;
    for (&at, set) in sets.iter().zip(&answer) {
        if at != 0 && bytes > 0 {
            uaccess::copy_to_user(process.space(), at, set).map_err(|_| Errno::EFAULT)?;
        }
    }
    Ok(ready)
}

/// Fill in `answer` for the first `count` descriptors and count the bits set.
///
/// A descriptor closed since the sets were checked is not ready, rather than
/// an error, as on Linux.
fn scan_sets(
    process: &Process,
    count: usize,
    wanted: &[Option<Vec<u8>>; 3],
    answer: &mut [Vec<u8>; 3],
) -> Result<usize, Errno> {
    for set in answer.iter_mut() {
        set.fill(0);
    }
    let mut ready = 0;
    for number in 0..count {
        if !wanted.iter().flatten().any(|set| bit(set, number)) {
            continue;
        }
        let fd = i32::try_from(number).map_err(|_| Errno::EINVAL)?;
        let Ok(file) = fd::file(process, fd) else {
            continue;
        };
        let readiness = file.poll();
        let offered = [
            readiness.readable || readiness.hangup || readiness.error,
            readiness.writable || readiness.error,
            false,
        ];
        for ((set, want), offer) in answer.iter_mut().zip(wanted).zip(offered) {
            if offer && want.as_ref().is_some_and(|want| bit(want, number)) {
                set_bit(set, number);
                ready += 1;
            }
        }
    }
    Ok(ready)
}

/// Whether descriptor `number`'s bit is set in `set`.
///
/// Little-endian words make the bitmap a byte array with the low bit of each
/// byte first, on every architecture here.
fn bit(set: &[u8], number: usize) -> bool {
    set.get(number / 8)
        .is_some_and(|byte| byte & (1 << (number % 8)) != 0)
}

/// Set descriptor `number`'s bit in `set`.
fn set_bit(set: &mut [u8], number: usize) {
    if let Some(byte) = set.get_mut(number / 8) {
        *byte |= 1 << (number % 8);
    }
}
