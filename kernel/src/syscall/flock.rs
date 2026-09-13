//! `flock`: advisory locks on a whole file, held by an open file description.
//!
//! # Whose lock it is
//!
//! A `flock` lock belongs to the open file description, not to the process and
//! not to the descriptor number. Two descriptors made by `dup`, or inherited
//! across `fork`, share one description and so one lock: either may convert or
//! release it, and it lasts until the last of them is closed. Two separate
//! `open`s of the same file are two descriptions, and contend. That is what
//! busybox's `flock FILE -c CMD` relies on: it opens the file, locks it, and
//! forks the command, which inherits the description and the lock with it.
//!
//! # Released when the description goes, with no hook in it
//!
//! The table holds a [`Weak`] reference to each holder's description, and
//! every look at the table first sweeps out the holders whose description is
//! gone. So a lock ends when its description does -- on `close`, on `exit`, on
//! `execve` closing a close-on-exec descriptor -- without `ferrix_vfs::OpenFile`
//! or `crate::fs` having to know that locks exist.
//!
//! Nothing is woken at that moment, because nothing of this module runs then.
//! A waiter sees the lock gone at its wait's next recheck, which
//! `WaitQueue::wait_until_deadline` makes every few milliseconds whether or not
//! it is woken. `LOCK_UN` and a conversion do wake the waiters, since those
//! happen inside a call that can.
//!
//! The table never upgrades a reference. Liveness is read with
//! [`Weak::strong_count`] and identity by address, so the table's lock is
//! never where a description's last strong reference is dropped -- which would
//! run the description's teardown, and a pipe end's wake-ups, under it.
//!
//! # Which file
//!
//! A lock is on the file, so it is keyed by the filesystem the description's
//! mount belongs to and the inode number within it -- not by the inode's
//! `Arc`, of which a filesystem may hand out more than one for one file. The
//! filesystem's address cannot be reused by another filesystem while a holder
//! is live, because the holder's description keeps its mount, and so the
//! filesystem, alive; and dead holders are swept before any key is compared.
//!
//! # A conversion is not atomic
//!
//! Asking for a lock a description already holds in the other mode first gives
//! up the one it has, then asks, as Linux's `flock_lock_inode` does. A
//! `LOCK_NB` conversion that is refused therefore leaves the description with
//! no lock at all, which is what `flock(2)` documents.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN};
use ferrix_sync::SpinLock;
use ferrix_vfs::OpenFile;

use crate::sched::WaitQueue;
use crate::syscall::fd;
use crate::syscall::process::Process;

/// A wait with no deadline of its own: a release or a signal ends it.
const FOREVER: u64 = u64::MAX;

/// The file a lock is on. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    /// The filesystem instance's address, as a number.
    filesystem: usize,
    /// The inode number within it.
    ino: u64,
}

/// The two modes a lock is held in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `LOCK_SH`: any number of descriptions at once.
    Shared,
    /// `LOCK_EX`: one description, with no shared holder beside it.
    Exclusive,
}

/// One description's lock on one file.
#[derive(Debug)]
struct Holder {
    /// The file.
    key: Key,
    /// The description holding it. Dead once the description is dropped.
    owner: Weak<OpenFile>,
    /// How it is held.
    mode: Mode,
}

/// What one look at the table found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// The description holds the lock in the mode it asked for.
    Taken,
    /// Another description holds a lock that conflicts with it.
    Contended,
}

/// Every lock held, on every file. A list rather than a map: a system holds a
/// handful at once, and a sweep has to visit every holder anyway.
static HOLDERS: SpinLock<Vec<Holder>> = SpinLock::new(Vec::new());

/// Woken when a call releases or converts a lock.
static RELEASED: WaitQueue = WaitQueue::new();

/// `flock(fd, operation)`.
///
/// # Errors
///
/// `EBADF` for a descriptor that names nothing, or only a name (`O_PATH`);
/// `EINVAL` for an operation that is not `LOCK_SH`, `LOCK_EX` or `LOCK_UN`,
/// with or without `LOCK_NB`; `EAGAIN` -- `EWOULDBLOCK` -- when `LOCK_NB` meets
/// a conflicting lock; `EINTR` when a signal arrives during the wait.
pub(crate) fn sys_flock(process: &Process, fd: i32, operation: u32) -> Result<usize, Errno> {
    // Held for the whole call, as Linux's `fdget` holds the file: a `close`
    // on another thread must not end the description out from under a wait.
    let file = fd::file(process, fd)?;
    if file.is_path() {
        return Err(Errno::EBADF);
    }
    let mode = match operation & !LOCK_NB {
        LOCK_SH => Mode::Shared,
        LOCK_EX => Mode::Exclusive,
        LOCK_UN => {
            release(&file);
            RELEASED.wake_all();
            return Ok(0);
        }
        _ => return Err(Errno::EINVAL),
    };
    let key = key_of(&file);
    let owner = Arc::downgrade(&file);

    let (first, converted) = attempt(key, &owner, mode);
    if converted {
        RELEASED.wake_all();
    }
    if first == Attempt::Taken {
        return Ok(0);
    }
    if operation & LOCK_NB != 0 {
        return Err(Errno::EAGAIN);
    }

    let mut outcome = Err(Errno::EINTR);
    let mut converted = false;
    let _ = RELEASED.wait_until_deadline(
        || {
            let (now, gave_up) = attempt(key, &owner, mode);
            converted |= gave_up;
            if now == Attempt::Taken {
                outcome = Ok(0);
                return true;
            }
            process.signal_pending()
        },
        FOREVER,
    );
    // Only if another descriptor sharing this description took the other
    // mode while this one waited, and this call then converted it.
    if converted {
        RELEASED.wake_all();
    }
    outcome
}

/// One look at the table for `owner`, which wants `key` in `mode`.
///
/// Sweeps out the dead holders, gives up `owner`'s lock if it holds one in the
/// other mode, and takes `mode` if nothing else conflicts. Answers what it
/// found, and whether it gave a lock up.
fn attempt(key: Key, owner: &Weak<OpenFile>, mode: Mode) -> (Attempt, bool) {
    let mut holders = HOLDERS.lock();
    holders.retain(|holder| holder.owner.strong_count() > 0);
    let mut converted = false;
    if let Some(at) = holders
        .iter()
        .position(|holder| Weak::ptr_eq(&holder.owner, owner))
    {
        if holders.get(at).is_some_and(|holder| holder.mode == mode) {
            return (Attempt::Taken, false);
        }
        let _ = holders.swap_remove(at);
        converted = true;
    }
    let contended = holders.iter().any(|holder| {
        holder.key == key && (mode == Mode::Exclusive || holder.mode == Mode::Exclusive)
    });
    if contended {
        return (Attempt::Contended, converted);
    }
    holders.push(Holder {
        key,
        owner: Weak::clone(owner),
        mode,
    });
    (Attempt::Taken, converted)
}

/// Give up whatever lock `file` holds, sweeping the dead holders as it goes.
fn release(file: &Arc<OpenFile>) {
    let mine = Arc::as_ptr(file);
    HOLDERS.lock().retain(|holder| {
        holder.owner.strong_count() > 0 && !core::ptr::eq(holder.owner.as_ptr(), mine)
    });
}

/// The file `file` is open on.
fn key_of(file: &OpenFile) -> Key {
    Key {
        filesystem: Arc::as_ptr(file.location().mount.filesystem())
            .cast::<()>()
            .addr(),
        ino: file.inode().metadata().ino,
    }
}
