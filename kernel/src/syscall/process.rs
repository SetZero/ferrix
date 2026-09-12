//! The per-process state a system call reads or changes.
//!
//! Everything here is state that belongs to a *program*, not to a thread and
//! not to the kernel: where its heap ends, what it has asked to happen on each
//! signal, and the address it wants cleared when it dies. A [`Process`] owns
//! an [`AddressSpace`] and the handlers take `&Process`, so a handler never
//! has to reach for an ambient "current" anything.
//!
//! # Why the handlers take this explicitly
//!
//! Because it is the only way to test them before user mode exists. The boot
//! self-check builds a real `Process` over a real `AddressSpace` and calls the
//! handlers directly, so `mmap` is exercised against the actual VMA tree and
//! the actual page tables on all three architectures — months before a program
//! can call it. A handler that read a global "current process" instead could
//! not be reached at all until the privilege transition landed, and would then
//! be tested for the first time in the same commit as the transition.
//!
//! The one place that *does* need an ambient answer is [`super::dispatch`],
//! which has to find the caller's process from the running task. That is
//! [`current`], one function, which asks the scheduler for the running task and
//! the task for its process.
//!
//! # A process is a task's, not the other way round
//!
//! A program runs as a scheduled task of its own ([`start`]), and the task
//! holds the [`Arc`] that keeps its process alive. The process keeps only weak
//! references back, which is enough to find its tasks when something outside
//! ends it ([`kill`]). Ending is a condition with two sides: [`Process::is_terminated`]
//! for anything that polls, and a wait queue woken once for anything that
//! blocks ([`Process::wait_for_exit`]). `exit_group` from inside and `kill` from
//! outside both reach the same `terminate`, and the first one to get there
//! decides the status.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_sync::SpinLock;
use ferrix_vfs::fd::FdTable;
use ferrix_vfs::{Context, OpenFile};
use ferrix_vma::VmaFlags;

use crate::fs;
use crate::object::{self, HandleTable};
use crate::sched::{self, Task, WaitQueue};
use crate::syscall::fd;
use crate::syscall::registry;
use crate::syscall::signal::Signals;
use crate::user::space::{AddressSpace, SpaceError};

/// A program, as far as the system call layer is concerned.
#[derive(Debug)]
pub(crate) struct Process {
    /// What it can see.
    space: Arc<AddressSpace>,
    /// Its process id, from [`registry::allocate`]: what `getpid` answers,
    /// and what `/proc`, `kill` and `wait4` find it by. Zero only if every
    /// pid was in use when it was made, in which case nothing can find it.
    pid: u32,
    /// The file mode creation mask: the permission bits a new file or
    /// directory is made without. An atomic rather than a field under the
    /// state lock, because `umask` is a swap and nothing reads it together
    /// with anything else.
    umask: AtomicU32,
    /// The handles it holds, for the native ABI.
    ///
    /// A lock of its own rather than a field of `state`: a channel write looks
    /// handles up and takes them out, and has no business waiting on a `brk`
    /// or a signal mask to do it. See `crate::syscall::native`.
    handles: SpinLock<HandleTable>,
    /// Its file descriptors, and the open file descriptions they name.
    ///
    /// A lock of its own for the reason `handles` has one, and one more: a
    /// `read` of the console waits for a person to type, so the table is only
    /// ever held for the lookup, and nothing else should have to wait behind a
    /// lookup either. See `crate::syscall::fd`.
    ///
    /// Behind an `Arc` so that `clone(CLONE_FILES)` can give a second process
    /// the same table rather than a copy of it.
    files: Arc<SpinLock<FdTable<Arc<OpenFile>>>>,
    /// Where its `/` and its working directory are.
    ///
    /// Cloned out by every call that walks a path, rather than held across
    /// the walk: a walk calls into filesystems, and a `chdir` on another thread
    /// has no reason to wait for one. Behind an `Arc` for `clone(CLONE_FS)`,
    /// as `files` is for `CLONE_FILES`.
    fs: Arc<SpinLock<Context>>,
    /// Everything else, behind one lock. One lock per process rather than a
    /// global one, for the same reason the address space has its own: two
    /// processes calling `brk` at once should contend for nothing.
    state: SpinLock<State>,
    /// Where its first task enters user mode. Set once, by `exec::load`.
    startup: SpinLock<Option<Startup>>,
    /// Set by whichever of `exit_group` and `kill` gets there first.
    ending: AtomicBool,
    /// Its exit status, valid once `terminated` is.
    status: AtomicI32,
    /// The terminated condition. Set after `status`, so a reader who sees it
    /// always reads the status that goes with it.
    terminated: AtomicBool,
    /// Woken once, when it terminates.
    exited: WaitQueue,
    /// The tasks running its code. Weak, because a task keeps its process
    /// alive and not the other way round.
    tasks: SpinLock<Vec<Weak<Task>>>,
}

