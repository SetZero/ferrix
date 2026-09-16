//! Reading and writing one message.

use crate::arg::{Arg, ArgType, Fd, Signature};
use crate::objects::ObjectId;
use crate::{HEADER_BYTES, MAX_MESSAGE, padded};

/// Why bytes are not the message they were read as.
///
/// Every one of these is a client that broke the protocol, and the server's
/// answer to each is `wl_display.error` and the connection closed. They are
/// separate so the log says which.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// Fewer bytes than the message says it has. Not a protocol error by
    /// itself: the rest may not have arrived yet.
    Incomplete {
        /// How many bytes the message needs in all.
        needed: usize,
        /// How many there are.
        have: usize,
    },
    /// A size smaller than a header, or larger than [`MAX_MESSAGE`], or not
    /// a multiple of four.
    Size(usize),
    /// An argument ran past the end of the message.
    Truncated {
        /// The argument's position in the signature.
        argument: usize,
    },
    /// A string or array whose length field runs past the message, or a
    /// length that overflows.
    Length {
        /// The argument's position in the signature.
        argument: usize,
        /// The length it claimed.
        claimed: u32,
    },
    /// A string that is not UTF-8, or has no NUL where its length says.
    /// libwayland passes bytes through; a Rust server that stored them would
    /// have to carry `&[u8]` everywhere a name is used, so the check is
    /// here.
    NotAString {
        /// The argument's position in the signature.
        argument: usize,
    },
    /// A null where the protocol does not allow one.
    NotNullable {
        /// The argument's position in the signature.
        argument: usize,
    },
    /// An `fd` argument with no descriptor left of those that arrived.
    NoDescriptor {
        /// The argument's position in the signature.
        argument: usize,
    },
    /// The message would be longer than [`MAX_MESSAGE`], or carry more than
    /// [`crate::MAX_FDS`] descriptors.
    TooLarge,
    /// A value that cannot be written: a string with a NUL inside it, or an
    /// argument whose type is not the signature's.
    BadArgument {
        /// The argument's position in the signature.
        argument: usize,
    },
}

/// A message's header: who sent it, which message it is and how long.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// The object the message is to, or from.
    pub sender: ObjectId,
    /// Which of that interface's requests or events.
    pub opcode: u16,
    /// The whole message in bytes, the header included.
    pub size: usize,
}

impl Header {
    /// Read a header from the front of `bytes`.
    ///
    /// The size is checked here rather than by the caller, because every
    /// later read trusts it.
    pub fn read(bytes: &[u8]) -> Result<Self, Error> {
        let Some(head) = bytes.get(..HEADER_BYTES) else {
            return Err(Error::Incomplete {
                needed: HEADER_BYTES,
                have: bytes.len(),
            });
        };
        let sender = read_u32(head, 0).ok_or(Error::Size(0))?;
        let packed = read_u32(head, 4).ok_or(Error::Size(0))?;
        let size = (packed >> 16) as usize;
        let opcode = (packed & 0xFFFF) as u16;
        if !(HEADER_BYTES..=MAX_MESSAGE).contains(&size) || !size.is_multiple_of(4) {
            return Err(Error::Size(size));
        }
        Ok(Self {
            sender: ObjectId(sender),
            opcode,
            size,
        })
    }

    /// The header's eight bytes.
    #[must_use]
    pub fn write(&self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0u8; HEADER_BYTES];
        let packed = ((self.size as u32) << 16) | u32::from(self.opcode);
        let (first, second) = bytes.split_at_mut(4);
        first.copy_from_slice(&self.sender.0.to_le_bytes());
        second.copy_from_slice(&packed.to_le_bytes());
        bytes
    }
}

/// Reads whole messages out of a stream of bytes and the descriptors that
/// came with them.
///
/// The bytes are borrowed, so a string or array argument is a slice of the
/// receive buffer and nothing is copied. [`Reader::consumed`] says how much
/// of the buffer whole messages took, which is what the caller compacts
/// away; a partial message at the end stays for the next read.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    fds: &'a [Fd],
    fd_at: usize,
}

