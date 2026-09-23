//! Jobs: containers of processes, and where kill authority lives.
//!
//! `docs/ARCHITECTURE.md` §3 and §7. A userspace driver that wedges has to be
//! killable as a unit, together with anything it started, and a job is that
//! unit: a tree of jobs, each holding processes, where killing one ends every
//! process in it and in every job beneath it.
//!
//! Since stage 13 a job is also a cgroup (`docs/CGROUPS.md`): cgroupfs is a
//! view of this tree. So every process is in exactly one job -- the root
//! job's, [`root`], unless something put it elsewhere -- and a fork's child
//! is in its parent's.
//!
//! # Who keeps whom alive
//!
//! A process holds its job strongly (`Process::job`), a child holds its
//! parent strongly, and a parent holds a *named* child strongly, as a
//! directory holds its entries, until the name is removed. An anonymous child,
//! which native `job_create` makes, is held only by its handles, its members
//! and its children. A job does not hold its processes: it finds them by
//! walking the process registry for the ones whose job it is, or is above.
//!
//! # Populated, counted
//!
//! Whether a job has a member that has not ended is what `cgroup.events`
//! reports and what a service manager waits for, so it is counted rather than
//! found: `live` is how many of its own members have not been released, and
//! `busy` how many of its children are populated. Every change to either is
//! made under [`TREE`], one lock for the whole tree, so that a flip of a child
//! and the matching change to its parent cannot be applied out of order.
//!
//! # Two kills
//!
//! [`Job::kill`] is the native `job_kill`: it ends everything and seals the
//! job, which then refuses new processes and children for good.
//! [`Job::kill_members`] is cgroupfs's `cgroup.kill`: it ends everything and
//! leaves the job usable. While either runs, a fork into the job is ended as
//! soon as it is findable ([`Job::is_dying`]), so a loop of forks cannot
//! outrun it.
//!
//! # Lock order
//!
//! A process's membership lock, then [`TREE`], then one job's `state`. Two
//! jobs' `state` are held at once in one place only,
//! [`Job::remove_named_child`], parent then child; nothing takes a child's
//! and then its parent's, since the count walk takes one at a time going up.
//! The kills take one `state` at a time and let go of it before ending
//! anything, because `process::kill` wakes tasks and takes the scheduler's
//! locks. Nothing is woken under any of these.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sync::SpinLock;

use ferrix_cgroupfs::write::Limit;
use ferrix_native_abi::signals::Signals;
use ferrix_sync::Once;

use super::port::{Observer, PortError, register};
use crate::sched::WaitQueue;
use crate::syscall::process::{self, Process};
use crate::syscall::registry;

/// The status a process ended by a job kill reports.
///
/// 128 plus `SIGKILL`, which is what a shell prints for a killed child and
/// what `process::kill`'s own boot check uses.
pub(crate) const KILLED_STATUS: i32 = 137;

/// Why a job refused something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobError {
    /// The job has been killed, and takes no new processes or children.
    Killed,
    /// A child of that name already exists.
    Exists,
    /// No child has that name.
    Missing,
    /// It still has members or children, so it cannot be removed.
    Busy,
    /// A limit above it (`cgroup.max.depth`, `cgroup.max.descendants`)
    /// allows no further job there.
    Limited,
}

/// Serialises every change to a job's counts, across the whole tree.
///
/// One lock, as Linux's `css_set_lock` is one, because a change propagates
/// upward: two processes leaving two sibling jobs at once must each find the
/// parent's count as the other left it.
static TREE: SpinLock<()> = SpinLock::new(());

/// The next job's number.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// The root of the tree every process is in.
static ROOT: Once<Arc<Job>> = Once::new();

/// The root job: cgroupfs's root, and every process's job until something
/// moves it or it was forked from one elsewhere.
pub(crate) fn root() -> &'static Arc<Job> {
    ROOT.call_once(Job::new_root)
}

