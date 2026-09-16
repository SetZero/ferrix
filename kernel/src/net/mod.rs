//! The net core: one [`Stack`] behind one lock, and the task that drives it.
//!
//! `libs/net` is the whole of the logic and holds no lock, no clock and no
//! device. This module is the three things it lacks.
//!
//! # One lock, and nothing sleeps inside it
//!
//! The stack is behind a [`SpinLock`], which disables preemption, so nothing
//! that could sleep may happen while it is held -- no copy to user memory, no
//! allocation that could reclaim, no wait. Every call here takes what it needs
//! out of the stack into a kernel buffer, drops the lock, and only then
//! touches the program's memory. The pattern is `crate::fs::socket`'s and the
//! reason is the same one.
//!
//! # One wait queue for every socket
//!
//! A socket does not have a queue of its own: they all wait on
//! [`NetCore::progress`], which is woken whenever anything moved, and each
//! waiter re-checks its own condition. That is a thundering herd in the
//! textbook sense and it is the right trade here -- a host has tens of
//! sockets, not thousands, and the alternative is a queue per socket that the
//! stack would have to know about, which would put the kernel's waiting
//! machinery inside the library that is meant to be testable without it.
//!
//! # The clock is milliseconds
//!
//! `libs/net` and `libs/nettcp` count in milliseconds, because a
//! retransmission timeout is a hundred of them and a nanosecond counter of
//! them overflows a `u32` in four seconds. The kernel counts in nanoseconds.
//! The conversion is [`now`], and it is the only place the two meet.

pub(crate) mod check;
pub(crate) mod socket;

use alloc::vec::Vec;

use ferrix_net::Stack;
use ferrix_net::stack::{Config, Millis, Outgoing};
use ferrix_sync::Once;

use crate::sched::WaitQueue;
use crate::sync::SpinLock;

/// Nanoseconds in a millisecond.
const NANOS_PER_MILLI: u64 = 1_000_000;

/// How long the driving task sleeps when the stack has no deadline of its
/// own, so that a wake-up lost anywhere costs a few milliseconds rather than a
/// wedged host.
const RECHECK_MILLIS: Millis = 50;

/// What the stack has been asked to send, and nobody has taken.
///
/// A frame is handed to the driver that owns its interface. Until one exists
/// the queue is where frames for a real interface stop, and the counter says
/// how many: a boot check that finds it climbing has found a route pointing at
/// an interface with no driver behind it.
#[derive(Debug, Default)]
struct Pending {
    /// Frames waiting for a driver.
    frames: Vec<Outgoing>,
    /// How many were dropped because nothing took them.
    dropped: u64,
}

/// The kernel's half of the net core.
#[derive(Debug)]
pub(crate) struct NetCore {
    /// The stack itself.
    stack: SpinLock<Stack>,
    /// Frames on their way out.
    pending: SpinLock<Pending>,
    /// Woken whenever anything in the stack moved.
    progress: WaitQueue,
}

/// The one net core, made on first use.
static CORE: Once<NetCore> = Once::new();

/// The net core.
pub(crate) fn core() -> &'static NetCore {
    CORE.call_once(|| NetCore {
        stack: SpinLock::new(new_stack()),
        pending: SpinLock::new(Pending::default()),
        progress: WaitQueue::new(),
    })
}

/// A stack with its loopback, seeded from the clock.
///
/// The seed is what makes an initial sequence number and an ephemeral port
/// unguessable. A counter would be worse than nothing: it would look random in
/// a log and be predictable to anyone who saw one connection.
fn new_stack() -> Stack {
    let mut stack = Stack::new(Config::default());
    stack.seed(crate::timer::now_nanos().wrapping_mul(0x2545_F491_4F6C_DD1D));
    stack
}

/// The clock the stack counts in.
pub(crate) fn now() -> Millis {
    crate::timer::now_nanos() / NANOS_PER_MILLI
}

impl NetCore {
    /// Run `body` with the stack locked, then move what it produced and wake
    /// whoever was waiting.
    ///
    /// Nothing inside `body` may sleep: the lock disables preemption. Copy out
    /// of the stack and act on it afterwards.
    pub(crate) fn with<T>(&self, body: impl FnOnce(&mut Stack, Millis) -> T) -> T {
        let at = now();
        let answer = {
            let mut stack = self.stack.lock();
            let answer = body(&mut stack, at);
            self.take_frames(&mut stack, at);
            answer
        };
        self.progress.wake_all();
        answer
    }

    /// Let the clock reach now, and move what that produced.
    fn tick(&self) {
        let at = now();
        {
            let mut stack = self.stack.lock();
            stack.on_timer(at);
            self.take_frames(&mut stack, at);
        }
        self.progress.wake_all();
    }

    /// Empty the stack's egress into the pending queue.
    ///
    /// Called with the stack locked, and it allocates nothing the stack has
    /// not already allocated: the frames are moved, not copied.
    fn take_frames(&self, stack: &mut Stack, at: Millis) {
        let mut pending = self.pending.lock();
        while let Some(outgoing) = stack.poll_transmit(at) {
            if pending.frames.len() >= MAX_PENDING {
                pending.dropped += 1;
                continue;
            }
            pending.frames.push(outgoing);
        }
    }

    /// How many frames were dropped because no driver took them.
    pub(crate) fn dropped(&self) -> u64 {
        self.pending.lock().dropped
    }

    /// How many frames are waiting for a driver.
    pub(crate) fn queued(&self) -> usize {
        self.pending.lock().frames.len()
    }

    /// Where a socket waits.
    pub(crate) const fn progress(&self) -> &WaitQueue {
        &self.progress
    }

    /// When the stack next has something to do, in milliseconds.
    fn poll_at(&self) -> Option<Millis> {
        self.stack.lock().poll_at()
    }
}

/// How many frames may wait for a driver before the oldest are dropped.
///
/// A host whose driver has stopped taking frames must not grow a queue until
/// it runs out of memory; it must drop packets, which is what a network does
/// to a host that cannot keep up.
const MAX_PENDING: usize = 512;

/// Start the task that drives the stack.
///
/// # Errors
///
/// If the scheduler cannot take another task.
pub(crate) fn start() -> Result<(), &'static str> {
    let _ = core();
    let _ = crate::sched::spawn("net core", run, 0, ferrix_sched::NICE_0_WEIGHT)?;
    Ok(())
}

/// The driving task: let the clock reach now, move what that produced, and
/// sleep until the stack's next deadline.
fn run(_argument: usize) {
    loop {
        let core = core();
        core.tick();
        let at = now();
        let next = core
            .poll_at()
            .map_or(at.saturating_add(RECHECK_MILLIS), |deadline| {
                deadline.clamp(at, at.saturating_add(RECHECK_MILLIS))
            });
        // Never sleep until a moment that has already arrived. A deadline in
        // the past would otherwise turn this loop into a spin that starves
        // every other task on the processor, which is the worst way for a
        // timer to be wrong: everything else slows down and nothing says why.
        let next = next.max(at.saturating_add(1));
        let nanos = next.saturating_mul(NANOS_PER_MILLI);
        crate::sched::sleep_until(nanos);
    }
}
