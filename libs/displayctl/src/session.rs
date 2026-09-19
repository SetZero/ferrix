//! The display core's half of the conversation, as a state machine.
//!
//! A [`Session`] starts from an accepted HELLO. The core asks it for each
//! message it wants to send — [`Session::attach`], [`Session::scanout`],
//! [`Session::flush`], [`Session::detach`], [`Session::stop`] — and the
//! session refuses a request that would put the conversation in a state the
//! protocol does not have, such as showing a buffer the device has not
//! attached, or detaching one a scanout still shows. Every message from the
//! driver goes through [`Session::receive`], which accepts only the reply the
//! session is waiting for: an ATTACHED for a buffer being attached, a FLIPPED
//! for the oldest flush in flight, a DETACHED for a buffer being detached, a
//! STOPPED after STOP. Anything else is [`Refusal::Protocol`], after which
//! the session is broken and refuses everything, and the glue quiesces the
//! driver the way it quiesces a block driver that lies.
//!
//! Capacity is fixed, so the kernel side allocates nothing per message:
//! [`MAX_BUFFERS`] buffers and [`MAX_IN_FLIGHT`] flushes waiting for FLIPPED.

use crate::message::AttachObject;
use crate::message::{
    Attach, AttachError, Hello, MAX_SCANOUTS, Message, Rect, Refusal, ScanoutMode, Status,
};
use ferrix_native_abi::rights::Rights;

/// The most buffers one session tracks, attached or on their way.
pub const MAX_BUFFERS: usize = 32;

/// The most flushes waiting for FLIPPED at once.
pub const MAX_IN_FLIGHT: usize = 8;

/// A request the core may not make now. The conversation is unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RequestError {
    /// The session is broken or stopping.
    Closed,
    /// The ATTACH is not one a driver can act on.
    Attach(AttachError),
    /// A buffer with that id is already tracked.
    InUse,
    /// [`MAX_BUFFERS`] are already tracked.
    Full,
    /// No attached buffer has that id.
    NotAttached,
    /// The buffer is on a scanout or has a flush in flight.
    Busy,
    /// [`MAX_IN_FLIGHT`] flushes are already waiting.
    TooManyFlushes,
    /// The scanout index is not below the HELLO's count.
    NoSuchScanout,
    /// The rectangle is empty or runs past the buffer.
    Rect,
}

/// What a driver's message meant, once accepted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// An ATTACH finished; on success the buffer may be shown and flushed.
    Attached {
        /// The buffer.
        buffer: u32,
        /// How it went.
        status: Status,
    },
    /// A flush finished: the page flip's completion event is due.
    Flipped {
        /// The flush.
        sequence: u64,
        /// The buffer it flushed.
        buffer: u32,
        /// How it went.
        status: Status,
    },
    /// A DETACH finished. On failure the range stays the device's, and the
    /// core must never hand it out again.
    Detached {
        /// The buffer.
        buffer: u32,
        /// How it went.
        status: Status,
    },
    /// The driver stopped.
    Stopped,
}

/// What the session remembers of a buffer, whichever way it was made.
///
/// Its id and its shape, which is all the rules here are about: a rectangle
/// shown or flushed has to be inside it. Where its pixels *are* -- a range
/// of the card VMO, or a resource the device already holds -- is the
/// driver's business once the buffer is made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Buffer {
    /// Its id.
    pub buffer: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    Free,
    Attaching(Buffer),
    Attached(Buffer),
    Detaching(Buffer),
    /// A buffer whose DETACH the device refused: its pages may still be the
    /// device's, so the id and the slot are never used again.
    Lost(u32),
}

