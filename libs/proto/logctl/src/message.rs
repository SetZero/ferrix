//! The messages, as bytes.
//!
//! Every message starts with its type and its length in bytes, four each,
//! little-endian. READ and REFUSED have fixed lengths; DATA's is its header
//! and the bytes it carries. No message carries a handle.
//!
//! ```text
//! READ     driver -> kernel, 16 bytes
//!   8 max u32 (at least 1)   12 reserved u32
//! DATA     kernel -> driver, 24 + count bytes
//!   8 lost u64   16 count u32 (1 to MAX_DATA)   20 reserved u32
//!   24 the log's bytes, count of them
//! REFUSED  kernel -> driver, 12 bytes
//!   8 reason u32
//! ```
//!
//! `max` is the most bytes the driver wants in the DATA that answers; the
//! kernel sends at most [`MAX_DATA`] whatever it asks for. `lost` is how many
//! bytes of the log went between the end of the previous DATA -- or, for the
//! first, the start of the log -- and this one's first byte: overwritten
//! before the reader came for them, because the log is a ring. The bytes are
//! the log's as the console was given them, a bare newline and all, with no
//! record structure: lines are the reader's to find.
//!
//! Reserved bytes are written as zero and a message with any of them set is
//! malformed, so they can be given a meaning later without an old reader
//! misreading them.

use ::core::fmt;

/// READ's type.
pub const READ: u32 = 1;
/// DATA's type.
pub const DATA: u32 = 2;
/// REFUSED's type.
pub const REFUSED: u32 = 3;

/// Bytes of the type and the length.
pub const HEADER_BYTES: usize = 8;
/// Bytes of READ.
pub const READ_BYTES: usize = 16;
/// Bytes of DATA before the log's bytes.
pub const DATA_HEADER_BYTES: usize = 24;
/// Bytes of REFUSED.
pub const REFUSED_BYTES: usize = 12;
/// Bytes of the longest message: a page.
pub const MAX_BYTES: usize = 4096;
/// The most log bytes one DATA carries.
pub const MAX_DATA: usize = MAX_BYTES - DATA_HEADER_BYTES;

/// Why the kernel ended the conversation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// A message the kernel does not take: malformed, one the driver does not
    /// send, or a READ while one is outstanding.
    Protocol,
}

impl Refusal {
    const fn raw(self) -> u32 {
        match self {
            Refusal::Protocol => 1,
        }
    }

    /// The refusal a REFUSED's reason names, if it names one.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Refusal::Protocol),
            _ => None,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::Protocol => "a message out of turn, or one the kernel does not take",
        })
    }
}

/// One message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Message<'a> {
    /// The driver asks for up to `max` bytes of the log.
    Read {
        /// The most bytes the driver wants, at least one.
        max: u32,
    },
    /// The kernel answers a READ.
    Data {
        /// Bytes the log lost since the previous DATA, or since it began.
        lost: u64,
        /// The log's bytes, one to [`MAX_DATA`] of them.
        bytes: &'a [u8],
    },
    /// The kernel ends the conversation, and the claim with it.
    Refused(Refusal),
}

/// Why bytes are not a message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageError {
    /// Shorter than its header.
    Short,
    /// A type nobody defined.
    Type(u32),
    /// A length that is not the type's, or not the message's.
    Length,
    /// A field out of range, or a reserved field set.
    Field,
    /// No room for the message in the buffer given to encode it into.
    Room,
}

impl Message<'_> {
    /// The message's type.
    #[must_use]
    pub const fn kind(&self) -> u32 {
        match self {
            Message::Read { .. } => READ,
            Message::Data { .. } => DATA,
            Message::Refused(_) => REFUSED,
        }
    }

    /// The message's length in bytes, as encoded.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        match self {
            Message::Read { .. } => READ_BYTES,
            Message::Data { bytes, .. } => DATA_HEADER_BYTES + bytes.len(),
            Message::Refused(_) => REFUSED_BYTES,
        }
    }

    /// Encode the message at the front of `out`, and answer its length.
    ///
    /// # Errors
    ///
    /// [`MessageError::Room`] when `out` is shorter than the message, and
    /// [`MessageError::Field`] for one that would not decode: a READ for no
    /// bytes, or a DATA of none or of more than [`MAX_DATA`].
    pub fn encode_into(&self, out: &mut [u8]) -> Result<usize, MessageError> {
        let len = self.encoded_len();
        let out = out.get_mut(..len).ok_or(MessageError::Room)?;
        out.fill(0);
        match self {
            Message::Read { max } => {
                if *max == 0 {
                    return Err(MessageError::Field);
                }
                put_header(out, READ, len);
                put(out, 8, &max.to_le_bytes());
            }
            Message::Data { lost, bytes } => {
                data_header(out, *lost, bytes.len())?;
                put(out, DATA_HEADER_BYTES, bytes);
            }
            Message::Refused(reason) => {
                put_header(out, REFUSED, len);
                put(out, 8, &reason.raw().to_le_bytes());
            }
        }
        Ok(len)
    }
}

