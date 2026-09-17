//! The Wayland client: connect, bind, make a window, draw.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use compositor_protocol::core::{
    self, wl_compositor, wl_display, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use compositor_protocol::xdg_shell::{self, xdg_surface, xdg_toplevel, xdg_wm_base};
use compositor_render::Pattern;
use compositor_socket::{Connection, RecvError, socket_path};
use compositor_wire::{Arg, ArgType, Fd, Interface, ObjectId, Reader, Writer};

use crate::shm::Shared;

/// The objects this client makes, at fixed ids. A client may name its own
/// objects however it likes as long as it does not reuse one, and fixed
/// numbers make the code and its log readable.
mod id {
    use compositor_wire::ObjectId;

    pub(super) const DISPLAY: ObjectId = ObjectId(1);
    pub(super) const REGISTRY: ObjectId = ObjectId(2);
    pub(super) const SYNC: ObjectId = ObjectId(3);
    pub(super) const COMPOSITOR: ObjectId = ObjectId(4);
    pub(super) const SHM: ObjectId = ObjectId(5);
    pub(super) const SHELL: ObjectId = ObjectId(6);
    pub(super) const SURFACE: ObjectId = ObjectId(7);
    pub(super) const XDG_SURFACE: ObjectId = ObjectId(8);
    pub(super) const TOPLEVEL: ObjectId = ObjectId(9);
    pub(super) const POOL: ObjectId = ObjectId(10);
    pub(super) const BUFFER: ObjectId = ObjectId(11);
}

/// How long to run before giving up, so a test can never hang.
///
/// A real client runs until it is closed; this one runs until the compositor
/// goes away, which closes the socket, or until this passes.
const DEADLINE: Duration = Duration::from_secs(15);

/// Connect, make a window, draw `pattern` in it, and keep drawing until the
/// compositor goes away.
///
/// # Errors
///
/// A sentence saying what could not be done.
pub fn run(pattern: Pattern, title: &str) -> Result<String, String> {
    let display =
        std::env::var("WAYLAND_DISPLAY").map_err(|_| "WAYLAND_DISPLAY is not set".to_owned())?;
    let path = socket_path(&display).map_err(|error| format!("the socket: {error}"))?;
    run_on(&path, pattern, title)
}

