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
//! ends it ([`kill`]). Ending has two moments. The request, after which
//! [`Process::is_terminated`] is true and its threads leave; and the release,
//! once the last of them has, after which [`Process::is_released`] is true and
//! [`Process::wait_for_exit`] returns. `exit_group`, a last thread's `exit` and
//! `kill` all make the same request, and the first one to get there decides the
//! status.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use crate::sync::SpinLock;
use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::signals::Signals as ObjectSignals;
use ferrix_vfs::fd::FdTable;
use ferrix_vfs::{Context, OpenFile};
use ferrix_vma::VmaFlags;

use crate::fs;
use crate::object::port::{self, Observer, PortError};
use crate::object::{self, HandleTable};
use crate::sched::{self, Task, WaitQueue};
use crate::syscall::credentials::Credentials;
use crate::syscall::fd;
use crate::syscall::registry;
use crate::syscall::signal::{Origin, Posted, Signals};
use crate::syscall::thread::{self, Thread};
use crate::syscall::{attributes, futex, kill, uaccess};
use crate::user::space::{AddressSpace, MMAP_MIN_ADDR, SpaceError};
use ferrix_linux_abi::types::SIGCHLD;

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
    /// When it was made, in nanoseconds on the counter: `stat`'s start time.
    started: u64,
    /// What it was started as, which only `/proc` reads.
    identity: SpinLock<Identity>,
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
    /// Set by the first start, so a process runs one program's task and a
    /// second start is refused rather than running a second task in it.
    start_claimed: AtomicBool,
    /// Set by whichever of `exit_group` and `kill` gets there first.
    ending: AtomicBool,
    /// Its threads that have started and not yet ended: raised before a
    /// thread's task is spawned, lowered as the task ends. When it reaches
    /// zero on a process that is ending, the process lets go of what it holds.
    live_threads: AtomicU32,
    /// Set by the one [`Process::release`] that runs, as it starts.
    released: AtomicBool,
    /// Set as that release finishes, after its orphans have gone on and before
    /// its parent is told: what `wait4` reaps by.
    release_finished: AtomicBool,
    /// The status its first thread left with through `exit`, which is the
    /// process's status when its last thread ends the same way.
    leader_status: AtomicI32,
    /// How it ended, and who is waiting to hear. Apart from the process,
    /// because a handle to the process holds it: see [`Exit`].
    exit: Arc<Exit>,
    /// The tasks running its code. Weak, because a task keeps its process
    /// alive and not the other way round.
    tasks: SpinLock<Vec<Weak<Task>>>,
    /// Its threads, each listed before it can run -- and a fork child's before
    /// the child can be found -- so that a signal sent to the process is
    /// judged against the mask of the thread that will take it. Weak, as
    /// `tasks` is.
    threads: SpinLock<Vec<Weak<Thread>>>,
    /// Registers its first task resumes from instead of entering at the
    /// program's start: set for a fork child, taken once.
    resume: SpinLock<Option<crate::arch::UserRegs>>,
    /// The process that created it, or the one it was handed to when that one
    /// ended, if that process still exists. Weak, because a parent keeps its
    /// children (until it waits for them) and not the other way round.
    parent: SpinLock<Weak<Process>>,
    /// Its process group, which job control and `kill(0, …)` address.
    pgid: AtomicU32,
    /// Its session.
    sid: AtomicU32,
    /// The children it has not yet waited for, ended or not. Strong, so an
    /// ended child stays findable -- a zombie -- until `wait4` takes it.
    children: SpinLock<Vec<Arc<Process>>>,
    /// Woken whenever one of its children ends.
    child_exited: WaitQueue,
    /// The signal its parent is told with when it ends; `SIGCHLD` for an
    /// ordinary fork, whatever `clone` asked for otherwise.
    exit_signal: AtomicU32,
    /// Set by a successful `execve`: what a `vfork` parent waits for, besides
    /// the child ending.
    execed: AtomicBool,
    /// Woken when `execed` is set or it ends.
    vfork_done: WaitQueue,
    /// Woken when a signal is sent to it: what `pause`, `rt_sigsuspend` and
    /// `rt_sigtimedwait` wait on.
    signalled: WaitQueue,
    /// The signal that stopped it, or zero while it runs.
    stopped: AtomicU32,
    /// A stop its parent has not yet been told of by `wait4`, or zero.
    stop_report: AtomicU32,
    /// Whether it continued since its parent was last told.
    continue_report: AtomicBool,
    /// Woken when it continues, or ends, which is what a stopped task waits for.
    resumed: WaitQueue,
    /// Its user and group ids and supplementary groups. A lock of its own:
    /// `getuid` has no business waiting on a `brk`, and a `set*id` call must
    /// see and change every id it names at once.
    credentials: SpinLock<Credentials>,
}

/// Where a program starts: the two numbers `exec::load` computes and the task
/// that runs it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Startup {
    /// Its first instruction.
    pub(crate) entry: u64,
    /// Its initial stack pointer, with the startup image above it.
    pub(crate) stack: u64,
    /// What its first argument register holds on entry: zero for a Linux
    /// program, whose startup image is on its stack, and a native process's
    /// bootstrap handle.
    pub(crate) argument: u64,
}

/// The parts of a process the lock protects.
#[derive(Debug, Default, Clone)]
struct State {
    /// The heap, once something has asked for one.
    heap: Option<Heap>,
    /// Dispositions, the blocked mask and the alternate stack.
    signals: Signals,
}

