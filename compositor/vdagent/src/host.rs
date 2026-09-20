//! The host's side: vdagent over the socket `user/vport` offers.
//!
//! `ferrix-vdagent` is the protocol -- the chunk framing and the clipboard
//! messages -- and this is the socket it is spoken over: a non-blocking
//! `AF_UNIX` stream, a reassembler fed whatever arrives, and a queue of bytes
//! on their way out.
//!
//! # The capability exchange decides the layout
//!
//! Every clipboard message has four possible layouts and which one a peer
//! means is not in the message (`docs/CLIPBOARD.md` §4.2). So nothing is
//! decoded before `VD_AGENT_ANNOUNCE_CAPABILITIES` has arrived: until then
//! [`Host::shape`] is `None` and the only message that can be read is the
//! announcement itself, whose layout never varies.

use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;

use ferrix_vdagent::chunk::{self, Reassembler};
use ferrix_vdagent::message::{AGENT_CAPS, Message, Shape};

/// The largest selection carried in either direction
/// (`docs/CLIPBOARD.md` §7a).
///
/// Large enough for any text a person copies, small enough that a hostile
/// host cannot exhaust the guest: a megabyte is one buffer here and one
/// buffer in the compositor, and a grab larger than this is answered
/// `CLIPBOARD`/`NONE`.
pub(crate) const MAX_SELECTION: usize = 1024 * 1024;

/// Bytes read from the socket in one go.
const READ_CHUNK: usize = 16 * 1024;

/// The socket, the framing, and what has been agreed.
#[derive(Debug)]
pub(crate) struct Host {
    /// The connection to `user/vport`.
    stream: UnixStream,
    /// Bytes arrived and not yet framed.
    incoming: Vec<u8>,
    /// Bytes to go out, ahead of whatever the socket has taken.
    outgoing: Vec<u8>,
    /// How many of `outgoing` have gone.
    sent: usize,
    /// Where assembled messages are put: [`MAX_SELECTION`] plus the header
    /// of a `CLIPBOARD` carrying that much.
    buffer: Vec<u8>,
    /// The layout both sides agreed to, once the host has said what it can
    /// do.
    shape: Option<Shape>,
    /// Whether the stream is still framed at a message boundary.
    broken: bool,
}

impl Host {
    /// Connect to the port and announce what this agent can do.
    ///
    /// # Errors
    ///
    /// A sentence saying what could not be done. A socket that is not there
    /// is the ordinary case for a boot without `--clipboard`, and the caller
    /// tells the two apart by [`std::io::ErrorKind::NotFound`] in the text.
    pub(crate) fn connect(path: &Path) -> Result<Host, String> {
        let stream = UnixStream::connect(path)
            .map_err(|error| format!("connecting to {}: {error}", path.display()))?;
        stream
            .set_nonblocking(true)
            .map_err(|error| format!("the port: {error}"))?;
        let mut host = Host {
            stream,
            incoming: Vec::new(),
            outgoing: Vec::new(),
            sent: 0,
            buffer: vec![0; MAX_SELECTION + 64],
            shape: None,
            broken: false,
        };
        // The announcement's own layout never varies, so it can be sent
        // before anything has been agreed. `request` asks the host for its
        // capabilities, which is what decides every later message's shape.
        host.send(&Message::AnnounceCapabilities {
            request: true,
            caps: AGENT_CAPS,
        })?;
        Ok(host)
    }

    /// Queue `message` for the host.
    ///
    /// # Errors
    ///
    /// A sentence: a message that will not encode, which for a clipboard
    /// larger than the buffer is the one that happens.
    pub(crate) fn send(&mut self, message: &Message<'_>) -> Result<(), String> {
        let shape = self.shape.unwrap_or(Shape::PLAIN);
        let mut encoded = vec![0_u8; message.encoded_len(shape)];
        let written = message
            .encode(shape, &mut encoded)
            .map_err(|error| format!("encoding a message for the host: {error}"))?;
        let body = encoded.get(..written).unwrap_or_default();
        let mut framed = vec![0_u8; chunk::framed_len(body.len())];
        let framed_len = chunk::frame(body, &mut framed)
            .map_err(|error| format!("framing a message for the host: {error}"))?;
        self.outgoing
            .extend_from_slice(framed.get(..framed_len).unwrap_or_default());
        self.flush();
        Ok(())
    }

    /// Write what is queued, as much as the socket will take.
    ///
    /// A socket that is full answers `WouldBlock`, which is the ordinary
    /// case and leaves the rest for the next turn.
    pub(crate) fn flush(&mut self) {
        while self.sent < self.outgoing.len() {
            let Some(rest) = self.outgoing.get(self.sent..) else {
                break;
            };
            match self.stream.write(rest) {
                Ok(0) => break,
                Ok(written) => self.sent += written,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.broken = true;
                    break;
                }
            }
        }
        if self.sent >= self.outgoing.len() {
            self.outgoing.clear();
            self.sent = 0;
        }
    }

    /// Take whatever has arrived. Answers whether the port is still there.
    pub(crate) fn receive(&mut self) -> bool {
        let mut room = [0_u8; READ_CHUNK];
        loop {
            match self.stream.read(&mut room) {
                // The driver closed the connection: the host end of the port
                // went away, or the device did.
                Ok(0) => return false,
                Ok(read) => self
                    .incoming
                    .extend_from_slice(room.get(..read).unwrap_or_default()),
                Err(error) if error.kind() == ErrorKind::WouldBlock => return true,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
    }

    /// The next message the host sent, if a whole one has arrived.
    ///
    /// Returns `None` both for "not yet" and for a message that could not be
    /// decoded: a refused message is not an error the agent can do anything
    /// about, and `docs/CLIPBOARD.md` §4 says an unimplemented type is not an
    /// error at all.
    ///
    /// The message is handed to `with` rather than returned because it
    /// borrows the assembly buffer, which belongs to this struct.
    pub(crate) fn next_message<T>(&mut self, with: impl FnOnce(&Message<'_>) -> T) -> Option<T> {
        if self.broken {
            return None;
        }
        let shape = self.shape.unwrap_or(Shape::PLAIN);
        let mut reassembler = Reassembler::new(&mut self.buffer);
        let mut taken = 0;
        // The reassembler stops at each message boundary, so this feeds until
        // it has one or the bytes run out.
        while taken < self.incoming.len() && reassembler.message().is_none() {
            let Some(rest) = self.incoming.get(taken..) else {
                break;
            };
            match reassembler.feed(rest) {
                Ok(0) => break,
                Ok(fed) => taken += fed,
                Err(_) => {
                    // The stream is no longer known to be at a boundary, so
                    // nothing after this is believed.
                    self.broken = true;
                    self.incoming.clear();
                    return None;
                }
            }
        }
        let assembled = reassembler.message()?;
        let decoded = Message::decode(assembled, shape).ok();
        let answer = decoded.as_ref().map(with);
        let learned = match decoded {
            Some(Message::AnnounceCapabilities { caps, .. }) => Some(Shape::from_caps(caps)),
            _ => None,
        };
        let _ = self.incoming.drain(..taken);
        if let Some(shape) = learned {
            self.shape = Some(shape);
        }
        answer
    }

    /// Whether the framing was lost, after which nothing more is read.
    #[must_use]
    pub(crate) const fn is_broken(&self) -> bool {
        self.broken
    }
}
