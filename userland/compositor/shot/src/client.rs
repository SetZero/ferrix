//! The Wayland side of taking a screenshot.
//!
//! `zwlr_screencopy_manager_v1.capture_output` gives a frame; the frame says
//! what buffer to make; `wl_shm` makes one; `copy` hands it over; `ready`
//! says it is written. That is the whole protocol, and every screenshot
//! program on wlroots is this plus a file format.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use compositor_protocol::core::{self, wl_display, wl_registry, wl_shm, wl_shm_pool};
use compositor_protocol::screencopy::{
    self, zwlr_screencopy_frame_v1 as frame, zwlr_screencopy_manager_v1 as manager,
};
use compositor_shm::Shared;
use compositor_socket::{Connection, RecvError};
use compositor_wire::{Arg, ArgType, Fd, Interface, ObjectId, Reader, Writer};

/// The objects this client makes, at fixed ids.
mod id {
    use compositor_wire::ObjectId;

    pub(super) const DISPLAY: ObjectId = ObjectId(1);
    pub(super) const REGISTRY: ObjectId = ObjectId(2);
    pub(super) const SYNC: ObjectId = ObjectId(3);
    pub(super) const SHM: ObjectId = ObjectId(4);
    pub(super) const MANAGER: ObjectId = ObjectId(5);
    pub(super) const OUTPUT: ObjectId = ObjectId(6);
    pub(super) const FRAME: ObjectId = ObjectId(7);
    pub(super) const POOL: ObjectId = ObjectId(8);
    pub(super) const BUFFER: ObjectId = ObjectId(9);
}

/// How long to wait for the compositor to write the screenshot.
const PATIENCE: Duration = Duration::from_secs(20);

/// What a screenshot holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shot {
    /// In pixels.
    pub width: u32,
    /// In pixels.
    pub height: u32,
    /// The pixels as a PPM holds them: red, green and blue a pixel, in row
    /// order. The compositor's frames are opaque, so nothing is lost by
    /// leaving the fourth byte out, and this is what an expected image is
    /// compared against.
    pub pixels: Vec<u8>,
}

impl Shot {
    /// The line `shot` prints: the size and the digest.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "shot: {}x{} {:016x}",
            self.width,
            self.height,
            digest(&self.pixels)
        )
    }
}

