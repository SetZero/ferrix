//! What a task is.
//!
//! A kernel thread: a stack, a saved stack pointer, and the bookkeeping that
//! says where it is. Everything about *choosing* between tasks is
//! `ferrix_sched`'s; everything here is what the choice is made about.
//!
//! # Where the saved stack pointer lives
//!
//! In an [`UnsafeCell`], written by the CPU switching away from the task and
//! read by the CPU switching to it. Neither is a data race, because both hold
//! the same run queue's lock: the lock is taken before the decision and
//! released by the *incoming* context after the switch, so a task's saved
//! stack pointer is only ever touched by the one CPU that owns its queue at
//! that moment. That hand-over is what `SpinLock::lock_manually` exists for.

use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};

use ferrix_sched::{CpuSet, EntityState};

use crate::user::space::AddressSpace;
use crate::vmap::Stack;

/// A task's name, unique for the life of the machine.
pub(crate) type TaskId = u64;

/// Runnable: on a run queue, or running.
pub(crate) const RUNNABLE: u8 = 0;
/// Blocked: waiting for something, and on no run queue.
pub(crate) const BLOCKED: u8 = 1;
/// Dead: it has returned or exited, and its stack is waiting to be freed.
pub(crate) const DEAD: u8 = 2;

/// One kernel thread.
#[derive(Debug)]
pub(crate) struct Task {
    /// Its name.
    pub(crate) id: TaskId,
    /// What to call it in the boot log.
    pub(crate) name: &'static str,
    /// What it runs. `None` for a context that was already running when the
    /// scheduler adopted it: the boot task, and each CPU's idle task.
    entry: Option<(fn(usize), usize)>,
    /// Its stack, unless it is running on one boot gave it.
    stack: Option<Stack>,
    /// Where its stack pointer is kept while it is not running.
    stack_pointer: UnsafeCell<u64>,
    /// The address space its user half is translated through, or `None` for a
    /// kernel thread.
    ///
    /// Holding an [`Arc`] here is what keeps the tables alive while a
    /// processor is walking them: the root register is installed by
    /// `sched::choose_next` on the way in, and the only thing standing between
    /// those tables and the frame allocator is this reference. A task that
    /// dies keeps it until it is reaped, which is after the last switch away
    /// from it.
    address_space: Option<Arc<AddressSpace>>,
    /// [`RUNNABLE`], [`BLOCKED`] or [`DEAD`].
    state: AtomicU8,
    /// The logical CPU whose queue owns it.
    cpu: AtomicU64,
    /// Whether a run queue holds it, as a queued entity or as the running one.
    queued: AtomicBool,
    /// Whether it may be moved to another CPU.
    /// The processors it may run on.
    affinity: CpuSet,
    /// Its share of a CPU.
    weight: AtomicU32,
    /// The lag it left its last queue with.
    vlag: AtomicI64,
    /// Real nanoseconds it has run for, mirrored out of the queue so that a
    /// reader needs no lock.
    sum_exec: AtomicU64,
    /// What `sum_exec` was when a measurement window opened.
    baseline: AtomicU64,
    /// Whether a measurement window is counting this task. A task that joined
    /// a queue after the window opened is not: it was never owed the service
    /// handed out before it arrived, and counting it would make the fairness
    /// check report a violation that is really an arrival.
    measured: AtomicBool,
    /// When it should wake, or zero if it is not sleeping. Read and cleared
    /// under the owning queue's lock, at the moment the task leaves it.
    sleep_until: AtomicU64,
    /// How many times it has been switched to.
    switches: AtomicU64,
    /// Which CPUs it has run on, one bit each.
    cpus_run_on: AtomicU64,
}

// SAFETY: every field but `stack_pointer` is an atomic or immutable. The cell
// is written by the CPU that switches away from this task and read by the one
// that switches to it, and both hold the lock of the run queue that owns the
// task at that moment — so the accesses are ordered by that lock and never
// overlap.
unsafe impl Sync for Task {}

