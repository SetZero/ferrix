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
//! [`current`], one function, and it is the only thing here that changes when
//! stage 6 gives `Task` its address space field.

use alloc::sync::Arc;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_sync::SpinLock;
use ferrix_vma::VmaFlags;

use crate::syscall::signal::Signals;
use crate::user::space::{AddressSpace, SpaceError};

/// A program, as far as the system call layer is concerned.
#[derive(Debug)]
pub(crate) struct Process {
    /// What it can see.
    space: Arc<AddressSpace>,
    /// Everything else, behind one lock. One lock per process rather than a
    /// global one, for the same reason the address space has its own: two
    /// processes calling `brk` at once should contend for nothing.
    state: SpinLock<State>,
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
            state: SpinLock::new(State::default()),
        }
    }

    /// What it can see.
    pub(crate) fn space(&self) -> &Arc<AddressSpace> {
        &self.space
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

/// Round up to a page, or `None` if that would leave the address space.
fn round_up(at: u64) -> Option<u64> {
    at.checked_add(PAGE_SIZE - 1)
        .map(|at| at & !(PAGE_SIZE - 1))
}

/// The process running on this processor, if any.
///
/// One slot, guarded, rather than a field on `Task` — which is where it
/// belongs and where stage 6 is putting it. Until then this is what a system
/// call and a page fault from user mode both need to find, and it is deliberately
/// the *only* thing they use to find it: when `Task` gains its field, this
/// function's body changes and nothing else does.
///
/// # Why a lock and not a per-CPU word
///
/// Because an `Arc` has to be kept alive for as long as the program runs, and
/// a raw per-CPU pointer would not do that. Only one program runs at a time
/// today, so contention is not a question; when the scheduler owns this, the
/// reference will live in the task and the lock will go with the slot.
static CURRENT: SpinLock<Option<Arc<Process>>> = SpinLock::new(None);

/// The process the running thread belongs to.
///
/// `None` for a kernel thread, which is every thread that is not inside
/// [`set_current`]'s window.
pub(crate) fn current() -> Option<Arc<Process>> {
    CURRENT.lock().clone()
}

/// Make `process` the one this processor is running, and give back whatever
/// was there.
///
/// The caller must put the old value back: this is a save/restore pair rather
/// than a setter, so that a program started from inside another one cannot
/// lose the outer one.
pub(crate) fn set_current(process: Option<Arc<Process>>) -> Option<Arc<Process>> {
    let mut slot = CURRENT.lock();
    core::mem::replace(&mut slot, process)
}

/// Make a process over a fresh address space, for the self-checks.
///
/// # Errors
///
/// Whatever [`AddressSpace::new`] refuses.
pub(crate) fn new_for_check() -> Result<Arc<Process>, SpaceError> {
    Ok(Arc::new(Process::new(AddressSpace::new()?)))
}