/// The same, on a socket named directly rather than through the environment.
///
/// The environment is one per process, so two clients running in one
/// process -- which is how the compositor's own test runs them -- cannot each
/// set `WAYLAND_DISPLAY`.
///
/// # Errors
///
/// A sentence saying what could not be done.
pub fn run_on(path: &std::path::Path, pattern: Pattern, title: &str) -> Result<String, String> {
    let stream = UnixStream::connect(path)
        .map_err(|error| format!("connecting to {}: {error}", path.display()))?;
    let mut connection =
        Connection::new(stream).map_err(|error| format!("the connection: {error}"))?;

    let mut out = Writer::new();
    // `wl_display.get_registry` is every client's first request.
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
    flush(&mut connection, &mut out)?;

    let mut state = Client {
        pattern,
        title: title.to_owned(),
        globals: BTreeMap::new(),
        bound: false,
        width: 0,
        height: 0,
        acked: false,
        drawn: 0,
        shared: None,
        buffer_size: (0, 0),
        released: 0,
    };

    let started = Instant::now();
    while started.elapsed() < DEADLINE {
        match connection.receive() {
            Ok(_) => {}
            Err(RecvError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(RecvError::Closed) => break,
            Err(error) => return Err(format!("reading: {error:?}")),
        }
        let consumed = state.read(connection.bytes(), &connection.fds(), &mut out)?;
        if consumed > 0 {
            connection.consume(consumed, 0);
        }
        flush(&mut connection, &mut out)?;
    }

    Ok(format!(
        "pattern: {:?} {}x{} frames {} title {}",
        state.pattern, state.width, state.height, state.drawn, state.title
    ))
}

/// What the client knows.
struct Client {
    pattern: Pattern,
    title: String,
    /// The registry's names, by interface.
    globals: BTreeMap<String, (u32, u32)>,
    bound: bool,
    width: i32,
    height: i32,
    acked: bool,
    drawn: u32,
    shared: Option<Shared>,
    buffer_size: (i32, i32),
    released: u32,
}

impl Client {
    /// Read whatever arrived, and answer it.
    fn read(&mut self, bytes: &[u8], fds: &[Fd], out: &mut Writer) -> Result<usize, String> {
        let mut reader = Reader::new(bytes, fds);
        while !reader.is_done() {
            let Ok(header) = reader.peek() else {
                break;
            };
            let Some(interface) = self.interface_of(header.sender) else {
                // An object this client did not make: the compositor is
                // speaking about something it invented, which it may not.
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
            self.event(header.sender, header.opcode, &args, out)?;
        }
        Ok(reader.consumed())
    }

    /// Which interface an object of this client's speaks.
    fn interface_of(&self, id: ObjectId) -> Option<&'static Interface> {
        Some(match id {
            id::DISPLAY => &core::WL_DISPLAY,
            id::REGISTRY => &core::WL_REGISTRY,
            id::SYNC => &core::WL_CALLBACK,
            id::SHM => &core::WL_SHM,
            id::SURFACE => &core::WL_SURFACE,
            id::XDG_SURFACE => &xdg_shell::XDG_SURFACE,
            id::TOPLEVEL => &xdg_shell::XDG_TOPLEVEL,
            id::SHELL => &xdg_shell::XDG_WM_BASE,
            id::BUFFER => &core::WL_BUFFER,
            _ => return None,
        })
    }

    /// Answer one event.
    fn event(
        &mut self,
        sender: ObjectId,
        opcode: u16,
        args: &[Arg<'_>],
        out: &mut Writer,
    ) -> Result<(), String> {
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
                let _ = self.globals.insert(interface.to_owned(), (name, version));
            }
            id::SYNC if opcode == core::wl_callback::event::DONE => {
                // The registry has been announced whole: bind what is needed.
                self.bind(out)?;
            }
            id::SHELL if opcode == xdg_wm_base::event::PING => {
                let serial = args.first().and_then(Arg::as_uint).unwrap_or(0);
                request(
                    out,
                    id::SHELL,
                    xdg_wm_base::request::PONG,
                    &[ArgType::Uint],
                    &[Arg::Uint(serial)],
                );
            }
            id::TOPLEVEL if opcode == xdg_toplevel::event::CONFIGURE => {
                let (width, height) = (
                    args.first().and_then(Arg::as_int).unwrap_or(0),
                    args.get(1).and_then(Arg::as_int).unwrap_or(0),
                );
                // A zero means "you choose", which every client answers with
                // the size it would like.
                self.width = if width > 0 { width } else { 640 };
                self.height = if height > 0 { height } else { 480 };
            }
            id::XDG_SURFACE if opcode == xdg_surface::event::CONFIGURE => {
                let serial = args.first().and_then(Arg::as_uint).unwrap_or(0);
                request(
                    out,
                    id::XDG_SURFACE,
                    xdg_surface::request::ACK_CONFIGURE,
                    &[ArgType::Uint],
                    &[Arg::Uint(serial)],
                );
                self.acked = true;
                self.draw(out)?;
            }
            id::TOPLEVEL if opcode == xdg_toplevel::event::CLOSE => {
                return Err("the compositor asked this window to close".to_owned());
            }
            id::BUFFER if opcode == core::wl_buffer::event::RELEASE => {
                self.released = self.released.saturating_add(1);
            }
            _ => {}
        }
        Ok(())
    }

    /// Bind what a window needs, and ask for one.
    fn bind(&mut self, out: &mut Writer) -> Result<(), String> {
        if self.bound {
            return Ok(());
        }
        for (interface, id, want) in [
            ("wl_compositor", id::COMPOSITOR, 6u32),
            ("wl_shm", id::SHM, 1),
            ("xdg_wm_base", id::SHELL, 6),
        ] {
            let (name, offered) = *self
                .globals
                .get(interface)
                .ok_or_else(|| format!("the compositor offers no {interface}"))?;
            let version = want.min(offered);
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
            .map_err(|error| format!("binding {interface}: {error:?}"))?;
        }
        self.bound = true;

        // The window, in the order the protocol requires.
        request(
            out,
            id::COMPOSITOR,
            wl_compositor::request::CREATE_SURFACE,
            &[ArgType::NewId],
            &[Arg::NewId(id::SURFACE)],
        );
        request(
            out,
            id::SHELL,
            xdg_wm_base::request::GET_XDG_SURFACE,
            &[ArgType::NewId, ArgType::Object { nullable: false }],
            &[Arg::NewId(id::XDG_SURFACE), Arg::Object(id::SURFACE)],
        );
        request(
            out,
            id::XDG_SURFACE,
            xdg_surface::request::GET_TOPLEVEL,
            &[ArgType::NewId],
            &[Arg::NewId(id::TOPLEVEL)],
        );
        let title = self.title.clone();
        request(
            out,
            id::TOPLEVEL,
            xdg_toplevel::request::SET_TITLE,
            &[ArgType::Str { nullable: false }],
            &[Arg::Str(Some(&title))],
        );
        request(
            out,
            id::TOPLEVEL,
            xdg_toplevel::request::SET_APP_ID,
            &[ArgType::Str { nullable: false }],
            &[Arg::Str(Some("rocks.magical.pattern"))],
        );
        // The first commit carries no buffer: it asks to be configured.
        request(out, id::SURFACE, wl_surface::request::COMMIT, &[], &[]);
        Ok(())
    }

    /// Draw the pattern at the size the compositor gave, and commit it.
    fn draw(&mut self, out: &mut Writer) -> Result<(), String> {
        if !self.acked || self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        let (width, height) = (self.width, self.height);
        let stride = width.saturating_mul(4);
        let len = usize::try_from(stride.saturating_mul(height))
            .map_err(|_| "a window too large to draw".to_owned())?;

        // A pool is made once and grown if the window does; the buffer is
        // made afresh each size, as a client that never keeps two.
        let fresh = self.shared.is_none() || self.buffer_size != (width, height);
        if fresh {
            // The ids are fixed, so the old objects have to go before the
            // new ones can take their numbers. A server is right to refuse a
            // `new_id` that is already live, and this one does.
            if self.shared.is_some() {
                request(out, id::BUFFER, core::wl_buffer::request::DESTROY, &[], &[]);
                request(out, id::POOL, wl_shm_pool::request::DESTROY, &[], &[]);
                // The mapping goes with them: a pool destroyed while the
                // compositor still reads a buffer from it keeps its memory
                // until that buffer is gone, which it now is.
                self.shared = None;
            }
            let shared = Shared::new(len).map_err(|error| format!("shared memory: {error}"))?;
            let fd = shared.as_raw_fd();
            self.shared = Some(shared);
            request_with_fd(
                out,
                id::SHM,
                wl_shm::request::CREATE_POOL,
                &[ArgType::NewId, ArgType::Fd, ArgType::Int],
                &[
                    Arg::NewId(id::POOL),
                    Arg::Fd(Fd(fd)),
                    Arg::Int(i32::try_from(len).unwrap_or(i32::MAX)),
                ],
            );
            request(
                out,
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
                    Arg::Int(width),
                    Arg::Int(height),
                    Arg::Int(stride),
                    Arg::Uint(self.pattern.format().wl_shm()),
                ],
            );
            self.buffer_size = (width, height);
        }

        // The pattern itself, drawn by `compositor/render` so the client and
        // the expected image are made from one piece of code.
        let pixels = self.pattern.draw(
            u32::try_from(width).unwrap_or(0),
            u32::try_from(height).unwrap_or(0),
        );
        if let Some(shared) = self.shared.as_mut() {
            let room = shared.bytes_mut();
            let take = pixels.len().min(room.len());
            if let (Some(to), Some(from)) = (room.get_mut(..take), pixels.get(..take)) {
                to.copy_from_slice(from);
            }
        }

        request(
            out,
            id::SURFACE,
            wl_surface::request::ATTACH,
            &[
                ArgType::Object { nullable: true },
                ArgType::Int,
                ArgType::Int,
            ],
            &[Arg::Object(id::BUFFER), Arg::Int(0), Arg::Int(0)],
        );
        request(
            out,
            id::SURFACE,
            wl_surface::request::DAMAGE_BUFFER,
            &[ArgType::Int, ArgType::Int, ArgType::Int, ArgType::Int],
            &[Arg::Int(0), Arg::Int(0), Arg::Int(width), Arg::Int(height)],
        );
        request(out, id::SURFACE, wl_surface::request::COMMIT, &[], &[]);
        self.drawn = self.drawn.saturating_add(1);
        Ok(())
    }
}

/// Queue one request.
fn request(
    out: &mut Writer,
    sender: ObjectId,
    opcode: u16,
    signature: &'static [ArgType],
    args: &[Arg<'_>],
) {
    // The only way this fails is a message longer than the format allows,
    // which none of this client's is.
    let _ = out.write(sender, opcode, signature, args);
}

/// Queue one request that carries a descriptor.
fn request_with_fd(
    out: &mut Writer,
    sender: ObjectId,
    opcode: u16,
    signature: &'static [ArgType],
    args: &[Arg<'_>],
) {
    request(out, sender, opcode, signature, args);
}

/// Send everything queued.
fn flush(connection: &mut Connection, out: &mut Writer) -> Result<(), String> {
    if out.is_empty() {
        return Ok(());
    }
    let (bytes, fds) = out.take();
    connection
        .send(&bytes, &fds)
        .map_err(|error| format!("writing: {error:?}"))
}

/// So the module compiles on a host without the descriptor plumbing.
const _: fn() -> io::Result<()> = || Ok(());
