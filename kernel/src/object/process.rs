//! The process as the core knows it: an address space, a number, a handle
//! table and a place in the job tree -- and nothing a personality adds.
//!
//! # What is the core's and what is the personality's
//!
//! A process is the container the core enforces isolation between, so the
//! core has to be able to name one. But almost everything a running program
//! has is a personality's: its descriptors, its root and working directory,
//! its signal dispositions and masks, its `brk`, its credentials, its parent
//! and children. None of that has to be correct for one process to be kept
//! out of another's memory, so none of it is here.
//!
//! What is here is what the core itself enforces or reports:
//!
//! - the [`AddressSpace`], which is the isolation;
//! - the pid, which is how anything outside the process names it;
//! - when it was made;
//! - the native ABI's [`HandleTable`], which is its capabilities;
//! - its [`Job`], which is where kill authority over it lives;
//! - how it ended ([`Exit`]), which is what a handle to it holds.
//!
//! # How the personality's half is reached
//!
//! Not from here. A personality's process *contains* one of these and adds
//! its own state beside it; this type has no field that leads back. Where the
//! core has to hold a process as a whole -- the pid table, a native handle to
//! a process nobody has started, a scheduled thread -- it holds a [`Host`]:
//! the personality's object, seen only through the few questions the core
//! has to ask of it. The personality recovers its own type from a `Host` by
//! [`downcast`], which answers `None` for any other.
//!
//! So the dependency points one way. The personality names this module; this
//! module names nothing above the core.
//!
//! # The pid table
//!
//! Which process is pid 42, and which processes exist at all, are questions
//! only something outside every process can answer; a job answers the second
//! whenever it is killed. The table is here, keyed by number, holding a weak
//! [`Host`] per number so that being listed never keeps a process alive.
//!
//! Numbers are chosen cyclically, as Linux's `alloc_pid` does: the number
//! after the last one handed out, skipping any still in use, wrapping past
//! [`PID_MAX`] to [`RESERVED`]. Not the lowest free number, which would hand a
//! just-freed pid straight to the next process -- so a `kill` aimed at a
//! process that had just exited would reach an unrelated one. A number is
//! reserved from the moment it is chosen, before the process is shared, so
//! two processes made at once cannot be given the same one.

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use ferrix_native_abi::signals::Signals as ObjectSignals;

use crate::object::job::{self, Job, JobError};
use crate::object::port::{self, Observer, PortError};
use crate::object::{self as objects, HandleTable};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::user::space::AddressSpace;

/// A process, as the core enforces and reports it.
#[derive(Debug)]
pub(crate) struct Process {
    /// What it can see.
    space: Arc<AddressSpace>,
    /// Its number, from [`allocate`]: what the pid table finds it by. Zero
    /// only if every number was in use when it was made, in which case
    /// nothing can find it.
    pid: u32,
    /// When it was made, in nanoseconds on the counter.
    started: u64,
    /// The handles it holds, for the native ABI.
    ///
    /// A lock of its own: a channel write looks handles up and takes them
    /// out, and has no business waiting on anything else the process has.
    handles: SpinLock<HandleTable>,
    /// The job it is in, which is its cgroup. Every process is in exactly one:
    /// the root job, its parent's for a fork, or wherever it was moved. Its
    /// lock comes before any job's (see `object::job`, "Lock order").
    membership: SpinLock<Arc<Job>>,
    /// Whether it is counted among its job's live members: from when it is
    /// made until it leaves. Changed only under `membership`.
    counted: AtomicBool,
    /// How it ended, and who is waiting to hear. Apart from the process,
    /// because a handle to the process holds it: see [`Exit`].
    exit: Arc<Exit>,
}

impl Process {
    /// A process over `space`, numbered `pid` -- which the caller has
    /// reserved with [`allocate`] or [`allocate_init`], or zero -- and
    /// counted in `job` from now on.
    pub(crate) fn new(space: Arc<AddressSpace>, pid: u32, job: Arc<Job>) -> Process {
        let mut flipped = job::Flipped::new();
        job.count_in(&mut flipped);
        job::notify(flipped);
        Process {
            space,
            pid,
            started: crate::timer::now_nanos(),
            handles: SpinLock::new(HandleTable::new(objects::HANDLE_LIMIT)),
            membership: SpinLock::new(job),
            counted: AtomicBool::new(true),
            exit: Arc::new(Exit::new()),
        }
    }

    /// What it can see.
    pub(crate) fn space(&self) -> &Arc<AddressSpace> {
        &self.space
    }