impl<'a> Reader<'a> {
    /// Read `bytes`, taking descriptors from `fds` in order as `fd`
    /// arguments call for them.
    #[must_use]
    pub const fn new(bytes: &'a [u8], fds: &'a [Fd]) -> Self {
        Self {
            bytes,
            at: 0,
            fds,
            fd_at: 0,
        }
    }

    /// How many bytes whole messages have taken.
    #[must_use]
    pub const fn consumed(&self) -> usize {
        self.at
    }

    /// How many descriptors have been handed out.
    #[must_use]
    pub const fn descriptors_taken(&self) -> usize {
        self.fd_at
    }

    /// Whether there are not even eight bytes left.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.bytes.len() - self.at < HEADER_BYTES
    }

    /// The header of the next message, without taking it.
    ///
    /// The caller needs this to know which object and opcode the message is
    /// for, and so which [`Signature`] to read it with.
    pub fn peek(&self) -> Result<Header, Error> {
        let rest = self.bytes.get(self.at..).unwrap_or(&[]);
        Header::read(rest)
    }

    /// Read the next message's arguments, given its signature, and move past
    /// it.
    ///
    /// On any error nothing is consumed, so a caller that meets
    /// [`Error::Incomplete`] can read more bytes and try the same buffer
    /// again.
    pub fn read(&mut self, signature: Signature) -> Result<(Header, Vec<Arg<'a>>), Error> {
        let header = self.peek()?;
        let rest = self.bytes.get(self.at..).unwrap_or(&[]);
        if rest.len() < header.size {
            return Err(Error::Incomplete {
                needed: header.size,
                have: rest.len(),
            });
        }
        let body = rest
            .get(HEADER_BYTES..header.size)
            .ok_or(Error::Size(header.size))?;
        let mut cursor = Cursor { body, at: 0 };
        let mut fd_at = self.fd_at;
        let mut args = Vec::with_capacity(signature.len());
        for (argument, kind) in signature.iter().enumerate() {
            args.push(read_arg(
                *kind,
                argument,
                &mut cursor,
                self.fds,
                &mut fd_at,
            )?);
        }
        // Trailing bytes are a client saying one thing in its header and
        // another in its arguments. libwayland ignores them; ignoring them
        // here would let a message smuggle bytes past every check.
        if cursor.at != body.len() {
            return Err(Error::Size(header.size));
        }
        self.at += header.size;
        self.fd_at = fd_at;
        Ok((header, args))
    }

    /// Skip the next message without reading its arguments, for an opcode
    /// the server does not know: the descriptors it carries must still be
    /// accounted for, so the caller passes how many the signature has.
    pub fn skip(&mut self, descriptors: usize) -> Result<Header, Error> {
        let header = self.peek()?;
        let rest = self.bytes.get(self.at..).unwrap_or(&[]);
        if rest.len() < header.size {
            return Err(Error::Incomplete {
                needed: header.size,
                have: rest.len(),
            });
        }
        let fd_at = self
            .fd_at
            .checked_add(descriptors)
            .filter(|end| *end <= self.fds.len())
            .ok_or(Error::NoDescriptor { argument: 0 })?;
        self.at += header.size;
        self.fd_at = fd_at;
        Ok(header)
    }
}

/// Builds one message's bytes and the descriptors that go beside them.
#[derive(Clone, Debug, Default)]
pub struct Writer {
    bytes: Vec<u8>,
    fds: Vec<Fd>,
}

