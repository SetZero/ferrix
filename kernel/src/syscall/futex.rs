//! `futex`: sleeping on a word of user memory, and being woken for it.
//!
//! A libc's locks, condition variables and `pthread_join` are a user-space
//! compare-and-swap with this call behind it for the slow path. busybox
//! reaches it through glibc even with one thread -- `system`, `popen` and
//! stdio's locks all lock, and a lock that sees a waiter bit calls in -- so a
//! missing answer is a hang rather than an error.
//!
//! # What a wait is keyed by
//!
//! The address space and the user address, as a pair. Two processes' words
//! at the same address are different words, and nothing here shares memory
//! between address spaces yet, so a "shared" futex (one without
//! `FUTEX_PRIVATE_FLAG`) is keyed the same way as a private one. That is the
//! whole difference Linux draws between them -- a shared key names the page's
//! backing object -- and it becomes a difference here when `MAP_SHARED`
//! memory can be seen by two spaces at once. The space is safe to name by
//! address: every waiter holds its process, and so the space, for as long as
//! it is on the table.
//!
//! # The lost wake-up, again
//!
//! `FUTEX_WAIT` promises to sleep only if the word still holds the value the
//! caller saw, and a `FUTEX_WAKE` that follows a change must find every waiter
//! that saw the old value. Both are kept by one lock: the word is read, and
//! the waiter put on the table, while the table's lock is held, and a waker
//! takes the same lock to look. A waker that changed the word before the read
//! makes the read see the change, and one that changed it after finds the
//! waiter listed. The read goes through [`uaccess`] like every other, faulting
//! the page in; it is done once before the lock is taken, so that the read
//! under it only ever finds a page already present.
//!
//! A waker rouses a waiter while still holding the lock, and a waiter leaving
//! for any reason takes the lock before it returns. So a wake can never land
//! after its waiter has gone on to sleep for something else -- the failure
//! `sched::wait`'s comments describe, a sleep cut short by a wake meant for an
//! earlier one.
//!
//! # One table
//!
//! Linux hashes keys into buckets, each with its own lock. There is one bucket
//! here, because nothing yet makes enough futex calls at once to contend for
//! it, and one bucket makes a requeue between two keys a single lock rather
//! than an ordering rule. Splitting it is a change to [`TABLE`] and the three
//! functions that lock it.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sync::SpinLock;
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    FUTEX_CLOCK_REALTIME, FUTEX_CMP_REQUEUE, FUTEX_PRIVATE_FLAG, FUTEX_REQUEUE, FUTEX_WAIT,
    FUTEX_WAIT_BITSET, FUTEX_WAKE, FUTEX_WAKE_BITSET,
};

use crate::sched::{self, Task, WaitQueue};
use crate::syscall::process::Process;
use crate::syscall::time::TimeWidth;
use crate::syscall::uaccess;
use crate::user::space::AddressSpace;

/// A bitset that matches every waiter: what the plain `WAIT` and `WAKE` use.
const FUTEX_BITSET_MATCH_ANY: u32 = 0xFFFF_FFFF;

/// Nanoseconds in a second.
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Which word a waiter sleeps on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    /// The address space, by address. See the module documentation.
    space: usize,
    /// The word's user address.
    address: u64,
}

impl Key {
    /// The key for `address` in `space`.
    fn new(space: &Arc<AddressSpace>, address: u64) -> Key {
        Key {
            space: Arc::as_ptr(space).addr(),
            address,
        }
    }
}

/// One task asleep in `FUTEX_WAIT`, as its wakers see it.
#[derive(Debug)]
struct Sleeper {
    /// The task to make runnable. `None` only before the scheduler starts,
    /// when a wait spins on `woken` instead.
    task: Option<Arc<Task>>,
    /// Set by the wake that took it off the table.
    woken: AtomicBool,
}

impl Sleeper {
    /// Mark it woken and make its task runnable. Called with [`TABLE`] held.
    fn rouse(&self) {
        self.woken.store(true, Ordering::Release);
        if let Some(task) = &self.task {
            sched::wake(task);
        }
    }
}

/// A waiter on the table.
#[derive(Debug)]
struct Entry {
    /// What it waits on; changed in place by a requeue.
    key: Key,
    /// Which wakes it answers to: `FUTEX_WAKE_BITSET` rouses it only if the
    /// two share a bit.
    bitset: u32,
    /// Who it is.
    sleeper: Arc<Sleeper>,
}

/// Every waiter, oldest first, so that a wake rouses in arrival order as
/// Linux's does.
static TABLE: SpinLock<Vec<Entry>> = SpinLock::new(Vec::new());

/// What waiters block on. Nobody wakes it as a whole: a wake rouses its
/// waiters' tasks one by one, and this is only the blocking mechanism.
static SLEEP: WaitQueue = WaitQueue::new();