    /// Its process id; zero if it was made with every pid in use.
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// When it was made, in nanoseconds on the counter.
    pub(crate) fn started(&self) -> u64 {
        self.started
    }

    /// Do something with the handle table, under its lock.
    ///
    /// Whatever `change` takes out of the table it should hand back rather
    /// than drop, so that the object dies after the lock is released: an
    /// object's drop can free memory and drain other objects, and
    /// `crate::object::dispose` is where that belongs.
    pub(crate) fn with_handles<R>(&self, change: impl FnOnce(&mut HandleTable) -> R) -> R {
        change(&mut self.handles.lock())
    }

    /// The job it is in: its cgroup.
    pub(crate) fn job(&self) -> Arc<Job> {
        Arc::clone(&self.membership.lock())
    }

    /// Move it into `to`, counting it there and not where it was, if it is
    /// still counted. Its threads go with it: a job holds processes.
    ///
    /// # Errors
    ///
    /// [`JobError::Killed`] if `to`, or a job above it, has been killed;
    /// [`JobError::Removed`] if `rmdir` took it; [`JobError::Internal`] if
    /// the no-internal-process rule keeps processes out of it.
    pub(crate) fn move_to(&self, to: &Arc<Job>) -> Result<(), JobError> {
        let mut flipped = job::Flipped::new();
        let left = {
            let mut membership = self.membership.lock();
            if to.refuses() {
                return Err(JobError::Killed);
            }
            if Arc::ptr_eq(&membership, to) {
                return Ok(());
            }
            if self.counted.load(Ordering::Acquire) {
                to.count_in_checked(&mut flipped)?;
                membership.count_out(&mut flipped);
            } else {
                to.admits()?;
            }
            core::mem::replace(&mut *membership, Arc::clone(to))
        };
        // Outside the lock: it may be the last reference to that job.
        drop(left);
        job::notify(flipped);
        Ok(())
    }

    /// Stop counting it among its job's live members, once. What makes its
    /// job empty when it was the last: called as the process lets go of what
    /// it holds, and as it is dropped if it never did.
    pub(crate) fn leave_job(&self) {
        let mut flipped = job::Flipped::new();
        {
            let membership = self.membership.lock();
            if self.counted.swap(false, Ordering::AcqRel) {
                membership.count_out(&mut flipped);
            }
        }
        job::notify(flipped);
    }

    /// How it ended, and who is waiting to hear.
    pub(crate) fn exit(&self) -> &Exit {
        &self.exit
    }

    /// A reference to how it ends, which outlives it.
    pub(crate) fn exit_record(&self) -> Arc<Exit> {
        Arc::clone(&self.exit)
    }

    /// Whether it has terminated, by exiting or by being killed.
    pub(crate) fn is_terminated(&self) -> bool {
        self.exit.is_terminated()
    }

    /// How it ended, once it has.
    pub(crate) fn exit_status(&self) -> Option<i32> {
        self.exit.status()
    }

    /// The queue woken as it lets go of what it held: once its handles and
    /// descriptors are closed, and again once it is released.
    pub(crate) fn exited(&self) -> &WaitQueue {
        self.exit.exited()
    }
}

impl Drop for Process {
    /// Leave its job's count if it never did, and give the pid back. The
    /// number is not used again until allocation comes round to it.
    ///
    /// A process dropped without being released -- one built and never
    /// shared, as a failed `execve` of a new program leaves -- leaves its
    /// job's count here instead. A released one already has, and this does
    /// nothing, so the reaper, which drops released processes only, never
    /// wakes anything from here.
    fn drop(&mut self) {
        self.leave_job();
        if self.pid != 0 {
            release(self.pid);
        }
    }
}

/// The personality's process a core [`Process`] lives in, seen from the core.
///
/// What the core holds when it has to hold a process as a whole, and asks
/// when a decision is the personality's: how a process ends, and what happens
/// as its threads come and go, depend on what the personality keeps beside
/// the core fields -- descriptors to close, a parent to tell -- and the core
/// does not know what that is.
///
/// `Any`, so that the personality can have its own type back ([`downcast`]).
pub(crate) trait Host: Any + Send + Sync + fmt::Debug {
    /// The core process inside it.
    fn core(&self) -> &Process;

    /// End it from outside with `status`: what a job kill, and dropping the
    /// last handle to a process nobody started, do. Its threads find out on
    /// their way back to user mode; nothing here waits for that.
    fn kill(&self, status: i32);

