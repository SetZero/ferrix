//! The audio core's half of one card's conversation.
//!
//! [`judge`] takes the driver's HELLO and decides what the card publishes:
//! the first stream that plays [`crate::pcm::VERSION_1`]'s configuration, and
//! nothing else in version 1, since capture is not in it (`docs/AUDIO.md`
//! §7). A [`Session`] then takes every message from the driver through
//! [`Session::receive`], which accepts ELAPSED and HALTED about the published
//! stream while the card runs and STOPPED after STOP. Anything else is
//! [`Refusal::Protocol`], after which the session is broken and refuses
//! everything, and the glue quiesces the driver, as it quiesces an input
//! driver that lies.

use ferrix_native_abi::rights::Rights;

use crate::message::{
    DIRECTION_CAPTURE, Hello, MAX_PUBLISHED, MAX_STREAMS, Message, Offer, PORT_RIGHTS, Published,
    RATES_DEFINED, Ready, Refusal, Submit, VERSION,
};
use crate::pcm::{Config, Effects, Stream, VERSION_1};
use ferrix_linux_abi::sound::FORMAT_LAST;

/// What the core publishes of a card: the device's stream number and its
/// configuration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Publication {
    /// The device's stream number.
    pub stream: u32,
    /// Its one configuration.
    pub config: Config,
    /// How many of the device's streams were left out, for the boot line.
    pub left_out: u32,
}

impl Publication {
    /// READY for this publication on card `card`. The stream's buffer VMO
    /// rides with it.
    #[must_use]
    pub fn ready(&self, card: u32) -> Ready {
        let mut streams = [Published::default(); MAX_PUBLISHED];
        if let Some(first) = streams.first_mut() {
            *first = Published {
                stream: self.stream,
                rate: self.config.rate,
                // `FORMAT_*` numbers are below 64.
                format: self.config.format as u8,
                channels: self.config.channels as u8,
                period_bytes: self.config.period_bytes(),
                buffer_bytes: self.config.buffer_bytes(),
            };
        }
        Ready {
            card,
            published: 1,
            streams,
        }
    }
}

fn valid(offer: &Offer) -> bool {
    offer.direction <= DIRECTION_CAPTURE
        && offer.channels_min != 0
        && offer.channels_min <= offer.channels_max
        && offer.rates & !RATES_DEFINED == 0
        && offer.formats >> (FORMAT_LAST + 1) == 0
}

/// Judge a HELLO that came with handles of `rights`.
///
/// # Errors
///
/// [`Refusal::Version`] for another protocol version, [`Refusal::Hello`]
/// for handles other than one port with exactly [`PORT_RIGHTS`], a stream
/// count of zero or over [`MAX_STREAMS`], or an offer no device could make
/// (a direction that does not exist, an empty channel range, a rate or
/// format bit that is not defined), and [`Refusal::Nothing`] when no stream
/// plays version 1's configuration.
pub fn judge(hello: &Hello, rights: &[Rights]) -> Result<Publication, Refusal> {
    if hello.version != VERSION {
        return Err(Refusal::Version);
    }
    if rights != [PORT_RIGHTS] {
        return Err(Refusal::Hello);
    }
    let streams = hello.streams as usize;
    if streams == 0 || streams > MAX_STREAMS {
        return Err(Refusal::Hello);
    }
    let offers = hello.offers.get(..streams).ok_or(Refusal::Hello)?;
    if !offers.iter().all(valid) {
        return Err(Refusal::Hello);
    }
    let config = VERSION_1;
    let stream = offers
        .iter()
        .position(|offer| offer.offers(config.format, config.rate, config.channels))
        .ok_or(Refusal::Nothing)?;
    Ok(Publication {
        // Below `MAX_STREAMS`.
        stream: stream as u32,
        config,
        left_out: hello.streams - 1,
    })
}

/// What a message from the driver came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Received {
    /// It moved the stream; the effects say what next.
    Stream,
    /// The driver answered STOP: the card is gone.
    Stopped,
}

/// One card's conversation, after READY.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Session {
    publication: Publication,
    stopping: bool,
    broken: bool,
}

impl Session {
    /// A session for an accepted publication.
    #[must_use]
    pub const fn new(publication: Publication) -> Self {
        Self {
            publication,
            stopping: false,
            broken: false,
        }
    }

    /// What it publishes.
    #[must_use]
    pub const fn publication(&self) -> &Publication {
        &self.publication
    }

    /// Whether the driver lied and the session refuses everything.
    #[must_use]
    pub const fn is_broken(&self) -> bool {
        self.broken
    }

    /// The core sends STOP.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// The messages `effects` asks to send, in order: every SUBMIT, then
    /// HALT.
    pub fn messages<'a>(&self, effects: &'a Effects) -> impl Iterator<Item = Message> + 'a {
        let stream = self.publication.stream;
        effects
            .submits()
            .map(move |submit| {
                Message::Submit(Submit {
                    stream,
                    sequence: submit.sequence,
                    offset: submit.offset,
                    bytes: submit.bytes,
                })
            })
            .chain(effects.halt.then_some(Message::Halt { stream }))
    }

    /// Take a message from the driver, moving `stream` by it at `now`.
    ///
    /// # Errors
    ///
    /// [`Refusal::Protocol`] for a message out of turn, one about a stream
    /// not published, or a report the stream refuses as a lie. The session
    /// is broken from then on.
    pub fn receive(
        &mut self,
        message: &Message,
        stream: &mut Stream,
        now: u64,
        effects: &mut Effects,
    ) -> Result<Received, Refusal> {
        if self.broken {
            return Err(Refusal::Protocol);
        }
        let published = self.publication.stream;
        let result = match *message {
            Message::Elapsed(elapsed) if !self.stopping && elapsed.stream == published => stream
                .elapsed(elapsed.sequence, elapsed.played, now, effects)
                .map(|()| Received::Stream),
            Message::Halted {
                stream: which,
                unplayed,
            } if !self.stopping && which == published => {
                stream.halted(unplayed, effects).map(|()| Received::Stream)
            }
            Message::Stopped if self.stopping => return Ok(Received::Stopped),
            _ => {
                self.broken = true;
                return Err(Refusal::Protocol);
            }
        };
        result.map_err(|_| {
            self.broken = true;
            Refusal::Protocol
        })
    }
}
