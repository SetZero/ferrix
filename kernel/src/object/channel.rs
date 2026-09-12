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

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES};
use ferrix_objects::message::{Limits, Message, MessageQueue, ReceiveError, SendError};
use ferrix_sync::SpinLock;

use super::{Transfer, dispose};

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
            });
            let this = Endpoint {
                peer: Arc::downgrade(&other),
                inbox: SpinLock::new(MessageQueue::new(LIMITS)),
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
        let refused = {
            let mut inbox = peer.inbox.lock();
            if !inbox.accepts(bytes.len(), handle_count) {
                return Err(WriteFailure::TooBig);
            }
            if inbox.is_full() {
                return Err(WriteFailure::Full);
            }
            let handles = take().map_err(WriteFailure::Take)?;
            inbox.push(Message { bytes, handles }).err()
        };
        // Checked above under the same lock, so this is unreachable; if it
        // ever were reached, the handles are already out of the sender's
        // table and the only safe thing left is to free them, after the lock.
        match refused {
            None => Ok(()),
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
    /// # Errors
    ///
    /// [`ReadError`]. A message too large stays queued.
    pub(crate) fn read(
        &self,
        byte_capacity: usize,
        handle_capacity: usize,
    ) -> Result<ChannelMessage, ReadError> {
        let taken = self
            .inbox
            .lock()
            .pop_fitting(byte_capacity, handle_capacity);
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
    pub(crate) fn unread(&self, message: ChannelMessage) {
        self.inbox.lock().unpop(message);
    }

    /// Whether nobody holds the other end.
    pub(crate) fn peer_closed(&self) -> bool {
        self.peer.strong_count() == 0
    }

    /// Whether `other` is this endpoint's peer.
    pub(crate) fn is_peer(&self, other: &Arc<Endpoint>) -> bool {
        core::ptr::eq(self.peer.as_ptr(), Arc::as_ptr(other))
    }

    /// What a waiter on this end would see now.
    #[expect(
        dead_code,
        reason = "ports and object waits are the next handlers, and they read signals; \
                  defining the level here keeps it next to the state it describes"
    )]
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

impl Drop for Endpoint {
    /// Free what was queued and never read, one level at a time.
    fn drop(&mut self) {
        let unread = self.inbox.get_mut().drain();
        dispose(
            unread
                .into_iter()
                .flat_map(|message| message.handles.into_iter().map(|(object, _)| object)),
        );
    }
}