impl Slot {
    const fn buffer(&self) -> Option<u32> {
        match self {
            Self::Free => None,
            Self::Attaching(attach) | Self::Attached(attach) | Self::Detaching(attach) => {
                Some(attach.buffer)
            }
            Self::Lost(buffer) => Some(*buffer),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct InFlight {
    sequence: u64,
    buffer: u32,
}

/// The core's side of one driver's conversation.
#[derive(Clone, Debug)]
pub struct Session {
    modes: [ScanoutMode; MAX_SCANOUTS],
    scanouts: usize,
    card_bytes: u64,
    slots: [Slot; MAX_BUFFERS],
    shown: [u32; MAX_SCANOUTS],
    flushes: [InFlight; MAX_IN_FLIGHT],
    flush_head: usize,
    flush_count: usize,
    next_sequence: u64,
    stopping: bool,
    broken: bool,
}

impl Session {
    /// Accept a driver's HELLO, with the rights of the handles it came with,
    /// for a card VMO of `card_bytes`.
    pub fn accept(
        hello: &Hello,
        handle_rights: &[Rights],
        card_bytes: u64,
    ) -> Result<Self, Refusal> {
        hello.validate(handle_rights)?;
        Ok(Self {
            modes: hello.modes,
            scanouts: usize::from(hello.scanouts),
            card_bytes,
            slots: [Slot::Free; MAX_BUFFERS],
            shown: [0; MAX_SCANOUTS],
            flushes: [InFlight {
                sequence: 0,
                buffer: 0,
            }; MAX_IN_FLIGHT],
            flush_head: 0,
            flush_count: 0,
            next_sequence: 1,
            stopping: false,
            broken: false,
        })
    }

    /// The scanouts' preferred modes, as the driver reported them.
    #[must_use]
    pub fn modes(&self) -> &[ScanoutMode] {
        self.modes.get(..self.scanouts).unwrap_or(&[])
    }

    /// Whether the driver broke the protocol.
    #[must_use]
    pub const fn is_broken(&self) -> bool {
        self.broken
    }

    fn open(&self) -> Result<(), RequestError> {
        if self.broken || self.stopping {
            return Err(RequestError::Closed);
        }
        Ok(())
    }

    fn find(&self, buffer: u32) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.buffer() == Some(buffer))
    }

    fn attached(&self, buffer: u32) -> Result<Buffer, RequestError> {
        match self.find(buffer).and_then(|index| self.slots.get(index)) {
            Some(Slot::Attached(attach)) => Ok(*attach),
            _ => Err(RequestError::NotAttached),
        }
    }

    fn in_flight(&self) -> impl Iterator<Item = &InFlight> {
        (0..self.flush_count)
            .filter_map(|offset| self.flushes.get((self.flush_head + offset) % MAX_IN_FLIGHT))
    }

    /// Whether `buffer`'s ATTACH is waiting for ATTACHED.
    #[must_use]
    pub fn is_attaching(&self, buffer: u32) -> bool {
        self.find(buffer)
            .and_then(|index| self.slots.get(index))
            .is_some_and(|slot| matches!(slot, Slot::Attaching(_)))
    }

    /// ATTACH a buffer.
    pub fn attach(&mut self, attach: Attach) -> Result<Message, RequestError> {
        self.open()?;
        attach
            .validate(self.card_bytes)
            .map_err(RequestError::Attach)?;
        if self.find(attach.buffer).is_some() {
            return Err(RequestError::InUse);
        }
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| **slot == Slot::Free)
            .ok_or(RequestError::Full)?;
        *slot = Slot::Attaching(Buffer {
            buffer: attach.buffer,
            width: attach.width,
            height: attach.height,
        });
        Ok(Message::Attach(attach))
    }

