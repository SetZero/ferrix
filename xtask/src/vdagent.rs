//! The host's half of vdagent, as `cargo xtask test-clipboard` plays it.
//!
//! `docs/CLIPBOARD.md` §9: the gate does not use QEMU's `qemu-vdagent`
//! chardev, because that bridges to the UI's clipboard and a headless boot
//! has no UI to bridge to. It attaches a plain socket chardev instead and
//! speaks vdagent on it exactly as a viewer would, which makes the whole
//! guest path -- device, driver, agent, compositor -- the thing under test
//! and the host's own clipboard no part of it.
//!
//! The protocol is `libs/vdagent`, the same crate the guest's agent uses.
//! That is not a test speaking to itself: the crate encodes and decodes
//! bytes and knows nothing of who owns the clipboard or when, and everything
//! this file asserts is about what the guest *did*, on the far side of a
//! device.

use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use ferrix_vdagent::chunk::{self, Reassembler};
use ferrix_vdagent::message::{AGENT_CAPS, ClipboardType, Message, Selection, Shape, Types};

use crate::{Error, Result};

/// The largest message the viewer will assemble: `docs/CLIPBOARD.md` §7a's
/// megabyte and the header of a `CLIPBOARD` carrying it.
const MAX_MESSAGE: usize = 1024 * 1024 + 64;

/// How long the viewer waits between looks at the socket.
const IDLE: Duration = Duration::from_millis(5);

/// The host end of the port, and the conversation on it.
pub(crate) struct Viewer {
    /// The connection to QEMU's socket chardev.
    stream: UnixStream,
    /// Bytes arrived and not yet framed.
    incoming: Vec<u8>,
    /// Where assembled messages are put.
    buffer: Vec<u8>,
    /// The layout the guest's capabilities asked for.
    shape: Shape,
    /// Whether the guest has announced itself.
    announced: bool,
    /// The serial the next grab carries.
    serial: u32,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer")
            .field("shape", &self.shape)
            .field("announced", &self.announced)
            .finish_non_exhaustive()
    }
}

impl Viewer {
    /// Connect to the socket QEMU is listening on, waiting for it to appear.
    ///
    /// QEMU makes the socket as it starts and the guest takes a while to
    /// boot, so the deadline is for the file rather than for the guest.
    pub(crate) fn connect(path: &Path, deadline: Instant) -> Result<Viewer> {
        loop {
            match UnixStream::connect(path) {
                Ok(stream) => {
                    stream.set_nonblocking(true)?;
                    return Ok(Viewer {
                        stream,
                        incoming: Vec::new(),
                        buffer: vec![0; MAX_MESSAGE],
                        shape: Shape::QEMU_CLIPBOARD,
                        announced: false,
                        serial: 0,
                    });
                }
                Err(error) if Instant::now() >= deadline => {
                    return Err(Error::new(format!(
                        "connecting to the clipboard socket at {}: {error}",
                        path.display()
                    )));
                }
                Err(_) => std::thread::sleep(IDLE),
            }
        }
    }

    /// Announce what a viewer can do, and wait for the guest to announce
    /// back: until it has, nothing else can be said, because the
    /// capabilities are what decide every clipboard message's layout.
    pub(crate) fn handshake(&mut self, deadline: Instant) -> Result<()> {
        self.send(&Message::AnnounceCapabilities {
            request: true,
            caps: AGENT_CAPS,
        })?;
        let announced = self.wait(deadline, |message| {
            matches!(message, Message::AnnounceCapabilities { .. }).then_some(())
        })?;
        announced.ok_or_else(|| {
            Error::new("the guest's agent never announced its capabilities".to_owned())
        })
    }

    /// Grab the clipboard as a viewer whose person has just copied text.
    pub(crate) fn grab(&mut self) -> Result<()> {
        self.serial = self.serial.wrapping_add(1);
        let types = Types::new(&[ClipboardType::Utf8Text])
            .map_err(|error| Error::new(format!("a grab: {error}")))?;
        self.send(&Message::ClipboardGrab {
            selection: Selection::Clipboard,
            serial: Some(self.serial),
            types,
        })
    }

    /// Answer the guest's request for the clipboard with `text`.
    pub(crate) fn answer(&mut self, text: &str) -> Result<()> {
        self.send(&Message::Clipboard {
            selection: Selection::Clipboard,
            kind: ClipboardType::Utf8Text,
            data: text.as_bytes(),
        })
    }

    /// Wait for the guest to ask for the clipboard, and answer it with
    /// `text`. Answers whether it asked.
    pub(crate) fn serve(&mut self, text: &str, deadline: Instant) -> Result<bool> {
        let asked = self.wait(deadline, |message| {
            matches!(
                message,
                Message::ClipboardRequest {
                    selection: Selection::Clipboard,
                    kind: ClipboardType::Utf8Text,
                }
            )
            .then_some(())
        })?;
        if asked.is_none() {
            return Ok(false);
        }
        self.answer(text)?;
        Ok(true)
    }