/// FNV-1a over `bytes`, which is how a whole screen is compared through a
/// serial port.
///
/// Not a cryptographic hash and not meant to be one: what it has to do is
/// differ when one pixel differs, which FNV does, and be short enough to
/// print on one line and simple enough that a test on the host and a program
/// on the guest cannot disagree about it.
#[must_use]
pub fn digest(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Take a screenshot of the `which`th screen.
///
/// # Errors
///
/// A sentence saying what could not be done, including the compositor not
/// offering the protocol, having no such screen, or refusing the frame.
pub fn take(socket: &std::path::Path, which: usize) -> Result<Shot, String> {
    let mut session = Session::connect(socket, which)?;
    session.run()?;
    session.shot.take().ok_or_else(|| {
        session
            .failure
            .clone()
            .unwrap_or_else(|| "the compositor never wrote the screenshot".to_owned())
    })
}

/// One connection, taking one screenshot.
struct Session {
    connection: Connection,
    out: Writer,
    globals: BTreeMap<String, (u32, u32)>,
    /// The `wl_output` globals, in the order the registry announced them.
    outputs: Vec<(u32, u32)>,
    /// Which screen was asked for.
    which: usize,
    bound: bool,
    /// The memory the screenshot is written into, once its size is known.
    shared: Option<Shared>,
    /// What the compositor said to make: width, height and stride.
    shape: Option<(u32, u32, u32)>,
    /// The screenshot, once `ready` has arrived.
    shot: Option<Shot>,
    /// Why it failed, if it did.
    failure: Option<String>,
    /// Whether there is nothing left to wait for.
    done: bool,
}

impl Session {
    /// Connect and ask for the registry.
    fn connect(socket: &std::path::Path, which: usize) -> Result<Self, String> {
        let stream = std::os::unix::net::UnixStream::connect(socket)
            .map_err(|error| format!("connecting to {}: {error}", socket.display()))?;
        let connection =
            Connection::new(stream).map_err(|error| format!("the connection: {error}"))?;
        let mut out = Writer::new();
        request(
            &mut out,
            id::DISPLAY,
            wl_display::request::GET_REGISTRY,
            &[ArgType::NewId],
            &[Arg::NewId(id::REGISTRY)],
        );
        request(
            &mut out,
            id::DISPLAY,
            wl_display::request::SYNC,
            &[ArgType::NewId],
            &[Arg::NewId(id::SYNC)],
        );
        Ok(Self {
            connection,
            out,
            globals: BTreeMap::new(),
            outputs: Vec::new(),
            which,
            bound: false,
            shared: None,
            shape: None,
            shot: None,
            failure: None,
            done: false,
        })
    }

    /// Read until the screenshot is written or the time is up.
    fn run(&mut self) -> Result<(), String> {
        let started = Instant::now();
        self.flush()?;
        while started.elapsed() < PATIENCE {
            match self.connection.receive() {
                Ok(_) => {}
                Err(RecvError::WouldBlock) => {}
                Err(RecvError::Closed) => break,
                Err(error) => return Err(format!("reading: {error:?}")),
            }
            let (consumed, claimed) = self.read()?;
            if consumed > 0 {
                self.connection.consume(consumed, claimed);
            }
            self.flush()?;
            if self.done {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Err("the compositor never answered the screenshot".to_owned())
    }

    /// Send whatever is queued.
    fn flush(&mut self) -> Result<(), String> {
        if self.out.is_empty() {
            // What a full socket left queued still has to go.
            return self
                .connection
                .flush()
                .map_err(|error| format!("writing: {error:?}"));
        }
        let (bytes, fds) = self.out.take();
        self.connection
            .send(&bytes, &fds)
            .map_err(|error| format!("writing: {error:?}"))
    }

    /// Read whatever arrived, and answer it.
    fn read(&mut self) -> Result<(usize, usize), String> {
        let fds = self.connection.fds();
        let bytes = self.connection.bytes().to_vec();
        let mut reader = Reader::new(&bytes, &fds);
        while !reader.is_done() {
            let Ok(header) = reader.peek() else {
                break;
            };
            let Some(interface) = interface_of(header.sender) else {
                return Err(format!("an event for object {}", header.sender.0));
            };
            let Some(method) = interface.event(header.opcode) else {
                return Err(format!("{} has no event {}", interface.name, header.opcode));
            };
            let (_, args) = match reader.read(method.signature) {
                Ok(read) => read,
                Err(compositor_wire::Error::Incomplete { .. }) => break,
                Err(error) => return Err(format!("{}.{}: {error:?}", interface.name, method.name)),
            };
            self.event(header.sender, header.opcode, &args)?;
        }
        Ok((reader.consumed(), reader.descriptors_taken()))
    }

    /// Answer one event.
    fn event(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) -> Result<(), String> {
        match sender {
            id::DISPLAY if opcode == wl_display::event::ERROR => {
                let text = args.get(2).and_then(Arg::as_str).unwrap_or("");
                return Err(format!("the compositor refused this client: {text}"));
            }
            id::REGISTRY if opcode == wl_registry::event::GLOBAL => {
                let (Some(name), Some(interface), Some(version)) = (
                    args.first().and_then(Arg::as_uint),
                    args.get(1).and_then(Arg::as_str),
                    args.get(2).and_then(Arg::as_uint),
                ) else {
                    return Ok(());
                };
                if interface == "wl_output" {
                    self.outputs.push((name, version));
                }
                let _ = self.globals.insert(interface.to_owned(), (name, version));
            }
            id::SYNC if opcode == core::wl_callback::event::DONE => self.bind()?,
            id::FRAME if opcode == frame::event::BUFFER => {
                let value = |at: usize| args.get(at).and_then(Arg::as_uint).unwrap_or(0);
                self.offered(value(0), value(1), value(2), value(3))?;
            }
            // `buffer_done` says the list of formats is complete, and the
            // one above is the only one this asks for.
            id::FRAME if opcode == frame::event::BUFFER_DONE => {}
            id::FRAME if opcode == frame::event::READY => {
                self.read_back();
                self.done = true;
            }
            id::FRAME if opcode == frame::event::FAILED => {
                self.failure = Some("the compositor refused the frame".to_owned());
                self.done = true;
            }
            _ => {}
        }
        Ok(())
    }

    /// Bind what a screenshot needs, and ask for the frame.
    fn bind(&mut self) -> Result<(), String> {
        if self.bound {
            return Ok(());
        }
        self.bound = true;
        for (interface, id, want) in [
            ("wl_shm", id::SHM, 1u32),
            ("zwlr_screencopy_manager_v1", id::MANAGER, 3),
        ] {
            let (name, offered) = self
                .globals
                .get(interface)
                .copied()
                .ok_or_else(|| format!("the compositor offers no {interface}"))?;
            bind(&mut self.out, name, interface, want.min(offered), id)?;
        }
        let (name, version) = self
            .outputs
            .get(self.which)
            .copied()
            .ok_or_else(|| format!("the compositor has no screen {}", self.which))?;
        bind(&mut self.out, name, "wl_output", version.min(4), id::OUTPUT)?;
        request(
            &mut self.out,
            id::MANAGER,
            manager::request::CAPTURE_OUTPUT,
            &[
                ArgType::NewId,
                ArgType::Int,
                ArgType::Object { nullable: false },
            ],
            // No cursor: nothing draws one into the frame to leave out.
            &[Arg::NewId(id::FRAME), Arg::Int(0), Arg::Object(id::OUTPUT)],
        );
        Ok(())
    }

    /// Make the buffer the compositor said to make, and hand it over.
    fn offered(&mut self, format: u32, width: u32, height: u32, stride: u32) -> Result<(), String> {
        if self.shared.is_some() {
            return Ok(());
        }
        let len = usize::try_from(stride)
            .ok()
            .and_then(|stride| stride.checked_mul(usize::try_from(height).ok()?))
            .filter(|len| *len > 0)
            .ok_or_else(|| format!("a {width}x{height} frame at stride {stride}"))?;
        let shared = Shared::new(len).map_err(|error| format!("the memory: {error}"))?;
        request_with_fd(
            &mut self.out,
            id::SHM,
            wl_shm::request::CREATE_POOL,
            &[ArgType::NewId, ArgType::Fd, ArgType::Int],
            &[
                Arg::NewId(id::POOL),
                Arg::Fd(Fd(shared.as_raw_fd())),
                Arg::Int(i32::try_from(len).unwrap_or(i32::MAX)),
            ],
        );
        let int = |value: u32| Arg::Int(i32::try_from(value).unwrap_or(0));
        request(
            &mut self.out,
            id::POOL,
            wl_shm_pool::request::CREATE_BUFFER,
            &[
                ArgType::NewId,
                ArgType::Int,
                ArgType::Int,
                ArgType::Int,
                ArgType::Int,
                ArgType::Uint,
            ],
            &[
                Arg::NewId(id::BUFFER),
                Arg::Int(0),
                int(width),
                int(height),
                int(stride),
                Arg::Uint(format),
            ],
        );
        // The pool is thrown away as soon as the buffer is made, which is
        // what `grim` and every toolkit does: `wl_shm_pool.destroy`
        // releases the object, and the memory stays until the last buffer
        // made from it is gone. A compositor that unmaps at `destroy`
        // fails this copy, so taking the screenshot is the test.
        request(
            &mut self.out,
            id::POOL,
            wl_shm_pool::request::DESTROY,
            &[],
            &[],
        );
        request(
            &mut self.out,
            id::FRAME,
            frame::request::COPY,
            &[ArgType::Object { nullable: false }],
            &[Arg::Object(id::BUFFER)],
        );
        self.shape = Some((width, height, stride));
        self.shared = Some(shared);
        Ok(())
    }

    /// Take the pixels out of the buffer the compositor wrote into.
    fn read_back(&mut self) {
        let (Some((width, height, stride)), Some(shared)) = (self.shape, self.shared.as_mut())
        else {
            return;
        };
        let bytes = shared.bytes_mut();
        let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 3);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let at = y * stride as usize + x * 4;
                // `XRGB8888` is little-endian, so the bytes are blue,
                // green, red and the one that is not a channel.
                let Some(pixel) = bytes.get(at..at + 4) else {
                    return;
                };
                let byte = |at: usize| pixel.get(at).copied().unwrap_or(0);
                pixels.extend_from_slice(&[byte(2), byte(1), byte(0)]);
            }
        }
        self.shot = Some(Shot {
            width,
            height,
            pixels,
        });
    }
}