/// A job.
#[derive(Debug)]
pub(crate) struct Job {
    /// The job this one is inside, kept alive by it. `None` for a root.
    ///
    /// Holding it is its purpose: a parent outlives every job beneath it, so
    /// a kill of the parent can still reach them, and a count can propagate
    /// to it.
    parent: Option<Arc<Job>>,
    /// Its number, unique for the life of the kernel: what cgroupfs names an
    /// anonymous job by (`job-<id>`) and numbers its inodes from.
    id: u64,
    /// Its name among its parent's children, or `None` for one native
    /// `job_create` made.
    name: Option<Box<str>>,
    /// Its members' counts, its children, and whether it has been killed.
    state: SpinLock<Members>,
    /// Woken when it is killed, for anything waiting on `TERMINATED`.
    waiters: WaitQueue,
    /// Woken whenever it becomes populated or empty.
    events: WaitQueue,
}

/// What a job holds.
#[derive(Debug)]
struct Members {
    /// Set once, by the first [`Job::kill`] reaching this job, and never
    /// cleared.
    killed: bool,
    /// How many [`Job::kill_members`] are running over it.
    killing: u32,
    /// The jobs directly inside this one that have no name.
    children: Vec<Weak<Job>>,
    /// The jobs directly inside this one that have a name, held until the
    /// name is removed.
    named: Vec<Arc<Job>>,
    /// Its own members not yet released. Changed only under [`TREE`].
    live: usize,
    /// Its children that are populated. Changed only under [`TREE`].
    busy: usize,
    /// Port registrations waiting for it to be killed.
    observers: Vec<Observer>,
    /// `cgroup.max.depth`: how many levels of jobs may be made beneath it.
    max_depth: Limit,
    /// `cgroup.max.descendants`: how many jobs may be beneath it at once.
    max_descendants: Limit,
}

impl Default for Members {
    fn default() -> Members {
        Members {
            killed: false,
            killing: 0,
            children: Vec::new(),
            named: Vec::new(),
            live: 0,
            busy: 0,
            observers: Vec::new(),
            max_depth: Limit::Max,
            max_descendants: Limit::Max,
        }
    }
}

impl Members {
    /// Whether it, or anything beneath it, has a member not yet released.
    fn populated(&self) -> bool {
        self.live != 0 || self.busy != 0
    }
}

/// Jobs whose populated state a count change flipped, to be told once every
/// lock is let go ([`notify`]).
pub(crate) type Flipped = Vec<Arc<Job>>;

/// Wake whatever waits on each job a count change flipped.
pub(crate) fn notify(flipped: Flipped) {
    for job in flipped {
        job.events.wake_all();
    }
}

impl Job {
    /// A job with no parent.
    pub(crate) fn new_root() -> Arc<Job> {
        Arc::new(Job::bare(None, None))
    }

    /// A job inside `parent`, or none, not yet listed anywhere.
    fn bare(parent: Option<Arc<Job>>, name: Option<Box<str>>) -> Job {
        Job {
            parent,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            name,
            state: SpinLock::new(Members::default()),
            waiters: WaitQueue::new(),
            events: WaitQueue::new(),
        }
    }

    /// A new anonymous job inside this one.
    ///
    /// # Errors
    ///
    /// [`JobError::Killed`].
    pub(crate) fn new_child(self: &Arc<Job>) -> Result<Arc<Job>, JobError> {
        let mut members = self.state.lock();
        if members.killed {
            return Err(JobError::Killed);
        }
        let child = Arc::new(Job::bare(Some(Arc::clone(self)), None));
        // Pruned as it grows, so a job that makes and drops children in a
        // loop does not keep a list of every one it ever had.
        members.children.retain(|child| child.strong_count() > 0);
        members.children.push(Arc::downgrade(&child));
        Ok(child)
    }

