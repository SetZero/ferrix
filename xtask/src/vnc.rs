//! A VNC viewer, as much of one as asks a server for the pointer's shape.
//!
//! A pointer on virtio-gpu's cursor plane is not in QEMU's screendump: the
//! plane is the host's to show, over the frame, and a screendump is the
//! frame. What does show it is a display that asks for it, and QEMU's VNC
//! server is one: a viewer that offers the `RichCursor` pseudo-encoding is
//! handed the image the guest set, and draws it where its own mouse is. That
//! is exactly what makes a served desktop's pointer free of lag, so it is
//! also the right judge of it. This speaks as much of RFB 3.8 as that takes:
//! no authentication, 32-bit pixels, `RichCursor` and `Raw`, and updates read
//! until a shape arrives.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::{Error, Result};

/// The `RichCursor` pseudo-encoding: the pointer's image and a mask.
const RICH_CURSOR: i32 = -239;
/// `Raw`: every pixel as it is.
const RAW: i32 = 0;

/// A pointer's shape as a VNC server sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shape {
    /// The hotspot, from the image's top-left corner.
    pub(crate) hot: (u16, u16),
    /// The image's size.
    pub(crate) size: (u16, u16),
    /// Four bytes a pixel, blue first: the pixel format [`Viewer`] asks for.
    pub(crate) pixels: Vec<u8>,
    /// A bit a pixel, most significant first, each row whole bytes: set
    /// where the pointer is drawn.
    pub(crate) mask: Vec<u8>,
}

impl Shape {
    /// Whether the pixel at (`x`, `y`) is drawn.
    pub(crate) fn shows(&self, x: u16, y: u16) -> bool {
        let row = usize::from(self.size.0).div_ceil(8);
        self.mask
            .get(usize::from(y) * row + usize::from(x) / 8)
            .is_some_and(|byte| byte & (0x80 >> (x % 8)) != 0)
    }

    /// The colour at (`x`, `y`), blue first.
    pub(crate) fn pixel(&self, x: u16, y: u16) -> Option<[u8; 3]> {
        let at = (usize::from(y) * usize::from(self.size.0) + usize::from(x)) * 4;
        match *self.pixels.get(at..at + 3)? {
            [blue, green, red] => Some([blue, green, red]),
            _ => None,
        }
    }
}

/// The big-endian `u16` at `at`, or 0 past the end.
fn be16(bytes: &[u8], at: usize) -> u16 {
    bytes
        .get(at..at + 2)
        .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
        .map_or(0, u16::from_be_bytes)
}

/// The big-endian `u32` at `at`, or 0 past the end.
fn be32(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map_or(0, u32::from_be_bytes)
}

/// A connection to a VNC server, handshaken, offering `RichCursor`.
#[derive(Debug)]
pub(crate) struct Viewer {
    stream: TcpStream,
    size: (u16, u16),
}