impl Writer {
    /// An empty writer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            fds: Vec::new(),
        }
    }

    /// The bytes written so far.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The descriptors to send beside them, in order.
    #[must_use]
    pub fn descriptors(&self) -> &[Fd] {
        &self.fds
    }

    /// Whether nothing has been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Forget everything written, keeping the buffers.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.fds.clear();
    }

    /// Take the bytes and descriptors, leaving the writer empty.
    #[must_use]
    pub fn take(&mut self) -> (Vec<u8>, Vec<Fd>) {
        (
            core::mem::take(&mut self.bytes),
            core::mem::take(&mut self.fds),
        )
    }

    /// Append a message from `sender` with `opcode` and `args`, which must
    /// match `signature`.
    ///
    /// On any error nothing is appended, so a writer that refuses one
    /// message still holds exactly the messages before it.
    pub fn write(
        &mut self,
        sender: ObjectId,
        opcode: u16,
        signature: Signature,
        args: &[Arg<'_>],
    ) -> Result<(), Error> {
        if args.len() != signature.len() {
            return Err(Error::BadArgument {
                argument: signature.len().min(args.len()),
            });
        }
        let start = self.bytes.len();
        let fds_start = self.fds.len();
        match self.append(sender, opcode, signature, args) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.bytes.truncate(start);
                self.fds.truncate(fds_start);
                Err(error)
            }
        }
    }

    fn append(
        &mut self,
        sender: ObjectId,
        opcode: u16,
        signature: Signature,
        args: &[Arg<'_>],
    ) -> Result<(), Error> {
        let start = self.bytes.len();
        self.bytes.extend_from_slice(&[0u8; HEADER_BYTES]);
        for (argument, (kind, arg)) in signature.iter().zip(args).enumerate() {
            self.write_arg(*kind, argument, *arg)?;
        }
        let size = self.bytes.len() - start;
        if size > MAX_MESSAGE {
            return Err(Error::TooLarge);
        }
        if self.fds.len() > crate::MAX_FDS {
            return Err(Error::TooLarge);
        }
        let header = Header {
            sender,
            opcode,
            size,
        }
        .write();
        let slot = self
            .bytes
            .get_mut(start..start + HEADER_BYTES)
            .ok_or(Error::TooLarge)?;
        slot.copy_from_slice(&header);
        Ok(())
    }

    fn write_arg(&mut self, kind: ArgType, argument: usize, arg: Arg<'_>) -> Result<(), Error> {
        let bad = Err(Error::BadArgument { argument });
        match (kind, arg) {
            (ArgType::Int, Arg::Int(value)) => self.word(value.cast_unsigned()),
            (ArgType::Uint, Arg::Uint(value)) => self.word(value),
            (ArgType::Fixed, Arg::Fixed(value)) => self.word(value.to_raw().cast_unsigned()),
            (ArgType::Object { nullable }, Arg::Object(id)) => {
                if id.is_null() && !nullable {
                    return Err(Error::NotNullable { argument });
                }
                self.word(id.0);
            }
            (ArgType::NewId, Arg::NewId(id)) => {
                if id.is_null() {
                    return Err(Error::NotNullable { argument });
                }
                self.word(id.0);
            }
            (
                ArgType::AnyNewId,
                Arg::AnyNewId {
                    interface,
                    version,
                    id,
                },
            ) => {
                self.string(Some(interface), argument)?;
                self.word(version);
                if id.is_null() {
                    return Err(Error::NotNullable { argument });
                }
                self.word(id.0);
            }
            (ArgType::Str { nullable }, Arg::Str(value)) => {
                if value.is_none() && !nullable {
                    return Err(Error::NotNullable { argument });
                }
                self.string(value, argument)?;
            }
            (ArgType::Array, Arg::Array(bytes)) => {
                let len = u32::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
                self.word(len);
                self.bytes.extend_from_slice(bytes);
                self.pad(bytes.len());
            }
            (ArgType::Fd, Arg::Fd(fd)) => self.fds.push(fd),
            _ => return bad,
        }
        Ok(())
    }

    fn word(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: Option<&str>, argument: usize) -> Result<(), Error> {
        let Some(text) = value else {
            self.word(0);
            return Ok(());
        };
        if text.as_bytes().contains(&0) {
            return Err(Error::BadArgument { argument });
        }
        let with_nul = text.len().checked_add(1).ok_or(Error::TooLarge)?;
        self.word(u32::try_from(with_nul).map_err(|_| Error::TooLarge)?);
        self.bytes.extend_from_slice(text.as_bytes());
        self.bytes.push(0);
        self.pad(with_nul);
        Ok(())
    }

    fn pad(&mut self, len: usize) {
        self.bytes.resize(self.bytes.len() + (padded(len) - len), 0);
    }
}

