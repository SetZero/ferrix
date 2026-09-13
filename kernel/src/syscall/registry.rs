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

/// One past the largest pid: Linux's default `pid_max`.
pub(crate) const PID_MAX: u32 = 32_768;

/// Where numbering resumes after wrapping. Linux's `RESERVED_PIDS`: the
/// numbers below it stay with whatever started at boot.
const RESERVED: u32 = 300;

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
    last: 0,
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

/// Share a process and make it findable by its pid.
///
/// Every path that makes a process calls this where it would otherwise have
/// called `Arc::new`: a process that is shared but not registered is one
/// `kill` and `/proc` cannot see.
pub(crate) fn register(process: Process) -> Arc<Process> {
    let process = Arc::new(process);
    let pid = process.pid();
    if pid != 0 {
        let _ = REGISTRY.lock().live.insert(pid, Arc::downgrade(&process));
    }
    process
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
    let registry = REGISTRY.lock();
    registry.live.values().filter_map(Weak::upgrade).collect()
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
    Ok(3)
}
