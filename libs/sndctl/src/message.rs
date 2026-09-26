//! The messages, as bytes.
//!
//! Every message starts with its type and its length, four bytes each, and has
//! exactly the length its type fixes, in `libs/inputctl`'s shape. Handles ride
//! in the channel message's handle array; the glue reads their rights, and
//! [`PORT_RIGHTS`] and [`BUFFER_RIGHTS`] say what they must be.
//!
//! ```text
//! HELLO    driver -> core, 184 bytes, handles [driver port]
//!   8 version u16   10 reserved u16   12 location u32
//!   16 streams u32   20 reserved u32
//!   24 stream 0: direction u8, channels_min u8, channels_max u8, reserved u8,
//!                rates u32 (bits over RATES_HZ), formats u64 (FORMAT_* bits)
//!      ... MAX_STREAMS (10) of them, 16 bytes each; past `streams`, zero
//! READY    core -> driver, 56 bytes, handles [core port, one buffer VMO per
//!          published stream, in order]
//!   8 card u32   12 published u32
//!   16 published stream 0: stream u32, rate u32, format u8, channels u8,
//!                          reserved u16, period_bytes u32, buffer_bytes u32
//!      ... MAX_PUBLISHED (2) of them, 20 bytes each; past `published`, zero
//! REFUSED  core -> driver, 12 bytes: 8 reason u32
//! SUBMIT   core -> driver, 24 bytes
//!   8 stream u32   12 sequence u32   16 offset u32   20 bytes u32
//! ELAPSED  driver -> core, 24 bytes
//!   8 stream u32   12 sequence u32   16 played u32 (0 or 1)
//!   20 latency_bytes u32
//! HALT     core -> driver, 16 bytes: 8 stream u32   12 reserved u32
//! HALTED   driver -> core, 16 bytes: 8 stream u32   12 unplayed u32
//! STOP, STOPPED               8 bytes
//! ```
//!
//! HELLO says what the device offers in the core's terms, not the device's:
//! formats as ALSA's `FORMAT_*` bit numbers, rates as bits over this crate's
//! [`RATES_HZ`], so that a driver for another device (`docs/AUDIO.md` §7,
//! the DK1) speaks the same protocol. The driver translates.
//!
//! Reserved bytes are written as zero and a message with any of them set is
//! malformed, so they can be given a meaning later without an old reader
//! misreading them. So are the entries past a count.

use ::core::fmt;

use ferrix_native_abi::rights::Rights;

/// The protocol version this crate speaks.
pub const VERSION: u16 = 1;

/// HELLO's type.
pub const HELLO: u32 = 1;
/// READY's type.
pub const READY: u32 = 2;
/// REFUSED's type.
pub const REFUSED: u32 = 3;
/// SUBMIT's type.
pub const SUBMIT: u32 = 4;
/// ELAPSED's type.
pub const ELAPSED: u32 = 5;
/// HALT's type.
pub const HALT: u32 = 6;
/// HALTED's type.
pub const HALTED: u32 = 7;
/// STOP's type.
pub const STOP: u32 = 8;
/// STOPPED's type.
pub const STOPPED: u32 = 9;

/// Bytes of the type and length, and all of STOP and STOPPED.
pub const HEADER_BYTES: usize = 8;

/// The most streams HELLO describes: QEMU's limit, and `libs/virtio::snd`'s.
pub const MAX_STREAMS: usize = 10;
/// The most streams the core publishes on one card: a playback and a capture.
pub const MAX_PUBLISHED: usize = 2;

/// The rates a stream may offer, one bit each in HELLO, in Hz. The same
/// fourteen, in the same order, as virtio-snd's enum.
pub const RATES_HZ: [u32; 14] = [
    5512, 8000, 11025, 16000, 22050, 32000, 44100, 48000, 64000, 88200, 96000, 176_400, 192_000,
    384_000,
];

/// The bit for `hz` in HELLO's rates, if [`RATES_HZ`] has it.
#[must_use]
pub fn rate_bit(hz: u32) -> Option<u32> {
    RATES_HZ
        .iter()
        .position(|rate| *rate == hz)
        .and_then(|index| u32::try_from(index).ok())
        .map(|index| 1 << index)
}

/// Every rate bit defined.
pub const RATES_DEFINED: u32 = (1 << RATES_HZ.len()) - 1;

/// A stream that plays, in HELLO.
pub const DIRECTION_PLAYBACK: u8 = 0;
/// A stream that records.
pub const DIRECTION_CAPTURE: u8 = 1;