/// Where a program starts: the two numbers `exec::load` computes and the task
/// that runs it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Startup {
    /// Its first instruction.
    pub(crate) entry: u64,
    /// Its initial stack pointer, with the startup image above it.
    pub(crate) stack: u64,
}

/// The parts of a process the lock protects.
#[derive(Debug, Default)]
struct State {
    /// The heap, once something has asked for one.
    heap: Option<Heap>,
    /// The address `set_tid_address` asked to have cleared when this thread
    /// dies, and which a threaded program's `pthread_join` waits on. Zero
    /// means nothing was registered.
    clear_child_tid: u64,
    /// Dispositions, the blocked mask and the alternate stack.
    signals: Signals,
}

/// The classic `brk` heap: one region that grows upward.
#[derive(Debug, Clone, Copy)]
struct Heap {
    /// Where it starts, fixed for the life of the process.
    start: u64,
    /// The program break: the first address past the heap.
    brk: u64,
    /// How much is actually reserved, which is `brk` rounded up to a page.
    mapped_to: u64,
}

impl Process {
    /// A process over an address space, with no heap yet.
    pub(crate) fn new(space: Arc<AddressSpace>) -> Process {
        Process {
            space,
            pid: registry::allocate().unwrap_or(0),
            umask: AtomicU32::new(DEFAULT_UMASK),
            handles: SpinLock::new(HandleTable::new(object::HANDLE_LIMIT)),
            files: Arc::new(SpinLock::new(fd::standard_streams())),
            fs: Arc::new(SpinLock::new(fs::namespace().context())),
            state: SpinLock::new(State::default()),
            startup: SpinLock::new(None),
            ending: AtomicBool::new(false),
            status: AtomicI32::new(0),
            terminated: AtomicBool::new(false),
            exited: WaitQueue::new(),
            tasks: SpinLock::new(Vec::new()),
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

    /// Its descriptor table.
    ///
    /// A lock rather than a closure, unlike [`Process::with_handles`], because
    /// the table's own methods already hand back what they displace. Hold the
    /// guard for a table operation and no longer: clone the description out,
    /// and drop what `remove` or `install` returns after the guard is gone.
    pub(crate) fn files(&self) -> &Arc<SpinLock<FdTable<Arc<OpenFile>>>> {
        &self.files
    }

    /// Its root and working directory. Clone the context out before walking a
    /// path with it.
    pub(crate) fn fs_context(&self) -> &Arc<SpinLock<Context>> {
        &self.fs
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

    /// Record the address to clear when this thread exits, and report the
    /// thread id, which is what `set_tid_address` returns.
    ///
    /// musl uses the *return value* as its process id during startup, so this
    /// must answer with a real identifier. It is one of the few calls where a
    /// plausible-looking stub is worse than an error: an `ENOSYS` musl
    /// survives, a wrong pid it does not.
    pub(crate) fn set_clear_child_tid(&self, address: u64, tid: usize) -> usize {
        self.state.lock().clear_child_tid = address;
        tid
    }

    /// The address `set_tid_address` registered, or zero.
    pub(crate) fn clear_child_tid(&self) -> u64 {
        self.state.lock().clear_child_tid
    }

    /// Read or change the signal state, under the process lock.
    ///
    /// A closure rather than a guard, so that nothing a handler does with user
    /// memory can happen while the lock is held: every copy to or from the
    /// program is outside it.
    pub(crate) fn with_signals<R>(&self, change: impl FnOnce(&mut Signals) -> R) -> R {
        change(&mut self.state.lock().signals)
    }

    /// Put the heap just past the loaded image, before anything asks for it.
    ///
    /// Without this, the first `brk` places the heap above the highest thing
    /// mapped -- and the highest thing mapped is the stack, at the very top of
    /// the user half, so the heap would begin at `USER_VIRT_END` and every
    /// attempt to grow it would be refused. glibc survives that by falling back
    /// to `mmap` for everything, which is how the first busybox run showed it:
    /// `brk(0)` answering `0x800000000000`.
    ///
    /// A page of gap is left after the image, so that a heap overrun backwards
    /// faults rather than writing over the last page of `.bss`.
    pub(crate) fn set_heap_base(&self, image_end: u64) {
        let Some(start) = round_up(image_end).and_then(|end| end.checked_add(PAGE_SIZE)) else {
            return;
        };
        let mut state = self.state.lock();
        if state.heap.is_none() {
            state.heap = Some(Heap {
                start,
                brk: start,
                mapped_to: start,
            });
        }
    }

    /// Move the program break, and report where it now is.
    ///
    /// # The convention, which is not an error convention
    ///
    /// `brk` does not report failure. It returns the break, and the caller
    /// compares it with what it asked for: unchanged means refused. That is
    /// why this returns a bare `u64` and why a request that cannot be met
    /// returns the *current* break rather than an error — a libc that got
    /// `-ENOMEM` here would read it as an enormous valid break and walk off
    /// the end of its heap.
    ///
    /// `brk(0)` is the query every libc opens with.
    pub(crate) fn set_break(&self, want: u64) -> u64 {
        let mut state = self.state.lock();

        let heap = match state.heap {
            Some(heap) => heap,
            None => {
                // First call. Place the heap above everything the ELF loader
                // mapped, so the two never have to agree on a number, with a
                // page of gap so a heap overrun cannot walk straight into the
                // last data page.
                let after = self.space.highest_mapped().unwrap_or(0);
                let start = after.saturating_add(PAGE_SIZE);
                let heap = Heap {
                    start,
                    brk: start,
                    mapped_to: start,
                };
                state.heap = Some(heap);
                heap
            }
        };

        // A query, or a request below the start: report where we are.
        if want == 0 || want < heap.start {
            return heap.brk;
        }

        let page_end = round_up(want);
        let Some(page_end) = page_end else {
            return heap.brk;
        };

        if page_end > heap.mapped_to {
            // Growing. Reserve the new pages; they cost nothing until touched.
            let len = page_end - heap.mapped_to;
            if self
                .space
                .map_anonymous(heap.mapped_to, len, VmaFlags::READ_WRITE)
                .is_err()
            {
                return heap.brk;
            }
        } else if page_end < heap.mapped_to {
            // Shrinking. Give the pages back now rather than at exit: a
            // program that frees half its heap expects the memory returned.
            let len = heap.mapped_to - page_end;
            if self.space.unmap(page_end, len).is_err() {
                return heap.brk;
            }
        }

        let heap = Heap {
            start: heap.start,
            brk: want,
            mapped_to: page_end,
        };
        state.heap = Some(heap);
        heap.brk
    }
}

/// The mask a process starts with: group and others may not write.
///
/// Linux's own default for `init`, and what nearly every login leaves in
/// place, so a file a program creates before anything has called `umask` gets
/// the mode it would get on Linux.
const DEFAULT_UMASK: u32 = 0o022;

impl Process {
    /// The permission bits new files and directories are made without.
    pub(crate) fn umask(&self) -> u32 {
        self.umask.load(Ordering::Relaxed)
    }

    /// `umask`: replace the mask, keeping only permission bits, and report
    /// the old one.
    pub(crate) fn set_umask(&self, mask: u32) -> u32 {
        self.umask.swap(mask & 0o777, Ordering::Relaxed)
    }
}

/// Round up to a page, or `None` if that would leave the address space.
fn round_up(at: u64) -> Option<u64> {
    at.checked_add(PAGE_SIZE - 1)
        .map(|at| at & !(PAGE_SIZE - 1))
}

impl Process {
    /// Record where its first task enters user mode.
    pub(crate) fn set_startup(&self, startup: Startup) {
        *self.startup.lock() = Some(startup);
    }

    /// Where its first task enters user mode, once a program is loaded.
    pub(crate) fn startup(&self) -> Option<Startup> {
        *self.startup.lock()
    }

    /// Whether it has terminated, by exiting or by being killed.
    pub(crate) fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
    }

    /// How it ended, once it has.
    pub(crate) fn exit_status(&self) -> Option<i32> {
        self.is_terminated()
            .then(|| self.status.load(Ordering::Acquire))
    }

    /// The queue woken, once, when it terminates -- for a waiter that has its
    /// own condition to check alongside [`Process::is_terminated`].
    pub(crate) fn exited(&self) -> &WaitQueue {
        &self.exited
    }

    /// Block until it terminates or `deadline` passes, and report how it ended
    /// if it has.
    pub(crate) fn wait_for_exit(&self, deadline: u64) -> Option<i32> {
        let _ = self
            .exited()
            .wait_until_deadline(|| self.is_terminated(), deadline);
        self.exit_status()
    }

    /// End it with `status`, unless something already has. Answers whether
    /// this call was the one that did.
    ///
    /// Everything the process held beyond its address space is released here,
    /// at the moment it ends, rather than when its last reference goes: a
    /// killed process whose task has not yet noticed must not keep its
    /// resources until it does. The address space goes when the last task
    /// holding it is reaped, because a processor may still be translating
    /// through it until then. The waiters are woken last, when there is nothing
    /// left to observe half done.
    fn terminate(&self, status: i32) -> bool {
        if self.ending.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.status.store(status, Ordering::Release);
        self.terminated.store(true, Ordering::Release);
        *self.state.lock() = State::default();
        // The handles too, and outside every lock: an object's drop can free
        // memory and drain other objects, which is `object::dispose`'s job,
        // and must not run under this process's table lock or state lock.
        object::dispose(self.with_handles(HandleTable::clear));
        self.exited.wake_all();
        true
    }
}

impl Drop for Process {
    /// Give the pid back. The number is not used again until allocation comes
    /// round to it; see [`registry`].
    fn drop(&mut self) {
        if self.pid != 0 {
            registry::release(self.pid);
        }
    }
}

/// The process the running task belongs to.
///
/// `None` for a kernel thread.
pub(crate) fn current() -> Option<Arc<Process>> {
    sched::current().and_then(|task| task.process().cloned())
}

/// Load a program into a new process, without running it.
pub(crate) use super::exec::load;

/// Run `process`'s program as a task of its own.
///
/// Separate from [`load`] so that something can be put into the process
/// between the two -- a handle, say -- before its first instruction runs.
///
/// # Errors
///
/// If no program was loaded into it, or the scheduler has no stack for its
/// task.
pub(crate) fn start(process: &Arc<Process>) -> Result<Arc<Task>, &'static str> {
    start_on(process, None)
}

/// [`start`], pinned to processor `cpu` when that is `Some`.
///
/// # Errors
///
/// As [`start`].
pub(crate) fn start_on(
    process: &Arc<Process>,
    cpu: Option<usize>,
) -> Result<Arc<Task>, &'static str> {
    if process.startup().is_none() {
        return Err("the process has no program loaded");
    }
    let task = sched::spawn_user("user", run_program, Arc::clone(process), cpu)?;
    process.tasks.lock().push(Arc::downgrade(&task));
    Ok(task)
}

