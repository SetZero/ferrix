//! The messages: a sixteen-byte header, and a body whose layout the agreed
//! capabilities decide.
//!
//! ```text
//! header   protocol u32 = 1   type u32   opaque u32   size u32
//!
//! ANNOUNCE_CAPABILITIES  request u32, then the bitmap as u32 words
//! CLIPBOARD_GRAB         [selection u8, pad u8 x 3] [serial u32] type u32 ...
//! CLIPBOARD_REQUEST      [selection u8, pad u8 x 3] type u32
//! CLIPBOARD              [selection u8, pad u8 x 3] type u32, then the data
//! CLIPBOARD_RELEASE      [selection u8, pad u8 x 3]
//! ```
//!
//! The bracketed fields are there only when the capability that defines them
//! was agreed ([`Shape`]). `opaque` is zero in every message here; SPICE uses
//! it for file transfers, which this does not carry.
//!
//! # The padding is read loosely and written as zero
//!
//! This tree's rule is that reserved bytes must be zero and a message with
//! any of them set is malformed, so that they can be given a meaning later.
//! The three bytes after a selection number are the one place that rule is
//! not applied, and the reason is that the peer is not ours: the rule exists
//! to stop *our* senders quietly relying on a field, and refusing a peer's
//! message over padding would turn a future SPICE release that puts something
//! there into a clipboard that silently stops working. So they are written as
//! zero, always, and ignored on the way in.

use crate::{Error, part, part_mut, put_u32, u32_at};

/// `VD_AGENT_PROTOCOL`: the only protocol version there has ever been.
pub const PROTOCOL: u32 = 1;

/// Bytes of the message header.
pub const HEADER_BYTES: usize = 16;

/// Bytes of a selection field and its padding.
const SELECTION_BYTES: usize = 4;

// -- Message types, `vd_agent.h` ------------------------------------------

/// `VD_AGENT_CLIPBOARD`: here is the data that was asked for.
pub const CLIPBOARD: u32 = 4;
/// `VD_AGENT_ANNOUNCE_CAPABILITIES`: what I can do, and possibly a request
/// that you say what you can do.
pub const ANNOUNCE_CAPABILITIES: u32 = 6;
/// `VD_AGENT_CLIPBOARD_GRAB`: I own this selection now, and these are the
/// types I can give it in.
pub const CLIPBOARD_GRAB: u32 = 7;
/// `VD_AGENT_CLIPBOARD_REQUEST`: send me this selection, in this type.
pub const CLIPBOARD_REQUEST: u32 = 8;
/// `VD_AGENT_CLIPBOARD_RELEASE`: I no longer own it.
pub const CLIPBOARD_RELEASE: u32 = 9;

// -- Capabilities, `vd_agent.h` -------------------------------------------

/// The capability bits, by their index in the bitmap.
pub mod cap {
    /// `VD_AGENT_CAP_MOUSE_STATE`.
    pub const MOUSE_STATE: u32 = 0;
    /// `VD_AGENT_CAP_CLIPBOARD`: the old clipboard, where a grab carried the
    /// data. Announced by nobody here and implemented by nothing here.
    pub const CLIPBOARD: u32 = 3;
    /// `VD_AGENT_CAP_CLIPBOARD_BY_DEMAND`: a grab announces types and the
    /// data moves when it is asked for. The modern clipboard.
    pub const CLIPBOARD_BY_DEMAND: u32 = 5;
    /// `VD_AGENT_CAP_CLIPBOARD_SELECTION`: clipboard messages carry which
    /// selection they are about.
    pub const CLIPBOARD_SELECTION: u32 = 6;
    /// `VD_AGENT_CAP_MAX_CLIPBOARD`: a negotiated maximum size.
    pub const MAX_CLIPBOARD: u32 = 10;
    /// `VD_AGENT_CAP_CLIPBOARD_NO_RELEASE_ON_REGRAB`.
    pub const CLIPBOARD_NO_RELEASE_ON_REGRAB: u32 = 16;
    /// `VD_AGENT_CAP_CLIPBOARD_GRAB_SERIAL`: a grab carries a serial, and the
    /// newer serial wins a race.
    pub const CLIPBOARD_GRAB_SERIAL: u32 = 17;