/// `futex` and `futex_time64`, which differ only in `width`: the size of the
/// `timespec` the timeout argument points at.
///
/// # Errors
///
/// `EAGAIN` if a wait's or a compare-requeue's word no longer holds the value
/// given; `ETIMEDOUT` if a wait's timeout passes; `EINTR` if the caller is
/// ended while it waits; `EINVAL` for a misaligned word, a zero bitset, a
/// negative count or a malformed timeout; `EFAULT` for a word or timeout that
/// cannot be read; `ENOSYS` for an operation not implemented here, the
/// priority-inheritance ones and `FUTEX_WAKE_OP` among them, and for
/// `FUTEX_CLOCK_REALTIME` on an operation that has no timeout.
pub(crate) fn sys_futex(process: &Process, a: &[u64; 6], width: TimeWidth) -> Result<usize, Errno> {
    let [address, op, value, timeout, address2, value3] = *a;
    let op = op as u32;
    // The ABI's `u32 val` and `u32 val3`; the upper half of a 64-bit register
    // is whatever the caller left there.
    let value = value as u32;
    let value3 = value3 as u32;
    let command = op & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);
    if op & FUTEX_CLOCK_REALTIME != 0 && !matches!(command, FUTEX_WAIT | FUTEX_WAIT_BITSET) {
        return Err(Errno::ENOSYS);
    }
    match command {
        FUTEX_WAIT => {
            let deadline = if timeout == 0 {
                None
            } else {
                let relative = read_timespec(process, timeout, width)?;
                Some(crate::timer::now_nanos().saturating_add(relative))
            };
            wait(process, address, value, FUTEX_BITSET_MATCH_ANY, deadline)
        }
        FUTEX_WAIT_BITSET => {
            if value3 == 0 {
                return Err(Errno::EINVAL);
            }
            // Absolute. Every clock here is the counter -- see
            // `time::sys_clock_gettime` -- so a deadline on `CLOCK_MONOTONIC`
            // and one on `CLOCK_REALTIME` are the same number.
            let deadline = if timeout == 0 {
                None
            } else {
                Some(read_timespec(process, timeout, width)?)
            };
            wait(process, address, value, value3, deadline)
        }
        FUTEX_WAKE => wake(process, address, value as i32, FUTEX_BITSET_MATCH_ANY),
        FUTEX_WAKE_BITSET => {
            if value3 == 0 {
                return Err(Errno::EINVAL);
            }
            wake(process, address, value as i32, value3)
        }
        // The fourth argument is a count, not a pointer, for these two: Linux
        // reads it as `val2`, the register narrowed to an `int`.
        FUTEX_REQUEUE => requeue(
            process,
            address,
            address2,
            value as i32,
            timeout as i32,
            None,
        ),
        FUTEX_CMP_REQUEUE => requeue(
            process,
            address,
            address2,
            value as i32,
            timeout as i32,
            Some(value3),
        ),
        _ => Err(Errno::ENOSYS),
    }
}

/// The key for a futex word, which must be aligned as a 32-bit word is.
fn key(process: &Process, address: u64) -> Result<Key, Errno> {
    if !address.is_multiple_of(4) {
        return Err(Errno::EINVAL);
    }
    Ok(Key::new(process.space(), address))
}

/// Read the futex word at `address`.
fn read_word(process: &Process, address: u64) -> Result<u32, Errno> {
    let mut bytes = [0_u8; 4];
    uaccess::copy_from_user(process.space(), address, &mut bytes).map_err(|_| Errno::EFAULT)?;
    Ok(u32::from_le_bytes(bytes))
}

/// Sleep on `address` if it holds `expected`, until a wake whose bitset
/// shares a bit with `bitset`, the caller is ended, or `deadline` passes.
fn wait(
    process: &Process,
    address: u64,
    expected: u32,
    bitset: u32,
    deadline: Option<u64>,
) -> Result<usize, Errno> {
    let key = key(process, address)?;
    // Faulted in here, outside the lock, and read again under it.
    let _ = read_word(process, address)?;
    let sleeper = Arc::new(Sleeper {
        task: sched::current(),
        woken: AtomicBool::new(false),
    });
    {
        let mut table = TABLE.lock();
        if read_word(process, address)? != expected {
            return Err(Errno::EAGAIN);
        }
        table.push(Entry {
            key,
            bitset,
            sleeper: Arc::clone(&sleeper),
        });
    }

    let _ = SLEEP.wait_until_deadline(
        // A pending signal ends the wait with `EINTR`, as it ends every wait.
        || sleeper.woken.load(Ordering::Acquire) || process.signal_pending(),
        deadline.unwrap_or(u64::MAX),
    );

    // Off the table however it left, and under the lock, so that a waker which
    // found it has finished rousing it before this returns.
    let mut table = TABLE.lock();
    table.retain(|entry| !Arc::ptr_eq(&entry.sleeper, &sleeper));
    drop(table);
    // Woken wins over the other two: a wake that took this waiter off the
    // table counted it, and reporting a timeout would lose that wake for
    // whoever the waker meant it for.
    if sleeper.woken.load(Ordering::Acquire) {
        Ok(0)
    } else if process.signal_pending() {
        // A restart code, not `EINTR`: a futex wait restarts under
        // `SA_RESTART`, which is how glibc's and musl's condition variables
        // survive a handled signal. The way back settles it.
        Err(Errno::ERESTARTSYS)
    } else {
        Err(Errno::ETIMEDOUT)
    }
}