impl Viewer {
    /// Connect to the server on the loopback's `port`, trying until
    /// `deadline`: a boot's server is listening a moment after QEMU starts.
    ///
    /// # Errors
    ///
    /// Nothing listening by `deadline`, or a server that wants a password
    /// or is not speaking RFB.
    pub(crate) fn connect(port: u16, deadline: Instant) -> Result<Self> {
        let stream = loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => break stream,
                Err(error) if Instant::now() >= deadline => {
                    return Err(Error::new(format!("vnc: nothing on port {port}: {error}")));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        stream.set_read_timeout(Some(Duration::from_millis(250)))?;
        let _ = stream.set_nodelay(true);
        let mut viewer = Self {
            stream,
            size: (0, 0),
        };
        viewer.handshake(deadline)?;
        Ok(viewer)
    }

    fn handshake(&mut self, deadline: Instant) -> Result<()> {
        let version = self.read_exact(12, deadline)?;
        if !version.starts_with(b"RFB ") {
            return Err(Error::new(format!("vnc: not a VNC server: {version:?}")));
        }
        self.stream.write_all(b"RFB 003.008\n")?;
        let count = self.read_exact(1, deadline)?.first().copied().unwrap_or(0);
        if count == 0 {
            return Err(Error::new("vnc: the server refused the connection"));
        }
        let kinds = self.read_exact(usize::from(count), deadline)?;
        if !kinds.contains(&1) {
            return Err(Error::new(format!(
                "vnc: the server wants authentication: {kinds:?}"
            )));
        }
        self.stream.write_all(&[1])?;
        if self.read_exact(4, deadline)? != [0, 0, 0, 0] {
            return Err(Error::new("vnc: the server refused the connection"));
        }
        // Shared, as a viewer beside another viewer is.
        self.stream.write_all(&[1])?;
        let init = self.read_exact(24, deadline)?;
        self.size = (be16(&init, 0), be16(&init, 2));
        let name = be32(&init, 20);
        let _ = self.read_exact(name as usize, deadline)?;
        // 32 bits a pixel, true colour, little-endian, red at 16: what the
        // guest draws.
        self.stream.write_all(&[
            0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0,
        ])?;
        let mut encodings = vec![2, 0, 0, 2];
        encodings.extend_from_slice(&RICH_CURSOR.to_be_bytes());
        encodings.extend_from_slice(&RAW.to_be_bytes());
        self.stream.write_all(&encodings)?;
        Ok(())
    }

    /// Read updates, asking for them as a viewer does, until the server
    /// sends the pointer's shape: `None` if none came by `deadline`.
    ///
    /// # Errors
    ///
    /// A server that closed, or sent something this does not read.
    pub(crate) fn cursor(&mut self, deadline: Instant) -> Result<Option<Shape>> {
        self.request(false)?;
        while Instant::now() < deadline {
            let Some(kind) = self.read_some(1, deadline)? else {
                // Quiet: ask again, as a viewer that is still watching does.
                self.request(true)?;
                continue;
            };
            match kind.first().copied().unwrap_or(0) {
                0 => {
                    if let Some(shape) = self.update(deadline)? {
                        return Ok(Some(shape));
                    }
                    self.request(true)?;
                }
                // A bell.
                2 => {}
                // Cut text: its length and its bytes.
                3 => {
                    let header = self.read_exact(7, deadline)?;
                    let len = be32(&header, 3);
                    let _ = self.read_exact(len as usize, deadline)?;
                }
                other => {
                    return Err(Error::new(format!(
                        "vnc: a server message this does not read: {other}"
                    )));
                }
            }
        }
        Ok(None)
    }

    /// Ask for the whole screen, or for what changed since the last update.
    fn request(&mut self, incremental: bool) -> Result<()> {
        let (width, height) = self.size;
        let mut message = vec![3, u8::from(incremental), 0, 0, 0, 0];
        message.extend_from_slice(&width.to_be_bytes());
        message.extend_from_slice(&height.to_be_bytes());
        self.stream.write_all(&message)?;
        Ok(())
    }

    /// The rest of one `FramebufferUpdate`: the shape, if it carried one.
    fn update(&mut self, deadline: Instant) -> Result<Option<Shape>> {
        let header = self.read_exact(3, deadline)?;
        let rects = be16(&header, 1);
        let mut shape = None;
        for _ in 0..rects {
            let rect = self.read_exact(12, deadline)?;
            let (x, y) = (be16(&rect, 0), be16(&rect, 2));
            let (width, height) = (be16(&rect, 4), be16(&rect, 6));
            let encoding = be32(&rect, 8).cast_signed();
            let pixels = usize::from(width) * usize::from(height) * 4;
            match encoding {
                RAW => {
                    let _ = self.read_exact(pixels, deadline)?;
                }
                RICH_CURSOR => {
                    let image = self.read_exact(pixels, deadline)?;
                    let mask = self.read_exact(
                        usize::from(width).div_ceil(8) * usize::from(height),
                        deadline,
                    )?;
                    shape = Some(Shape {
                        hot: (x, y),
                        size: (width, height),
                        pixels: image,
                        mask,
                    });
                }
                other => {
                    return Err(Error::new(format!(
                        "vnc: an encoding this does not read: {other}"
                    )));
                }
            }
        }
        Ok(shape)
    }

    /// `len` bytes, however long they take before `deadline`.
    fn read_exact(&mut self, len: usize, deadline: Instant) -> Result<Vec<u8>> {
        let mut bytes = vec![0u8; len];
        let mut have = 0;
        while have < len {
            let Some(rest) = bytes.get_mut(have..) else {
                break;
            };
            match self.stream.read(rest) {
                Ok(0) => return Err(Error::new("vnc: the server closed the connection")),
                Ok(read) => have += read,
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    if Instant::now() >= deadline {
                        return Err(Error::new("vnc: the server stopped half way"));
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(bytes)
    }

    /// `len` bytes if any arrive before the read times out, `None` if none
    /// do; the rest waited for as [`Viewer::read_exact`] does.
    fn read_some(&mut self, len: usize, deadline: Instant) -> Result<Option<Vec<u8>>> {
        let mut first = [0u8; 1];
        match self.stream.read(&mut first) {
            Ok(0) => Err(Error::new("vnc: the server closed the connection")),
            Ok(_) => {
                let mut bytes = vec![first[0]];
                bytes.extend(self.read_exact(len - 1, deadline)?);
                Ok(Some(bytes))
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }
}
