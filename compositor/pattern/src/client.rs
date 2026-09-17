//! The Wayland client: connect, bind, make a window, draw.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use compositor_protocol::core::{
    self, wl_compositor, wl_display, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use compositor_protocol::layer_shell::{self, zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use compositor_protocol::xdg_shell::{self, xdg_surface, xdg_toplevel, xdg_wm_base};
use compositor_render::Pattern;
use compositor_socket::{Connection, RecvError, socket_path};
use compositor_wire::{Arg, ArgType, Fd, Interface, ObjectId, Reader, Writer};

use compositor_shm::Shared;

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
    pub(super) const LAYER_SHELL: ObjectId = ObjectId(15);
    pub(super) const LAYER_SURFACE: ObjectId = ObjectId(16);
    pub(super) const SEAT: ObjectId = ObjectId(12);
    pub(super) const OUTPUT: ObjectId = ObjectId(17);
    pub(super) const KEYBOARD: ObjectId = ObjectId(13);
    pub(super) const POINTER: ObjectId = ObjectId(14);
}

/// How long to run before giving up, so a test can never hang.
///
/// A real client runs until it is closed; this one runs until the compositor
/// goes away, which closes the socket, or until this passes. Long enough that
/// it is never what ends a test: it is a window under a compositor's control,
/// and a window that vanished on its own would look like a compositor that
/// closed it.
const DEADLINE: Duration = Duration::from_secs(600);

/// What this client asks the compositor to make of its surface.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Shape {
    /// An `xdg_toplevel`: a window the layout tiles.
    #[default]
    Window,
    /// A `zwlr_layer_surface_v1` on the `top` layer, anchored across the top
    /// edge, this many pixels tall and reserving all of them: what a bar
    /// asks for.
    Bar(u32),
}

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

/// The same, as a bar rather than a window.
///
/// # Errors
///
/// A sentence saying what could not be done.
pub fn run_shaped(pattern: Pattern, title: &str, shape: Shape) -> Result<String, String> {
    let display =
        std::env::var("WAYLAND_DISPLAY").map_err(|_| "WAYLAND_DISPLAY is not set".to_owned())?;
    let path = socket_path(&display).map_err(|error| format!("the socket: {error}"))?;
    run_shaped_on(&path, pattern, title, shape)
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
    run_shaped_on(path, pattern, title, Shape::Window)
}

/// The same, with the surface given the role `shape` names.
///
/// # Errors
///
/// A sentence saying what could not be done.
pub fn run_shaped_on(
    path: &std::path::Path,
    pattern: Pattern,
    title: &str,
    shape: Shape,
) -> Result<String, String> {
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
        shape,
        title: title.to_owned(),
        globals: BTreeMap::new(),
        bound: false,
        width: 0,
        height: 0,
        acked: false,
        drawn: 0,
        shared: None,
        buffer_size: (0, 0),
        buffer_format: None,
        released: 0,
        keys: 0,
        seat: false,
        scale: 1,
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
        "pattern: {:?} {}x{} frames {} keys {} title {}",
        state.pattern, state.width, state.height, state.drawn, state.keys, state.title
    ))
}

