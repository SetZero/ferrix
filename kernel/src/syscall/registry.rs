//! Process identifiers, and finding a live process by one.
//!
//! Two questions only something outside a process can answer: which process
//! is pid 42, and which processes exist at all. `/proc` asks both on every
//! listing, `kill` and `wait4` ask the first, and a diagnostic dump asks the
//! second. One table answers them, keyed by pid.
//!
//! # Weak references
//!
//! The table holds a [`Weak`] per process, so being listed never keeps a
//! process alive: what ends a process is its last owner letting go, not a
//! directory listing that happened to be holding it. A process removes its
//! own entry when it is dropped, and a lookup that races the drop fails to
//! upgrade and reports nothing, which is the truth by then.
//!
//! # How a number is chosen
//!
//! Cyclically, as Linux's `alloc_pid` does: the number after the last one
//! handed out, skipping any still in use, wrapping past [`PID_MAX`] to
//! [`RESERVED`]. Not the lowest free number, which would be simpler and would
//! hand a just-freed pid straight to the next process — so a `kill` aimed at
//! a process that had just exited would reach an unrelated one. A pid is
//! reserved from the moment it is chosen, before the process is shared, so
//! two processes made at once cannot be given the same one.

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use crate::sync::SpinLock;

use crate::syscall::process::{self, Process};
use crate::syscall::thread::Thread;

/// One past the largest pid: Linux's default `pid_max`.
pub(crate) const PID_MAX: u32 = 32_768;

/// Where numbering resumes after wrapping. Linux's `RESERVED_PIDS`: the
/// numbers below it stay with whatever started at boot.
const RESERVED: u32 = 300;

/// The pid Linux gives the first user process, which programs rely on: a shell
/// running as init reports `$$` as 1, its children see 1 as their parent, and
/// busybox's `init` refuses to run as anything else. [`allocate`] never hands
/// it out; [`allocate_init`] does.
pub(crate) const INIT_PID: u32 = 1;

/// The table.
#[derive(Debug)]
struct Registry {
    /// Every pid in use. An entry that does not upgrade is reserved for a
    /// process still being built, or held by one being dropped.
    live: BTreeMap<u32, Weak<Process>>,
    /// The pid handed out last.
    last: u32,
}

/// The one table: pids are global until stage 13's pid namespaces.
static REGISTRY: SpinLock<Registry> = SpinLock::new(Registry {
    live: BTreeMap::new(),
    // So the first ordinary pid is 2: 1 is init's.
    last: INIT_PID,
});

/// Choose and reserve a pid, or `None` if every one is in use.
pub(crate) fn allocate() -> Option<u32> {
    let mut guard = REGISTRY.lock();
    let registry = &mut *guard;
    let mut candidate = registry.last;
    for _ in 0..PID_MAX {
        candidate = if candidate + 1 >= PID_MAX {
            RESERVED
        } else {
            candidate + 1
        };
        if let Entry::Vacant(slot) = registry.live.entry(candidate) {
            let _ = slot.insert(Weak::new());
            registry.last = candidate;
            return Some(candidate);
        }
    }
    None
}

/// Reserve [`INIT_PID`] for the process init starts, or `None` if a process
/// still holds it.
pub(crate) fn allocate_init() -> Option<u32> {
    let mut guard = REGISTRY.lock();
    if let Entry::Vacant(slot) = guard.live.entry(INIT_PID) {
        let _ = slot.insert(Weak::new());
        return Some(INIT_PID);
    }
    None
}

/// Whether no process, live or on its way out, holds `pid`.
pub(crate) fn is_free(pid: u32) -> bool {
    !REGISTRY.lock().live.contains_key(&pid)
}

/// Share a process and make it findable by its pid.
///
/// Every path that makes a process calls this where it would otherwise have
/// called `Arc::new`: a process that is shared but not registered is one
/// `kill` and `/proc` cannot see.
pub(crate) fn register(process: Process) -> Arc<Process> {
    let process = Arc::new(process);
    publish(&process);
    process
}

/// List a fork child's first thread on it, then make the child findable, in
/// that order: nothing can find the child without the mask its thread
/// inherited. See [`publish`].
pub(crate) fn publish_forked(child: &Arc<Process>, thread: &Arc<Thread>) {
    child.add_thread(thread);
    publish(child);
}

/// Choose a thread id for a new thread of `process`, which is already shared
/// and findable, and have it find `process` from the start: thread ids and
/// pids are one space, as on Linux, so `kill` or `prlimit` given a thread's
/// id reach its process. `None` if every number is in use.
pub(crate) fn allocate_thread(process: &Arc<Process>) -> Option<u32> {
    let tid = allocate()?;
    let _ = REGISTRY.lock().live.insert(tid, Arc::downgrade(process));
    Some(tid)
}

