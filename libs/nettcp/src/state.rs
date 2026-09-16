//! The eleven states of RFC 9293, and why a connection left one.

/// Where a connection is in its life.
///
/// The names are the standard's, and the numbers `/proc/net/tcp` reports are
/// [`State::procfs_code`], which is a different order and is Linux's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// No connection: either not started, or finished and forgotten.
    Closed,
    /// Waiting for a connection request. A listener, not a connection.
    Listen,
    /// A connection request has been sent and not yet answered.
    SynSent,
    /// A connection request arrived and was answered; its answer is awaited.
    SynReceived,
    /// Open in both directions.
    Established,
    /// This end has closed; the peer has not acknowledged it or closed.
    FinWait1,
    /// This end's close was acknowledged; the peer has not closed.
    FinWait2,
    /// The peer has closed; this end has not.
    CloseWait,
    /// Both ends closed at once; this end's close is unacknowledged.
    Closing,
    /// The peer closed, this end answered, and the answer is unacknowledged.
    LastAck,
    /// Both ends closed and acknowledged; waiting out the old segments.
    TimeWait,
}

impl State {
    /// Whether data may still be sent.
    #[must_use]
    pub const fn can_send(self) -> bool {
        matches!(self, State::Established | State::CloseWait)
    }

    /// Whether data may still arrive.
    #[must_use]
    pub const fn can_receive(self) -> bool {
        matches!(self, State::Established | State::FinWait1 | State::FinWait2)
    }

    /// Whether the three-way handshake has finished.
    #[must_use]
    pub const fn is_synchronised(self) -> bool {
        !matches!(
            self,
            State::Closed | State::Listen | State::SynSent | State::SynReceived
        )
    }

    /// The number this state is reported as in `/proc/net/tcp`.
    ///
    /// Linux's `TCP_ESTABLISHED` is 1 and the rest follow its own enumeration,
    /// which is not the order above; `netstat` and `ss` read these.
    #[must_use]
    pub const fn procfs_code(self) -> u8 {
        match self {
            State::Established => 1,
            State::SynSent => 2,
            State::SynReceived => 3,
            State::FinWait1 => 4,
            State::FinWait2 => 5,
            State::TimeWait => 6,
            State::Closed => 7,
            State::CloseWait => 8,
            State::LastAck => 9,
            State::Listen => 10,
            State::Closing => 11,
        }
    }
}

/// Why a connection stopped being usable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failure {
    /// The peer answered the connection request with a reset.
    Refused,
    /// The peer reset an open connection.
    Reset,
    /// Nothing was acknowledged for long enough to give up.
    TimedOut,
    /// The peer's segments could not be made sense of.
    Protocol,
}