/// What a process was started as.
///
/// A lock of its own, like the handle table: `/proc/<pid>/cmdline` read from
/// another process has no business waiting on this one's `brk`.
#[derive(Debug, Default)]
struct Identity {
    /// The path it was started from: `/proc/<pid>/exe`.
    exe: Vec<u8>,
    /// Its argument vector: `/proc/<pid>/cmdline`.
    args: Vec<Vec<u8>>,
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
        Process::with_pid(space, registry::allocate().unwrap_or(0))
    }

    /// [`Process::new`], for the process init starts: pid 1 when no other
    /// process holds it, any other pid when one does.
    pub(crate) fn new_init(space: Arc<AddressSpace>) -> Process {
        let pid = registry::allocate_init()
            .or_else(registry::allocate)
            .unwrap_or(0);
        Process::with_pid(space, pid)
    }

    /// A process over an address space, numbered `pid`, which the caller has
    /// reserved in the registry.
    fn with_pid(space: Arc<AddressSpace>, pid: u32) -> Process {
        Process {
            space,
            pid,
            umask: AtomicU32::new(DEFAULT_UMASK),
            started: crate::syscall::time::now_nanos(),
            identity: SpinLock::new(Identity::default()),
            handles: SpinLock::new(HandleTable::new(object::HANDLE_LIMIT)),
            files: Arc::new(SpinLock::new(fd::standard_streams())),
            fs: Arc::new(SpinLock::new(fs::namespace().context())),
            state: SpinLock::new(State::default()),
            startup: SpinLock::new(None),
            start_claimed: AtomicBool::new(false),
            ending: AtomicBool::new(false),
            live_threads: AtomicU32::new(0),
            released: AtomicBool::new(false),
            release_finished: AtomicBool::new(false),
            leader_status: AtomicI32::new(0),
            exit: Arc::new(Exit::new()),
            tasks: SpinLock::new(Vec::new()),
            threads: SpinLock::new(Vec::new()),
            resume: SpinLock::new(None),
            parent: SpinLock::new(Weak::new()),
            // A process the kernel starts leads its own group and session.
            // A fork child inherits its parent's instead, below.
            pgid: AtomicU32::new(pid),
            sid: AtomicU32::new(pid),
            children: SpinLock::new(Vec::new()),
            child_exited: WaitQueue::new(),
            exit_signal: AtomicU32::new(SIGCHLD),
            execed: AtomicBool::new(false),
            vfork_done: WaitQueue::new(),
            signalled: WaitQueue::new(),
            stopped: AtomicU32::new(0),
            stop_report: AtomicU32::new(0),
            continue_report: AtomicBool::new(false),
            resumed: WaitQueue::new(),
            // A process the kernel starts is root's. A fork child takes its
            // parent's instead, below.
            credentials: SpinLock::new(Credentials::root()),
        }
    }

    /// A copy of `parent` over `space`, which is already a copy of its
    /// address space: what `fork` makes.
    ///
    /// What is copied is what Linux copies: the file descriptor table and the
    /// working directory and root (or the same ones, shared, when `clone` asks
    /// for `CLONE_FILES` or `CLONE_FS`), the heap and the signal dispositions,
    /// the process group and session, the umask, the user and group ids and
    /// supplementary groups, and the program's start. What is not is
    /// what belongs to the parent alone: its pid, its children, its threads,
    /// and its handles, which the native ABI passes on only explicitly.
    pub(crate) fn forked(
        parent: &Arc<Process>,
        space: Arc<AddressSpace>,
        share_files: bool,
        share_fs: bool,
    ) -> Process {
        let mut child = Process::new(space);
        child.files = if share_files {
            Arc::clone(&parent.files)
        } else {
            Arc::new(SpinLock::new(parent.files.lock().clone()))
        };
        child.fs = if share_fs {
            Arc::clone(&parent.fs)
        } else {
            Arc::new(SpinLock::new(parent.fs.lock().clone()))
        };
        let mut state = parent.state.lock().clone();
        state.signals.reset_for_fork();
        child.state = SpinLock::new(state);
        child.startup = SpinLock::new(parent.startup());
        child.parent = SpinLock::new(Arc::downgrade(parent));
        child.pgid = AtomicU32::new(parent.pgid());
        child.sid = AtomicU32::new(parent.sid());
        child.umask = AtomicU32::new(parent.umask());
        child.credentials = SpinLock::new(parent.credentials.lock().clone());
        child
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

    /// When it was made, in nanoseconds on the counter.
    pub(crate) fn started(&self) -> u64 {
        self.started
    }

    /// Record what it was started as: the path and the arguments.
    pub(crate) fn record_exec(&self, exe: &[u8], args: &[&[u8]]) {
        let exe = exe.to_vec();
        let args = args.iter().map(|arg| arg.to_vec()).collect();
        let mut identity = self.identity.lock();
        identity.exe = exe;
        identity.args = args;
    }

    /// The path it was started from, empty if nothing was.
    pub(crate) fn exe(&self) -> Vec<u8> {
        self.identity.lock().exe.clone()
    }

    /// Its argument vector.
    pub(crate) fn args(&self) -> Vec<Vec<u8>> {
        self.identity.lock().args.clone()
    }

    /// The command name: the last component of the path it was started from,
    /// cut to the fifteen bytes Linux's `TASK_COMM_LEN` leaves room for.
    pub(crate) fn comm(&self) -> Vec<u8> {
        let identity = self.identity.lock();
        let base = identity
            .exe
            .rsplit(|&byte| byte == b'/')
            .next()
            .unwrap_or_default();
        base.iter().copied().take(15).collect()
    }

    /// The heap's start and the end of what is reserved for it, once
    /// something has placed it.
    pub(crate) fn heap_range(&self) -> Option<(u64, u64)> {
        self.state
            .lock()
            .heap
            .map(|heap| (heap.start, heap.mapped_to))
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

    /// Read or change the signal state, under the process lock.
    ///
    /// A closure rather than a guard, so that nothing a handler does with user
    /// memory can happen while the lock is held: every copy to or from the
    /// program is outside it.
    pub(crate) fn with_signals<R>(&self, change: impl FnOnce(&mut Signals) -> R) -> R {
        change(&mut self.state.lock().signals)
    }

    /// Forget what the old program set up, as `execve` does before loading the
    /// new one: its heap and its signal handlers. The address space is emptied
    /// by the caller, which also forgets the address its thread asked to have
    /// cleared.
    pub(crate) fn reset_for_exec(&self) {
        let mut state = self.state.lock();
        state.heap = None;
        state.signals.reset_for_exec();
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
                // last data page. An empty space starts from the lowest address
                // anything may be mapped at, not from zero.
                let after = self.space.highest_mapped().unwrap_or(MMAP_MIN_ADDR);
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
        }

        let old_mapped_to = heap.mapped_to;
        let heap = Heap {
            start: heap.start,
            brk: want,
            mapped_to: page_end,
        };
        state.heap = Some(heap);
        drop(state);

        if page_end < old_mapped_to {
            // Shrinking. Give the pages back now rather than at exit: a
            // program that frees half its heap expects the memory returned.
            //
            // **After the state is written and its lock gone.** An unmap
            // waits for every other processor to drop its translations, and
            // `state` is the lock a signal or a `kill` from another processor
            // spins on; a shootdown may not be asked for under a lock that
            // disables preemption, and `smp` checks that it is not. The
            // range is this heap's own, page-aligned and non-empty, so the
            // unmap cannot be refused, and nothing else names it: a second
            // caller that grows the heap meanwhile finds the pages still
            // mapped and is refused, and one that shrinks it further takes
            // a range below this one. Unreachable until threads share a
            // process; then one case is worth knowing: a fork by another
            // thread inside this window clones the still-mapped tail into a
            // child whose heap already says it ends here, so that child's
            // heap can never grow over the tail and the tail lives until it
            // exits. Harmless, and a `brk` lock that may sleep would close
            // it.
            let _ = self.space.unmap(page_end, old_mapped_to - page_end);
        }
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

    /// Its user and group ids and supplementary groups, under their lock:
    /// `change` sees all of them at once, and what it changes changes
    /// together. Nothing that waits may be done inside it.
    pub(crate) fn with_credentials<R>(&self, change: impl FnOnce(&mut Credentials) -> R) -> R {
        change(&mut self.credentials.lock())
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

    /// Have its first task resume from `regs` rather than enter the program:
    /// a fork child, which carries on from its parent's system call.
    pub(crate) fn set_resume(&self, regs: crate::arch::UserRegs) {
        *self.resume.lock() = Some(regs);
    }

    /// The registers to resume from, once.
    fn take_resume(&self) -> Option<crate::arch::UserRegs> {
        self.resume.lock().take()
    }

    /// Whether it has terminated, by exiting or by being killed.
    pub(crate) fn is_terminated(&self) -> bool {
        self.exit.is_terminated()
    }

    /// How it ended, once it has.
    pub(crate) fn exit_status(&self) -> Option<i32> {
        self.is_terminated()
            .then(|| self.exit.status.load(Ordering::Acquire))
    }

    /// The queue woken, once, when it has ended and let go of what it held --
    /// for a waiter that has its own condition to check alongside
    /// [`Process::is_released`].
    pub(crate) fn exited(&self) -> &WaitQueue {
        self.exit.exited()
    }

    /// Block until it has ended and let go of what it held, or `deadline`
    /// passes, and report how it ended if it has.
    ///
    /// Never for a process of the caller's own: its release waits for the
    /// caller's thread to leave, which a caller waiting here never does.
    pub(crate) fn wait_for_exit(&self, deadline: u64) -> Option<i32> {
        let _ = self
            .exited()
            .wait_until_deadline(|| self.is_released(), deadline);
        if self.is_released() {
            self.exit_status()
        } else {
            None
        }
    }

    /// Whether it has ended and let go of its handles and descriptors: its
    /// last thread gone, or none ever started. What `wait4` reaps by, so that
    /// no process is reaped while a thread of it is still in the kernel.
    pub(crate) fn is_released(&self) -> bool {
        self.release_finished.load(Ordering::Acquire)
    }

    /// Count a thread about to start. Before its task is spawned, because the
    /// task can reach its exit on another processor before the spawn returns.
    pub(crate) fn thread_starting(&self) {
        let _ = self.live_threads.fetch_add(1, Ordering::AcqRel);
    }

    /// Count a thread gone -- one that `ended`, or one whose task could not be
    /// spawned -- and, if that was its last thread, end the process or let go
    /// of what it holds.
    ///
    /// The count reaching zero is one step, so this is where a last thread's
    /// `exit` ends the process: two last threads leaving at once cannot each
    /// see the other and both leave it running. With the process already
    /// ending, this releases it; either this sees the ending, or the ending
    /// sees no thread live, since each reads what the other writes after
    /// writing its own. A thread that never started ends nothing, so a start
    /// that failed leaves the process to be started again.
    pub(crate) fn thread_gone(&self, ended: bool) {
        let before = self
            .live_threads
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                live.checked_sub(1)
            });
        if before != Ok(1) {
            return;
        }
        if self.is_terminated() {
            self.release();
        } else if ended {
            // With the status its first thread left with. `end` finds no
            // thread live and releases.
            let _ = self.end(self.leader_status.load(Ordering::Acquire), 0);
        }
    }

    /// End it with `status`, unless something already has. Answers whether
    /// this call was the one that did.
    fn terminate(&self, status: i32) -> bool {
        self.end(status, 0)
    }

    /// End it with `status`, recording `signal` as what ended it when that is
    /// not zero. The one path `exit_group`, `kill` and a last thread's `exit`
    /// take.
    ///
    /// Ending has two moments, as on Linux. This is the first: the status is
    /// recorded, [`Process::is_terminated`] turns true, and whatever its threads
    /// wait in is woken so that they leave. The second, [`Process::release`],
    /// lets go of what it holds once its last thread has gone -- here and now
    /// if none is live: a process never started, or one whose threads have all
    /// ended. Native process creation's `Control` relies on that when it drops
    /// an unstarted child, and the orphan checks when they end processes that
    /// never ran.
    ///
    /// A thread still in the kernel therefore keeps its process's descriptors
    /// and orphans until it reaches its exit, which every wait it can be in
    /// allows: each ends once its process is terminated or a signal is
    /// pending. A wait that did not would hold the release back for as long as
    /// it lasted.
    fn end(&self, status: i32, signal: u32) -> bool {
        if self.ending.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.exit.record(status, signal);
        // A `vfork` parent waits for this, and a thread in `pause` or stopped
        // waits for a signal or a continue; none should wait for the release.
        self.vfork_done.wake_all();
        self.signalled.wake_all();
        self.resumed.wake_all();
        // Its tasks find out on their way back to user mode: one blocked in a
        // call is woken to, and one running in user mode is interrupted to.
        // Waiting for a tick is not enough, because a task alone on its
        // processor gets none -- the scheduler leaves a lone task to run -- and
        // a program spinning there would outlive its end until it chose to make
        // a call. The caller's own task, if it is one, is already on its way.
        let current = sched::current();
        let tasks: Vec<Arc<Task>> = self.tasks.lock().iter().filter_map(Weak::upgrade).collect();
        for task in &tasks {
            if current.as_ref().is_some_and(|me| Arc::ptr_eq(me, task)) {
                continue;
            }
            sched::wake(task);
            sched::interrupt(task);
        }
        drop(tasks);
        if self.live_threads.load(Ordering::Acquire) == 0 {
            self.release();
        }
        true
    }

    /// Let go of everything it holds beyond its address space, once, after
    /// [`Process::end`] and when no thread of it is live.
    ///
    /// The address space goes when the last task holding it is reaped, because
    /// a processor may still be translating through it until then. The waiters
    /// are woken last, when there is nothing left to observe half done. Runs
    /// with interrupts open: closing handles and firing watchers take plain
    /// locks.
    fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        // The heap record and the signal tables go now. Taken under the lock
        // and dropped after it.
        let state = core::mem::take(&mut *self.state.lock());
        drop(state);
        // The handles too, and outside every lock: an object's drop can free
        // memory and drain other objects, which is `object::dispose`'s job,
        // and must not run under this process's table lock or state lock.
        object::dispose(self.with_handles(HandleTable::close));
        // Its descriptors close now, as Linux's exit closes them, rather than
        // when the last reference to the process goes -- which a parent that
        // has not reaped it yet still holds. Otherwise a pipe's write end
        // outlives the program that wrote, and `ls | wc -l` hangs: `wc` waits
        // for an end of file that only comes when the shell reaps `ls`, and the
        // shell reaps nothing until `wc` ends. Only when no other process
        // shares the table (`CLONE_FILES`), whose descriptors are still its
        // own. Taken out under the lock and dropped after it, because closing
        // a pipe end wakes its queues.
        if Arc::strong_count(&self.files) == 1 {
            let closed: Vec<Arc<OpenFile>> = {
                let mut files = self.files.lock();
                let open: Vec<i32> = files.iter().map(|(fd, _)| fd).collect();
                open.into_iter()
                    .filter_map(|fd| files.remove(fd).ok())
                    .collect()
            };
            for file in &closed {
                fd::closed(self, file);
            }
            drop(closed);
        }
        // Whoever watches it through a port hears now, once its handles and
        // descriptors are closed: a driver's pins are given back or kept
        // before `devmgr` learns that the driver has gone. Taken before the
        // queue is woken, so a waiter it wakes sees the handle's `TERMINATED`.
        let observers = self.exit.close();
        self.exit.exited.wake_all();
        self.vfork_done.wake_all();
        self.signalled.wake_all();
        self.resumed.wake_all();
        for observer in observers {
            observer.fire(ObjectSignals::TERMINATED);
        }

        // Its own children are orphaned, and go where Linux's
        // `forget_original_parent` sends them: to the nearest ancestor still
        // running that asked to reap orphaned descendants, or else to init.
        // Each is sent the signal it asked for on its parent's death.
        //
        // An orphan is put in its new parent's list before its parent is
        // changed, so one ending at this moment disowns itself from a list it
        // is already in. One that ended before the change told this process,
        // which has ended and hears nothing, so its new parent is told here;
        // if the orphan ends in between, the new parent is told twice, and
        // `SIGCHLD` does not queue.
        //
        // With nobody to take them -- the boot checks run before init, and init
        // itself may end -- they are released as before: an ended one here, a
        // running one when it ends.
        let orphans = core::mem::take(&mut *self.children.lock());
        let reaper = self.reaper_for_orphans();
        for orphan in orphans {
            let death_signal = attributes::get(&orphan).parent_death_signal;
            let Some(reaper) = &reaper else {
                *orphan.parent.lock() = Weak::new();
                kill::send(&orphan, death_signal, Origin::Kernel);
                continue;
            };
            // Linux's reason, verbatim: "We don't want people slaying init."
            // An orphan made with another exit signal tells its new parent with
            // `SIGCHLD`, which the new parent expects.
            orphan.exit_signal.store(SIGCHLD, Ordering::Release);
            reaper.adopt(Arc::clone(&orphan));
            *orphan.parent.lock() = Arc::downgrade(reaper);
            kill::send(&orphan, death_signal, Origin::Kernel);
            if orphan.is_released() {
                if reaper.with_signals(|signals| signals.reaps_children_automatically()) {
                    reaper.disown(&orphan);
                }
                let (code, told) = orphan.end_report();
                kill::tell_parent(&orphan, SIGCHLD, code, told);
            }
        }

        // A parent that asked never to wait lets it go before it can be seen
        // released, so that parent's `wait4` never reaps it.
        let parent = self.parent.lock().upgrade();
        if let Some(parent) = parent
            && parent.with_signals(|signals| signals.reaps_children_automatically())
        {
            parent.disown(self);
        }
        // Released: what `wait4` reaps by and `wait_for_exit` waits for. After
        // its orphans have gone on, so a parent that reaps it finds nothing left
        // to do, and the queue woken again for the waiters that wait for this.
        self.release_finished.store(true, Ordering::Release);
        self.exit.exited.wake_all();
        // Its parent is told -- woken, and sent the signal it was created with,
        // `SIGCHLD` for a fork. It stays in the parent's list, ended, until
        // `wait4` takes it.
        let (code, told) = self.end_report();
        kill::tell_parent(self, self.exit_signal.load(Ordering::Acquire), code, told);
    }

    /// How its end is reported to a parent: the `si_code` and the status or
    /// signal that goes with it. Valid once it has terminated.
    fn end_report(&self) -> (i32, i32) {
        let signal = self.exit.ended_by.load(Ordering::Acquire);
        if signal == 0 {
            (
                kill::CLD_EXITED,
                self.exit.status.load(Ordering::Acquire) & 0xFF,
            )
        } else {
            (kill::CLD_KILLED, signal as i32)
        }
    }

    /// Where its children go when it ends: the nearest ancestor still running
    /// that set `PR_SET_CHILD_SUBREAPER`, or else init, if init is running and
    /// is not this process.
    fn reaper_for_orphans(&self) -> Option<Arc<Process>> {
        let mut ancestor = self.parent();
        while let Some(candidate) = ancestor {
            if !candidate.is_terminated() && attributes::get(&candidate).child_subreaper {
                return Some(candidate);
            }
            ancestor = candidate.parent();
        }
        registry::find(registry::INIT_PID)
            .filter(|init| !init.is_terminated() && !core::ptr::eq(Arc::as_ptr(init), self))
    }
}