/// Give back thread id `tid` of `process`, if it still names that process.
/// Called by a thread other than its process's first as it is dropped.
pub(crate) fn release_thread(tid: u32, process: &Process) {
    let mut registry = REGISTRY.lock();
    if registry
        .live
        .get(&tid)
        .is_some_and(|entry| core::ptr::eq(entry.as_ptr(), process))
    {
        let _ = registry.live.remove(&tid);
    }
}

/// How many numbers name `process`: its pid, and one for each of its threads
/// that holds an id of its own. For the check that a thread which replaced
/// its process's program gave its own id back.
pub(crate) fn numbers_naming(process: &Process) -> usize {
    REGISTRY
        .lock()
        .live
        .values()
        .filter(|entry| core::ptr::eq(entry.as_ptr(), process))
        .count()
}

/// Make a process that is already shared findable by its pid.
///
/// For a process that must be complete before `kill`, a process group's
/// signal or `/proc` can reach it: a fork child, whose first thread -- with
/// the blocked mask a signal sent to it is judged against -- is listed first.
pub(crate) fn publish(process: &Arc<Process>) {
    let pid = process.pid();
    if pid != 0 {
        let _ = REGISTRY.lock().live.insert(pid, Arc::downgrade(process));
    }
}

/// Give a pid back. Called by a process as it is dropped.
pub(crate) fn release(pid: u32) {
    let _ = REGISTRY.lock().live.remove(&pid);
}

/// The live process with this pid.
pub(crate) fn find(pid: u32) -> Option<Arc<Process>> {
    REGISTRY.lock().live.get(&pid).and_then(Weak::upgrade)
}

/// Every live process, in ascending pid order.
///
/// The strong references are taken under the lock and the lock released
/// before they are returned, so a caller that drops the last reference to a
/// process drops it with the table unlocked — `release` needs the lock.
pub(crate) fn live() -> Vec<Arc<Process>> {
    // Each process once, under its pid and not under its other threads' ids.
    // Filtered after the lock is let go, since a reference dropped here may be
    // a process's last, and dropping a process takes the lock.
    let entries: Vec<(u32, Arc<Process>)> = {
        let registry = REGISTRY.lock();
        registry
            .live
            .iter()
            .filter_map(|(&number, entry)| entry.upgrade().map(|process| (number, process)))
            .collect()
    };
    entries
        .into_iter()
        .filter(|(number, process)| process.pid() == *number)
        .map(|(_, process)| process)
        .collect()
}

/// The boot self-check's part: numbers are distinct, found, listed in order,
/// given back when a process goes, and not handed straight out again.
///
/// Returns how many processes it numbered.
pub(crate) fn check() -> Result<u32, &'static str> {
    let make = || process::new_for_check().map_err(|_| "no address space for the pid check");
    let first = make()?;
    let second = make()?;
    let (one, two) = (first.pid(), second.pid());
    if one == 0 || two == 0 || one == two {
        return Err("two processes were not given two distinct pids");
    }
    let found = |pid: u32, process: &Arc<Process>| {
        find(pid).is_some_and(|found| Arc::ptr_eq(&found, process))
    };
    if !found(one, &first) || !found(two, &second) {
        return Err("a process is not found by its own pid");
    }
    let listed: Vec<u32> = live().iter().map(|process| process.pid()).collect();
    let ascending = listed
        .windows(2)
        .all(|pair| matches!(pair, [low, high] if low < high));
    if !ascending || !listed.contains(&one) || !listed.contains(&two) {
        return Err("the live processes are not listed once each, in pid order");
    }

    drop(first);
    drop(second);
    if find(one).is_some() || find(two).is_some() {
        return Err("a dropped process is still found by its pid");
    }
    let third = make()?;
    if third.pid() == one || third.pid() == two {
        return Err("a pid was handed out again straight after it was given back");
    }
    if [one, two, third.pid()].contains(&INIT_PID) {
        return Err("an ordinary process was given init's pid");
    }

    // Init's pid, which nothing but init takes. Boot checks run before init
    // starts, so it is free here; taken and given straight back.
    if !is_free(INIT_PID) {
        return Err("init's pid was taken before init started");
    }
    if allocate_init() != Some(INIT_PID) {
        return Err("init was not given pid 1 while it was free");
    }
    if allocate_init().is_some() {
        return Err("init's pid was handed out twice");
    }
    release(INIT_PID);
    if !is_free(INIT_PID) {
        return Err("init's pid was not given back");
    }
    Ok(3)
}