/// Rouse up to `count` waiters on `address` whose bitset shares a bit with
/// `bitset`, oldest first, and report how many.
fn wake(process: &Process, address: u64, count: i32, bitset: u32) -> Result<usize, Errno> {
    let key = key(process, address)?;
    Ok(rouse(key, count, bitset, Sleeper::rouse))
}

/// Take up to `count` matching waiters off the table, calling `with` on each.
///
/// A `count` of zero or less still takes one, as Linux's `futex_wake` does:
/// it counts a waiter before comparing against the limit.
fn rouse(key: Key, count: i32, bitset: u32, with: fn(&Sleeper)) -> usize {
    let limit = usize::try_from(count.max(1)).unwrap_or(1);
    let mut roused = 0;
    TABLE.lock().retain(|entry| {
        if roused >= limit || entry.key != key || entry.bitset & bitset == 0 {
            return true;
        }
        with(&entry.sleeper);
        roused += 1;
        false
    });
    roused
}

/// `FUTEX_REQUEUE` and, with `expected`, `FUTEX_CMP_REQUEUE`: rouse up to
/// `wake_count` waiters on `from` and move up to `move_count` more onto `to`.
/// Answers how many were roused or moved.
fn requeue(
    process: &Process,
    from: u64,
    to: u64,
    wake_count: i32,
    move_count: i32,
    expected: Option<u32>,
) -> Result<usize, Errno> {
    let (Ok(wake_count), Ok(move_count)) =
        (usize::try_from(wake_count), usize::try_from(move_count))
    else {
        return Err(Errno::EINVAL);
    };
    let source = key(process, from)?;
    let target = key(process, to)?;
    if expected.is_some() {
        let _ = read_word(process, from)?;
    }

    let mut table = TABLE.lock();
    if let Some(expected) = expected
        && read_word(process, from)? != expected
    {
        return Err(Errno::EAGAIN);
    }
    let mut taken = 0_usize;
    let mut moved = Vec::new();
    let mut index = 0;
    while let Some(entry) = table.get(index) {
        if entry.key != source || taken >= wake_count.saturating_add(move_count) {
            index += 1;
            continue;
        }
        taken += 1;
        let entry = table.remove(index);
        if taken <= wake_count {
            entry.sleeper.rouse();
        } else {
            moved.push(Entry {
                key: target,
                ..entry
            });
        }
    }
    // Onto the end of the queue, behind anything already waiting on `to`.
    table.extend(moved);
    Ok(taken)
}

/// Rouse up to `count` waiters on `address` in `space`: what a process's end
/// does for the address `CLONE_CHILD_CLEARTID` or `set_tid_address` gave it.
pub(crate) fn wake_address(space: &Arc<AddressSpace>, address: u64, count: i32) -> usize {
    rouse(
        Key::new(space, address),
        count,
        FUTEX_BITSET_MATCH_ANY,
        Sleeper::rouse,
    )
}

/// How many waiters are on `address` in `process`'s space right now.
///
/// For the self-check, which has to know its waiter is asleep before it can
/// tell a wake that works from one that arrived first.
pub(crate) fn waiters_on(process: &Process, address: u64) -> usize {
    let key = Key::new(process.space(), address);
    TABLE.lock().iter().filter(|entry| entry.key == key).count()
}

/// Take up to `count` waiters off `address` and report them woken without
/// rousing them: a wake with the one bug a count cannot see.
///
/// Exists for the self-check's negative control, which must show that the
/// check fails a wake like this one rather than trusting the count it returns.
pub(crate) fn forget_waiters(process: &Process, address: u64, count: i32) -> usize {
    rouse(
        Key::new(process.space(), address),
        count,
        FUTEX_BITSET_MATCH_ANY,
        |_| {},
    )
}

/// Read a `struct timespec` of `width` as nanoseconds.
fn read_timespec(process: &Process, at: u64, width: TimeWidth) -> Result<u64, Errno> {
    let wide = width == TimeWidth::Wide || size_of::<usize>() == 8;
    let field = if wide { 8 } else { 4 };
    let mut bytes = [0_u8; 16];
    let used = bytes.get_mut(..field * 2).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), at, used).map_err(|_| Errno::EFAULT)?;
    let signed = |from: usize| -> Result<i64, Errno> {
        let slice = bytes.get(from..from + field).ok_or(Errno::EINVAL)?;
        Ok(if wide {
            i64::from_le_bytes(slice.try_into().map_err(|_| Errno::EINVAL)?)
        } else {
            i64::from(i32::from_le_bytes(
                slice.try_into().map_err(|_| Errno::EINVAL)?,
            ))
        })
    };
    let seconds = u64::try_from(signed(0)?).map_err(|_| Errno::EINVAL)?;
    let nanos = u64::try_from(signed(field)?).map_err(|_| Errno::EINVAL)?;
    if nanos >= NANOS_PER_SECOND {
        return Err(Errno::EINVAL);
    }
    Ok(seconds
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add(nanos))
}