impl Process {
    /// Its parent, if it has one that still exists.
    pub(crate) fn parent(&self) -> Option<Arc<Process>> {
        self.parent.lock().upgrade()
    }

    /// The queue woken when a signal is sent to it.
    pub(crate) fn signalled(&self) -> &WaitQueue {
        &self.signalled
    }

    /// Whether a wait it is in should end: it has ended, or a signal the
    /// waiting thread does not block is pending. What every call that waits
    /// checks, beside its own condition, to return `EINTR`.
    ///
    /// Asked of the calling thread when the caller is one of its threads, and
    /// otherwise of its first. A kernel task waiting on behalf of a process
    /// with no thread at all -- a self-check's -- has no mask to block with,
    /// so any signal sent to the process ends its wait.
    pub(crate) fn signal_pending(&self) -> bool {
        match self.signal_taker() {
            Some(thread) => thread.signal_pending(),
            None => self.is_terminated() || self.with_signals(|signals| signals.pending() != 0),
        }
    }

    /// Make sure its tasks look at a signal just made pending: one blocked in a
    /// call is woken, so its wait sees [`Process::signal_pending`], and one
    /// running in user mode on another processor is interrupted, so it comes
    /// back through the kernel to have it delivered.
    pub(crate) fn notify_signal(&self) {
        self.signalled.wake_all();
        let tasks: Vec<Arc<Task>> = self.tasks.lock().iter().filter_map(Weak::upgrade).collect();
        for task in &tasks {
            sched::wake(task);
            sched::interrupt(task);
        }
    }