/// What the client knows.
struct Client {
    pattern: Pattern,
    shape: Shape,
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
    /// The format the live buffer is in, since a pattern change changes it
    /// and a buffer may not be reinterpreted.
    buffer_format: Option<u32>,
    released: u32,
    /// The buffer scale to draw at: the largest any `wl_output` announced.
    ///
    /// A toolkit picks a surface's scale from the outputs it has entered;
    /// this client has one window and takes the largest scale offered,
    /// which on a machine whose screens all have the same scale is that
    /// scale, and on a mixed one is the sharper of the two.
    scale: i32,
    /// How many keys have been pressed, which also says which pattern is
    /// drawn: the client draws a different one after each key, so that a key
    /// arriving is visible on the screen and not only in a log line.
    keys: u32,
    /// Whether the seat has been bound and asked for its objects.
    seat: bool,
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
            id::LAYER_SHELL => &layer_shell::ZWLR_LAYER_SHELL_V1,
            id::LAYER_SURFACE => &layer_shell::ZWLR_LAYER_SURFACE_V1,
            id::SEAT => &core::WL_SEAT,
            id::KEYBOARD => &core::WL_KEYBOARD,
            id::POINTER => &core::WL_POINTER,
            id::SHELL => &xdg_shell::XDG_WM_BASE,
            id::BUFFER => &core::WL_BUFFER,
            id::OUTPUT => &core::WL_OUTPUT,
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
            id::OUTPUT if opcode == core::wl_output::event::SCALE => {
                let scale = args.first().and_then(Arg::as_int).unwrap_or(1);
                if scale > self.scale {
                    self.scale = scale;
                    say(&format!("pattern: output scale {scale}"));
                    // The buffer is the wrong size now, and the window's
                    // logical size has not changed: draw it again.
                    self.draw(out)?;
                }
            }
            id::SEAT if opcode == wl_seat::event::CAPABILITIES => {
                self.seat(args.first().and_then(Arg::as_uint).unwrap_or(0), out);
            }
            id::LAYER_SURFACE if opcode == zwlr_layer_surface_v1::event::CONFIGURE => {
                let serial = args.first().and_then(Arg::as_uint).unwrap_or(0);
                let (width, height) = (
                    args.get(1).and_then(Arg::as_uint).unwrap_or(0),
                    args.get(2).and_then(Arg::as_uint).unwrap_or(0),
                );
                request(
                    out,
                    id::LAYER_SURFACE,
                    zwlr_layer_surface_v1::request::ACK_CONFIGURE,
                    &[ArgType::Uint],
                    &[Arg::Uint(serial)],
                );
                self.width = i32::try_from(width).unwrap_or(0);
                self.height = i32::try_from(height).unwrap_or(0);
                self.acked = true;
                say(&format!("pattern: layer {width}x{height}"));
                self.draw(out)?;
            }
            id::LAYER_SURFACE if opcode == zwlr_layer_surface_v1::event::CLOSED => {
                return Err("the compositor closed this layer surface".to_owned());
            }
            id::KEYBOARD => self.keyboard(opcode, args, out)?,
            id::POINTER => self.pointer(opcode, args),
            _ => {}
        }
        Ok(())
    }

    /// Ask the seat for the objects it says it has.
    ///
    /// A client may only ask for a capability the seat announced, so this
    /// waits for `wl_seat.capabilities` rather than asking on binding. A seat
    /// that announces nothing leaves the client with no keyboard, which is a
    /// machine with nothing plugged in.
    fn seat(&mut self, capabilities: u32, out: &mut Writer) {
        if self.seat {
            return;
        }
        self.seat = true;
        for (bit, id, opcode) in [
            (
                wl_seat::capability::KEYBOARD,
                id::KEYBOARD,
                wl_seat::request::GET_KEYBOARD,
            ),
            (
                wl_seat::capability::POINTER,
                id::POINTER,
                wl_seat::request::GET_POINTER,
            ),
        ] {
            if capabilities & bit == 0 {
                continue;
            }
            request(out, id::SEAT, opcode, &[ArgType::NewId], &[Arg::NewId(id)]);
        }
        say(&format!("pattern: seat capabilities {capabilities:#x}"));
    }

    /// What the keyboard said. Every event is printed, because this client is
    /// how `cargo xtask test-seat` sees what reached a window.
    fn keyboard(&mut self, opcode: u16, args: &[Arg<'_>], out: &mut Writer) -> Result<(), String> {
        match opcode {
            wl_keyboard::event::KEYMAP => {
                let format = args.first().and_then(Arg::as_uint).unwrap_or(0);
                let size = args.get(2).and_then(Arg::as_uint).unwrap_or(0);
                say(&format!("pattern: keymap format {format} size {size}"));
            }
            wl_keyboard::event::ENTER => {
                say("pattern: keyboard enter");
            }
            wl_keyboard::event::LEAVE => {
                say("pattern: keyboard leave");
            }
            wl_keyboard::event::KEY => {
                let code = args.get(2).and_then(Arg::as_uint).unwrap_or(0);
                let state = args.get(3).and_then(Arg::as_uint).unwrap_or(0);
                say(&format!("pattern: key {code} state {state}"));
                if state == wl_keyboard::key_state::PRESSED {
                    // Draw something else, so that a key arriving shows on
                    // the screen and not only in this line.
                    self.keys = self.keys.saturating_add(1);
                    self.pattern = if self.keys % 2 == 1 {
                        Pattern::Gradient
                    } else {
                        Pattern::Checkerboard
                    };
                    self.draw(out)?;
                }
            }
            wl_keyboard::event::MODIFIERS => {
                let depressed = args.get(1).and_then(Arg::as_uint).unwrap_or(0);
                let locked = args.get(3).and_then(Arg::as_uint).unwrap_or(0);
                say(&format!(
                    "pattern: modifiers depressed {depressed:#x} locked {locked:#x}"
                ));
            }
            wl_keyboard::event::REPEAT_INFO => {
                let rate = args.first().and_then(Arg::as_int).unwrap_or(0);
                let delay = args.get(1).and_then(Arg::as_int).unwrap_or(0);
                say(&format!("pattern: repeat {rate} after {delay}"));
            }
            _ => {}
        }
        Ok(())
    }

    /// What the pointer said.
    fn pointer(&mut self, opcode: u16, args: &[Arg<'_>]) {
        match opcode {
            wl_pointer::event::ENTER => say("pattern: pointer enter"),
            wl_pointer::event::LEAVE => say("pattern: pointer leave"),
            wl_pointer::event::MOTION => {
                let (x, y) = (
                    args.get(1).and_then(Arg::as_fixed),
                    args.get(2).and_then(Arg::as_fixed),
                );
                if let (Some(x), Some(y)) = (x, y) {
                    say(&format!(
                        "pattern: pointer at {} {}",
                        x.to_int(),
                        y.to_int()
                    ));
                }
            }
            wl_pointer::event::BUTTON => {
                let button = args.get(2).and_then(Arg::as_uint).unwrap_or(0);
                let state = args.get(3).and_then(Arg::as_uint).unwrap_or(0);
                say(&format!("pattern: button {button} state {state}"));
            }
            wl_pointer::event::AXIS => {
                let axis = args.get(1).and_then(Arg::as_uint).unwrap_or(0);
                let value = args.get(2).and_then(Arg::as_fixed);
                if let Some(value) = value {
                    say(&format!("pattern: axis {axis} by {}", value.to_int()));
                }
            }
            _ => {}
        }
    }

    /// Bind what a window needs, and ask for one.
    fn bind(&mut self, out: &mut Writer) -> Result<(), String> {
        if self.bound {
            return Ok(());
        }
        // `wl_seat` is wanted and not required: a compositor with nothing
        // plugged in may offer none, and a client that refused to start over
        // it would be a client that only runs on a machine with a keyboard.
        let bar = matches!(self.shape, Shape::Bar(_));
        for (interface, id, want, required) in [
            ("wl_compositor", id::COMPOSITOR, 6u32, true),
            ("wl_shm", id::SHM, 1, true),
            ("xdg_wm_base", id::SHELL, !bar as u32 * 6, !bar),
            ("wl_seat", id::SEAT, 7, false),
            // `wl_output` for its scale: a client on a scaled monitor draws
            // a buffer that many times the size and says so, or the
            // compositor has to stretch what it sent.
            ("wl_output", id::OUTPUT, 4, false),
            ("zwlr_layer_shell_v1", id::LAYER_SHELL, 5, bar),
        ] {
            if want == 0 {
                continue;
            }
            let offer = self.globals.get(interface).copied();
            if offer.is_none() && !required {
                say(&format!("pattern: the compositor offers no {interface}"));
                continue;
            }
            let (name, offered) =
                offer.ok_or_else(|| format!("the compositor offers no {interface}"))?;
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

        // The surface, then the role, in the order the protocol requires.
        request(
            out,
            id::COMPOSITOR,
            wl_compositor::request::CREATE_SURFACE,
            &[ArgType::NewId],
            &[Arg::NewId(id::SURFACE)],
        );
        if let Shape::Bar(height) = self.shape {
            return self.become_bar(height, out);
        }
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
    /// Ask for a `zwlr_layer_surface_v1` across the top edge: what a bar
    /// asks for, in the order `waybar` asks for it.
    fn become_bar(&mut self, height: u32, out: &mut Writer) -> Result<(), String> {
        request(
            out,
            id::LAYER_SHELL,
            zwlr_layer_shell_v1::request::GET_LAYER_SURFACE,
            &[
                ArgType::NewId,
                ArgType::Object { nullable: false },
                ArgType::Object { nullable: true },
                ArgType::Uint,
                ArgType::Str { nullable: false },
            ],
            &[
                Arg::NewId(id::LAYER_SURFACE),
                Arg::Object(id::SURFACE),
                // A null output: the compositor chooses, which is what a bar
                // with no monitor configured asks for.
                Arg::Object(ObjectId(0)),
                Arg::Uint(zwlr_layer_shell_v1::layer::TOP),
                Arg::Str(Some("pattern-bar")),
            ],
        );
        request(
            out,
            id::LAYER_SURFACE,
            zwlr_layer_surface_v1::request::SET_ANCHOR,
            &[ArgType::Uint],
            &[Arg::Uint(
                zwlr_layer_surface_v1::anchor::TOP
                    | zwlr_layer_surface_v1::anchor::LEFT
                    | zwlr_layer_surface_v1::anchor::RIGHT,
            )],
        );
        request(
            out,
            id::LAYER_SURFACE,
            zwlr_layer_surface_v1::request::SET_SIZE,
            &[ArgType::Uint, ArgType::Uint],
            // Zero across, because it is anchored to both side edges and the
            // compositor decides; the height is the bar's own.
            &[Arg::Uint(0), Arg::Uint(height)],
        );
        request(
            out,
            id::LAYER_SURFACE,
            zwlr_layer_surface_v1::request::SET_EXCLUSIVE_ZONE,
            &[ArgType::Int],
            &[Arg::Int(i32::try_from(height).unwrap_or(0))],
        );
        // A surface with a role and no buffer: the compositor answers with a
        // configure, and the first commit is what asks for it.
        request(out, id::SURFACE, wl_surface::request::COMMIT, &[], &[]);
        Ok(())
    }

    fn draw(&mut self, out: &mut Writer) -> Result<(), String> {
        if !self.acked || self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        // The window's size is in logical pixels; the buffer is in the
        // screen's own, which on a monitor at `scale = 2` is twice as many
        // each way. `set_buffer_scale` is what tells the compositor that the
        // larger buffer is the same window and not a larger one.
        let scale = self.scale.max(1);
        let (width, height) = (
            self.width.saturating_mul(scale),
            self.height.saturating_mul(scale),
        );
        let stride = width.saturating_mul(4);
        let len = usize::try_from(stride.saturating_mul(height))
            .map_err(|_| "a window too large to draw".to_owned())?;

        // A pool is made once and grown if the window does; the buffer is
        // made afresh each size, as a client that never keeps two.
        // A pattern change changes the format, and a `wl_buffer` cannot be
        // reinterpreted: it is made afresh for a new format as for a new
        // size.
        let format = self.pattern.format().wl_shm();
        let fresh = self.shared.is_none()
            || self.buffer_size != (width, height)
            || self.buffer_format != Some(format);
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
                    Arg::Uint(format),
                ],
            );
            self.buffer_size = (width, height);
            self.buffer_format = Some(format);
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
            wl_surface::request::SET_BUFFER_SCALE,
            &[ArgType::Int],
            &[Arg::Int(scale)],
        );
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

/// Say a line on the standard output, flushed: this client runs as a child
/// of the compositor with the console for its output, and a line held in a
/// buffer is a line a test never sees.
fn say(line: &str) {
    use std::io::Write as _;

    let mut out = io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
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
