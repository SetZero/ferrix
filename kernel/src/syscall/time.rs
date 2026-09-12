//! Clocks, and the bytes a libc asks for before `main`.
//!
//! Two groups of call that look unrelated and are here for the same reason:
//! a static binary reaches for both during startup, before it has printed
//! anything, and a missing answer to either does not fail loudly. The first
//! real program to run on Ferrix -- busybox's `sh` -- span forever on
//! `clock_gettime(CLOCK_MONOTONIC)` returning `ENOSYS`, waiting for time to
//! pass on a clock that could not say it had.

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_MONOTONIC_COARSE, CLOCK_MONOTONIC_RAW, CLOCK_REALTIME,
    CLOCK_REALTIME_COARSE,
};
use ferrix_sync::SpinLock;

use crate::arch;
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// Nanoseconds in a second.
const NANOS: u64 = 1_000_000_000;

/// How wide the two fields of a `timespec` or `timeval` are.
///
/// The reason `clock_gettime` and `clock_gettime64` are separate calls on
/// ARMv7-A: the first writes two `long`s, which are 32 bits there, and the
/// second writes two 64-bit fields whatever the architecture. On the 64-bit
/// pair both come out the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimeWidth {
    /// Two `long`s: this architecture's pointer width.
    Native,
    /// Two 64-bit fields.
    Wide,
}

/// Nanoseconds since the high-resolution counter started.
///
/// In 128 bits, and not as a courtesy. The counter runs at 100 MHz on the
/// x86-64 QEMU machine, so `counter * 10^9` overflows 64 bits after about three
/// minutes of uptime -- and with overflow checks on, that is a kernel panic
/// inside a system call three minutes into the first interactive session.
fn now_nanos() -> u64 {
    let hz = arch::counter_hz();
    if hz == 0 {
        return 0;
    }
    let nanos = u128::from(arch::counter_now()) * u128::from(NANOS) / u128::from(hz);
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

/// `clock_gettime` and `clock_gettime64`.
///
/// # Every clock is the counter, for now
///
/// The monotonic clocks are honestly the counter: they start at boot and never
/// go backwards, which is all they promise. The real-time clocks are the same
/// counter and so read as a few seconds past the start of 1970, because
/// nothing yet reads a real-time clock chip. That is a wrong answer rather than
/// a missing one, and it is chosen deliberately: a program that gets `EINVAL`
/// for `CLOCK_REALTIME` usually aborts, while one that gets 1970 usually
/// prints a strange date and carries on. The CPU-time clocks are refused,
/// because there is no accounting of CPU time per process to report.
pub(crate) fn sys_clock_gettime(
    process: &Process,
    clock: u64,
    at: u64,
    width: TimeWidth,
) -> Result<usize, Errno> {
    let clock = u32::try_from(clock).map_err(|_| Errno::EINVAL)?;
    match clock {
        CLOCK_REALTIME
        | CLOCK_REALTIME_COARSE
        | CLOCK_MONOTONIC
        | CLOCK_MONOTONIC_RAW
        | CLOCK_MONOTONIC_COARSE
        | CLOCK_BOOTTIME => {}
        _ => return Err(Errno::EINVAL),
    }
    let nanos = now_nanos();
    write_pair(process, at, nanos / NANOS, nanos % NANOS, width)?;
    Ok(0)
}

/// `gettimeofday`: the same clock, in microseconds.
///
/// A null `tv` is legal and asks for nothing. The timezone argument is
/// obsolete, and ignored, as Linux ignores it.
pub(crate) fn sys_gettimeofday(process: &Process, tv: u64) -> Result<usize, Errno> {
    if tv == 0 {
        return Ok(0);
    }
    let nanos = now_nanos();
    write_pair(
        process,
        tv,
        nanos / NANOS,
        (nanos % NANOS) / 1_000,
        TimeWidth::Native,
    )?;
    Ok(0)
}

/// Write two fields, at the width the call's structure has.
fn write_pair(
    process: &Process,
    at: u64,
    first: u64,
    second: u64,
    width: TimeWidth,
) -> Result<(), Errno> {
    let native_is_wide = size_of::<usize>() == 8;
    if width == TimeWidth::Wide || native_is_wide {
        let mut bytes = [0_u8; 16];
        let fields = first.to_le_bytes().into_iter().chain(second.to_le_bytes());
        for (slot, byte) in bytes.iter_mut().zip(fields) {
            *slot = byte;
        }
        return uaccess::copy_to_user(process.space(), at, &bytes).map_err(|_| Errno::EFAULT);
    }
    // A 32-bit `time_t`. Seconds since boot fit for 136 years; the day they do
    // not is reported rather than wrapped.
    let first = u32::try_from(first).map_err(|_| Errno::EOVERFLOW)?;
    let second = u32::try_from(second).map_err(|_| Errno::EOVERFLOW)?;
    let mut bytes = [0_u8; 8];
    let fields = first.to_le_bytes().into_iter().chain(second.to_le_bytes());
    for (slot, byte) in bytes.iter_mut().zip(fields) {
        *slot = byte;
    }
    uaccess::copy_to_user(process.space(), at, &bytes).map_err(|_| Errno::EFAULT)
}

/// The generator's state. Zero means "not seeded yet".
static STATE: SpinLock<u64> = SpinLock::new(0);

/// The most `getrandom` hands out in one call.
///
/// A short count is legal and every caller loops on it, so this bounds a
/// kernel stack buffer rather than the program's request.
const CHUNK: usize = 256;

/// `getrandom`.
///
/// # This is not random
///
/// It is xorshift64*, seeded from the high-resolution counter, and it is here
/// because a libc that is refused `getrandom` at startup goes looking for
/// `/dev/urandom` and there is no `/dev` yet. What it produces differs from
/// boot to boot and is good enough for a hash table's seed or a stack
/// protector's canary on a machine nobody is attacking. It is **not** good
/// enough for a key, and no flag makes it so: `GRND_RANDOM` is accepted and
/// means nothing different. The entropy pool is a later stage's, and when it
/// lands this function's body changes and its callers do not.
pub(crate) fn sys_getrandom(
    process: &Process,
    buf: u64,
    len: u64,
    flags: u64,
) -> Result<usize, Errno> {
    const GRND_NONBLOCK: u64 = 1;
    const GRND_RANDOM: u64 = 2;
    const GRND_INSECURE: u64 = 4;
    if flags & !(GRND_NONBLOCK | GRND_RANDOM | GRND_INSECURE) != 0 {
        return Err(Errno::EINVAL);
    }
    let count = usize::try_from(len).unwrap_or(usize::MAX).min(CHUNK);
    if count == 0 {
        return Ok(0);
    }

    let mut bytes = [0_u8; CHUNK];
    {
        let mut state = STATE.lock();
        if *state == 0 {
            *state = arch::counter_now() | 1;
        }
        for slot in bytes.iter_mut().take(count) {
            let mut x = *state;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            *state = x;
            *slot = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
    }
    let chunk = bytes.get(..count).ok_or(Errno::EINVAL)?;
    uaccess::copy_to_user(process.space(), buf, chunk).map_err(|_| Errno::EFAULT)?;
    Ok(count)
}
