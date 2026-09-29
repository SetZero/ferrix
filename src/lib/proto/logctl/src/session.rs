//! The kernel's half of one reader's conversation.
//!
//! The driver sends READ; the kernel answers it with DATA once the log has a
//! byte the reader has not had, and not before; then the driver may send the
//! next. One READ outstanding at a time keeps the channel's queue to one
//! message whatever the driver does, and lets the driver pace the log to its
//! own link. Any other message from the driver -- a second READ, a DATA or a
//! REFUSED, which are the kernel's -- is [`Refusal::Protocol`], after which
//! the session refuses everything and the glue ends the claim.
//!
//! What the log lost is counted from the reader's cursor, which the glue
//! keeps. A read that found only lost bytes -- every one it copied replaced
//! while it copied -- has nothing to send, so its count is carried into the
//! next DATA rather than dropped.

use crate::message::{MAX_DATA, Message, Refusal};

/// One reader's conversation.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Session {
    /// The room the outstanding READ asked for, capped at [`MAX_DATA`].
    wanted: Option<usize>,
    /// Bytes lost since the last DATA, not yet told.
    lost: u64,
    /// Whether the driver broke the protocol.
    broken: bool,
}

impl Session {
    /// A reader that has asked for nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Session {
            wanted: None,
            lost: 0,
            broken: false,
        }
    }

    /// Take a message from the driver.
    ///
    /// # Errors
    ///
    /// [`Refusal::Protocol`] for anything but a READ while none is
    /// outstanding, and for everything once that has happened.
    pub fn receive(&mut self, message: &Message<'_>) -> Result<(), Refusal> {
        match message {
            Message::Read { max } if !self.broken && self.wanted.is_none() => {
                let max = usize::try_from(*max).unwrap_or(MAX_DATA);
                self.wanted = Some(max.min(MAX_DATA));
                Ok(())
            }
            _ => {
                self.broken = true;
                Err(Refusal::Protocol)
            }
        }
    }

    /// How many bytes the outstanding READ has room for, if one is.
    #[must_use]
    pub const fn wanted(&self) -> Option<usize> {
        self.wanted
    }

    /// Say what a read of the log for the outstanding READ found: `copied`
    /// bytes, after `lost` it skipped. Answers the lost count the DATA
    /// carrying those bytes says, and ends the READ, when there is a byte to
    /// send; otherwise carries `lost` and keeps the READ outstanding.
    pub fn read(&mut self, copied: usize, lost: u64) -> Option<u64> {
        self.lost = self.lost.saturating_add(lost);
        if copied == 0 || self.wanted.is_none() {
            return None;
        }
        self.wanted = None;
        Some(core::mem::take(&mut self.lost))
    }
}
