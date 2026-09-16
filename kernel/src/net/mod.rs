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
pub(crate) mod ifreq;
pub(crate) mod netlink;
pub(crate) mod packet;
pub(crate) mod socket;

use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_net::stack::{Config, Millis, Outgoing};
use ferrix_net::{Interface, Stack};
use ferrix_sync::Once;

use crate::object::port::Port;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

/// The key a transmit wake-up is queued on a ring's port with: its bells are 1
/// and 2 and its control channel 3.
pub(crate) const TRANSMIT_KEY: u64 = 4;

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
    /// The port each driven interface's ring sleeps on, which hears when
    /// frames are queued for that interface. Without it a frame waits until
    /// the ring wakes for its driver or its recheck, up to 20 ms, and every
    /// acknowledgment of a download pays that.
    transmit_wakers: SpinLock<Vec<(u32, Arc<Port>)>>,
}

/// The one net core, made on first use.
static CORE: Once<NetCore> = Once::new();

/// The net core.
pub(crate) fn core() -> &'static NetCore {
    CORE.call_once(|| NetCore {
        stack: SpinLock::new(new_stack()),
        pending: SpinLock::new(Pending::default()),
        progress: WaitQueue::new(),
        transmit_wakers: SpinLock::new(Vec::new()),
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
        let (answer, queued_for) = {
            let mut stack = self.stack.lock();
            let answer = body(&mut stack, at);
            (answer, self.take_frames(&mut stack, at))
        };
        self.progress.wake_all();
        self.wake_transmitters(&queued_for);
        answer
    }

    /// Let the clock reach now, and move what that produced.
    fn tick(&self) {
        let at = now();
        let queued_for = {
            let mut stack = self.stack.lock();
            stack.on_timer(at);
            self.take_frames(&mut stack, at)
        };
        self.progress.wake_all();
        self.wake_transmitters(&queued_for);
    }

    /// Empty the stack's egress into the pending queue, and answer which
    /// interfaces got frames.
    ///
    /// Called with the stack locked, and it allocates nothing the stack has
    /// not already allocated but that short list: the frames are moved, not
    /// copied.
    fn take_frames(&self, stack: &mut Stack, at: Millis) -> Vec<u32> {
        let mut pending = self.pending.lock();
        let mut interfaces = Vec::new();
        while let Some(outgoing) = stack.poll_transmit(at) {
            if pending.frames.len() >= MAX_PENDING {
                pending.dropped += 1;
                continue;
            }
            if !interfaces.contains(&outgoing.interface) {
                interfaces.push(outgoing.interface);
            }
            pending.frames.push(outgoing);
        }
        interfaces
    }

    /// Have `port` told when frames are queued for `interface`.
    pub(crate) fn wake_on_transmit(&self, interface: u32, port: &Arc<Port>) {
        let mut wakers = self.transmit_wakers.lock();
        wakers.retain(|(index, _)| *index != interface);
        wakers.push((interface, Arc::clone(port)));
    }

    /// Ring the port of each interface in `interfaces` that has one, with the
    /// stack unlocked. A port that already holds a packet is left alone: the
    /// ring drains everything it finds when it wakes, so one is enough.
    fn wake_transmitters(&self, interfaces: &[u32]) {
        if interfaces.is_empty() {
            return;
        }
        let ports: Vec<Arc<Port>> = self
            .transmit_wakers
            .lock()
            .iter()
            .filter(|(index, _)| interfaces.contains(index))
            .map(|(_, port)| Arc::clone(port))
            .collect();
        for port in ports {
            if port.is_empty() {
                let _ = port.queue_user(TRANSMIT_KEY, [0, 0]);
            }
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

    /// Take the frames waiting for an interface's driver.
    ///
    /// The driver asks for these when it has room. A frame for an interface
    /// whose driver has gone is dropped by [`NetCore::forget_interface`], not
    /// left to grow the queue.
    pub(crate) fn take_outgoing(&self, interface: u32, want: usize) -> Vec<Vec<u8>> {
        let mut pending = self.pending.lock();
        let mut mine = Vec::new();
        let mut kept = Vec::with_capacity(pending.frames.len());
        for outgoing in pending.frames.drain(..) {
            if outgoing.interface == interface && mine.len() < want {
                mine.push(outgoing.frame);
            } else {
                kept.push(outgoing);
            }
        }
        pending.frames = kept;
        mine
    }

    /// Hand a frame that arrived to the stack.
    pub(crate) fn receive(&self, interface: u32, frame: &[u8]) {
        self.with(|stack, at| stack.receive(interface, frame, at));
    }

    /// Add an interface, and answer the index it was given.
    pub(crate) fn add_interface(&self, interface: Interface) -> u32 {
        self.with(|stack, _| stack.add_interface(interface))
    }

    /// Take an interface away, and drop what was waiting for it.
    pub(crate) fn forget_interface(&self, index: u32) {
        let _ = self.with(|stack, _| stack.remove_interface(index));
        self.transmit_wakers
            .lock()
            .retain(|(interface, _)| *interface != index);
        let mut pending = self.pending.lock();
        pending.frames.retain(|frame| frame.interface != index);
    }

    /// Say whether an interface's link is up.
    pub(crate) fn set_carrier(&self, index: u32, up: bool) {
        self.with(|stack, _| {
            if let Some(interface) = stack.interface_mut(index) {
                if up {
                    interface.flags |=
                        ferrix_net::iface::IFF_RUNNING | ferrix_net::iface::IFF_LOWER_UP;
                } else {
                    interface.flags &=
                        !(ferrix_net::iface::IFF_RUNNING | ferrix_net::iface::IFF_LOWER_UP);
                }
            }
        });
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