/// A walk through one message's body.
struct Cursor<'a> {
    body: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn word(&mut self, argument: usize) -> Result<u32, Error> {
        let value = read_u32(self.body, self.at).ok_or(Error::Truncated { argument })?;
        self.at += 4;
        Ok(value)
    }

    /// The `len` bytes after a length word, stepping over the padding after
    /// them.
    ///
    /// The padding's bytes are not looked at, and must not be: libwayland's
    /// `serialize_closure` copies a string's or array's bytes and then moves
    /// the cursor on by the rounded-up length, leaving the padding as
    /// whatever the buffer held. `wl_closure_send` zeroes that buffer with
    /// `zalloc`, but `wl_closure_queue` -- which is the path a queued
    /// request takes -- uses `malloc`, so the padding a real client sends is
    /// uninitialised heap. A server that required it to be zero would drop
    /// clients at random.
    fn block(&mut self, argument: usize, len: usize) -> Result<&'a [u8], Error> {
        let end = self
            .at
            .checked_add(padded(len))
            .filter(|end| *end <= self.body.len())
            .ok_or(Error::Truncated { argument })?;
        let block = self
            .body
            .get(self.at..self.at + len)
            .ok_or(Error::Truncated { argument })?;
        self.at = end;
        Ok(block)
    }
}

fn read_arg<'a>(
    kind: ArgType,
    argument: usize,
    cursor: &mut Cursor<'a>,
    fds: &[Fd],
    fd_at: &mut usize,
) -> Result<Arg<'a>, Error> {
    Ok(match kind {
        ArgType::Int => Arg::Int(cursor.word(argument)?.cast_signed()),
        ArgType::Uint => Arg::Uint(cursor.word(argument)?),
        ArgType::Fixed => Arg::Fixed(crate::Fixed::from_raw(cursor.word(argument)?.cast_signed())),
        ArgType::Object { nullable } => {
            let id = ObjectId(cursor.word(argument)?);
            if id.is_null() && !nullable {
                return Err(Error::NotNullable { argument });
            }
            Arg::Object(id)
        }
        ArgType::NewId => {
            let id = ObjectId(cursor.word(argument)?);
            if id.is_null() {
                return Err(Error::NotNullable { argument });
            }
            Arg::NewId(id)
        }
        ArgType::AnyNewId => {
            let interface =
                read_string(cursor, argument)?.ok_or(Error::NotNullable { argument })?;
            let version = cursor.word(argument)?;
            let id = ObjectId(cursor.word(argument)?);
            if id.is_null() {
                return Err(Error::NotNullable { argument });
            }
            Arg::AnyNewId {
                interface,
                version,
                id,
            }
        }
        ArgType::Str { nullable } => {
            let text = read_string(cursor, argument)?;
            if text.is_none() && !nullable {
                return Err(Error::NotNullable { argument });
            }
            Arg::Str(text)
        }
        ArgType::Array => {
            let claimed = cursor.word(argument)?;
            let len = usize::try_from(claimed).map_err(|_| Error::Length { argument, claimed })?;
            Arg::Array(cursor.block(argument, len)?)
        }
        ArgType::Fd => {
            let fd = fds.get(*fd_at).ok_or(Error::NoDescriptor { argument })?;
            *fd_at += 1;
            Arg::Fd(*fd)
        }
    })
}

fn read_string<'a>(cursor: &mut Cursor<'a>, argument: usize) -> Result<Option<&'a str>, Error> {
    let claimed = cursor.word(argument)?;
    if claimed == 0 {
        return Ok(None);
    }
    let len = usize::try_from(claimed).map_err(|_| Error::Length { argument, claimed })?;
    let block = cursor.block(argument, len)?;
    // The length counts the NUL, which must be there and must be last.
    let (last, text) = block.split_last().ok_or(Error::NotAString { argument })?;
    if *last != 0 || text.contains(&0) {
        return Err(Error::NotAString { argument });
    }
    let text = core::str::from_utf8(text).map_err(|_| Error::NotAString { argument })?;
    Ok(Some(text))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let word: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(word))
}