/// Everything a new task needs, which is more than a function should take as
/// loose arguments: nine of them, four of which are integers, is a call whose
/// meaning depends on getting the order right.
///
/// **No longer `Copy`**, and the reason is the point rather than an
/// inconvenience. It used to be, on the grounds that nothing here owned a
/// resource whose release the type system tracks — a [`Stack`] is freed by
/// `vmap::free_stack` against the address in it rather than by dropping
/// anything. An [`Arc<AddressSpace>`] is exactly such a resource: copying the
/// descriptor would duplicate a reference without raising the count, and the
/// page tables would be freed while a processor still had their root in its
/// register. `Clone` stays, because cloning does raise it.
#[derive(Clone, Debug)]
pub(crate) struct NewTask {
    /// Its identifier, never reused.
    pub(crate) id: TaskId,
    /// What it is called, for the boot log and for diagnostics.
    pub(crate) name: &'static str,
    /// Where it starts, and the one argument it is handed.
    pub(crate) entry: fn(usize),
    /// That argument.
    pub(crate) argument: usize,
    /// The stack it owns, and the pointer into it a switch resumes.
    pub(crate) stack: Stack,
    /// Where in that stack `prepare_stack` left its first frame.
    pub(crate) stack_pointer: u64,
    /// Its scheduling weight.
    pub(crate) weight: u32,
    /// The processor it starts on.
    pub(crate) cpu: usize,
    /// The processors it may run on. A task pinned to one is an affinity of
    /// one, which is the same thing said once rather than twice.
    pub(crate) affinity: CpuSet,
    /// The address space it runs in, or `None` for a kernel thread.
    pub(crate) address_space: Option<Arc<AddressSpace>>,
}

impl Task {
    /// A task that will start at `entry` on a stack of its own.
    pub(crate) fn new(new: NewTask) -> Task {
        let NewTask {
            id,
            name,
            entry,
            argument,
            stack,
            stack_pointer,
            weight,
            cpu,
            affinity,
            address_space,
        } = new;
        Task {
            id,
            name,
            entry: Some((entry, argument)),
            stack: Some(stack),
            stack_pointer: UnsafeCell::new(stack_pointer),
            state: AtomicU8::new(RUNNABLE),
            cpu: AtomicU64::new(cpu as u64),
            queued: AtomicBool::new(false),
            affinity,
            address_space,
            weight: AtomicU32::new(weight),
            vlag: AtomicI64::new(0),
            sum_exec: AtomicU64::new(0),
            baseline: AtomicU64::new(0),
            measured: AtomicBool::new(false),
            sleep_until: AtomicU64::new(0),
            switches: AtomicU64::new(0),
            cpus_run_on: AtomicU64::new(0),
        }
    }

    /// A task for a context that is already running: the boot task, and each
    /// CPU's idle task. Its stack is whoever started it, and its saved stack
    /// pointer is filled in the first time it is switched away from.
    pub(crate) fn adopt(id: TaskId, name: &'static str, weight: u32, cpu: usize) -> Task {
        Task {
            id,
            name,
            entry: None,
            stack: None,
            stack_pointer: UnsafeCell::new(0),
            state: AtomicU8::new(RUNNABLE),
            cpu: AtomicU64::new(cpu as u64),
            queued: AtomicBool::new(false),
            // An adopted context — the boot task, or a processor's idle task —
            // is the one thing that genuinely cannot move: it *is* that
            // processor's context. An affinity of exactly its own processor
            // says so in the same terms as everything else.
            affinity: CpuSet::of(cpu),
            // The boot task and the idle tasks are the kernel's own and have
            // no user half to translate.
            address_space: None,
            weight: AtomicU32::new(weight),
            vlag: AtomicI64::new(0),
            sum_exec: AtomicU64::new(0),
            baseline: AtomicU64::new(0),
            measured: AtomicBool::new(false),
            sleep_until: AtomicU64::new(0),
            switches: AtomicU64::new(0),
            cpus_run_on: AtomicU64::new(0),
        }
    }

    /// What it runs, if it has not started yet.
    pub(crate) const fn entry(&self) -> Option<(fn(usize), usize)> {
        self.entry
    }

    /// Its stack, for the reaper to free.
    pub(crate) const fn stack(&self) -> Option<Stack> {
        self.stack
    }

    /// Where to save its stack pointer.
    ///
    /// # Safety
    ///
    /// The caller must hold the lock of the run queue that owns this task,
    /// and must only pass the pointer to the context switch.
    pub(crate) const unsafe fn stack_pointer_slot(&self) -> *mut u64 {
        self.stack_pointer.get()
    }

    /// The stack pointer it was last saved at.
    ///
    /// # Safety
    ///
    /// The caller must hold the lock of the run queue that owns this task.
    pub(crate) unsafe fn saved_stack_pointer(&self) -> u64 {
        // SAFETY: the caller holds the owning queue's lock, which is what
        // orders this read against the write made by whoever switched away.
        unsafe { *self.stack_pointer.get() }
    }