    /// Count a thread about to start. Before its task is spawned, because the
    /// task can reach its exit on another processor before the spawn returns.
    fn thread_starting(&self);

    /// Count a thread gone -- one that `ended`, or one whose task could not be
    /// spawned -- and, if that was its last, end the process or let go of
    /// what it holds, as the personality decides.
    fn thread_gone(&self, ended: bool);

    /// Whether a wait on its behalf should end early: it is ending, or the
    /// personality has something for the waiting thread -- a signal, say --
    /// that no wait may sleep through. What a wait in the item checks beside
    /// its own condition, as the personality's own waits do.
    fn wait_interrupted(&self) -> bool;
}

/// The personality's own type back from a [`Host`], or `None` if `host` is
/// not a `T`.
pub(crate) fn downcast<T: Host>(host: Arc<dyn Host>) -> Option<Arc<T>> {
    let any: Arc<dyn Any + Send + Sync> = host;
    any.downcast::<T>().ok()
}

/// How a process ended, and who is waiting to hear.
///
/// Apart from the process, because this is what a handle to a process holds.
/// A handle kept past the end must not keep the address space and everything
/// else the process owned, and a wait needs nothing else. The personality
/// records how it ended ([`Exit::record`]) and closes it once the process has
/// let go of what it held ([`Exit::close`]); nothing else writes it.
#[derive(Debug)]
pub(crate) struct Exit {
    /// Its exit status, valid once `terminated` is.
    status: AtomicI32,
    /// The signal that ended it, or zero.
    ended_by: AtomicU32,
    /// The terminated condition. Set after `status`, so a reader who sees it
    /// always reads the status that goes with it.
    terminated: AtomicBool,
    /// Woken as it lets go of what it held: once its handles and descriptors are
    /// closed, and again once it is released.
    exited: WaitQueue,
    /// Port registrations waiting for it to end, and `None` once it has
    /// closed its handles and descriptors and they have been taken.
    observers: SpinLock<Option<Vec<Observer>>>,
    /// Set under the observers lock as they are taken, so that
    /// [`Exit::is_closed`], which every poll of a handle's signals asks, need
    /// not take the lock.
    closed: AtomicBool,
}

impl Exit {
    /// Not ended, and watched by nobody.
    fn new() -> Exit {
        Exit {
            status: AtomicI32::new(0),
            ended_by: AtomicU32::new(0),
            terminated: AtomicBool::new(false),
            exited: WaitQueue::new(),
            observers: SpinLock::new(Some(Vec::new())),
            closed: AtomicBool::new(false),
        }
    }

    /// Whether it has terminated, by exiting or by being killed. True from the
    /// moment it starts to end, before it has let go of anything.
    pub(crate) fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
    }

    /// Whether it has ended and closed its handles and descriptors: what a
    /// handle's `TERMINATED` signal reports, a little after
    /// [`Exit::is_terminated`] is true.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Its exit status, once it has terminated.
    pub(crate) fn status(&self) -> Option<i32> {
        self.is_terminated()
            .then(|| self.status.load(Ordering::Acquire))
    }

    /// The signal that ended it, if one did.
    pub(crate) fn signal(&self) -> Option<u32> {
        let signal = self.ended_by.load(Ordering::Acquire);
        (self.is_terminated() && signal != 0).then_some(signal)
    }

    /// The queue woken as it lets go of what it held.
    pub(crate) fn exited(&self) -> &WaitQueue {
        &self.exited
    }

    /// Queue `observer`'s packet once it has ended and closed its handles, or
    /// at once if it already has.
    ///
    /// # Errors
    ///
    /// [`PortError::Full`] at [`port::MAX_OBSERVERS`] registrations.
    pub(crate) fn observe(&self, observer: Observer) -> Result<(), PortError> {
        debug_assert!(
            crate::arch::interrupts_enabled(),
            "a process's watchers were reached with interrupts off, which its plain lock may not be"
        );
        let mut observers = self.observers.lock();
        if let Some(list) = observers.as_mut() {
            return port::register(list, observer);
        }
        drop(observers);
        observer.fire(ObjectSignals::TERMINATED);
        Ok(())
    }

    /// Record how it ended: with `status`, and by `signal` when that is not
    /// zero. The personality's end does this once, first.
    pub(crate) fn record(&self, status: i32, signal: u32) {
        self.ended_by.store(signal, Ordering::Release);
        self.status.store(status, Ordering::Release);
        self.terminated.store(true, Ordering::Release);
    }

    /// Take the registrations waiting for it, for the caller to fire once it
    /// holds no lock, and refuse to keep any more.
    pub(crate) fn close(&self) -> Vec<Observer> {
        debug_assert!(
            crate::arch::interrupts_enabled(),
            "a process's watchers were reached with interrupts off, which its plain lock may not be"
        );
        let mut observers = self.observers.lock();
        self.closed.store(true, Ordering::Release);
        observers.take().unwrap_or_default()
    }
}