/// Bytes of one stream in HELLO.
pub const STREAM_BYTES: usize = 16;
/// Bytes of HELLO.
pub const HELLO_BYTES: usize = 24 + MAX_STREAMS * STREAM_BYTES;
/// Bytes of one published stream in READY.
pub const PUBLISHED_BYTES: usize = 20;
/// Bytes of READY.
pub const READY_BYTES: usize = 16 + MAX_PUBLISHED * PUBLISHED_BYTES;
/// Bytes of REFUSED.
pub const REFUSED_BYTES: usize = 12;
/// Bytes of SUBMIT and ELAPSED.
pub const SUBMIT_BYTES: usize = 24;
/// Bytes of HALT and HALTED.
pub const HALT_BYTES: usize = 16;
/// Bytes of the longest message.
pub const MAX_BYTES: usize = HELLO_BYTES;

/// Exactly the rights the core holds the driver's port with.
pub const PORT_RIGHTS: Rights = Rights(Rights::WRITE.0 | Rights::TRANSFER.0);
/// Exactly the rights the driver is given a stream's buffer with: it reads
/// it, maps it and pins it for the device to read, and writes nothing.
pub const BUFFER_RIGHTS: Rights = Rights(Rights::READ.0 | Rights::MAP.0);

/// One stream in HELLO: what the device offers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Offer {
    /// `DIRECTION_*`.
    pub direction: u8,
    /// Fewest channels.
    pub channels_min: u8,
    /// Most channels.
    pub channels_max: u8,
    /// Bits over [`RATES_HZ`].
    pub rates: u32,
    /// `FORMAT_*` bits, bit `n` for format `n`.
    pub formats: u64,
}

impl Offer {
    /// Whether it plays `format` at `hz` with `channels` channels.
    #[must_use]
    pub fn offers(&self, format: u32, hz: u32, channels: u32) -> bool {
        self.direction == DIRECTION_PLAYBACK
            && format < 64
            && self.formats & (1 << format) != 0
            && rate_bit(hz).is_some_and(|bit| self.rates & bit != 0)
            && u32::from(self.channels_min) <= channels
            && channels <= u32::from(self.channels_max)
    }
}

/// HELLO: the driver describes its device.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Hello {
    /// The protocol version the driver speaks.
    pub version: u16,
    /// The device's location, as devmgr's START gave it.
    pub location: u32,
    /// How many of `offers` are the device's.
    pub streams: u32,
    /// Each stream's offer; zero past `streams`.
    pub offers: [Offer; MAX_STREAMS],
}

/// One published stream in READY: which of the device's streams, and its
/// one configuration.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Published {
    /// The device's stream number, an index into HELLO's offers.
    pub stream: u32,
    /// Frames a second.
    pub rate: u32,
    /// `FORMAT_*`.
    pub format: u8,
    /// Channels.
    pub channels: u8,
    /// Bytes in a period.
    pub period_bytes: u32,
    /// Bytes in the buffer, whose VMO rides with READY.
    pub buffer_bytes: u32,
}

/// READY: the core took the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Ready {
    /// The card's number, `C` in `controlCc`.
    pub card: u32,
    /// How many of `streams` are published.
    pub published: u32,
    /// The published streams; zero past `published`.
    pub streams: [Published; MAX_PUBLISHED],
}

/// SUBMIT: a range of a stream's buffer to play.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Submit {
    /// The device's stream number.
    pub stream: u32,
    /// Its sequence number.
    pub sequence: u32,
    /// Where it starts in the buffer, in bytes.
    pub offset: u32,
    /// Its length in bytes.
    pub bytes: u32,
}

/// ELAPSED: the device finished a submission.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Elapsed {
    /// The device's stream number.
    pub stream: u32,
    /// The submission's sequence number.
    pub sequence: u32,
    /// False when the device refused the buffer.
    pub played: bool,
    /// What the device reported still to play, in bytes.
    pub latency_bytes: u32,
}

/// Why the core refused a driver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// A protocol version the core does not speak.
    Version,
    /// A HELLO whose rights, stream count or offers are wrong.
    Hello,
    /// No stream offers a configuration the core publishes.
    Nothing,
    /// A message the conversation did not allow at that point, or a report
    /// about a submission that is not the one in flight.
    Protocol,
}

impl Refusal {
    const fn raw(self) -> u32 {
        match self {
            Refusal::Version => 1,
            Refusal::Hello => 2,
            Refusal::Nothing => 3,
            Refusal::Protocol => 4,
        }
    }

    /// The refusal a REFUSED's reason names.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Refusal::Version),
            2 => Some(Refusal::Hello),
            3 => Some(Refusal::Nothing),
            4 => Some(Refusal::Protocol),
            _ => None,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::Version => "a protocol version the core does not speak",
            Refusal::Hello => "a HELLO the core cannot take",
            Refusal::Nothing => "no stream the core can publish",
            Refusal::Protocol => "a message out of turn",
        })
    }
}