/// End `process` from outside, with `status`.
///
/// Its tasks find out on their way back to user mode: one running there does
/// on its next tick, one blocked in a call is woken to, and one that has not
/// yet entered user mode never does. Nothing here waits for that; a caller
/// that needs the tasks gone waits for them.
pub(crate) fn kill(process: &Process, status: i32) {
    if !process.terminate(status) {
        return;
    }
    let tasks: Vec<Arc<Task>> = process
        .tasks
        .lock()
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    for task in &tasks {
        sched::wake(task);
    }
}

/// End the running task's process with `status`, and the task with it.
///
/// What `exit_group` does. The process reference is dropped before the task
/// ends, because nothing after `sched::exit` runs to drop it.
pub(crate) fn exit_current(status: i32) -> ! {
    if let Some(process) = current() {
        let _ = process.terminate(status);
    }
    sched::exit()
}

/// End the running task if its process has been ended from outside.
///
/// Called on every way back to user mode, with interrupts masked.
pub(crate) fn before_return_to_user() {
    let terminated = current().is_some_and(|process| process.is_terminated());
    if terminated {
        sched::exit();
    }
}

/// Where a program's task begins: enter user mode where `exec::load` said.
///
/// Returning ends the task, which is what happens if the process was killed
/// before it ever ran.
fn run_program(_argument: usize) {
    let startup = current()
        .filter(|process| !process.is_terminated())
        .and_then(|process| process.startup());
    let Some(Startup { entry, stack }) = startup else {
        return;
    };
    // SAFETY: this task was spawned in the process's address space, which the
    // scheduler installed when it switched here, along with the task's user
    // state and entry stack; `entry` and `stack` came from the loader and the
    // stack builder, both inside that space. Nothing owned is left on this
    // frame to leak: the process reference was dropped above.
    unsafe { crate::arch::enter_user(entry, stack) }
}

/// Make a process over a fresh address space, for the self-checks.
///
/// # Errors
///
/// Whatever [`AddressSpace::new`] refuses.
pub(crate) fn new_for_check() -> Result<Arc<Process>, SpaceError> {
    Ok(registry::register(Process::new(AddressSpace::new()?)))
}