    /// Its threads.
    pub(crate) fn threads(&self) -> Vec<Arc<Thread>> {
        self.threads
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    /// List `thread` as one of its own, if it is not already: before the
    /// thread can run, and for a fork child before the child is published.
    pub(crate) fn add_thread(&self, thread: &Arc<Thread>) {
        let mut threads = self.threads.lock();
        threads.retain(|listed| listed.strong_count() > 0);
        if !threads
            .iter()
            .any(|listed| core::ptr::eq(listed.as_ptr(), Arc::as_ptr(thread)))
        {
            threads.push(Arc::downgrade(thread));
        }
    }

    /// The thread a signal sent to the process is judged for: the caller when
    /// the caller is one of its threads, and its first otherwise.
    fn signal_taker(&self) -> Option<Arc<Thread>> {
        thread::current_of(self).or_else(|| self.threads().into_iter().next())
    }

    /// Record `signal` sent to it as a whole, or decide it needs no recording:
    /// [`Signals::post`] under its signal lock, judged against the blocked mask
    /// of the thread that would take it. A process with no thread yet -- a
    /// native child before `process_start` -- is judged against no mask, which
    /// is the truth: nothing in it can have blocked anything.
    pub(crate) fn post_signal(&self, signal: u32, origin: Origin) -> Posted {
        let taker = self.signal_taker();
        self.with_signals(|signals| {
            let blocked = taker
                .as_ref()
                .map_or(0, |thread| thread.with_own_signals(|own| own.blocked()));
            signals.post(blocked, signal, origin)
        })
    }

    /// Whether it is stopped.
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire) != 0
    }

    /// The queue woken when it continues or ends.
    pub(crate) fn resumed(&self) -> &WaitQueue {
        &self.resumed
    }

    /// Stop it for `signal`, and tell its parent. Its task waits on the way
    /// back to user mode until [`Process::leave_stop`].
    pub(crate) fn enter_stop(&self, signal: u32) {
        if self.is_terminated() {
            return;
        }
        self.stopped.store(signal, Ordering::Release);
        self.continue_report.store(false, Ordering::Release);
        self.stop_report.store(signal, Ordering::Release);
        kill::tell_parent(self, SIGCHLD, kill::CLD_STOPPED, signal as i32);
    }

    /// Continue it if it is stopped, and tell its parent: what `SIGCONT` does
    /// as it is sent.
    pub(crate) fn leave_stop(&self) {
        if self.stopped.swap(0, Ordering::AcqRel) == 0 {
            return;
        }
        self.stop_report.store(0, Ordering::Release);
        self.continue_report.store(true, Ordering::Release);
        self.resumed.wake_all();
        kill::tell_parent(
            self,
            SIGCHLD,
            kill::CLD_CONTINUED,
            ferrix_linux_abi::types::SIGCONT as i32,
        );
    }

    /// A child `select` accepts with a stop (when `stops`) or a continue (when
    /// `continues`) its parent has not been told of, and the signal that
    /// stopped it -- zero for a continue. The report is taken when `consume`.
    pub(crate) fn changed_child(
        &self,
        select: &dyn Fn(&Process) -> bool,
        stops: bool,
        continues: bool,
        consume: bool,
    ) -> Option<(Arc<Process>, u32)> {
        let children = self.children.lock();
        children
            .iter()
            .filter(|child| select(child))
            .find_map(|child| {
                let stop = match (stops, consume) {
                    (false, _) => 0,
                    (true, true) => child.stop_report.swap(0, Ordering::AcqRel),
                    (true, false) => child.stop_report.load(Ordering::Acquire),
                };
                let continued = match (continues, consume) {
                    (false, _) => false,
                    _ if stop != 0 => false,
                    (true, true) => child.continue_report.swap(false, Ordering::AcqRel),
                    (true, false) => child.continue_report.load(Ordering::Acquire),
                };
                (stop != 0 || continued).then(|| (Arc::clone(child), stop))
            })
    }

    /// Its parent's pid, or zero when it has none: a process the kernel
    /// started, or one whose parent has ended.
    pub(crate) fn parent_pid(&self) -> u32 {
        self.parent
            .lock()
            .upgrade()
            .map_or(0, |parent| parent.pid())
    }

    /// Its process group.
    pub(crate) fn pgid(&self) -> u32 {
        self.pgid.load(Ordering::Acquire)
    }

    /// Move it into process group `pgid`.
    pub(crate) fn set_pgid(&self, pgid: u32) {
        self.pgid.store(pgid, Ordering::Release);
    }

    /// Its session.
    pub(crate) fn sid(&self) -> u32 {
        self.sid.load(Ordering::Acquire)
    }

    /// Make it the leader of a new session and of a new process group, both
    /// numbered by its pid: what `setsid` does.
    pub(crate) fn lead_new_session(&self) {
        self.sid.store(self.pid, Ordering::Release);
        self.pgid.store(self.pid, Ordering::Release);
    }

    /// The signal its parent is told with when it ends.
    pub(crate) fn set_exit_signal(&self, signal: u32) {
        self.exit_signal.store(signal, Ordering::Release);
    }

    /// Take `child` into its list of children.
    pub(crate) fn adopt(&self, child: Arc<Process>) {
        self.children.lock().push(child);
    }

    /// Let `child` go from its list of children, if it is there.
    pub(crate) fn disown(&self, child: &Process) {
        self.children
            .lock()
            .retain(|held| !core::ptr::eq(Arc::as_ptr(held), child));
    }

    /// Whether `pid` is one of its children, ended or not.
    pub(crate) fn has_child(&self, pid: u32) -> bool {
        self.children.lock().iter().any(|child| child.pid() == pid)
    }

    /// A child `select` accepts that has ended, taken out of the list when
    /// `remove` is set.
    ///
    /// # Errors
    ///
    /// `ECHILD` if no child at all is one `select` accepts, ended or not: the
    /// difference between "wait longer" and "there is nothing to wait for".
    pub(crate) fn reap_child(
        &self,
        select: &dyn Fn(&Process) -> bool,
        remove: bool,
    ) -> Result<Option<Arc<Process>>, Errno> {
        let mut children = self.children.lock();
        let mut any = false;
        let mut ended = None;
        for (at, child) in children.iter().enumerate() {
            if !select(child) {
                continue;
            }
            any = true;
            if child.is_released() {
                ended = Some(at);
                break;
            }
        }
        match ended {
            Some(at) if remove => Ok(Some(children.remove(at))),
            Some(at) => Ok(children.get(at).map(Arc::clone)),
            None if any => Ok(None),
            None => Err(Errno::ECHILD),
        }
    }

    /// Whether a child `select` accepts has ended: a `wait4` sleeper's
    /// condition.
    pub(crate) fn has_ended_child(&self, select: &dyn Fn(&Process) -> bool) -> bool {
        self.children
            .lock()
            .iter()
            .any(|child| select(child) && child.is_released())
    }

    /// The queue woken whenever one of its children ends.
    pub(crate) fn child_exited(&self) -> &WaitQueue {
        &self.child_exited
    }

    /// The status word `wait4` reports for it once it has ended: the exit
    /// code in the second byte, or the signal that ended it in the low seven
    /// bits.
    pub(crate) fn wait_status(&self) -> Option<i32> {
        let status = self.exit_status()?;
        let signal = self.exit.ended_by.load(Ordering::Acquire);
        Some(if signal == 0 {
            (status & 0xFF) << 8
        } else {
            (signal & 0x7F) as i32
        })
    }

    /// The signal that ended it, if a signal did.
    pub(crate) fn ended_by_signal(&self) -> Option<u32> {
        let signal = self.exit.ended_by.load(Ordering::Acquire);
        (self.is_terminated() && signal != 0).then_some(signal)
    }

    /// Record a successful `execve`, releasing a `vfork` parent.
    pub(crate) fn mark_execed(&self) {
        self.execed.store(true, Ordering::Release);
        self.vfork_done.wake_all();
    }

    /// Block `caller` until this `vfork` child has called `execve` or ended,
    /// which is what `vfork` promises its parent.
    pub(crate) fn wait_vfork_release(&self, caller: &Process) {
        let _ = self.vfork_done.wait_until_deadline(
            || {
                self.execed.load(Ordering::Acquire)
                    || self.is_terminated()
                    || caller.is_terminated()
            },
            u64::MAX,
        );
    }
}

