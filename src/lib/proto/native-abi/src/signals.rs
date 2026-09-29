//! The observable state of an object.
//!
//! A signal is a level, not an event: `READABLE` is asserted for as long as a
//! channel holds a message, not once per message. That is what lets a port
//! wait be armed *after* the state it waits for came true and still fire —
//! the kernel checks the level when the wait is registered — and it is why
//! one driver thread can service many sources without a lost wake-up between
//! looking and arming.

use core::ops::BitOr;

/// A set of signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct Signals(
    /// The flag word. Only the bits in [`Signals::ALL`] are defined.
    pub u32,
);

impl Signals {
    /// Nothing asserted.
    pub const NONE: Signals = Signals(0);
    /// Something can be read without waiting: a channel holds a message, a
    /// port holds a packet, an interrupt has fired and not been acknowledged.
    pub const READABLE: Signals = Signals(1 << 0);
    /// A write would not wait: a channel's peer has room in its queue.
    pub const WRITABLE: Signals = Signals(1 << 1);
    /// The other end of a channel has been closed. Messages already queued
    /// can still be read; nothing new will arrive.
    pub const PEER_CLOSED: Signals = Signals(1 << 2);
    /// A process or job has ended. Asserted once and never cleared.
    pub const TERMINATED: Signals = Signals(1 << 3);
    /// A job holds no process that has not ended, neither of its own nor in a
    /// job beneath it: what `cgroup.events` reports as `populated 0`
    /// (`docs/CGROUPS.md` §5). Unlike [`Signals::TERMINATED`] it is a level
    /// that comes and goes: cleared when a process arrives, asserted again
    /// when the last one ends. A job nothing was ever put in asserts it.
    pub const EMPTY: Signals = Signals(1 << 4);

    /// Every defined signal.
    pub const ALL: Signals = Signals(0x1F);

    /// Whether any signal in `other` is also in `self`.
    ///
    /// *Any*, not all, because that is what a waiter means: wake me when
    /// there is a message *or* the peer has gone.
    #[must_use]
    pub const fn intersects(self, other: Signals) -> bool {
        self.0 & other.0 != 0
    }

    /// The signals in both.
    #[must_use]
    pub const fn intersection(self, other: Signals) -> Signals {
        Signals(self.0 & other.0)
    }

    /// A signal set read from an argument register.
    ///
    /// `None` for an undefined bit, for the reason
    /// [`crate::rights::Requested::from_register`] refuses one: a wait for a
    /// signal this kernel never asserts is a wait that never ends.
    #[must_use]
    pub const fn from_register(value: u64) -> Option<Signals> {
        if value & !(Signals::ALL.0 as u64) == 0 {
            Some(Signals(value as u32))
        } else {
            None
        }
    }
}

impl BitOr for Signals {
    type Output = Signals;

    fn bitor(self, other: Signals) -> Signals {
        Signals(self.0 | other.0)
    }
}