    /// What it is doing.
    pub(crate) fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }

    /// Say what it is doing.
    pub(crate) fn set_state(&self, state: u8) {
        self.state.store(state, Ordering::Release);
    }

    /// Whether it may be moved to another CPU.
    /// Whether it is confined to a single processor, which is what makes it
    /// ineligible for stealing or balancing.
    pub(crate) fn is_pinned(&self) -> bool {
        self.affinity.len() <= 1
    }

    /// Whether `cpu` is one of the processors it may run on.
    pub(crate) fn may_run_on(&self, cpu: usize) -> bool {
        self.affinity.contains(cpu)
    }

    /// The queue that owns it.
    pub(crate) fn cpu(&self) -> usize {
        self.cpu.load(Ordering::Acquire) as usize
    }

    /// Say which queue owns it. Only under that queue's lock, and under both
    /// when it moves.
    pub(crate) fn set_cpu(&self, cpu: usize) {
        self.cpu.store(cpu as u64, Ordering::Release);
    }

    /// Whether a run queue holds it.
    pub(crate) fn is_queued(&self) -> bool {
        self.queued.load(Ordering::Acquire)
    }

    /// Say whether a run queue holds it. Only under that queue's lock.
    pub(crate) fn set_queued(&self, queued: bool) {
        self.queued.store(queued, Ordering::Release);
    }

    /// What it carries between queues.
    pub(crate) fn entity_state(&self) -> EntityState {
        EntityState {
            weight: self.weight.load(Ordering::Relaxed),
            vlag: self.vlag.load(Ordering::Relaxed),
            sum_exec: self.sum_exec.load(Ordering::Relaxed),
        }
    }

    /// Remember what it left a queue with.
    pub(crate) fn store_entity_state(&self, state: EntityState) {
        self.weight.store(state.weight, Ordering::Relaxed);
        self.vlag.store(state.vlag, Ordering::Relaxed);
        self.sum_exec.store(state.sum_exec, Ordering::Relaxed);
    }

    /// Charge it for time on a CPU.
    pub(crate) fn add_runtime(&self, nanos: u64) {
        let _ = self.sum_exec.fetch_add(nanos, Ordering::Relaxed);
    }

    /// Real nanoseconds it has run for.
    pub(crate) fn runtime(&self) -> u64 {
        self.sum_exec.load(Ordering::Relaxed)
    }

    /// Start a measurement window here, and count this task in it.
    pub(crate) fn open_window(&self) {
        self.baseline
            .store(self.sum_exec.load(Ordering::Relaxed), Ordering::Relaxed);
        self.measured.store(true, Ordering::Release);
    }

    /// Stop counting this task.
    pub(crate) fn close_window(&self) {
        self.measured.store(false, Ordering::Release);
    }

    /// Whether a measurement window is counting it.
    pub(crate) fn is_measured(&self) -> bool {
        self.measured.load(Ordering::Acquire)
    }

    /// What it has run since the window opened, given its total.
    pub(crate) fn since_baseline(&self, total: u64) -> u64 {
        total.saturating_sub(self.baseline.load(Ordering::Relaxed))
    }

    /// Say when it should wake.
    pub(crate) fn set_sleep_deadline(&self, deadline: u64) {
        self.sleep_until.store(deadline, Ordering::Relaxed);
    }

    /// Take its wake-up time, leaving it not sleeping.
    pub(crate) fn take_sleep_deadline(&self) -> Option<u64> {
        match self.sleep_until.swap(0, Ordering::Relaxed) {
            0 => None,
            deadline => Some(deadline),
        }
    }

    /// Note that it is about to run on `cpu`.
    pub(crate) fn note_switch(&self, cpu: usize) {
        let _ = self.switches.fetch_add(1, Ordering::Relaxed);
        if cpu < 64 {
            let _ = self.cpus_run_on.fetch_or(1 << cpu, Ordering::Relaxed);
        }
    }

    /// How many times it has been switched to.
    pub(crate) fn switches(&self) -> u64 {
        self.switches.load(Ordering::Relaxed)
    }

    /// Which CPUs it has run on, one bit each.
    pub(crate) fn cpus_run_on(&self) -> u64 {
        self.cpus_run_on.load(Ordering::Relaxed)
    }

    /// The address space it runs in, or `None` if it is a kernel thread.
    ///
    /// Borrowed rather than cloned, because the caller that matters is the
    /// switch path: it compares this against the outgoing task's by pointer
    /// and installs a root, all under the run queue lock, and raising a
    /// reference count on every switch to say what a borrow already says would
    /// be a contended atomic on the hottest path in the kernel.
    pub(crate) fn address_space(&self) -> Option<&Arc<AddressSpace>> {
        self.address_space.as_ref()
    }
}