    /// A bitmap with just `bit` set.
    #[must_use]
    pub const fn bit(bit: u32) -> u32 {
        1 << bit
    }
}

/// What QEMU 9.2.4 announces with `clipboard=on` (`vdagent_send_caps` in
/// `ui/vdagent.c`), and so what this agent announces back: the clipboard by
/// demand, per selection, with grab serials.
pub const AGENT_CAPS: u32 = cap::bit(cap::CLIPBOARD_BY_DEMAND)
    | cap::bit(cap::CLIPBOARD_SELECTION)
    | cap::bit(cap::CLIPBOARD_GRAB_SERIAL);

// -- Selections and types --------------------------------------------------

/// Which selection a clipboard message is about.
///
/// `VD_AGENT_CLIPBOARD_SELECTION_SECONDARY` exists in the protocol, QEMU maps
/// it to no clipboard of its own, and Wayland has no third selection to put
/// it in, so it is refused rather than carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// The selection a paste uses. `VD_AGENT_CLIPBOARD_SELECTION_CLIPBOARD`.
    Clipboard,
    /// The selection a middle click pastes.
    /// `VD_AGENT_CLIPBOARD_SELECTION_PRIMARY`.
    Primary,
}

impl Selection {
    /// Its number on the wire.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Selection::Clipboard => 0,
            Selection::Primary => 1,
        }
    }

    /// The selection `number` names.
    ///
    /// # Errors
    ///
    /// [`Error::Selection`] for anything but 0 and 1.
    pub const fn from_number(number: u8) -> Result<Selection, Error> {
        match number {
            0 => Ok(Selection::Clipboard),
            1 => Ok(Selection::Primary),
            other => Err(Error::Selection(other)),
        }
    }
}

/// What a clipboard message holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardType {
    /// `VD_AGENT_CLIPBOARD_NONE`: nothing, which is how a side answers a
    /// request it cannot satisfy, and what an unfilled slot of a type list
    /// holds.
    #[default]
    None,
    /// `VD_AGENT_CLIPBOARD_UTF8_TEXT`, the MIME type
    /// `text/plain;charset=utf-8` on the Wayland side.
    Utf8Text,
    /// `VD_AGENT_CLIPBOARD_IMAGE_PNG`, the MIME type `image/png`.
    ImagePng,
}

impl ClipboardType {
    /// Its number on the wire.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            ClipboardType::None => 0,
            ClipboardType::Utf8Text => 1,
            ClipboardType::ImagePng => 2,
        }
    }

    /// The type `number` names.
    ///
    /// # Errors
    ///
    /// [`Error::Type`] for a number this crate does not carry -- the image
    /// formats SPICE calls optional, and the file list.
    pub const fn from_number(number: u32) -> Result<ClipboardType, Error> {
        match number {
            0 => Ok(ClipboardType::None),
            1 => Ok(ClipboardType::Utf8Text),
            2 => Ok(ClipboardType::ImagePng),
            other => Err(Error::Type(other)),
        }
    }

    /// The MIME type the Wayland clipboard names this by, or `None` for
    /// [`ClipboardType::None`], which is not a type but the absence of one.
    #[must_use]
    pub const fn mime(self) -> Option<&'static str> {
        match self {
            ClipboardType::None => None,
            ClipboardType::Utf8Text => Some("text/plain;charset=utf-8"),
            ClipboardType::ImagePng => Some("image/png"),
        }
    }
}

/// The most types a grab may list.
///
/// SPICE defines six and this crate carries two, so a peer listing more than
/// this is not a peer with more formats: it is a message being read at the
/// wrong offset, which is exactly what this bound exists to catch.
pub const MAX_TYPES: usize = 8;

/// The types a grab lists, in the order it listed them.
///
/// Types this crate does not carry are dropped rather than refused: a host
/// that can also give the selection as a TIFF is a host this agent can still
/// take text from. A grab that lists *only* types it cannot carry decodes to
/// an empty list, which is a grab of nothing and is treated as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Types {
    /// The types, the first `count` of them meaning anything.
    types: [ClipboardType; MAX_TYPES],
    /// How many there are.
    count: usize,
}

