//! Channels: two endpoints, each reading what the other writes.
//!
//! Each endpoint owns the queue of messages written *to* it, and knows its
//! peer only weakly. The weak link is what makes closing work without a
//! separate state: when the last handle to an endpoint goes, its reference
//! count reaches zero, its peer's link stops upgrading, and the peer sees
//! `PEER_CLOSED` from that moment. An endpoint travelling in a message is
//! still held, and still open, which is the right answer — it has an owner,
//! who simply has not read it yet.
//!
//! # A write is all or nothing
//!
//! [`Endpoint::write`] takes the sender's handles out of its table only while
//! it holds the peer's queue lock and after the queue has agreed to take the
//! message. A refused write therefore moves nothing: every handle the caller
//! named is still in its table under the same number. The lock order this
//! implies — a process's handle table, then a peer's queue — is never taken
//! the other way round: a read releases its own queue before it touches the
//! reader's table.
//!
//! # No cycles
//!
//! Two endpoints each queued in the other's inbox would keep each other alive
//! after every handle to both is closed, with everything they hold. So a
//! send carrying an endpoint is refused if it would close such a loop:
//! [`check_carry`] walks from what the message carries, through the endpoints
//! queued in each, looking for the end it is about to land in.

use alloc::collections::BTreeSet;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use crate::sync::SpinLock;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES};
use ferrix_objects::message::{Limits, Message, MessageQueue, ReceiveError, SendError};
use ferrix_objects::reach::{Reach, reaches};

use super::port::{Observer, PortError, register, triggered};
use super::{Object, Transfer, dispose};
use crate::sched::WaitQueue;

/// The most messages an endpoint holds unread.
///
/// Two hundred and fifty-six: deep enough that a driver servicing a burst
/// does not see its client wait, shallow enough that a client whose driver has
/// wedged is told to wait long before it has pinned megabytes of kernel heap.
const MAX_QUEUED: usize = 256;

/// What every channel carries.
const LIMITS: Limits = Limits {
    max_bytes: CHANNEL_MAX_BYTES,
    max_handles: CHANNEL_MAX_HANDLES,
    max_queued: MAX_QUEUED,
};

/// The most queued endpoints one send's cycle check may walk.
///
/// A program can nest endpoints as deep as memory allows, and the walk runs
/// under a lock every endpoint-carrying send waits on. A thousand and
/// twenty-four is far past anything a driver's control plane builds, and a
/// send that reaches it is refused as too big rather than allowed to hold
/// that lock for as long as the program likes.
const MAX_WALK: usize = 1024;

/// A message as it sits in a queue.
pub(crate) type ChannelMessage = Message<Transfer>;

/// Why a write did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteFailure<E> {
    /// Nobody holds the other end.
    PeerClosed,
    /// Larger than a channel carries.
    TooBig,
    /// The peer's queue is full.
    Full,
    /// Taking the handles out of the sender's table was refused.
    Take(E),
}

/// Why a read found nothing to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadError {
    /// Nothing is queued, and nobody holds the other end to write more.
    PeerClosed,
    /// Nothing is queued yet.
    Empty,
    /// The next message carries channel endpoints, and may only be taken
    /// holding [`super::TOPOLOGY`]. Still queued.
    NeedsTopology,
    /// The next message needs more room, and is still queued.
    TooSmall {
        /// Its size in bytes.
        bytes: usize,
        /// How many handles it carries.
        handles: usize,
    },
}

/// One end of a channel.
#[derive(Debug)]
pub(crate) struct Endpoint {
    /// The other end. Weak, so that closing one end is its count reaching zero.
    peer: Weak<Endpoint>,
    /// Messages written by the peer, waiting for this end to read them.
    inbox: SpinLock<MessageQueue<Transfer>>,
    /// Woken when this end's signals may have changed: a message arrived, the
    /// peer's queue gained room, or the peer closed.
    waiters: WaitQueue,
    /// Port registrations waiting on this end's signals. Taken only inside
    /// `inbox`'s lock, which is what serialises a registration against the
    /// change it waits for.
    observers: SpinLock<Vec<Observer>>,
}

impl Endpoint {
    /// A new channel's two ends.
    ///
    /// `None` only if the allocator's cyclic construction did not run its
    /// closure, which it always does; the `Option` is the price of not writing
    /// an `unwrap` in the kernel.
    pub(crate) fn pair() -> Option<(Arc<Endpoint>, Arc<Endpoint>)> {
        let mut second = None;
        let first = Arc::new_cyclic(|first| {
            let other = Arc::new(Endpoint {
                peer: Weak::clone(first),
                inbox: SpinLock::new(MessageQueue::new(LIMITS)),
                waiters: WaitQueue::new(),
                observers: SpinLock::new(Vec::new()),
            });
            let this = Endpoint {
                peer: Arc::downgrade(&other),
                inbox: SpinLock::new(MessageQueue::new(LIMITS)),
                waiters: WaitQueue::new(),
                observers: SpinLock::new(Vec::new()),
            };
            second = Some(other);
            this
        });
        second.map(|second| (first, second))
    }