/// How a process ended, and who is waiting to hear.
///
/// Apart from the process, because this is what a handle to a process holds.
/// A handle kept past the end must not keep the address space and everything
/// else the process owned, and a wait needs nothing else. [`Process::end`]
/// records how it ended and [`Process::release`] closes it; nothing else
/// writes it.
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

    /// The queue woken as it lets go of what it held; see
    /// [`Process::is_released`].
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

    /// Record how it ended.
    fn record(&self, status: i32, signal: u32) {
        self.ended_by.store(signal, Ordering::Release);
        self.status.store(status, Ordering::Release);
        self.terminated.store(true, Ordering::Release);
    }

    /// Take the registrations waiting for it, for the caller to fire once it
    /// holds no lock, and refuse to keep any more.
    fn close(&self) -> Vec<Observer> {
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
    process: Weak<Process>,
    /// The only strong reference to a process nobody has started.
    unstarted: SpinLock<Option<Arc<Process>>>,
}

impl ProcessRef {
    /// A handle's view of `process`, with no way back to it.
    pub(crate) fn new(process: &Process) -> ProcessRef {
        ProcessRef {
            exit: Arc::clone(&process.exit),
            control: None,
        }
    }

    /// A handle to `process`, made and not yet started, which holds it until a
    /// start takes it over or the last such handle is closed.
    pub(crate) fn created(process: &Arc<Process>) -> ProcessRef {
        ProcessRef {
            exit: Arc::clone(&process.exit),
            control: Some(Arc::new(Control {
                process: Arc::downgrade(process),
                unstarted: SpinLock::new(Some(Arc::clone(process))),
            })),
        }
    }

