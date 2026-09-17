//! The terminal's window: connect, make a toplevel, draw the grid, and send
//! what is typed to the program.
//!
//! The shape of a Wayland client is `compositor/pattern`'s -- the same
//! `compositor/wire` under it, the same fixed object ids, the same
//! connect-bind-configure-draw -- because that is the shape every client
//! has. What is different is what it draws and what it does with a key: a
//! grid of characters, and a byte written to the pseudoterminal.

use std::collections::BTreeMap;
use std::os::fd::FromRawFd;
use std::time::{Duration, Instant};

use compositor_protocol::core::{
    self, wl_compositor, wl_display, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use compositor_protocol::xdg_shell::{self, xdg_surface, xdg_toplevel, xdg_wm_base};
use compositor_shm::Shared;
use compositor_wire::{Arg, ArgType, Fd, Interface, ObjectId, Reader, Writer};

use crate::grid::Grid;
use crate::paint::{self, Colours};
use crate::pty::Pty;

/// The objects this client makes, at fixed ids.
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
    pub(super) const SEAT: ObjectId = ObjectId(12);
    pub(super) const KEYBOARD: ObjectId = ObjectId(13);
    pub(super) const OUTPUT: ObjectId = ObjectId(14);
}

/// How long to run before giving up, so a test can never hang.
const DEADLINE: Duration = Duration::from_secs(600);

/// How long to keep drawing after the program has finished, so that what it
/// wrote last is on the screen and can be looked at.
const LINGER: Duration = Duration::from_secs(60);

/// What the client knows.
struct Terminal {
    /// The program on the pseudoterminal.
    pty: Pty,
    /// What it has written.
    grid: Grid,
    /// What the title bar says.
    title: String,
    colours: Colours,
    /// The registry's names, by interface.
    globals: BTreeMap<String, (u32, u32)>,
    bound: bool,
    /// The window's size in logical pixels, as the compositor configured it.
    width: i32,
    height: i32,
    /// Buffer pixels to a logical one, from `wl_output.scale`.
    scale: i32,
    acked: bool,
    drawn: u32,
    shared: Option<Shared>,
    buffer_size: (i32, i32),
    /// Whether the grid has changed since the last frame.
    dirty: bool,
    /// Whether the seat has been asked for its keyboard.
    seat: bool,
    /// The modifiers in force, as `wl_keyboard.modifiers` reports them:
    /// depressed, latched and locked together, which is what selects a
    /// key's level -- `Caps Lock` is locked and reaches the shifted level
    /// just as `Shift` does.
    modifiers: u32,
    /// The layout group in force, which indexes `layouts`.
    group: usize,
    /// The layouts of the keymap the compositor handed this client, in
    /// group order. Empty until `wl_keyboard.keymap` arrives, and empty
    /// afterwards if the keymap named no layout this client ships tables
    /// for -- in which case the default table is read, as it always was.
    layouts: Vec<&'static compositor_xkb::generated::Layout>,
    /// When the program finished, if it has.
    finished: Option<Instant>,
}

/// Which bit `wl_keyboard.modifiers` uses for control.
///
/// The compositor's keymap is `compositor/xkb`'s, whose modifier order is
/// libxkbcommon's own: control is bit 2. Shift needs no constant here any
/// more -- which level a key is read at is the keymap's business, and
/// `Key::keysym` answers it from every mask the key declares rather than
/// from this one bit.
const CONTROL: u32 = 1 << 2;

