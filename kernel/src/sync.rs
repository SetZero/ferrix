//! The kernel's spin locks.
//!
//! `ferrix_sync` provides the primitives; this module says which the kernel
//! takes where, so that the choice is made once.
//!
//! * [`SpinLock`] — for data tasks share and no interrupt handler touches:
//!   a process's tables, a channel's queue, a filesystem's state. It keeps the
//!   holding task on its processor until the guard drops, and that is not
//!   optional. A ticket lock whose holder can be switched out stalls every
//!   waiter for a round of the run queue, and waiters switched out holding
//!   tickets pass the stall on; the thousand-task check spent fifty seconds
//!   in one such convoy. **Never block while holding one** — `schedule`
//!   stops the machine if asked to switch with the count raised.
//! * [`IrqSpinLock`] — for data an interrupt handler also touches. It masks
//!   interrupts, which also keeps the holder on its processor.
//! * The run queues' own locks are plain `ferrix_sync::SpinLock`s taken with
//!   interrupts masked and handed across a context switch; `sched::queue`
//!   says how.
//! * `ferrix_sync::SleepLock` — for a critical section that may block: the
//!   namespace's rename lock, held across path walks that read a disk. It is
//!   the one lock a holder may sleep under, and it must never be taken with a
//!   [`SpinLock`] held or preemption otherwise off: a task that blocks with
//!   the count raised is FX-0503. Its waiters sleep on a `sched::WaitQueue`,
//!   which [`SchedParker`] lends to every such lock a crate below the kernel
//!   makes.

/// A spin lock whose holder is not switched out while it holds it.
pub(crate) type SpinLock<T> = ferrix_sync::PreemptSpinLock<T, crate::sched::Preempt>;

/// What the kernel lends a `ferrix_sync::SleepLock` to wait on: a wait
/// queue of its own per lock, so that a release wakes that lock's waiters and
/// nobody else's.
///
/// Before the scheduler runs, a wait on the queue spins, so a lock made at
/// boot works then too; it just cannot be contended yet.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SchedParker;

impl ferrix_sync::Parker for SchedParker {
    fn new_parking(&self) -> alloc::boxed::Box<dyn ferrix_sync::Parking> {
        alloc::boxed::Box::new(crate::sched::WaitQueue::new())
    }
}