    /// How it ended, and who is waiting to hear.
    pub(crate) fn exit(&self) -> &Exit {
        &self.exit
    }

    /// Its exit status, once it has terminated.
    pub(crate) fn exit_status(&self) -> Option<i32> {
        self.exit
            .is_terminated()
            .then(|| self.exit.status.load(Ordering::Acquire))
    }

    /// The way back to the process, if this handle was made with one.
    pub(crate) fn control(&self) -> Option<&Arc<Control>> {
        self.control.as_ref()
    }
}

impl Control {
    /// The process, if it still exists.
    ///
    /// Never asked on a wait path: a wait needs only the [`Exit`], and a
    /// process that has gone answers `None` here, not a panic.
    pub(crate) fn process(&self) -> Option<Arc<Process>> {
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
    /// would free it without [`Process::end`], so no status would be recorded
    /// and its watchers would never hear. So it is killed first, and `end`
    /// runs.
    ///
    /// # Where this runs
    ///
    /// Only where a handle object is dropped. Every such drop goes through
    /// `object::dispose`, and every caller of that is in task context with
    /// interrupts on:
    /// - a native call's handler;
    /// - a channel's own drop or refusal, reached only inside such a drain;
    /// - [`Process::end`] closing a handle table, which since the fault-kill
    ///   fix never runs with interrupts masked.
    ///
    /// A drain running on another processor is that processor's calling task,
    /// not an interrupt. The idle reaper, which the rule "never kill in Drop"
    /// is about, drops only processes whose `end` has already emptied their
    /// table, so it never holds a `Control`. The assertion is the tripwire, as
    /// `Exit::close`'s is. The reference is taken out of the lock before the
    /// kill, so `end` runs under nothing of this lock's.
    fn drop(&mut self) {
        let unstarted = self.unstarted.lock().take();
        if let Some(process) = unstarted {
            debug_assert!(
                crate::arch::interrupts_enabled(),
                "an unstarted process's last handle was dropped with interrupts off"
            );
            kill(&process, object::job::KILLED_STATUS);
        }
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
/// If no program was loaded into it, if it has already been started, or if the
/// scheduler has no stack for its task.
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
    let claim = claim_start(process)?;
    claim.spawn(Arc::new(Thread::leader(process)), cpu, None)
}

/// The right to start a process, held by one starter at a time.
///
/// Taken before anything is put into the process that its program will find on
/// entry -- a native starter's bootstrap handle, and the start argument that
/// names it -- because two starts can race. With the claim taken first, a
/// second `process_start` is refused before it has moved a handle into a table
/// the child may already be reading, or overwritten the argument the first
/// start's task has yet to read. Dropped without starting, it gives the start
/// back, so a starter whose own preparation failed leaves the process
/// startable.
#[must_use = "dropping a claim gives the start back"]
pub(crate) struct StartClaim {
    /// The process it may start.
    process: Arc<Process>,
    /// Set once its task is spawned, after which the start is not given back.
    spent: bool,
}

/// Claim the start of `process`, which must have a program loaded and must not
/// have ended.
///
/// An ended process is refused with the claim already taken, so a kill that
/// lands after this answers finds a claim a starter holds, and the task that
/// start spawns returns before entering the program. Without the refusal, a
/// native `process_start` on a child killed before its start would spawn that
/// task and report a start that never ran anything.
///
/// # Errors
///
/// If no program was loaded into it, it has already ended, or it has already
/// been claimed or started.
pub(crate) fn claim_start(process: &Arc<Process>) -> Result<StartClaim, &'static str> {
    if process.startup().is_none() {
        return Err("the process has no program loaded");
    }
    let claim = take_claim(process)?;
    if process.is_terminated() {
        // Dropping the claim gives the start back, which no one can use now.
        return Err("the process has already ended");
    }
    Ok(claim)
}

/// Take the claim whether or not a program is loaded: a fork child resumes from
/// its parent's registers instead.
fn take_claim(process: &Arc<Process>) -> Result<StartClaim, &'static str> {
    process
        .start_claimed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| "the process has already started")?;
    Ok(StartClaim {
        process: Arc::clone(process),
        spent: false,
    })
}