/// A message, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Message {
    /// Driver to core.
    Hello(Hello),
    /// Core to driver.
    Ready(Ready),
    /// Core to driver.
    Refused(Refusal),
    /// Core to driver.
    Submit(Submit),
    /// Driver to core.
    Elapsed(Elapsed),
    /// Core to driver: stop the stream and hand back what is posted.
    Halt {
        /// The device's stream number.
        stream: u32,
    },
    /// Driver to core.
    Halted {
        /// The device's stream number.
        stream: u32,
        /// Submissions handed back unplayed.
        unplayed: u32,
    },
    /// Core to driver.
    Stop,
    /// Driver to core.
    Stopped,
}

/// Why bytes are not a message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageError {
    /// Shorter than a header.
    Short,
    /// A type this crate does not know.
    Type(u32),
    /// A length other than the type's, or other than the bytes given.
    Length,
    /// A field outside its range, or a reserved byte set.
    Field,
}

/// A message's bytes, as long as the longest.
#[derive(Clone, Copy, Debug)]
pub struct Encoded {
    bytes: [u8; MAX_BYTES],
    len: usize,
}

impl Encoded {
    /// The message's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }
}

impl Message {
    /// Its type.
    #[must_use]
    pub const fn kind(&self) -> u32 {
        match self {
            Message::Hello(_) => HELLO,
            Message::Ready(_) => READY,
            Message::Refused(_) => REFUSED,
            Message::Submit(_) => SUBMIT,
            Message::Elapsed(_) => ELAPSED,
            Message::Halt { .. } => HALT,
            Message::Halted { .. } => HALTED,
            Message::Stop => STOP,
            Message::Stopped => STOPPED,
        }
    }

    /// The length a type fixes.
    #[must_use]
    pub const fn length_of(kind: u32) -> Option<usize> {
        match kind {
            HELLO => Some(HELLO_BYTES),
            READY => Some(READY_BYTES),
            REFUSED => Some(REFUSED_BYTES),
            SUBMIT | ELAPSED => Some(SUBMIT_BYTES),
            HALT | HALTED => Some(HALT_BYTES),
            STOP | STOPPED => Some(HEADER_BYTES),
            _ => None,
        }
    }

    /// Its bytes.
    #[must_use]
    pub fn encode(&self) -> Encoded {
        let kind = self.kind();
        let len = Self::length_of(kind).unwrap_or(HEADER_BYTES);
        let mut out = Encoded {
            bytes: [0; MAX_BYTES],
            len,
        };
        let bytes = &mut out.bytes;
        put32(bytes, 0, kind);
        // Every length is below `MAX_BYTES`, far inside a `u32`.
        put32(bytes, 4, len as u32);
        match self {
            Message::Hello(hello) => encode_hello(bytes, hello),
            Message::Ready(ready) => encode_ready(bytes, ready),
            Message::Refused(reason) => put32(bytes, 8, reason.raw()),
            Message::Submit(submit) => {
                for (at, word) in [submit.stream, submit.sequence, submit.offset, submit.bytes]
                    .into_iter()
                    .enumerate()
                {
                    put32(bytes, 8 + at * 4, word);
                }
            }
            Message::Elapsed(elapsed) => {
                let played = u32::from(elapsed.played);
                for (at, word) in [
                    elapsed.stream,
                    elapsed.sequence,
                    played,
                    elapsed.latency_bytes,
                ]
                .into_iter()
                .enumerate()
                {
                    put32(bytes, 8 + at * 4, word);
                }
            }
            Message::Halt { stream } => put32(bytes, 8, *stream),
            Message::Halted { stream, unplayed } => {
                put32(bytes, 8, *stream);
                put32(bytes, 12, *unplayed);
            }
            Message::Stop | Message::Stopped => {}
        }
        out
    }

    /// Decode `bytes`, which must be one whole message.
    ///
    /// # Errors
    ///
    /// A [`MessageError`] for anything that is not exactly one well-formed
    /// message.
    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let kind = get32(bytes, 0).ok_or(MessageError::Short)?;
        let length = get32(bytes, 4).ok_or(MessageError::Short)? as usize;
        let expected = Self::length_of(kind).ok_or(MessageError::Type(kind))?;
        if length != expected || bytes.len() != expected {
            return Err(MessageError::Length);
        }
        decode_body(kind, bytes).ok_or(MessageError::Field)
    }
}

fn encode_hello(bytes: &mut [u8], hello: &Hello) {
    put16(bytes, 8, hello.version);
    put32(bytes, 12, hello.location);
    put32(bytes, 16, hello.streams);
    for (index, offer) in hello.offers.iter().enumerate() {
        let at = 24 + index * STREAM_BYTES;
        put(
            bytes,
            at,
            &[offer.direction, offer.channels_min, offer.channels_max, 0],
        );
        put32(bytes, at + 4, offer.rates);
        put(bytes, at + 8, &offer.formats.to_le_bytes());
    }
}