/// Which interface an object speaks.
fn interface_of(id: ObjectId) -> Option<&'static Interface> {
    Some(match id {
        id::DISPLAY => &core::WL_DISPLAY,
        id::REGISTRY => &core::WL_REGISTRY,
        id::SYNC => &core::WL_CALLBACK,
        id::SHM => &core::WL_SHM,
        id::MANAGER => &screencopy::ZWLR_SCREENCOPY_MANAGER_V1,
        id::OUTPUT => &core::WL_OUTPUT,
        id::FRAME => &screencopy::ZWLR_SCREENCOPY_FRAME_V1,
        id::POOL => &core::WL_SHM_POOL,
        id::BUFFER => &core::WL_BUFFER,
        _ => return None,
    })
}

/// Bind one global at a fixed id.
fn bind(
    out: &mut Writer,
    name: u32,
    interface: &'static str,
    version: u32,
    id: ObjectId,
) -> Result<(), String> {
    out.write(
        id::REGISTRY,
        wl_registry::request::BIND,
        &[ArgType::Uint, ArgType::AnyNewId],
        &[
            Arg::Uint(name),
            Arg::AnyNewId {
                interface,
                version,
                id,
            },
        ],
    )
    .map_err(|error| format!("binding {interface}: {error:?}"))
}

/// Queue one request.
fn request(
    out: &mut Writer,
    sender: ObjectId,
    opcode: u16,
    signature: &'static [ArgType],
    args: &[Arg<'_>],
) {
    let _ = out.write(sender, opcode, signature, args);
}

/// The same, for a request that carries a descriptor.
fn request_with_fd(
    out: &mut Writer,
    sender: ObjectId,
    opcode: u16,
    signature: &'static [ArgType],
    args: &[Arg<'_>],
) {
    request(out, sender, opcode, signature, args);
}