    /// A new job inside this one called `name`, which this one holds until
    /// the name is removed.
    ///
    /// # Errors
    ///
    /// [`JobError::Killed`], [`JobError::Exists`] if a named child already
    /// has that name, or [`JobError::Limited`] if a limit at or above it
    /// allows no further job.
    pub(crate) fn new_named_child(self: &Arc<Job>, name: &str) -> Result<Arc<Job>, JobError> {
        self.room_for_a_child()?;
        let mut members = self.state.lock();
        if members.killed {
            return Err(JobError::Killed);
        }
        if members.named.iter().any(|child| child.name() == Some(name)) {
            return Err(JobError::Exists);
        }
        let child = Arc::new(Job::bare(Some(Arc::clone(self)), Some(Box::from(name))));
        members.named.push(Arc::clone(&child));
        Ok(child)
    }

    /// Its name, if it has one.
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Its number.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Whether it is a root: the tree's, or one a boot check made alone.
    pub(crate) fn is_root(&self) -> bool {
        self.parent.is_none()
    }

    /// The jobs directly inside it that still exist, named ones first in the
    /// order they were made, then anonymous ones in theirs.
    pub(crate) fn children(&self) -> Vec<Arc<Job>> {
        let members = self.state.lock();
        members
            .named
            .iter()
            .cloned()
            .chain(members.children.iter().filter_map(Weak::upgrade))
            .collect()
    }

    /// Take the named child `name` out of it, as `rmdir` does: only when
    /// nothing is in the child, neither a member nor a job.
    ///
    /// The child's lock is taken while this one's is held. That is the one
    /// place two job locks are held at once, and it is safe because nothing
    /// takes a child's lock and then its parent's: the count walk takes one at
    /// a time going up.
    ///
    /// # Errors
    ///
    /// [`JobError::Missing`] when it has no such named child, and
    /// [`JobError::Busy`] when that child is populated or has children.
    pub(crate) fn remove_named_child(&self, name: &str) -> Result<Arc<Job>, JobError> {
        let mut members = self.state.lock();
        let at = members
            .named
            .iter()
            .position(|child| child.name() == Some(name))
            .ok_or(JobError::Missing)?;
        let child = members.named.get(at).cloned().ok_or(JobError::Missing)?;
        let busy = {
            let inner = child.state.lock();
            inner.populated()
                || !inner.named.is_empty()
                || inner.children.iter().any(|child| child.strong_count() > 0)
        };
        if busy {
            return Err(JobError::Busy);
        }
        let _ = members.named.remove(at);
        Ok(child)
    }

    /// How many jobs are beneath it.
    pub(crate) fn descendants(self: &Arc<Job>) -> u32 {
        let count = self.walk(|_| {}).len().saturating_sub(1);
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// Its `cgroup.max.depth` and `cgroup.max.descendants`.
    pub(crate) fn limits(&self) -> (Limit, Limit) {
        let members = self.state.lock();
        (members.max_depth, members.max_descendants)
    }

    /// Set its `cgroup.max.depth`.
    pub(crate) fn set_max_depth(&self, limit: Limit) {
        self.state.lock().max_depth = limit;
    }

    /// Set its `cgroup.max.descendants`.
    pub(crate) fn set_max_descendants(&self, limit: Limit) {
        self.state.lock().max_descendants = limit;
    }

    /// Whether a new child may be made in it, as Linux's
    /// `cgroup_check_hierarchy_limits` decides: no job at or above it may
    /// already hold as many descendants as its `cgroup.max.descendants`
    /// allows, nor be more levels above the new child than its
    /// `cgroup.max.depth` allows.
    fn room_for_a_child(self: &Arc<Job>) -> Result<(), JobError> {
        let mut level: u32 = 1;
        let mut at = Some(Arc::clone(self));
        while let Some(job) = at {
            let (depth, descendants) = job.limits();
            if !descendants.allows(job.descendants().saturating_add(1)) || !depth.allows(level) {
                return Err(JobError::Limited);
            }
            level = level.saturating_add(1);
            at = job.parent.clone();
        }
        Ok(())
    }

    /// The names from the root's child down to it, an anonymous job named
    /// `job-<id>`: what cgroupfs and `/proc/<pid>/cgroup` build its path
    /// from. Empty for a root.
    pub(crate) fn path_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut at = Some(self);
        while let Some(job) = at {
            if job.parent.is_some() {
                names.push(job.display_name());
            }
            at = job.parent.as_deref();
        }
        names.reverse();
        names
    }