/// What a handle to a process holds.
///
/// Its [`Exit`], and not the process: see there. A handle `process_create`
/// made also holds the process's [`Control`], shared by every duplicate of
/// that handle: the weak way back to the process that `process_start` needs.
#[derive(Debug, Clone)]
pub(crate) struct ProcessRef {
    /// How it ended.
    exit: Arc<Exit>,
    /// The way back to it, for a process made through the native ABI.
    control: Option<Arc<Control>>,
}

/// The way from a created process's handles back to the process.
///
/// Weak, so that a handle kept past the end keeps nothing of the process but
/// its [`Exit`]. Strong only until it starts: nothing else holds a process no
/// task runs, so its handles have to, and a start hands that reference over to
/// the process's task.
#[derive(Debug)]
pub(crate) struct Control {
    /// The process, while anything else holds it.
    process: Weak<dyn Host>,
    /// The only strong reference to a process nobody has started.
    unstarted: SpinLock<Option<Arc<dyn Host>>>,
}

impl ProcessRef {
    /// A handle's view of `process`, with no way back to it.
    pub(crate) fn new(process: &Process) -> ProcessRef {
        ProcessRef {
            exit: Arc::clone(&process.exit),
            control: None,
        }
    }

    /// A handle to `host`, made and not yet started, which holds it until a
    /// start takes it over or the last such handle is closed.
    pub(crate) fn created(host: Arc<dyn Host>) -> ProcessRef {
        ProcessRef {
            exit: Arc::clone(&host.core().exit),
            control: Some(Arc::new(Control {
                process: Arc::downgrade(&host),
                unstarted: SpinLock::new(Some(host)),
            })),
        }
    }

    /// How it ended, and who is waiting to hear.
    pub(crate) fn exit(&self) -> &Exit {
        &self.exit
    }

    /// Its exit status, once it has terminated.
    pub(crate) fn exit_status(&self) -> Option<i32> {
        self.exit.status()
    }

    /// The way back to the process, if this handle was made with one.
    pub(crate) fn control(&self) -> Option<&Arc<Control>> {
        self.control.as_ref()
    }
}

impl Control {
    /// The process, if it still exists and is a `T`.
    ///
    /// Never asked on a wait path: a wait needs only the [`Exit`], and a
    /// process that has gone answers `None` here, not a panic.
    pub(crate) fn process<T: Host>(&self) -> Option<Arc<T>> {
        downcast(self.process.upgrade()?)
    }

    /// The process, if it still exists, as the core holds it: what a start
    /// through the native ABI hands the personality back.
    pub(crate) fn host(&self) -> Option<Arc<dyn Host>> {
        self.process.upgrade()
    }

    /// Let go of the reference that kept it before it started, now that its
    /// task holds it.
    pub(crate) fn started(&self) {
        let held = self.unstarted.lock().take();
        drop(held);
    }
}

impl Drop for Control {
    /// End a process nobody started, once no handle is left that could start
    /// it.
    ///
    /// Such a process is held only here. Letting go of it without ending it
    /// would free it without its end running, so no status would be recorded
    /// and its watchers would never hear. So it is killed first.
    ///
    /// # Where this runs
    ///
    /// Only where a handle object is dropped. Every such drop goes through
    /// `object::dispose`, and every caller of that is in task context with
    /// interrupts on:
    /// - a native call's handler;
    /// - a channel's own drop or refusal, reached only inside such a drain;
    /// - a process's end closing a handle table, which since the fault-kill
    ///   fix never runs with interrupts masked.
    ///
    /// A drain running on another processor is that processor's calling task,
    /// not an interrupt. The idle reaper, which the rule "never kill in Drop"
    /// is about, drops only processes whose end has already emptied their
    /// table, so it never holds a `Control`. The assertion is the tripwire, as
    /// `Exit::close`'s is. The reference is taken out of the lock before the
    /// kill, so the end runs under nothing of this lock's.
    fn drop(&mut self) {
        let unstarted = self.unstarted.lock().take();
        if let Some(process) = unstarted {
            debug_assert!(
                crate::arch::interrupts_enabled(),
                "an unstarted process's last handle was dropped with interrupts off"
            );
            process.kill(job::KILLED_STATUS);
        }
    }
}

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

