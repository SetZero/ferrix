//! The native ABI's kernel objects, and what a handle names.
//!
//! Stage 9 of `docs/ROADMAP.md`. The rules — which handles are valid, which
//! rights they carry, when a queue is full — are `libs/objects` and
//! `libs/native-abi`, where the host tests and the fuzzer reach them. This is
//! the part that needs a kernel: the reference counts, the locks, and freeing
//! what an object held.
//!
//! # Dropping is deferred, and why
//!
//! An object can hold other objects. A channel endpoint holds the messages
//! queued for it, and a message holds handles to anything — including other
//! endpoints, holding further messages. Dropping the last reference to the
//! outermost one would drop everything inside it recursively, on a
//! sixteen-kibibyte kernel stack, to a depth the program chose. A chain of a
//! few thousand endpoints, each queued in the next, is a short loop in ring 3
//! and a guard-page fault in ring 0.
//!
//! So an object that contains objects never drops them itself: it hands them
//! to [`dispose`], which drops them one level at a time in a loop. However
//! deep the chain, the stack holds one drop at a time.

pub(crate) mod channel;
pub(crate) mod check;
pub(crate) mod interrupt;
pub(crate) mod io_mapping;
pub(crate) mod job;
pub(crate) mod pin;
pub(crate) mod port;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sync::SpinLock;
use ferrix_native_abi::rights::Rights;
use ferrix_native_abi::signals::Signals;

use crate::device::DeviceNode;
use crate::sched::WaitQueue;
use crate::user::vmo::Vmo;

/// The most handles one process may hold at once.
///
/// A resource limit, not a structural one: the table itself can grow to
/// `ferrix_objects::table::MAX_SLOTS`. Four thousand is far more than a driver
/// needs, and small enough that a program leaking handles in a loop hits it
/// long before the heap notices.
pub(crate) const HANDLE_LIMIT: usize = 4096;

/// Something a handle can name.
#[derive(Debug, Clone)]
pub(crate) enum Object {
    /// One end of a channel.
    Channel(Arc<channel::Endpoint>),
    /// A memory object.
    Vmo(Arc<Vmo>),
    /// A container of processes.
    Job(Arc<job::Job>),
    /// A device node, from which its interrupts and I/O mappings are minted.
    Device(Arc<DeviceNode>),
    /// A device interrupt.
    Interrupt(Arc<interrupt::Interrupt>),
    /// A device aperture a driver may map.
    IoMapping(Arc<io_mapping::IoMapping>),
    /// Pages of a VMO a device may reach.
    Pin(Arc<pin::Pin>),
    /// A process: how it ended, and not the process itself.
    Process(crate::syscall::process::ProcessRef),
    /// An event queue.
    Port(Arc<port::Port>),
}

/// A queue nothing is woken on, for objects whose signals never change.
///
/// A wait on one of them still ends: at its deadline, or when the waiting
/// process is killed, which the wait's own periodic recheck notices.
static QUIET: WaitQueue = WaitQueue::new();

impl Object {
    /// What a waiter on this object would see now.
    pub(crate) fn signals(&self) -> Signals {
        match self {
            Object::Channel(endpoint) => endpoint.signals(),
            Object::Vmo(_) => Signals::NONE,
            Object::Job(job) if job.is_killed() => Signals::TERMINATED,
            Object::Interrupt(interrupt) if interrupt.is_pending() => Signals::READABLE,
            Object::Port(port) if !port.is_empty() => Signals::READABLE,
            Object::Process(process) if process.exit().is_closed() => Signals::TERMINATED,
            Object::Job(_)
            | Object::Device(_)
            | Object::Interrupt(_)
            | Object::IoMapping(_)
            | Object::Pin(_)
            | Object::Process(_)
            | Object::Port(_) => Signals::NONE,
        }
    }

    /// The queue woken whenever this object's signals may have changed.
    ///
    /// "May have": a waiter is woken for any change and looks at the level
    /// again, so a queue woken too often costs a recheck and one woken too
    /// rarely is a wait that sleeps out its deadline.
    pub(crate) fn waiters(&self) -> &WaitQueue {
        match self {
            Object::Channel(endpoint) => endpoint.waiters(),
            Object::Job(job) => job.waiters(),
            Object::Port(port) => port.waiters(),
            Object::Process(process) => process.exit().exited(),
            // Woken from the interrupt handler itself; see `interrupt`.
            Object::Interrupt(interrupt) => interrupt.waiters(),
            Object::Vmo(_) | Object::Device(_) | Object::IoMapping(_) | Object::Pin(_) => &QUIET,
        }
    }
}