    /// Wait for the guest to grab the clipboard, ask for it, and give back
    /// what came: which is what a person pasting into the viewer's window
    /// makes happen.
    pub(crate) fn paste(&mut self, deadline: Instant) -> Result<Option<String>> {
        let grabbed = self.wait(deadline, |message| match message {
            Message::ClipboardGrab {
                selection: Selection::Clipboard,
                types,
                ..
            } => types.holds(ClipboardType::Utf8Text).then_some(()),
            _ => None,
        })?;
        if grabbed.is_none() {
            return Ok(None);
        }
        self.send(&Message::ClipboardRequest {
            selection: Selection::Clipboard,
            kind: ClipboardType::Utf8Text,
        })?;
        self.wait(deadline, |message| match message {
            Message::Clipboard {
                selection: Selection::Clipboard,
                kind: ClipboardType::Utf8Text,
                data,
            } => Some(String::from_utf8_lossy(data).into_owned()),
            _ => None,
        })
    }

    /// Send one message, framed.
    fn send(&mut self, message: &Message<'_>) -> Result<()> {
        let mut encoded = vec![0_u8; message.encoded_len(self.shape)];
        let written = message
            .encode(self.shape, &mut encoded)
            .map_err(|error| Error::new(format!("encoding {message:?}: {error}")))?;
        let body = encoded.get(..written).unwrap_or_default();
        let mut framed = vec![0_u8; chunk::framed_len(body.len())];
        let framed_len = chunk::frame(body, &mut framed)
            .map_err(|error| Error::new(format!("framing {message:?}: {error}")))?;
        let bytes = framed.get(..framed_len).unwrap_or_default();
        let mut sent = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while sent < bytes.len() {
            let Some(rest) = bytes.get(sent..) else {
                break;
            };
            match self.stream.write(rest) {
                Ok(0) => break,
                Ok(written) => sent += written,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(Error::new(
                            "the guest never read what the viewer sent".to_owned(),
                        ));
                    }
                    std::thread::sleep(IDLE);
                }
                Err(error) => return Err(Error::new(format!("writing to the guest: {error}"))),
            }
        }
        Ok(())
    }

    /// Read until `want` says a message is the one, or the time is up.
    fn wait<T>(
        &mut self,
        deadline: Instant,
        want: impl Fn(&Message<'_>) -> Option<T>,
    ) -> Result<Option<T>> {
        loop {
            while let Some(found) = self.next_message(&want)? {
                if let Some(found) = found {
                    return Ok(Some(found));
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            if !self.receive()? {
                return Ok(None);
            }
            std::thread::sleep(IDLE);
        }
    }

    /// Take whatever has arrived. Answers whether the socket is still open.
    fn receive(&mut self) -> Result<bool> {
        let mut room = [0_u8; 16 * 1024];
        loop {
            match self.stream.read(&mut room) {
                Ok(0) => return Ok(false),
                Ok(read) => self
                    .incoming
                    .extend_from_slice(room.get(..read).unwrap_or_default()),
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(true),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(Error::new(format!("reading from the guest: {error}"))),
            }
        }
    }

    /// The next whole message, passed to `want`.
    ///
    /// `Ok(None)` is "no whole message yet"; `Ok(Some(None))` is one that was
    /// not the one being waited for, which is how the caller knows to look
    /// again without reading more.
    fn next_message<T>(
        &mut self,
        want: &impl Fn(&Message<'_>) -> Option<T>,
    ) -> Result<Option<Option<T>>> {
        let mut reassembler = Reassembler::new(&mut self.buffer);
        let mut taken = 0;
        while taken < self.incoming.len() && reassembler.message().is_none() {
            let Some(rest) = self.incoming.get(taken..) else {
                break;
            };
            match reassembler.feed(rest) {
                Ok(0) => break,
                Ok(fed) => taken += fed,
                Err(error) => {
                    return Err(Error::new(format!("the guest's framing was lost: {error}")));
                }
            }
        }
        let Some(assembled) = reassembler.message() else {
            return Ok(None);
        };
        let decoded = Message::decode(assembled, self.shape)
            .map_err(|error| Error::new(format!("a message from the guest: {error}")))?;
        // An announcement is what decides every later message's layout, so
        // it is acted on here rather than left to the caller.
        let learned = match decoded {
            Message::AnnounceCapabilities { caps, .. } => Some(Shape::from_caps(caps & AGENT_CAPS)),
            _ => None,
        };
        let found = want(&decoded);
        let _ = self.incoming.drain(..taken);
        if let Some(shape) = learned {
            self.shape = shape;
            self.announced = true;
        }
        Ok(Some(found))
    }
}