/// The pid table.
#[derive(Debug)]
struct Table {
    /// Every number in use. `None` for one reserved for a process still being
    /// built; an entry that does not upgrade is held by one being dropped.
    live: BTreeMap<u32, Option<Weak<dyn Host>>>,
    /// The number handed out last.
    last: u32,
}

/// The one table: numbers are global until stage 13's pid namespaces.
static TABLE: SpinLock<Table> = SpinLock::new(Table {
    live: BTreeMap::new(),
    // So the first ordinary pid is 2: 1 is init's.
    last: INIT_PID,
});

/// Choose and reserve a number, or `None` if every one is in use.
pub(crate) fn allocate() -> Option<u32> {
    let mut guard = TABLE.lock();
    let table = &mut *guard;
    let mut candidate = table.last;
    for _ in 0..PID_MAX {
        candidate = if candidate + 1 >= PID_MAX {
            RESERVED
        } else {
            candidate + 1
        };
        if let Entry::Vacant(slot) = table.live.entry(candidate) {
            let _ = slot.insert(None);
            table.last = candidate;
            return Some(candidate);
        }
    }
    None
}

/// Reserve [`INIT_PID`] for the process init starts, or `None` if a process
/// still holds it.
pub(crate) fn allocate_init() -> Option<u32> {
    let mut guard = TABLE.lock();
    if let Entry::Vacant(slot) = guard.live.entry(INIT_PID) {
        let _ = slot.insert(None);
        return Some(INIT_PID);
    }
    None
}

/// Whether no process, live or on its way out, holds `number`.
pub(crate) fn is_free(number: u32) -> bool {
    !TABLE.lock().live.contains_key(&number)
}

/// Have `number` find `process` from now on: its pid once it is complete, or
/// one of its threads' numbers, which find their process too.
pub(crate) fn name(number: u32, process: Weak<dyn Host>) {
    let _ = TABLE.lock().live.insert(number, Some(process));
}

/// Give `number` back, whatever it names. Called by a process as it is
/// dropped, and by the boot check.
pub(crate) fn release(number: u32) {
    let _ = TABLE.lock().live.remove(&number);
}

/// Give `number` back if it still names `process`.
pub(crate) fn release_naming<H: Host>(number: u32, process: &H) {
    let mut table = TABLE.lock();
    if table
        .live
        .get(&number)
        .is_some_and(|entry| names(entry.as_ref(), process))
    {
        let _ = table.live.remove(&number);
    }
}

/// How many numbers name `process`: its pid, and one for each of its threads
/// that holds a number of its own.
pub(crate) fn numbers_naming<H: Host>(process: &H) -> usize {
    TABLE
        .lock()
        .live
        .values()
        .filter(|entry| names(entry.as_ref(), process))
        .count()
}

/// Whether a table entry leads to `process`. Compared by address and never by
/// upgrading: a reference upgraded under the table lock could be a process's
/// last, and dropping a process takes the lock.
fn names<H: Host>(entry: Option<&Weak<dyn Host>>, process: &H) -> bool {
    entry.is_some_and(|entry| core::ptr::addr_eq(entry.as_ptr(), process))
}

/// The live process `number` names.
pub(crate) fn find(number: u32) -> Option<Arc<dyn Host>> {
    TABLE
        .lock()
        .live
        .get(&number)
        .and_then(Option::as_ref)
        .and_then(Weak::upgrade)
}

/// Every live process, in ascending pid order, each once.
///
/// The strong references are taken under the lock and the lock released
/// before they are returned, so a caller that drops the last reference to a
/// process drops it with the table unlocked -- [`release`] needs the lock.
pub(crate) fn live() -> Vec<Arc<dyn Host>> {
    // Each process once, under its pid and not under its threads' numbers.
    // Filtered after the lock is let go, since a reference dropped here may be
    // a process's last, and dropping a process takes the lock.
    let entries: Vec<(u32, Arc<dyn Host>)> = {
        let table = TABLE.lock();
        table
            .live
            .iter()
            .filter_map(|(&number, entry)| {
                entry
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .map(|host| (number, host))
            })
            .collect()
    };
    entries
        .into_iter()
        .filter(|(number, host)| host.core().pid() == *number)
        .map(|(_, host)| host)
        .collect()
}