impl Types {
    /// A list of `types`.
    ///
    /// # Errors
    ///
    /// [`Error::TooManyTypes`] beyond [`MAX_TYPES`].
    pub fn new(types: &[ClipboardType]) -> Result<Types, Error> {
        if types.len() > MAX_TYPES {
            return Err(Error::TooManyTypes(types.len()));
        }
        let mut list = Types::default();
        for (slot, kind) in list.types.iter_mut().zip(types) {
            *slot = *kind;
        }
        list.count = types.len();
        Ok(list)
    }

    /// The types, in order.
    pub fn iter(&self) -> impl Iterator<Item = ClipboardType> + '_ {
        self.types.iter().take(self.count).copied()
    }

    /// How many there are.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Whether there are none, which is a grab of nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Whether `kind` is among them.
    #[must_use]
    pub fn holds(&self, kind: ClipboardType) -> bool {
        self.iter().any(|listed| listed == kind)
    }

    /// Push `kind`, ignoring it once the list is full.
    fn push(&mut self, kind: ClipboardType) {
        if let Some(slot) = self.types.get_mut(self.count) {
            *slot = kind;
            self.count += 1;
        }
    }
}

// -- The shape the capabilities agreed -------------------------------------

/// Which optional fields the clipboard messages carry, which is what the two
/// sides' capabilities decided and is not recoverable from the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// `VD_AGENT_CAP_CLIPBOARD_SELECTION` was agreed: every clipboard message
    /// starts with a selection number and three bytes of padding.
    pub selection: bool,
    /// `VD_AGENT_CAP_CLIPBOARD_GRAB_SERIAL` was agreed: a grab carries a
    /// serial after the selection.
    pub serial: bool,
}

impl Shape {
    /// What QEMU with `clipboard=on` agrees to, and the only shape this agent
    /// ever uses in anger.
    pub const QEMU_CLIPBOARD: Shape = Shape {
        selection: true,
        serial: true,
    };

    /// The oldest shape: one selection, no serials. Nothing announces this
    /// any more; it is here so that the decoder cannot be written in a way
    /// that assumes the fields are present.
    pub const PLAIN: Shape = Shape {
        selection: false,
        serial: false,
    };

    /// The shape a peer's capability bitmap asks for.
    #[must_use]
    pub const fn from_caps(caps: u32) -> Shape {
        Shape {
            selection: caps & cap::bit(cap::CLIPBOARD_SELECTION) != 0,
            serial: caps & cap::bit(cap::CLIPBOARD_GRAB_SERIAL) != 0,
        }
    }

    /// Bytes a clipboard message's body starts with before its own fields.
    const fn prefix(self) -> usize {
        if self.selection { SELECTION_BYTES } else { 0 }
    }
}

// -- The messages ----------------------------------------------------------

/// One message, borrowing whatever data it carries from the buffer it was
/// decoded out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message<'a> {
    /// What the sender can do, and whether it wants the same back.
    AnnounceCapabilities {
        /// Answer this with your own capabilities.
        request: bool,
        /// The first word of the bitmap, which is every capability there is:
        /// `VD_AGENT_END_CAP` is 18.
        caps: u32,
    },
    /// The sender owns `selection` and can give it in these types.
    ClipboardGrab {
        /// Which selection.
        selection: Selection,
        /// The serial, when the shape carries one. A grab whose serial is
        /// older than one already seen is the loser of a race.
        serial: Option<u32>,
        /// The types on offer.
        types: Types,
    },
    /// Send me `selection` as `kind`.
    ClipboardRequest {
        /// Which selection.
        selection: Selection,
        /// In which type.
        kind: ClipboardType,
    },
    /// Here is `selection` as `kind`. [`ClipboardType::None`] with no data is
    /// the answer to a request that could not be satisfied, and a request is
    /// always answered.
    Clipboard {
        /// Which selection.
        selection: Selection,
        /// Which type, or [`ClipboardType::None`] for a refusal.
        kind: ClipboardType,
        /// The bytes, as they are: no line-ending conversion happens in
        /// either direction, since neither side announces a line-ending
        /// capability.
        data: &'a [u8],
    },
    /// The sender no longer owns `selection`.
    ClipboardRelease {
        /// Which selection.
        selection: Selection,
    },
    /// A message this crate does not implement, which is not an error: a peer
    /// may send anything its own capabilities describe, and the answer to the
    /// mouse, the monitors and the file transfers is to ignore them.
    Other {
        /// Its type number.
        kind: u32,
        /// Its body, undecoded.
        data: &'a [u8],
    },
}