/// Run a terminal on `socket`, with `program` on the pseudoterminal.
///
/// # Errors
///
/// A sentence saying what could not be done.
pub fn run(
    socket: &std::path::Path,
    program: &str,
    arguments: &[String],
) -> Result<String, String> {
    use compositor_socket::{Connection, RecvError};

    let stream = std::os::unix::net::UnixStream::connect(socket)
        .map_err(|error| format!("connecting to {}: {error}", socket.display()))?;
    let mut connection =
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
    flush(&mut connection, &mut out)?;

    // The grid starts at what a terminal has always been until it is told
    // otherwise, and is resized when the window is configured.
    let grid = Grid::new(80, 24);
    let pty = Pty::start(program, arguments, (80, 24))
        .map_err(|error| format!("the pseudoterminal: {error}"))?;
    let mut state = Terminal {
        pty,
        grid,
        title: program.to_owned(),
        colours: Colours::default(),
        globals: BTreeMap::new(),
        bound: false,
        width: 0,
        height: 0,
        scale: 1,
        acked: false,
        drawn: 0,
        shared: None,
        buffer_size: (0, 0),
        dirty: true,
        seat: false,
        modifiers: 0,
        group: 0,
        layouts: Vec::new(),
        finished: None,
    };

    let started = Instant::now();
    while started.elapsed() < DEADLINE {
        match connection.receive() {
            Ok(_) => {}
            Err(RecvError::WouldBlock) => {}
            Err(RecvError::Closed) => break,
            Err(error) => return Err(format!("reading: {error:?}")),
        }
        let consumed = state.read(connection.bytes(), &connection.fds(), &mut out)?;
        if consumed > 0 {
            connection.consume(consumed, 0);
        }
        // What the program wrote, then the frame it changed.
        state.pump()?;
        if state.dirty {
            state.draw(&mut out)?;
        }
        flush(&mut connection, &mut out)?;
        if state.over() {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }

    Ok(format!(
        "term: {} {}x{} frames {} rows {}",
        state.title,
        state.width,
        state.height,
        state.drawn,
        state.grid.size().1
    ))
}

impl Terminal {
    /// Whether to stop: the program finished and its last screen has been
    /// shown for long enough to be looked at.
    fn over(&self) -> bool {
        self.finished.is_some_and(|when| when.elapsed() > LINGER)
    }

    /// Take whatever the program wrote.
    fn pump(&mut self) -> Result<(), String> {
        let mut buffer = [0u8; 4096];
        loop {
            let read = self
                .pty
                .read(&mut buffer)
                .map_err(|error| format!("reading the pseudoterminal: {error}"))?;
            if read == 0 {
                break;
            }
            self.grid.write(buffer.get(..read).unwrap_or(&[]));
            self.dirty = true;
        }
        if self.finished.is_none() && self.pty.done() {
            self.finished = Some(Instant::now());
        }
        Ok(())
    }

    /// Read whatever arrived, and answer it.
    fn read(&mut self, bytes: &[u8], fds: &[Fd], out: &mut Writer) -> Result<usize, String> {
        let mut reader = Reader::new(bytes, fds);
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
            self.event(header.sender, header.opcode, &args, out)?;
        }
        Ok(reader.consumed())
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
            id::SYNC if opcode == core::wl_callback::event::DONE => self.bind(out)?,
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
                // A zero is "you choose", which for a terminal is the size
                // its grid already has.
                self.width = if width > 0 { width } else { 640 };
                self.height = if height > 0 { height } else { 384 };
                self.refit();
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
                self.dirty = true;
            }
            id::TOPLEVEL if opcode == xdg_toplevel::event::CLOSE => {
                return Err("the compositor asked this window to close".to_owned());
            }
            id::OUTPUT if opcode == core::wl_output::event::SCALE => {
                let scale = args.first().and_then(Arg::as_int).unwrap_or(1);
                if scale > self.scale {
                    self.scale = scale;
                    self.refit();
                }
            }
            id::SEAT if opcode == wl_seat::event::CAPABILITIES => {
                self.keyboard_from(args.first().and_then(Arg::as_uint).unwrap_or(0), out);
            }
            id::KEYBOARD => self.key_event(opcode, args)?,
            _ => {}
        }
        Ok(())
    }

    /// Bind what a terminal needs, and ask for a window.
    fn bind(&mut self, out: &mut Writer) -> Result<(), String> {
        if self.bound {
            return Ok(());
        }
        self.bound = true;
        for (interface, id, want, required) in [
            ("wl_compositor", id::COMPOSITOR, 6u32, true),
            ("wl_shm", id::SHM, 1, true),
            ("xdg_wm_base", id::SHELL, 6, true),
            ("wl_seat", id::SEAT, 7, false),
            ("wl_output", id::OUTPUT, 4, false),
        ] {
            let offer = self.globals.get(interface).copied();
            let Some((name, offered)) = offer else {
                if required {
                    return Err(format!("the compositor offers no {interface}"));
                }
                continue;
            };
            out.write(
                id::REGISTRY,
                wl_registry::request::BIND,
                &[ArgType::Uint, ArgType::AnyNewId],
                &[
                    Arg::Uint(name),
                    Arg::AnyNewId {
                        interface,
                        version: want.min(offered),
                        id,
                    },
                ],
            )
            .map_err(|error| format!("binding {interface}: {error:?}"))?;
        }
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
        request(
            out,
            id::TOPLEVEL,
            xdg_toplevel::request::SET_TITLE,
            &[ArgType::Str { nullable: false }],
            &[Arg::Str(Some(&self.title))],
        );
        request(
            out,
            id::TOPLEVEL,
            xdg_toplevel::request::SET_APP_ID,
            &[ArgType::Str { nullable: false }],
            &[Arg::Str(Some("rocks.magical.term"))],
        );
        // The first commit carries no buffer: it asks to be configured.
        request(out, id::SURFACE, wl_surface::request::COMMIT, &[], &[]);
        Ok(())
    }

    /// Ask the seat for a keyboard, if it has one.
    fn keyboard_from(&mut self, capabilities: u32, out: &mut Writer) {
        if self.seat || capabilities & wl_seat::capability::KEYBOARD == 0 {
            return;
        }
        self.seat = true;
        request(
            out,
            id::SEAT,
            wl_seat::request::GET_KEYBOARD,
            &[ArgType::NewId],
            &[Arg::NewId(id::KEYBOARD)],
        );
    }

    /// What the keyboard said, turned into what the program reads.
    fn key_event(&mut self, opcode: u16, args: &[Arg<'_>]) -> Result<(), String> {
        match opcode {
            wl_keyboard::event::KEYMAP => {
                self.read_keymap(args);
            }
            wl_keyboard::event::MODIFIERS => {
                // depressed, latched, locked, group: a level is selected by
                // all three masks together, and the group says which of the
                // keymap's layouts the key is read in.
                let mask = |at| args.get(at).and_then(Arg::as_uint).unwrap_or(0);
                self.modifiers = mask(1) | mask(2) | mask(3);
                self.group = usize::try_from(mask(4)).unwrap_or(0);
            }
            wl_keyboard::event::KEY => {
                let code = args.get(2).and_then(Arg::as_uint).unwrap_or(0);
                let state = args.get(3).and_then(Arg::as_uint).unwrap_or(0);
                if state != wl_keyboard::key_state::PRESSED {
                    return Ok(());
                }
                self.typed(code);
            }
            _ => {}
        }
        Ok(())
    }

    /// Take the keymap the compositor handed this client and work out which
    /// of the shipped tables it is.
    ///
    /// `wl_keyboard.keymap` carries a descriptor and a length, and a client
    /// of any other compositor would map it and compile the text with
    /// libxkbcommon. There is no libxkbcommon here, so what is read out of
    /// the text is the name of each of its groups, which
    /// `compositor_xkb::groups_of` matches against the tables this client
    /// was built with. Without this the terminal read the first table
    /// whatever the keymap said, so a person with `input:kb_layout = de`
    /// typed an American keyboard's letters into it.
    fn read_keymap(&mut self, args: &[Arg<'_>]) {
        let Some(fd) = args.get(1).and_then(Arg::as_fd) else {
            return;
        };
        let size = args.get(2).and_then(Arg::as_uint).unwrap_or(0) as usize;
        let mut file = unsafe { std::fs::File::from_raw_fd(fd.0) };
        let mut text = String::new();
        // The length counts the terminating NUL, which is not part of the
        // text: a client that keeps it hands a NUL to its own parser.
        if std::io::Read::read_to_string(&mut file, &mut text).is_ok() {
            let end = text.find('\0').unwrap_or(text.len()).min(size);
            self.layouts = compositor_xkb::groups_of(&text[..end]);
        }
    }

    /// The table a key is read in: the group in force, or the default.
    fn table(&self) -> Option<&'static compositor_xkb::generated::Layout> {
        self.layouts
            .get(self.group)
            .or_else(|| self.layouts.first())
            .copied()
    }

    /// Send what the key with this evdev code types.
    fn typed(&mut self, code: u32) {
        let Ok(code) = u16::try_from(code) else {
            return;
        };
        // Every level, not only the shifted one: on a German keyboard `@`,
        // `|`, `~`, `[`, `]`, `{`, `}` and the backslash are all on the
        // third level, which `AltGr` reaches.
        let key = match self.table() {
            Some(layout) => layout.key(code),
            None => compositor_xkb::key(code),
        };
        let Some(keysym) = key.and_then(|key| key.keysym(self.modifiers)) else {
            return;
        };
        let control = self.modifiers & CONTROL != 0;
        if let Some(bytes) = crate::keys::bytes(keysym, control) {
            let _ = self.pty.write(&bytes);
        }
    }

    /// Make the grid the size the window is, and tell the program.
    fn refit(&mut self) {
        let scale = usize::try_from(self.scale.max(1)).unwrap_or(1);
        let (width, height) = (
            usize::try_from(self.width.max(0)).unwrap_or(0) * scale,
            usize::try_from(self.height.max(0)).unwrap_or(0) * scale,
        );
        let (columns, rows) = paint::fits(width, height, scale);
        if self.grid.size() == (columns, rows) {
            return;
        }
        self.grid.resize(columns, rows);
        self.pty.resize((
            u16::try_from(columns).unwrap_or(u16::MAX),
            u16::try_from(rows).unwrap_or(u16::MAX),
        ));
        self.dirty = true;
    }

    /// Draw the grid into a buffer and give it to the compositor.
    fn draw(&mut self, out: &mut Writer) -> Result<(), String> {
        if !self.acked || self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        let scale = self.scale.max(1);
        let (width, height) = (
            self.width.saturating_mul(scale),
            self.height.saturating_mul(scale),
        );
        let stride = width.saturating_mul(4);
        let len = usize::try_from(stride.saturating_mul(height))
            .map_err(|_| "a window too large to draw".to_owned())?;

        if self.shared.is_none() || self.buffer_size != (width, height) {
            if self.shared.is_some() {
                request(out, id::BUFFER, core::wl_buffer::request::DESTROY, &[], &[]);
                request(out, id::POOL, wl_shm_pool::request::DESTROY, &[], &[]);
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
                    // `XRGB8888`: a terminal has no transparency of its own.
                    Arg::Uint(1),
                ],
            );
            self.buffer_size = (width, height);
        }

        if let Some(shared) = self.shared.as_mut() {
            let (wide, tall) = (
                usize::try_from(width).unwrap_or(0),
                usize::try_from(height).unwrap_or(0),
            );
            paint::draw(
                shared.bytes_mut(),
                (wide, tall),
                usize::try_from(stride).unwrap_or(0),
                &self.grid,
                &self.colours,
                usize::try_from(scale).unwrap_or(1),
            );
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
        self.dirty = false;
        if self.drawn == 1 {
            // The window is up with the program's first output in it, which
            // is what a watcher waits for.
            let (columns, rows) = self.grid.size();
            say(&format!(
                "term: {} in {columns}x{rows} cells on {}x{} pixels",
                self.title, width, height
            ));
        }
        Ok(())
    }
}

/// Say a line on the standard output, flushed: a terminal started by the
/// compositor has the console for its output, and a line held in a buffer is
/// a line a test never sees.
fn say(line: &str) {
    use std::io::Write as _;

    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Which interface an object of this client's speaks.
fn interface_of(id: ObjectId) -> Option<&'static Interface> {
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
        id::SEAT => &core::WL_SEAT,
        id::KEYBOARD => &core::WL_KEYBOARD,
        id::OUTPUT => &core::WL_OUTPUT,
        _ => return None,
    })
}

/// Queue one request.
fn request(
    out: &mut Writer,
    sender: ObjectId,
    opcode: u16,
    signature: &'static [ArgType],
    args: &[Arg<'_>],
) {
    // A request that will not encode is this client's own mistake, and there
    // is nothing useful to do about it in the middle of a frame.
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

/// Send whatever is queued.
fn flush(connection: &mut compositor_socket::Connection, out: &mut Writer) -> Result<(), String> {
    if out.is_empty() {
        return Ok(());
    }
    let (bytes, fds) = out.take();
    connection
        .send(&bytes, &fds)
        .map_err(|error| format!("writing: {error:?}"))
}