impl StartClaim {
    /// Start the process with `argument` in its first argument register.
    ///
    /// The argument is written while the claim is held, so no other start can
    /// change it between here and the task reading it.
    ///
    /// # Errors
    ///
    /// If the process has no program loaded, or the scheduler has no stack for
    /// its task; the start is given back either way. A spawn that fails leaves
    /// `argument` written into the startup, which is harmless: the next start
    /// writes its own before anything reads it.
    pub(crate) fn start(
        self,
        cpu: Option<usize>,
        argument: u64,
    ) -> Result<Arc<Task>, &'static str> {
        let loaded = if let Some(startup) = self.process.startup.lock().as_mut() {
            startup.argument = argument;
            true
        } else {
            false
        };
        if !loaded {
            return Err("the process has no program loaded");
        }
        let thread = Arc::new(Thread::leader(&self.process));
        self.spawn(thread, cpu, None)
    }

    /// Run the process's first task, for `thread`, one of its own: entering
    /// its program, or with `state` resuming a fork child with its parent's
    /// thread pointer and floating-point registers.
    fn spawn(
        mut self,
        thread: Arc<Thread>,
        cpu: Option<usize>,
        state: Option<crate::arch::UserState>,
    ) -> Result<Arc<Task>, &'static str> {
        self.process.add_thread(&thread);
        let task = sched::spawn_user("user", run_program, thread, cpu, state)?;
        self.process.tasks.lock().push(Arc::downgrade(&task));
        // An end requested between the spawn and the push found no task to
        // wake or interrupt; it is told now, rather than running on in user
        // mode until it happens to make a call.
        if self.process.is_terminated() {
            sched::wake(&task);
            sched::interrupt(&task);
        }
        self.spent = true;
        Ok(task)
    }
}