impl<'a> Message<'a> {
    /// Decode one whole message -- header and body -- out of `bytes`.
    ///
    /// `bytes` is exactly one message, as [`crate::chunk::Reassembler`] hands
    /// it over: trailing bytes are a body longer than its layout, which is
    /// allowed, since a field added to a later protocol version must not stop
    /// an older reader.
    ///
    /// # Errors
    ///
    /// [`Error::Short`] for a body that does not hold its own layout,
    /// [`Error::Protocol`] for a header that is not this protocol, and the
    /// field errors for a selection or a type that is not defined.
    pub fn decode(bytes: &'a [u8], shape: Shape) -> Result<Message<'a>, Error> {
        let protocol = u32_at(bytes, 0)?;
        if protocol != PROTOCOL {
            return Err(Error::Protocol(protocol));
        }
        let kind = u32_at(bytes, 4)?;
        let size = u32_at(bytes, 12)? as usize;
        let body = bytes.get(HEADER_BYTES..HEADER_BYTES + size).ok_or({
            Error::Short {
                want: HEADER_BYTES + size,
                have: bytes.len(),
            }
        })?;
        Message::decode_body(kind, body, shape)
    }

    /// Decode a body whose type and length are already known.
    fn decode_body(kind: u32, body: &'a [u8], shape: Shape) -> Result<Message<'a>, Error> {
        // Every clipboard message but the capabilities starts the same way,
        // and reading the selection is the same four bytes each time.
        let selection = || -> Result<Selection, Error> {
            if !shape.selection {
                return Ok(Selection::Clipboard);
            }
            let number = *body.first().ok_or(Error::Short {
                want: SELECTION_BYTES,
                have: body.len(),
            })?;
            Selection::from_number(number)
        };
        let at = shape.prefix();

        match kind {
            ANNOUNCE_CAPABILITIES => Ok(Message::AnnounceCapabilities {
                request: u32_at(body, 0)? != 0,
                // A peer that announces nothing sends the request word and no
                // bitmap at all, which is a peer with no capabilities rather
                // than a malformed message.
                caps: u32_at(body, 4).unwrap_or(0),
            }),
            CLIPBOARD_GRAB => {
                let selection = selection()?;
                let (serial, at) = if shape.serial {
                    (Some(u32_at(body, at)?), at + 4)
                } else {
                    (None, at)
                };
                let listed = body.len().saturating_sub(at) / 4;
                if listed > MAX_TYPES {
                    return Err(Error::TooManyTypes(listed));
                }
                let mut types = Types::default();
                for index in 0..listed {
                    // A type this crate does not carry is dropped, not
                    // refused: the rest of the grab is still usable.
                    if let Ok(kind) = ClipboardType::from_number(u32_at(body, at + index * 4)?) {
                        types.push(kind);
                    }
                }
                Ok(Message::ClipboardGrab {
                    selection,
                    serial,
                    types,
                })
            }
            CLIPBOARD_REQUEST => Ok(Message::ClipboardRequest {
                selection: selection()?,
                kind: ClipboardType::from_number(u32_at(body, at)?)?,
            }),
            CLIPBOARD => {
                let selection = selection()?;
                let kind = ClipboardType::from_number(u32_at(body, at)?)?;
                Ok(Message::Clipboard {
                    selection,
                    kind,
                    data: part(body, at + 4, body.len())?,
                })
            }
            CLIPBOARD_RELEASE => Ok(Message::ClipboardRelease {
                selection: selection()?,
            }),
            other => Ok(Message::Other {
                kind: other,
                data: body,
            }),
        }
    }

    /// Its type number on the wire.
    #[must_use]
    pub const fn kind(&self) -> u32 {
        match self {
            Message::AnnounceCapabilities { .. } => ANNOUNCE_CAPABILITIES,
            Message::ClipboardGrab { .. } => CLIPBOARD_GRAB,
            Message::ClipboardRequest { .. } => CLIPBOARD_REQUEST,
            Message::Clipboard { .. } => CLIPBOARD,
            Message::ClipboardRelease { .. } => CLIPBOARD_RELEASE,
            Message::Other { kind, .. } => *kind,
        }
    }

    /// Bytes this message encodes to, header and all.
    #[must_use]
    pub fn encoded_len(&self, shape: Shape) -> usize {
        HEADER_BYTES + self.body_len(shape)
    }

    /// Bytes of its body.
    fn body_len(&self, shape: Shape) -> usize {
        let prefix = shape.prefix();
        match self {
            Message::AnnounceCapabilities { .. } => 8,
            Message::ClipboardGrab { types, .. } => {
                prefix + usize::from(shape.serial) * 4 + types.len() * 4
            }
            Message::ClipboardRequest { .. } => prefix + 4,
            Message::Clipboard { data, .. } => prefix + 4 + data.len(),
            Message::ClipboardRelease { .. } => prefix,
            Message::Other { data, .. } => data.len(),
        }
    }

    /// Write the message into `out`, and say how many bytes it took.
    ///
    /// The serial of a grab is written when the shape carries one and the
    /// message names one; a shape with serials and a grab without gets a zero
    /// serial, which is the oldest there is and so loses every race, rather
    /// than a message of the wrong length.
    ///
    /// # Errors
    ///
    /// [`Error::Short`] when `out` is smaller than [`Message::encoded_len`].
    pub fn encode(&self, shape: Shape, out: &mut [u8]) -> Result<usize, Error> {
        let want = self.encoded_len(shape);
        if out.len() < want {
            return Err(Error::Short {
                want,
                have: out.len(),
            });
        }
        let body_len = self.body_len(shape);
        put_u32(out, 0, PROTOCOL)?;
        put_u32(out, 4, self.kind())?;
        // `opaque`: SPICE's file transfers put an id here; nothing this crate
        // carries uses it.
        put_u32(out, 8, 0)?;
        put_u32(out, 12, u32::try_from(body_len).unwrap_or(u32::MAX))?;

        let body = part_mut(out, HEADER_BYTES, HEADER_BYTES + body_len)?;
        // The padding after a selection number, and the whole body of a
        // release, is zero; filling the body first means nothing later has to
        // remember to zero it.
        body.fill(0);
        match self {
            Message::AnnounceCapabilities { request, caps } => {
                put_u32(body, 0, u32::from(*request))?;
                put_u32(body, 4, *caps)?;
            }
            Message::ClipboardGrab {
                selection,
                serial,
                types,
            } => {
                let mut at = selection_into(body, shape, *selection)?;
                if shape.serial {
                    put_u32(body, at, serial.unwrap_or(0))?;
                    at += 4;
                }
                for (index, kind) in types.iter().enumerate() {
                    put_u32(body, at + index * 4, kind.number())?;
                }
            }
            Message::ClipboardRequest { selection, kind } => {
                let at = selection_into(body, shape, *selection)?;
                put_u32(body, at, kind.number())?;
            }
            Message::Clipboard {
                selection,
                kind,
                data,
            } => {
                let at = selection_into(body, shape, *selection)?;
                put_u32(body, at, kind.number())?;
                let end = at + 4 + data.len();
                part_mut(body, at + 4, end)?.copy_from_slice(data);
            }
            Message::ClipboardRelease { selection } => {
                let _ = selection_into(body, shape, *selection)?;
            }
            Message::Other { data, .. } => body.copy_from_slice(data),
        }
        Ok(want)
    }
}

/// Write a selection number and its padding at the front of a body, and say
/// how many bytes that took, which is none at all under a shape that carries
/// no selection.
fn selection_into(body: &mut [u8], shape: Shape, selection: Selection) -> Result<usize, Error> {
    if !shape.selection {
        return Ok(0);
    }
    let have = body.len();
    let slot = body.first_mut().ok_or(Error::Short {
        want: SELECTION_BYTES,
        have,
    })?;
    *slot = selection.number();
    Ok(SELECTION_BYTES)
}