    /// The name cgroupfs shows it by in its parent's directory.
    pub(crate) fn display_name(&self) -> String {
        match &self.name {
            Some(name) => String::from(&**name),
            None => format!("job-{}", self.id),
        }
    }

    /// Whether it, or a job beneath it, has a member that has not been
    /// released.
    pub(crate) fn is_populated(&self) -> bool {
        self.state.lock().populated()
    }

    /// How many of its own members have not been released.
    pub(crate) fn live(&self) -> usize {
        self.state.lock().live
    }

    /// The queue woken whenever it becomes populated or empty.
    pub(crate) fn events(&self) -> &WaitQueue {
        &self.events
    }

    /// Whether `job` is this job or beneath it.
    pub(crate) fn contains(&self, job: &Job) -> bool {
        let mut at = Some(job);
        while let Some(current) = at {
            if core::ptr::eq(current, self) {
                return true;
            }
            at = current.parent.as_deref();
        }
        false
    }

    /// Whether a process joining it now would be ended: it or a job above it
    /// has been killed, or is having its members killed.
    pub(crate) fn is_dying(&self) -> bool {
        let mut at = Some(self);
        while let Some(current) = at {
            let members = current.state.lock();
            if members.killed || members.killing != 0 {
                return true;
            }
            drop(members);
            at = current.parent.as_deref();
        }
        false
    }

    /// Count one more member of its own, which a process being made or moved
    /// in is. The jobs whose populated state that flipped go in `flipped`.
    ///
    /// Called with the member's membership lock held, or for a process no
    /// one else can reach yet.
    pub(crate) fn count_in(self: &Arc<Job>, flipped: &mut Flipped) {
        self.count(true, flipped);
    }

    /// Count one member of its own fewer: a process released or moved out.
    pub(crate) fn count_out(self: &Arc<Job>, flipped: &mut Flipped) {
        self.count(false, flipped);
    }

    /// Change `live` by one, and every ancestor's `busy` for as long as the
    /// job below it flipped.
    fn count(self: &Arc<Job>, arriving: bool, flipped: &mut Flipped) {
        let _tree = TREE.lock();
        let mut job = Arc::clone(self);
        let mut own = true;
        loop {
            let changed = {
                let mut members = job.state.lock();
                let before = members.populated();
                let count = if own {
                    &mut members.live
                } else {
                    &mut members.busy
                };
                // Never below zero: a count that would go there is a bug the
                // boot checks look for, not a reason to wrap.
                *count = if arriving {
                    count.saturating_add(1)
                } else {
                    count.saturating_sub(1)
                };
                before != members.populated()
            };
            if !changed {
                break;
            }
            flipped.push(Arc::clone(&job));
            let Some(parent) = job.parent.clone() else {
                break;
            };
            job = parent;
            own = false;
        }
    }

    /// Put `process` in this job, taking it out of the one it is in.
    ///
    /// # Errors
    ///
    /// [`JobError::Killed`] if this job, or one above it, has been killed, in
    /// which case the caller should not start it.
    pub(crate) fn adopt(self: &Arc<Job>, process: &Process) -> Result<(), JobError> {
        process.move_to(self)
    }

    /// Whether it has been killed, and so takes nothing new.
    pub(crate) fn refuses(&self) -> bool {
        let mut at = Some(self);
        while let Some(current) = at {
            if current.state.lock().killed {
                return true;
            }
            at = current.parent.as_deref();
        }
        false
    }

    /// Queue a packet with `observer` when this job is killed, or at once if
    /// it already has been.
    ///
    /// # Errors
    ///
    /// [`PortError::Full`] when the job already holds
    /// [`super::port::MAX_OBSERVERS`] registrations.
    pub(crate) fn observe(&self, observer: Observer) -> Result<(), PortError> {
        let mut members = self.state.lock();
        if members.killed {
            drop(members);
            observer.fire(Signals::TERMINATED);
            return Ok(());
        }
        register(&mut members.observers, observer)
    }