impl Drop for StartClaim {
    /// Give the start back, unless the task was spawned.
    fn drop(&mut self) {
        if !self.spent {
            self.process.start_claimed.store(false, Ordering::Release);
        }
    }
}

/// Run a fork child's first thread: its process resumes from the registers
/// [`Process::set_resume`] gave it, with `state` -- its parent's thread pointer
/// and floating-point registers, as they were -- loaded when it is first
/// switched to.
///
/// # Errors
///
/// As [`start`].
pub(crate) fn start_forked(
    thread: Arc<Thread>,
    state: crate::arch::UserState,
) -> Result<Arc<Task>, &'static str> {
    let claim = take_claim(thread.process())?;
    claim.spawn(thread, None, Some(state))
}

/// End `process` from outside, with `status`.
///
/// Its tasks find out on their way back to user mode: one running there is
/// interrupted to, one blocked in a call is woken to, and one that has not yet
/// entered user mode never does. Nothing here waits for that; a caller
/// that needs the tasks gone waits for them.
pub(crate) fn kill(process: &Process, status: i32) {
    // A status of 128 plus a signal number is how a shell spells death by that
    // signal, and it is how a waiting parent is told: as the signal.
    let signal = if (129..=192).contains(&status) {
        (status - 128) as u32
    } else {
        0
    };
    let _ = process.end(status, signal);
}

/// End the running task's process with `status`, and the task's thread with
/// it: what `exit_group` does.
pub(crate) fn exit_current(status: i32) -> ! {
    end_thread(Some(status), true)
}

/// End the running task's thread with `status`: what `exit` does. Its process
/// ends with it only when no other thread of it is live, with the status its
/// first thread left with.
pub(crate) fn exit_thread_current(status: i32) -> ! {
    end_thread(Some(status), false)
}

/// End the running task's thread with no status of its own: a thread leaving
/// because its process is ending, or one whose program never started.
pub(crate) fn leave_current() -> ! {
    end_thread(None, false)
}

/// End the running task's thread: end its process first when `group` asks;
/// clear and wake the address it asked to have cleared; and count it gone,
/// which ends the process if that was its last thread, or lets go of what the
/// process holds if it was already ending.
///
/// Interrupts are opened first. A thread leaving from the way back to user
/// mode, or from a program killed before it entered, arrives with them masked,
/// and letting go of a process takes plain locks. Every reference is dropped
/// before the task ends, because nothing after `sched::exit` runs to drop it.
fn end_thread(status: Option<i32>, group: bool) -> ! {
    crate::arch::enable_interrupts();
    if let Some(thread) = thread::current() {
        let process = Arc::clone(thread.process());
        if let Some(status) = status {
            if thread.tid() == process.pid() {
                process.leader_status.store(status, Ordering::Release);
            }
            if group {
                let _ = process.terminate(status);
            }
        }
        // The address `CLONE_CHILD_CLEARTID` or `set_tid_address` registered
        // is zeroed and its futex woken, which is how a `pthread_join` or a
        // `vfork`ing libc learns the thread is gone. A failure is ignored, as
        // Linux ignores it: the address was the program's to get right.
        let clear_child_tid = thread.take_clear_child_tid();
        if clear_child_tid != 0 {
            let space = process.space();
            let _ = uaccess::copy_to_user(space, clear_child_tid, &0_u32.to_le_bytes());
            let _ = futex::wake_address(space, clear_child_tid, 1);
        }
        drop(thread);
        process.thread_gone(true);
    }
    sched::exit()
}

/// Where a program's task begins: enter user mode where `exec::load` said.
///
/// Returning ends the task, which is what happens if the process was killed
/// before it ever ran.
fn run_program(_argument: usize) {
    let Some(process) = current() else {
        return;
    };
    // Masked from the test to the entry into user mode. The task starts with
    // interrupts open, and the return-to-user check runs only for traps taken
    // from user mode, so a kill whose interrupt landed between an open test
    // and the entry would be taken here, in kernel mode, and forgotten: the
    // program would enter user mode anyway and, alone on its processor, run
    // until its first system call. Masked, a kill after the test leaves its
    // interrupt pending, and it is taken from user mode on the first
    // instruction, where the check sees it. Entering user mode opens them.
    crate::arch::disable_interrupts();
    if process.is_terminated() {
        drop(process);
        leave_current();
    }
    if let Some(regs) = process.take_resume() {
        drop(process);
        // SAFETY: this task was spawned in the child's address space with its
        // parent's user state, both installed by the switch that got here, and
        // `regs` is a copy of the frame the parent's system call saved in that
        // same (forked) space. It lives on this task's own kernel stack, which
        // the resume path requires, and nothing owned is left on this frame.
        unsafe { crate::arch::resume_user(&regs) }
    }
    let startup = process.startup();
    drop(process);
    let Some(Startup {
        entry,
        stack,
        argument,
    }) = startup
    else {
        leave_current();
    };
    // SAFETY: this task was spawned in the process's address space, which the
    // scheduler installed when it switched here, along with the task's user
    // state and entry stack; `entry` and `stack` came from the loader and the
    // stack builder, both inside that space. Nothing owned is left on this
    // frame to leak: the process reference was dropped above.
    unsafe { crate::arch::enter_user(entry, stack, argument) }
}

/// Make a process over a fresh address space, for the self-checks.
///
/// # Errors
///
/// Whatever [`AddressSpace::new`] refuses.
pub(crate) fn new_for_check() -> Result<Arc<Process>, SpaceError> {
    Ok(registry::register(Process::new(AddressSpace::new()?)))
}