    /// Queue a message for the peer, taking its handles with `take` only once
    /// the peer's queue has room for it.
    ///
    /// `take` runs under the peer's queue lock. It is the caller's handle
    /// table removing the handles, and it must not take any lock a queue
    /// holder could be waiting for.
    ///
    /// # Errors
    ///
    /// [`WriteFailure`]; whichever it is, `take` either did not run or failed
    /// and removed nothing.
    pub(crate) fn write<E>(
        &self,
        bytes: Vec<u8>,
        handle_count: usize,
        take: impl FnOnce() -> Result<Vec<Transfer>, E>,
    ) -> Result<(), WriteFailure<E>> {
        let peer = self.peer.upgrade().ok_or(WriteFailure::PeerClosed)?;
        let (refused, fired) = {
            let mut inbox = peer.inbox.lock();
            if !inbox.accepts(bytes.len(), handle_count) {
                return Err(WriteFailure::TooBig);
            }
            if inbox.is_full() {
                return Err(WriteFailure::Full);
            }
            let handles = take().map_err(WriteFailure::Take)?;
            let refused = inbox.push(Message { bytes, handles }).err();
            let fired = if refused.is_none() {
                triggered(&mut peer.observers.lock(), Signals::READABLE)
            } else {
                Vec::new()
            };
            (refused, fired)
        };
        // Checked above under the same lock, so this is unreachable; if it
        // ever were reached, the handles are already out of the sender's
        // table and the only safe thing left is to free them, after the lock.
        match refused {
            None => {
                // After the queue lock is gone: a woken reader goes straight
                // for it, and a port's wake-up takes locks of its own.
                for observer in fired {
                    observer.fire(Signals::READABLE);
                }
                peer.waiters.wake_all();
                Ok(())
            }
            Some((why, message)) => {
                dispose(message.handles.into_iter().map(|(object, _)| object));
                Err(match why {
                    SendError::TooBig => WriteFailure::TooBig,
                    SendError::Full => WriteFailure::Full,
                })
            }
        }
    }

    /// Take the next message, if it fits.
    ///
    /// A message carrying channel endpoints is taken only when the caller
    /// holds [`super::TOPOLOGY`], and the caller keeps holding it until the
    /// message is delivered or put back with [`Endpoint::unread`]. Putting one
    /// back re-adds edges to the graph the cycle check walks, and doing that
    /// while a send is walking is how two processes could build the cycle
    /// the check exists to refuse. A caller without the lock gets
    /// [`ReadError::NeedsTopology`], takes it, and asks again.
    ///
    /// # Errors
    ///
    /// [`ReadError`]. A message too large stays queued.
    pub(crate) fn read(
        &self,
        byte_capacity: usize,
        handle_capacity: usize,
        topology_held: bool,
    ) -> Result<ChannelMessage, ReadError> {
        let (taken, was_full) = {
            let mut inbox = self.inbox.lock();
            let was_full = inbox.is_full();
            // Decided under the same lock as the pop, so the message looked at
            // is the message taken.
            let needs_topology = !topology_held
                && inbox.iter().next().is_some_and(|head| {
                    head.bytes.len() <= byte_capacity
                        && head.handles.len() <= handle_capacity
                        && carries_endpoints(head)
                });
            if needs_topology {
                return Err(ReadError::NeedsTopology);
            }
            (inbox.pop_fitting(byte_capacity, handle_capacity), was_full)
        };
        // A reader that makes room in a full queue is what a blocked writer
        // is waiting for.
        if was_full
            && taken.is_ok()
            && let Some(peer) = self.peer.upgrade()
        {
            peer.waiters.wake_all();
        }
        match taken {
            Ok(message) => Ok(message),
            Err(ReceiveError::TooSmall { bytes, handles }) => {
                Err(ReadError::TooSmall { bytes, handles })
            }
            // Asked after the queue was found empty, so a peer that wrote its
            // last message and then closed is read to the end first.
            Err(ReceiveError::Empty) if self.peer_closed() => Err(ReadError::PeerClosed),
            Err(ReceiveError::Empty) => Err(ReadError::Empty),
        }
    }

    /// Put back a message [`Endpoint::read`] took and the caller could not
    /// deliver, at the head of the queue.
    ///
    /// Holding [`super::TOPOLOGY`] if the message carries endpoints, as
    /// [`Endpoint::read`] required when it was taken.
    ///
    /// Wakes this end's waiters, because the message is readable again and a
    /// second reader may have gone to sleep while it was out.
    pub(crate) fn unread(&self, message: ChannelMessage) {
        self.inbox.lock().unpop(message);
        self.waiters.wake_all();
    }

    /// Whether nobody holds the other end.
    pub(crate) fn peer_closed(&self) -> bool {
        self.peer.strong_count() == 0
    }