/// A process's handle table.
pub(crate) type HandleTable = ferrix_objects::table::HandleTable<Object>;

/// An object travelling in a message, with the rights its handle carried.
///
/// The rights travel with it: a read-only VMO sent to another process arrives
/// read-only, which is what lets `devmgr` hand a driver less than it holds.
pub(crate) type Transfer = (Object, Rights);

/// Held by every send that carries a channel endpoint, from its cycle check
/// to its push.
///
/// Endpoints keep each other alive only through their queues, so only such a
/// send adds an edge to the graph of who keeps whom alive — and a cycle in
/// that graph is memory nothing can free. Checking for one is a walk, and a
/// walk is an answer only about a graph nobody is adding to, so the adders
/// take turns. Reads remove edges and never take it; sends of bytes and VMOs
/// add none and never take it.
///
/// Taken before a process's handle table or any queue, never while holding
/// either.
pub(crate) static TOPOLOGY: SpinLock<()> = SpinLock::new(());

/// Objects waiting to be dropped.
static ORPHANS: SpinLock<Vec<Object>> = SpinLock::new(Vec::new());

/// Whether some context is already draining [`ORPHANS`].
static DISPOSING: AtomicBool = AtomicBool::new(false);

/// Drop `objects`, and everything they contain, without recursing.
///
/// An object that holds no other object is dropped here, at once: its drop
/// cannot reach another, so it cannot recurse, and a close of one has to have
/// let go of what it held by the time it returns -- an interrupt's line, which
/// a restarted driver claims again straight away. The rest are queued.
///
/// If another drop is already draining the queue — this one was reached from
/// inside an object's `Drop` — the queued objects are left for that loop and
/// this returns at once, which is what bounds the depth at one. On another
/// processor the same thing happens, and the clear-then-recheck at the bottom
/// is what stops an object queued just as the drainer finished from being
/// stranded.
///
/// Call it with no lock held that an object's drop might need: a channel's
/// queue, a process's handle table.
pub(crate) fn dispose(objects: impl IntoIterator<Item = Object>) {
    for object in objects {
        if object.drops_at_once() {
            drop(object);
        } else {
            ORPHANS.lock().push(object);
        }
    }
    loop {
        if DISPOSING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        loop {
            let batch = core::mem::take(&mut *ORPHANS.lock());
            if batch.is_empty() {
                break;
            }
            // The lock is released before anything is dropped, so a drop
            // that disposes further objects can take it again.
            drop(batch);
        }
        DISPOSING.store(false, Ordering::Release);
        if ORPHANS.lock().is_empty() {
            return;
        }
    }
}

/// Whether some object may be waiting in [`dispose`]'s queue for another
/// context to drop it: a drain is under way, or the queue holds something.
///
/// What a close that has returned does not promise. A handle closed on one
/// processor while another is draining is queued, and its object lives on
/// until that drain reaches it; a caller about to conclude from an object
/// still being alive that something still holds it has to allow for that.
pub(crate) fn disposal_pending() -> bool {
    DISPOSING.load(Ordering::Acquire) || !ORPHANS.lock().is_empty()
}

impl Object {
    /// Whether [`dispose`] drops it where it is rather than queueing it.
    ///
    /// Those whose drop reaches no other object: it cannot recurse, and a
    /// close has let go of what they held by the time it returns. A channel
    /// end holds the messages queued for it and a job its processes, which
    /// is what the queue is for. A process handle holds only how the process
    /// ended, but is queued as well: the end of a process is the heaviest
    /// teardown in the kernel, and nothing waits on its handle's close.
    fn drops_at_once(&self) -> bool {
        match self {
            Object::Vmo(_)
            | Object::Device(_)
            | Object::Interrupt(_)
            | Object::IoMapping(_)
            | Object::Pin(_)
            | Object::Port(_) => true,
            Object::Channel(_) | Object::Job(_) | Object::Process(_) => false,
        }
    }
}

/// Run `f` as though another context were draining disposed objects -- what a
/// close sees while another processor disposes -- for the checks.
///
/// Waits for a real drain to finish first, so that the mark is this call's to
/// set and to clear, and drains afterwards whatever `f` left queued.
pub(crate) fn as_if_draining_elsewhere<R>(f: impl FnOnce() -> R) -> R {
    while DISPOSING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let result = f();
    DISPOSING.store(false, Ordering::Release);
    dispose([]);
    result
}
