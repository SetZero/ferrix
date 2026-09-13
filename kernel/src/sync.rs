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

/// A spin lock whose holder is not switched out while it holds it.
pub(crate) type SpinLock<T> = ferrix_sync::PreemptSpinLock<T, crate::sched::Preempt>;