    /// Queue a packet with `observer` the next time a message is readable on
    /// this end or its peer closes, or at once if either already holds.
    ///
    /// `READABLE` and `PEER_CLOSED` only. `WRITABLE` depends on the peer's
    /// queue, and taking that lock while holding this end's would take the
    /// two in the opposite order from the peer registering the other way
    /// round; the caller refuses it before reaching here.
    ///
    /// # Errors
    ///
    /// [`PortError::Full`] when this end already holds
    /// [`super::port::MAX_OBSERVERS`] registrations.
    pub(crate) fn observe(&self, observer: Observer) -> Result<(), PortError> {
        let inbox = self.inbox.lock();
        let mut asserted = Signals::NONE;
        if !inbox.is_empty() {
            asserted = asserted | Signals::READABLE;
        }
        if self.peer_closed() {
            asserted = asserted | Signals::PEER_CLOSED;
        }
        if observer.wants(asserted) {
            drop(inbox);
            observer.fire(asserted);
            return Ok(());
        }
        let registered = register(&mut self.observers.lock(), observer);
        drop(inbox);
        registered
    }

    /// The queue woken when this end's signals may have changed.
    pub(crate) fn waiters(&self) -> &WaitQueue {
        &self.waiters
    }

    /// What a waiter on this end would see now.
    pub(crate) fn signals(&self) -> Signals {
        let mut signals = Signals::NONE;
        if !self.inbox.lock().is_empty() {
            signals = signals | Signals::READABLE;
        }
        match self.peer.upgrade() {
            None => signals | Signals::PEER_CLOSED,
            Some(peer) if !peer.inbox.lock().is_full() => signals | Signals::WRITABLE,
            Some(_) => signals,
        }
    }
}

/// Whether sending `carried` through `writer` would close a cycle.
///
/// The message lands in the writer's peer's inbox, so it closes a cycle
/// exactly when that peer is reachable from something it carries, following
/// each endpoint into the endpoints queued in its own inbox. The peer itself
/// among `carried` is the one-step case.
///
/// Call it holding [`super::TOPOLOGY`], and make the send before releasing
/// it: the answer is only about a graph nothing else is adding edges to.
pub(crate) fn check_carry(writer: &Endpoint, carried: Vec<Arc<Endpoint>>) -> Reach {
    // A closed peer is no cycle, and the write itself will say it is closed.
    let Some(peer) = writer.peer.upgrade() else {
        return Reach::Clear;
    };
    reaches(
        carried,
        identity,
        identity(&peer),
        queued_endpoints,
        MAX_WALK,
    )
}

/// An endpoint's address, which is how the walk tells endpoints apart. Stable
/// for as long as the walk holds the `Arc`, which it does until it returns.
fn identity(endpoint: &Arc<Endpoint>) -> usize {
    Arc::as_ptr(endpoint) as usize
}

/// The endpoints queued, unread, in `endpoint`'s inbox: the edges the cycle
/// walk follows.
///
/// The match is exhaustive on purpose. An object kind added later that can
/// hold other objects has to be followed here, or it is a way round the check,
/// and the compiler is what asks the question.
fn queued_endpoints(endpoint: &Arc<Endpoint>) -> Vec<Arc<Endpoint>> {
    let inbox = endpoint.inbox.lock();
    let mut distinct = BTreeSet::new();
    inbox
        .iter()
        .flat_map(|message| message.handles.iter())
        .filter_map(|(object, _)| match object {
            Object::Channel(queued) => Some(queued),
            Object::Vmo(_)
            | Object::Job(_)
            | Object::Device(_)
            | Object::Interrupt(_)
            | Object::IoMapping(_)
            | Object::Port(_) => None,
        })
        // Once each, and no more than the walk could use: an inbox can hold
        // two hundred and fifty-six messages of sixty-four handles, and
        // cloning sixteen thousand references under the topology lock to
        // find the walk was too far anyway would be the cost the bound is
        // there to prevent.
        .filter(|queued| distinct.insert(Arc::as_ptr(queued) as usize))
        .take(MAX_WALK + 1)
        .map(Arc::clone)
        .collect()
}

/// Whether a message carries a channel endpoint.
fn carries_endpoints(message: &ChannelMessage) -> bool {
    message
        .handles
        .iter()
        .any(|(object, _)| matches!(object, Object::Channel(_)))
}

impl Drop for Endpoint {
    /// Tell the peer it is alone, and free what was queued and never read,
    /// one level at a time.
    fn drop(&mut self) {
        if let Some(peer) = self.peer.upgrade() {
            // Under the survivor's inbox lock, the lock its registrations are
            // made under, so one made a moment ago is found here.
            let fired = {
                let _inbox = peer.inbox.lock();
                triggered(&mut peer.observers.lock(), Signals::PEER_CLOSED)
            };
            for observer in fired {
                observer.fire(Signals::PEER_CLOSED);
            }
            peer.waiters.wake_all();
        }
        let unread = self.inbox.get_mut().drain();
        dispose(
            unread
                .into_iter()
                .flat_map(|message| message.handles.into_iter().map(|(object, _)| object)),
        );
    }
}