    /// `ATTACH_OBJ` a buffer whose pixels the device holds already.
    ///
    /// The same rules as [`Session::attach`] but for the range, which such a
    /// buffer has not got: an id no live buffer has, a shape the device can
    /// show, and room in the table.
    ///
    /// # Errors
    ///
    /// A request the conversation has no room or no state for.
    pub fn attach_object(&mut self, attach: AttachObject) -> Result<Message, RequestError> {
        self.open()?;
        attach.validate().map_err(RequestError::Attach)?;
        if self.find(attach.buffer).is_some() {
            return Err(RequestError::InUse);
        }
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| **slot == Slot::Free)
            .ok_or(RequestError::Full)?;
        *slot = Slot::Attaching(Buffer {
            buffer: attach.buffer,
            width: attach.width,
            height: attach.height,
        });
        Ok(Message::AttachObject(attach))
    }

    /// Show `rect` of `buffer` on `scanout`, or with buffer 0, turn it off.
    pub fn scanout(
        &mut self,
        scanout: u32,
        buffer: u32,
        rect: Rect,
    ) -> Result<Message, RequestError> {
        self.open()?;
        let index = scanout as usize;
        if index >= self.scanouts {
            return Err(RequestError::NoSuchScanout);
        }
        if buffer != 0 {
            let attach = self.attached(buffer)?;
            if !rect.inside(attach.width, attach.height) {
                return Err(RequestError::Rect);
            }
        }
        let shown = self
            .shown
            .get_mut(index)
            .ok_or(RequestError::NoSuchScanout)?;
        *shown = buffer;
        Ok(Message::Scanout {
            scanout,
            buffer,
            rect,
        })
    }

    /// FLUSH `rect` of `buffer`.
    pub fn flush(&mut self, buffer: u32, rect: Rect) -> Result<Message, RequestError> {
        self.open()?;
        let attach = self.attached(buffer)?;
        if !rect.inside(attach.width, attach.height) {
            return Err(RequestError::Rect);
        }
        if self.flush_count >= MAX_IN_FLIGHT {
            return Err(RequestError::TooManyFlushes);
        }
        let sequence = self.next_sequence;
        let at = (self.flush_head + self.flush_count) % MAX_IN_FLIGHT;
        let entry = self
            .flushes
            .get_mut(at)
            .ok_or(RequestError::TooManyFlushes)?;
        *entry = InFlight { sequence, buffer };
        self.flush_count += 1;
        self.next_sequence = sequence.wrapping_add(1).max(1);
        Ok(Message::Flush {
            buffer,
            sequence,
            rect,
        })
    }

    /// DETACH a buffer no scanout shows and no flush is waiting on.
    pub fn detach(&mut self, buffer: u32) -> Result<Message, RequestError> {
        self.open()?;
        let attach = self.attached(buffer)?;
        if self.shown.contains(&buffer) || self.in_flight().any(|flush| flush.buffer == buffer) {
            return Err(RequestError::Busy);
        }
        let index = self.find(buffer).ok_or(RequestError::NotAttached)?;
        let slot = self.slots.get_mut(index).ok_or(RequestError::NotAttached)?;
        *slot = Slot::Detaching(attach);
        Ok(Message::Detach { buffer })
    }

    /// STOP the driver. Every request after this is refused.
    pub fn stop(&mut self) -> Result<Message, RequestError> {
        self.open()?;
        self.stopping = true;
        Ok(Message::Stop)
    }

    fn violation(&mut self) -> Refusal {
        self.broken = true;
        Refusal::Protocol
    }

    /// Accept a message from the driver, if it is one the session waits for.
    pub fn receive(&mut self, message: &Message) -> Result<Event, Refusal> {
        if self.broken {
            return Err(Refusal::Protocol);
        }
        match *message {
            Message::Attached { buffer, status } => self.on_attached(buffer, status),
            Message::Flipped { sequence, status } => self.on_flipped(sequence, status),
            Message::Detached { buffer, status } => self.on_detached(buffer, status),
            Message::Stopped if self.stopping => Ok(Event::Stopped),
            _ => Err(self.violation()),
        }
    }

    fn on_attached(&mut self, buffer: u32, status: Status) -> Result<Event, Refusal> {
        let Some(slot) = self
            .find(buffer)
            .and_then(|index| self.slots.get_mut(index))
        else {
            return Err(self.violation());
        };
        let Slot::Attaching(attach) = *slot else {
            return Err(self.violation());
        };
        *slot = if status == Status::Ok {
            Slot::Attached(attach)
        } else {
            Slot::Free
        };
        Ok(Event::Attached { buffer, status })
    }

    fn on_flipped(&mut self, sequence: u64, status: Status) -> Result<Event, Refusal> {
        let oldest = self.in_flight().next().copied();
        match oldest {
            Some(flush) if flush.sequence == sequence => {
                self.flush_head = (self.flush_head + 1) % MAX_IN_FLIGHT;
                self.flush_count -= 1;
                Ok(Event::Flipped {
                    sequence,
                    buffer: flush.buffer,
                    status,
                })
            }
            _ => Err(self.violation()),
        }
    }

    fn on_detached(&mut self, buffer: u32, status: Status) -> Result<Event, Refusal> {
        let Some(slot) = self
            .find(buffer)
            .and_then(|index| self.slots.get_mut(index))
        else {
            return Err(self.violation());
        };
        let Slot::Detaching(_) = *slot else {
            return Err(self.violation());
        };
        *slot = if status == Status::Ok {
            Slot::Free
        } else {
            Slot::Lost(buffer)
        };
        Ok(Event::Detached { buffer, status })
    }
}
