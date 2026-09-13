//! Ports: an event queue a driver thread waits on.
//!
//! `docs/ARCHITECTURE.md` §3: how one driver thread services many sources. A
//! port holds packets. A program queues its own with `port_queue`; the kernel
//! queues one for each `object_wait_async` registration whose signals come
//! true; and `port_wait` takes the next, oldest first.
//!
//! # A registration is one-shot, and held by what it watches
//!
//! An [`Observer`] lives in the object it watches, under the same lock the
//! object takes to change the state it reports. Registering checks the state
//! under that lock and fires at once if a wanted signal is already asserted,
//! so a change can land neither between looking and registering nor between
//! registering and the change: whichever takes the lock second sees the other.
//! It fires once and is gone, which is what lets a driver loop — wait, handle,
//! register again — without packets piling up for a state it has not yet
//! dealt with.
//!
//! # User packets are bounded; signal packets are not
//!
//! A program that queues without reading is told to wait at
//! [`PORT_CAPACITY`]. A signal packet is queued regardless: refusing it would
//! lose the event its registration was waiting for, and the only way to make
//! one is a registration that already exists, which each object caps at
//! [`MAX_OBSERVERS`].
//!
//! # The queue's lock is interrupt-safe
//!
//! So that an interrupt handler can queue a packet for an interrupt bound to
//! the port. The handler wakes the port's waiters once it has let go of this
//! lock; see `object::interrupt`.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{PACKET_INTERRUPT, PACKET_SIGNAL, PACKET_USER, PortPacket};
use ferrix_sync::IrqSpinLock;

use crate::arch;
use crate::sched::WaitQueue;

/// The most user packets a port holds unread.
pub(crate) const PORT_CAPACITY: usize = 1024;

/// The most registrations one object holds at once.
pub(crate) const MAX_OBSERVERS: usize = 64;

/// Why a port or an object refused a packet or a registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortError {
    /// The port is full of user packets, or the object of registrations.
    Full,
}

/// An event queue.
#[derive(Debug)]
pub(crate) struct Port {
    /// Packets, oldest first. Reserved at its capacity, so a push below it
    /// never allocates.
    queue: IrqSpinLock<VecDeque<PortPacket>, arch::Irq>,
    /// Woken when a packet is queued.
    waiters: WaitQueue,
}

impl Port {
    /// An empty port.
    pub(crate) fn new() -> Arc<Port> {
        Arc::new(Port {
            queue: IrqSpinLock::new(VecDeque::with_capacity(PORT_CAPACITY)),
            waiters: WaitQueue::new(),
        })
    }

    /// Queue a program's own packet.
    ///
    /// # Errors
    ///
    /// [`PortError::Full`] at [`PORT_CAPACITY`].
    pub(crate) fn queue_user(&self, key: u64, data: [u64; 2]) -> Result<(), PortError> {
        {
            let mut queue = self.queue.lock();
            if queue.len() >= PORT_CAPACITY {
                return Err(PortError::Full);
            }
            queue.push_back(PortPacket {
                key,
                kind: PACKET_USER,
                signals: 0,
                data,
            });
        }
        self.waiters.wake_all();
        Ok(())
    }

    /// Queue the packet a registration produces. Never refused.
    fn queue_signal(&self, key: u64, asserted: Signals) {
        self.queue.lock().push_back(PortPacket {
            key,
            kind: PACKET_SIGNAL,
            signals: asserted.0,
            data: [0; 2],
        });
        self.waiters.wake_all();
    }

    /// Queue a bound interrupt's packet, from its interrupt handler.
    ///
    /// Touches only this queue's interrupt-safe lock and allocates nothing: a
    /// queue already at its reserved capacity drops the packet, and the
    /// interrupt stays pending, which a wait on the interrupt itself still
    /// sees. Wakes no waiter: the handler does that once it has let go of
    /// this lock and the interrupt's own. Returns whether the packet was
    /// queued.
    pub(crate) fn queue_from_interrupt(&self, key: u64, fired_at: u64) -> bool {
        let mut queue = self.queue.lock();
        if queue.len() >= queue.capacity() {
            return false;
        }
        queue.push_back(PortPacket {
            key,
            kind: PACKET_INTERRUPT,
            signals: 0,
            data: [fired_at, 0],
        });
        true
    }

    /// Queue a bound interrupt's packet from task context, for an interrupt
    /// that was already pending when it was bound.
    pub(crate) fn queue_interrupt(&self, key: u64, fired_at: u64) {
        self.queue.lock().push_back(PortPacket {
            key,
            kind: PACKET_INTERRUPT,
            signals: 0,
            data: [fired_at, 0],
        });
        self.waiters.wake_all();
    }

    /// Take the oldest packet.
    pub(crate) fn take(&self) -> Option<PortPacket> {
        self.queue.lock().pop_front()
    }

    /// Put back a packet [`Port::take`] gave and the caller could not deliver,
    /// ahead of everything else.
    pub(crate) fn put_back(&self, packet: PortPacket) {
        self.queue.lock().push_front(packet);
        self.waiters.wake_all();
    }

    /// Whether nothing is queued.
    pub(crate) fn is_empty(&self) -> bool {
        self.queue.lock().is_empty()
    }

    /// The queue woken when a packet arrives.
    pub(crate) fn waiters(&self) -> &WaitQueue {
        &self.waiters
    }
}

/// A one-shot request for a packet when signals come true.
#[derive(Debug)]
pub(crate) struct Observer {
    /// Where the packet goes. Weak, so a registration does not keep a port
    /// nobody holds alive.
    port: Weak<Port>,
    /// The key the packet carries.
    key: u64,
    /// The signals it waits for; any of them fires it.
    signals: Signals,
}

impl Observer {
    /// A registration for a packet on `port` with `key` when any of `signals`
    /// is asserted.
    pub(crate) fn new(port: &Arc<Port>, key: u64, signals: Signals) -> Observer {
        Observer {
            port: Arc::downgrade(port),
            key,
            signals,
        }
    }

    /// Whether `asserted` fires it.
    pub(crate) fn wants(&self, asserted: Signals) -> bool {
        self.signals.intersects(asserted)
    }

    /// Whether anyone still holds its port.
    fn is_live(&self) -> bool {
        self.port.strong_count() > 0
    }

    /// Queue its packet, reporting the wanted signals among `asserted`. Call
    /// with no lock held that the port's wake-up might need.
    pub(crate) fn fire(self, asserted: Signals) {
        if let Some(port) = self.port.upgrade() {
            port.queue_signal(self.key, asserted.intersection(self.signals));
        }
    }
}

/// Add `observer` to an object's list.
///
/// # Errors
///
/// [`PortError::Full`] at [`MAX_OBSERVERS`], counted after dropping any whose
/// port has gone.
pub(crate) fn register(list: &mut Vec<Observer>, observer: Observer) -> Result<(), PortError> {
    list.retain(Observer::is_live);
    if list.len() >= MAX_OBSERVERS {
        return Err(PortError::Full);
    }
    list.push(observer);
    Ok(())
}

/// Take out every registration in `list` that `asserted` fires, for the caller
/// to fire once it has let go of the object's lock.
pub(crate) fn triggered(list: &mut Vec<Observer>, asserted: Signals) -> Vec<Observer> {
    let (fire, keep): (Vec<Observer>, Vec<Observer>) = core::mem::take(list)
        .into_iter()
        .partition(|observer| observer.wants(asserted));
    *list = keep;
    fire
}