    /// Whether it has been killed.
    pub(crate) fn is_killed(&self) -> bool {
        self.state.lock().killed
    }

    /// The queue woken when it is killed.
    pub(crate) fn waiters(&self) -> &WaitQueue {
        &self.waiters
    }

    /// This job and every job beneath it, parents before children.
    ///
    /// Walked with a list rather than recursion, for the reason
    /// `object::dispose` drops that way: a job tree is as deep as a program
    /// made it. Each job's lock is taken alone, and `visit` runs under it.
    fn walk(self: &Arc<Job>, mut visit: impl FnMut(&mut Members)) -> Vec<Arc<Job>> {
        let mut pending = vec![Arc::clone(self)];
        let mut seen = Vec::new();
        while let Some(job) = pending.pop() {
            {
                let mut members = job.state.lock();
                visit(&mut members);
                pending.extend(members.children.iter().filter_map(Weak::upgrade));
                pending.extend(members.named.iter().cloned());
            }
            seen.push(job);
        }
        seen
    }

    /// End every live process in this job or beneath it with `status`, found
    /// in the process registry. Returns how many had not already ended.
    ///
    /// A process numbered 0 -- made when every pid was in use -- is not in
    /// the registry and is not found. `fork` refuses to make one, and nothing
    /// else can put one in a job but a boot check.
    fn end_members(&self, status: i32) -> usize {
        let mut ended = 0;
        for member in registry::live() {
            if !self.contains(&member.job()) {
                continue;
            }
            if !member.is_terminated() {
                ended += 1;
            }
            process::kill(&member, status);
        }
        ended
    }

    /// End every process in this job and in every job beneath it with
    /// `status`, and refuse anything added to any of them afterwards.
    ///
    /// Every job is marked first, each under its own lock, and the processes
    /// are found afterwards. A fork into one of them is either findable by
    /// then, or finds the mark once it is ([`Job::is_dying`]) and is ended by
    /// the fork itself. Its registrations fire and its waiters wake once its
    /// processes have been told.
    ///
    /// Returns how many of the processes it found had not already ended.
    pub(crate) fn kill(self: &Arc<Job>, status: i32) -> usize {
        let mut observers = Vec::new();
        let jobs = self.walk(|members| {
            members.killed = true;
            observers.append(&mut members.observers);
        });
        for observer in observers {
            observer.fire(Signals::TERMINATED);
        }
        let ended = self.end_members(status);
        for job in jobs {
            job.waiters.wake_all();
        }
        ended
    }

    /// End every process in this job and beneath it, as `cgroup.kill` does,
    /// and leave every job as usable as it was.
    ///
    /// Returns how many of the processes it found had not already ended.
    pub(crate) fn kill_members(self: &Arc<Job>) -> usize {
        let jobs = self.walk(|members| members.killing += 1);
        let ended = self.end_members(KILLED_STATUS);
        for job in jobs {
            let mut members = job.state.lock();
            members.killing = members.killing.saturating_sub(1);
        }
        ended
    }
}

impl Drop for Job {
    /// Let go of the chain of parents in a loop, not by recursion.
    ///
    /// A child holds its parent strongly, so dropping the last reference to
    /// the deepest job of a chain would drop each ancestor inside the drop of
    /// the one below it, a stack frame per job, to whatever depth a program
    /// built. A loop of `job_create` then `handle_close` builds a chain of
    /// hundreds of thousands with one handle open at a time. So each parent
    /// this job was the last holder of is taken apart here: its own parent is
    /// taken out first, and dropping it then has nothing above it to recurse
    /// into.
    fn drop(&mut self) {
        let mut next = self.parent.take();
        while let Some(parent) = next {
            next = match Arc::try_unwrap(parent) {
                Ok(mut owned) => owned.parent.take(),
                // Someone else still holds it, so this reference frees nothing.
                Err(_shared) => None,
            };
        }
    }
}