impl<'a> Message<'a> {
    /// Decode one message, which must be all of `bytes`.
    ///
    /// # Errors
    ///
    /// [`MessageError`], for anything that is not exactly one message as
    /// this module describes it.
    pub fn decode(bytes: &'a [u8]) -> Result<Message<'a>, MessageError> {
        let kind = get32(bytes, 0).ok_or(MessageError::Short)?;
        let length = get32(bytes, 4).ok_or(MessageError::Short)?;
        if usize::try_from(length).ok() != Some(bytes.len()) {
            return Err(MessageError::Length);
        }
        match kind {
            READ => decode_read(bytes),
            DATA => decode_data(bytes),
            REFUSED => decode_refused(bytes),
            other => Err(MessageError::Type(other)),
        }
    }
}

/// Write DATA's header into the first [`DATA_HEADER_BYTES`] of `out`, for
/// `count` bytes of the log that follow it: how the kernel sends DATA without
/// copying the log's bytes twice, reading them straight into the message.
///
/// # Errors
///
/// [`MessageError::Room`] when `out` is shorter than the header, and
/// [`MessageError::Field`] for a `count` of none or more than [`MAX_DATA`].
pub fn data_header(out: &mut [u8], lost: u64, count: usize) -> Result<(), MessageError> {
    if count == 0 || count > MAX_DATA {
        return Err(MessageError::Field);
    }
    let header = out.get_mut(..DATA_HEADER_BYTES).ok_or(MessageError::Room)?;
    header.fill(0);
    put_header(header, DATA, DATA_HEADER_BYTES + count);
    put(header, 8, &lost.to_le_bytes());
    // Below `MAX_DATA`, far inside a `u32`.
    put(header, 16, &(count as u32).to_le_bytes());
    Ok(())
}

fn decode_read(bytes: &[u8]) -> Result<Message<'_>, MessageError> {
    if bytes.len() != READ_BYTES {
        return Err(MessageError::Length);
    }
    let max = get32(bytes, 8).ok_or(MessageError::Short)?;
    if max == 0 || get32(bytes, 12) != Some(0) {
        return Err(MessageError::Field);
    }
    Ok(Message::Read { max })
}

fn decode_data(bytes: &[u8]) -> Result<Message<'_>, MessageError> {
    let lost = get64(bytes, 8).ok_or(MessageError::Length)?;
    let count = get32(bytes, 16).ok_or(MessageError::Length)?;
    let reserved = get32(bytes, 20).ok_or(MessageError::Length)?;
    let data = bytes.get(DATA_HEADER_BYTES..).ok_or(MessageError::Length)?;
    if usize::try_from(count).ok() != Some(data.len()) {
        return Err(MessageError::Length);
    }
    if data.is_empty() || data.len() > MAX_DATA || reserved != 0 {
        return Err(MessageError::Field);
    }
    Ok(Message::Data { lost, bytes: data })
}

fn decode_refused(bytes: &[u8]) -> Result<Message<'_>, MessageError> {
    if bytes.len() != REFUSED_BYTES {
        return Err(MessageError::Length);
    }
    let raw = get32(bytes, 8).ok_or(MessageError::Short)?;
    Refusal::from_raw(raw)
        .map(Message::Refused)
        .ok_or(MessageError::Field)
}

fn put_header(out: &mut [u8], kind: u32, len: usize) {
    put(out, 0, &kind.to_le_bytes());
    // Every length is at most `MAX_BYTES`, far inside a `u32`.
    put(out, 4, &(len as u32).to_le_bytes());
}

fn put(out: &mut [u8], at: usize, field: &[u8]) {
    if let Some(slot) = at
        .checked_add(field.len())
        .and_then(|end| out.get_mut(at..end))
    {
        slot.copy_from_slice(field);
    }
}

fn get32(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(field.try_into().ok()?))
}

fn get64(bytes: &[u8], at: usize) -> Option<u64> {
    let field = bytes.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(field.try_into().ok()?))
}