fn encode_ready(bytes: &mut [u8], ready: &Ready) {
    put32(bytes, 8, ready.card);
    put32(bytes, 12, ready.published);
    for (index, stream) in ready.streams.iter().enumerate() {
        let at = 16 + index * PUBLISHED_BYTES;
        put32(bytes, at, stream.stream);
        put32(bytes, at + 4, stream.rate);
        put(bytes, at + 8, &[stream.format, stream.channels, 0, 0]);
        put32(bytes, at + 12, stream.period_bytes);
        put32(bytes, at + 16, stream.buffer_bytes);
    }
}

fn decode_body(kind: u32, bytes: &[u8]) -> Option<Message> {
    let word = |at: usize| get32(bytes, at);
    Some(match kind {
        HELLO => Message::Hello(decode_hello(bytes)?),
        READY => Message::Ready(decode_ready(bytes)?),
        REFUSED => Message::Refused(Refusal::from_raw(word(8)?)?),
        SUBMIT => Message::Submit(Submit {
            stream: word(8)?,
            sequence: word(12)?,
            offset: word(16)?,
            bytes: word(20)?,
        }),
        ELAPSED => Message::Elapsed(Elapsed {
            stream: word(8)?,
            sequence: word(12)?,
            played: match word(16)? {
                0 => false,
                1 => true,
                _ => return None,
            },
            latency_bytes: word(20)?,
        }),
        HALT => {
            if word(12)? != 0 {
                return None;
            }
            Message::Halt { stream: word(8)? }
        }
        HALTED => Message::Halted {
            stream: word(8)?,
            unplayed: word(12)?,
        },
        STOP => Message::Stop,
        STOPPED => Message::Stopped,
        _ => return None,
    })
}

fn decode_hello(bytes: &[u8]) -> Option<Hello> {
    if get16(bytes, 10)? != 0 || get32(bytes, 20)? != 0 {
        return None;
    }
    let streams = get32(bytes, 16)?;
    if streams as usize > MAX_STREAMS {
        return None;
    }
    let mut hello = Hello {
        version: get16(bytes, 8)?,
        location: get32(bytes, 12)?,
        streams,
        offers: [Offer::default(); MAX_STREAMS],
    };
    for (index, offer) in hello.offers.iter_mut().enumerate() {
        let at = 24 + index * STREAM_BYTES;
        let head = bytes.get(at..at + 4)?;
        if head.get(3) != Some(&0) {
            return None;
        }
        *offer = Offer {
            direction: *head.first()?,
            channels_min: *head.get(1)?,
            channels_max: *head.get(2)?,
            rates: get32(bytes, at + 4)?,
            formats: get64(bytes, at + 8)?,
        };
        if index as u32 >= streams && *offer != Offer::default() {
            return None;
        }
    }
    Some(hello)
}

fn decode_ready(bytes: &[u8]) -> Option<Ready> {
    let published = get32(bytes, 12)?;
    if published as usize > MAX_PUBLISHED {
        return None;
    }
    let mut ready = Ready {
        card: get32(bytes, 8)?,
        published,
        streams: [Published::default(); MAX_PUBLISHED],
    };
    for (index, stream) in ready.streams.iter_mut().enumerate() {
        let at = 16 + index * PUBLISHED_BYTES;
        let tail = bytes.get(at + 8..at + 12)?;
        if tail.get(2..) != Some(&[0, 0][..]) {
            return None;
        }
        *stream = Published {
            stream: get32(bytes, at)?,
            rate: get32(bytes, at + 4)?,
            format: *tail.first()?,
            channels: *tail.get(1)?,
            period_bytes: get32(bytes, at + 12)?,
            buffer_bytes: get32(bytes, at + 16)?,
        };
        if index as u32 >= published && *stream != Published::default() {
            return None;
        }
    }
    Some(ready)
}

fn get16(bytes: &[u8], at: usize) -> Option<u16> {
    let field = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes(field.try_into().ok()?))
}

fn get32(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(field.try_into().ok()?))
}

fn get64(bytes: &[u8], at: usize) -> Option<u64> {
    let field = bytes.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(field.try_into().ok()?))
}

fn put(out: &mut [u8], at: usize, field: &[u8]) {
    if let Some(slot) = out.get_mut(at..at + field.len()) {
        slot.copy_from_slice(field);
    }
}

fn put16(out: &mut [u8], at: usize, value: u16) {
    put(out, at, &value.to_le_bytes());
}

fn put32(out: &mut [u8], at: usize, value: u32) {
    put(out, at, &value.to_le_bytes());
}
