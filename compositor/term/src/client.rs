//! The terminal's window: connect, make a toplevel, draw the grid, and send
//! what is typed to the program.
//!
//! The shape of a Wayland client is `compositor/pattern`'s -- the same
//! `compositor/wire` under it, the same fixed object ids, the same
//! connect-bind-configure-draw -- because that is the shape every client
//! has. What is different is what it draws and what it does with a key: a
//! grid of characters, and a byte written to the pseudoterminal.
//!
//! The pointer selects: a drag selects the cells it passes over, a double
//! click whole words and a triple click whole lines, and the wheel looks
//! back through the scrollback. Control-shift-C copies the selection to the
//! clipboard and control-shift-V pastes the clipboard, the keys every Linux
//! terminal uses, since control-C alone is the interrupt a program is owed.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use compositor_protocol::core::{
    self, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
    wl_display, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use compositor_protocol::xdg_shell::{self, xdg_surface, xdg_toplevel, xdg_wm_base};
use compositor_shm::Shared;
use compositor_wire::{Arg, ArgType, Fd, Interface, ObjectId, Reader, Writer};

use crate::grid::{CellDamage, Grid, Snapshot, Unit};
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
    pub(super) const POINTER: ObjectId = ObjectId(15);
    pub(super) const DATA_MANAGER: ObjectId = ObjectId(16);
    pub(super) const DATA_DEVICE: ObjectId = ObjectId(17);
    /// The first id a data source takes. A source serves one copy and is
    /// destroyed when the next one replaces it, so each copy makes another.
    pub(super) const FIRST_SOURCE: u32 = 32;
}

/// How long to keep drawing after the program has finished, so that what it
/// wrote last is on the screen and can be looked at.
///
/// Long enough to read a line or two, short enough that a shell's `exit`
/// closes the window rather than seeming to hang: `test_terminal`'s
/// screendump, the other thing this protects, happens within `SETTLE` of
/// boot -- a few seconds -- so it never comes close to this either way.
const LINGER: Duration = Duration::from_secs(2);

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
    /// What changed since the last frame. Small terminal writes stay small
    /// through rasterisation, Wayland damage and compositor composition.
    dirty: Option<Dirty>,
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
    /// The key held down, if any is, and what retyping it next means.
    repeat: Option<Repeating>,
    /// How long a key waits before its first repeat: `wl_keyboard.repeat_info`'s
    /// delay, zero until that event arrives. The protocol guarantees it
    /// arrives before any key press does.
    repeat_delay: Duration,
    /// The gap between repeats after the first, from `repeat_info`'s rate in
    /// keys a second. `None` for a rate of zero, which means "do not repeat"
    /// rather than "repeat as fast as possible".
    repeat_interval: Option<Duration>,
    /// Bytes on their way to the program: what was typed and what was
    /// pasted, written as fast as it reads them.
    input: Vec<u8>,
    /// The serial of the last key or button, which a copy names to say the
    /// person asked for it.
    serial: u32,
    /// The pointer, the selection it makes and the wheel.
    mouse: Mouse,
    /// The clipboard: what this terminal copied, and what it was offered.
    clipboard: Clipboard,
}

/// What the pointer is doing.
#[derive(Debug, Default)]
struct Mouse {
    /// Whether the seat has been asked for its pointer.
    asked: bool,
    /// Where it is on the window, in logical pixels.
    at: (f64, f64),
    /// Whether the left button is held, dragging a selection out.
    dragging: bool,
    /// The last press: when, on which cell, and which click of a run of
    /// them it was -- one, two or three.
    last: Option<(Instant, (usize, usize), u8)>,
    /// A wheel's notches, and the distance a smooth scroll moved, since the
    /// last `wl_pointer.frame`.
    notches: i32,
    distance: f64,
}

/// Both halves of copy and paste.
#[derive(Debug)]
struct Clipboard {
    /// Whether the data device has been asked for.
    device: bool,
    /// The source that is the clipboard now, if this terminal copied last.
    current: Option<ObjectId>,
    /// Every source still alive, with the text it serves: the current one,
    /// and any the compositor has not yet said it replaced.
    sources: BTreeMap<ObjectId, String>,
    /// The id the next source takes.
    next: u32,
    /// The offers the compositor has made, with the types each has.
    offers: BTreeMap<ObjectId, Vec<String>>,
    /// The offer that is the clipboard now, when someone else copied last.
    selection: Option<ObjectId>,
    /// A paste being read.
    pasting: Option<Paste>,
    /// Pipe ends to close once the request that carries them has gone:
    /// closing one before it is sent sends nothing.
    sent: Vec<OwnedFd>,
}

/// A paste on its way in through a pipe.
#[derive(Debug)]
struct Paste {
    pipe: std::fs::File,
    bytes: Vec<u8>,
    started: Instant,
}

/// Which bit `wl_keyboard.modifiers` uses for control.
///
/// The compositor's keymap is `compositor/xkb`'s, whose modifier order is
/// libxkbcommon's own: control is bit 2. Shift needs no constant here any
/// more -- which level a key is read at is the keymap's business, and
/// `Key::keysym` answers it from every mask the key declares rather than
/// from this one bit.
const CONTROL: u32 = 1 << 2;

/// Which bit `wl_keyboard.modifiers` uses for shift: the first of XKB's.
const SHIFT: u32 = 1 << 0;

/// The left button, as evdev numbers it and `wl_pointer.button` carries it.
const BTN_LEFT: u32 = 0x110;

/// How soon a second press on the same cell has to follow the first to be a
/// double click rather than a new single one.
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// How many rows a wheel's notch scrolls: three, as xterm and foot do.
const WHEEL_ROWS: i32 = 3;

/// The types copied text is offered as, and the ones a paste takes, in the
/// order a paste prefers them: the UTF-8 type every Wayland program agrees
/// on, the bare one some older ones offer, and X11's name for UTF-8 that
/// `XWayland`'s programs use.
const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// The most a paste takes. A clipboard holding a video, offered as text by
/// mistake, is not something to type at a shell.
const PASTE_LIMIT: usize = 16 << 20;

/// How long a paste waits for whoever copied to finish writing: a program
/// that took the request and never answered must not leave every later
/// paste waiting behind it.
const PASTE_PATIENCE: Duration = Duration::from_secs(5);

/// The key a held `wl_keyboard.key` is retyping, and when it does that next.
///
/// Wayland sends a key's press and its release and nothing in between --
/// autorepeat is the client's to do, from `wl_keyboard.repeat_info` -- so
/// this is what `run`'s loop consults on every pass. Only one key at a
/// time: a second key pressed while the first is still down replaces it,
/// which is the one case a real keyboard cannot make happen anyway.
#[derive(Clone)]
struct Repeating {
    /// The evdev code, so the matching release can find it.
    code: u16,
    /// What it sends, worked out once rather than on every retype.
    bytes: Vec<u8>,
    /// When this key next retypes itself.
    next: Instant,
}

/// What the next Wayland buffer update has to redraw.
#[derive(Clone, Copy, Debug)]
enum Dirty {
    /// A new buffer or a resize: nothing in it is a prior frame.
    Full,
    /// The cells whose previous pixels can be kept outside this rectangle.
    Cells(CellDamage),
}

impl Dirty {
    /// Combine independent updates before sending the next frame.
    fn joined(self, other: Self) -> Self {
        match (self, other) {
            (Self::Full, _) | (_, Self::Full) => Self::Full,
            (Self::Cells(left), Self::Cells(right)) => Self::Cells(left.joined(right)),
        }
    }
}

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
        dirty: Some(Dirty::Full),
        seat: false,
        modifiers: 0,
        group: 0,
        layouts: Vec::new(),
        finished: None,
        repeat: None,
        repeat_delay: Duration::ZERO,
        repeat_interval: None,
        input: Vec::new(),
        serial: 0,
        mouse: Mouse::default(),
        clipboard: Clipboard {
            device: false,
            current: None,
            sources: BTreeMap::new(),
            next: id::FIRST_SOURCE,
            offers: BTreeMap::new(),
            selection: None,
            pasting: None,
            sent: Vec::new(),
        },
    };

    // No deadline: a terminal lasts as long as its program or its window.
    // A test that boots one is bounded by its own QEMU timeout, and a
    // deadline here closed every desktop terminal ten minutes after it
    // opened.
    loop {
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
        // What the program wrote, then a held key retyping itself, then the
        // frame either changed.
        state.pump()?;
        state.autorepeat();
        state.read_paste();
        state.feed()?;
        if state.dirty.is_some() {
            state.draw(&mut out)?;
        }
        flush(&mut connection, &mut out)?;
        // The pipe ends a paste handed over have gone with that flush, and
        // this process's copies are what would keep the pipe open.
        state.clipboard.sent.clear();
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
        // The screen is small (a typical 640×384 terminal is 53×16 cells),
        // and comparing it after draining a PTY batch is much cheaper than
        // repainting its whole shared-memory buffer. Take that snapshot only
        // when there is output: the usual idle pass must allocate nothing at
        // all.
        let mut before = None;
        loop {
            let read = self
                .pty
                .read(&mut buffer)
                .map_err(|error| format!("reading the pseudoterminal: {error}"))?;
            if read == 0 {
                break;
            }
            if before.is_none() {
                before = Some(self.grid.snapshot());
            }
            self.grid.write(buffer.get(..read).unwrap_or(&[]));
        }
        if let Some(before) = before {
            self.damaged(&before);
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
            let Some(interface) = self.interface(header.sender) else {
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
                // A configure is an answer to be acknowledged, and a new
                // frame only when it changed what the frame is: the first
                // one, or a new size. Focus moving between windows configures
                // both with `activated` toggled, and a terminal that repainted
                // its whole grid for each was two whole windows composited
                // on every pass of the pointer from one to the other -- on a
                // slow compositor faster than it could draw them, until the
                // desktop was a slideshow (the DK1, 2026-09-24). The size
                // is `refit`'s to judge, which runs on the toplevel's half.
                if self.shared.is_none() {
                    self.dirty = Some(Dirty::Full);
                }
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
                self.devices_from(args.first().and_then(Arg::as_uint).unwrap_or(0), out);
            }
            id::KEYBOARD => self.key_event(opcode, args, out)?,
            id::POINTER => self.pointer_event(opcode, args),
            id::DATA_DEVICE => self.device_event(opcode, args, out),
            offer if self.clipboard.offers.contains_key(&offer) => {
                if opcode == wl_data_offer::event::OFFER
                    && let Some(mime) = args.first().and_then(Arg::as_str)
                    && let Some(types) = self.clipboard.offers.get_mut(&offer)
                {
                    types.push(mime.to_owned());
                }
            }
            source if self.clipboard.sources.contains_key(&source) => {
                self.source_event(source, opcode, args, out);
            }
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
            ("wl_data_device_manager", id::DATA_MANAGER, 3, false),
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
        // The clipboard is a device of the seat's, and a compositor with no
        // clipboard is one this terminal still runs on: it copies nothing.
        if self.globals.contains_key("wl_data_device_manager")
            && self.globals.contains_key("wl_seat")
        {
            self.clipboard.device = true;
            request(
                out,
                id::DATA_MANAGER,
                wl_data_device_manager::request::GET_DATA_DEVICE,
                &[ArgType::NewId, ArgType::Object { nullable: false }],
                &[Arg::NewId(id::DATA_DEVICE), Arg::Object(id::SEAT)],
            );
        }
        // The first commit carries no buffer: it asks to be configured.
        request(out, id::SURFACE, wl_surface::request::COMMIT, &[], &[]);
        Ok(())
    }

    /// Ask the seat for a keyboard and a pointer, if it has them.
    fn devices_from(&mut self, capabilities: u32, out: &mut Writer) {
        if !self.seat && capabilities & wl_seat::capability::KEYBOARD != 0 {
            self.seat = true;
            request(
                out,
                id::SEAT,
                wl_seat::request::GET_KEYBOARD,
                &[ArgType::NewId],
                &[Arg::NewId(id::KEYBOARD)],
            );
        }
        if !self.mouse.asked && capabilities & wl_seat::capability::POINTER != 0 {
            self.mouse.asked = true;
            request(
                out,
                id::SEAT,
                wl_seat::request::GET_POINTER,
                &[ArgType::NewId],
                &[Arg::NewId(id::POINTER)],
            );
        }
    }

    /// What the keyboard said, turned into what the program reads.
    fn key_event(&mut self, opcode: u16, args: &[Arg<'_>], out: &mut Writer) -> Result<(), String> {
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
            wl_keyboard::event::REPEAT_INFO => {
                let rate = args.first().and_then(Arg::as_int).unwrap_or(0);
                let delay = args.get(1).and_then(Arg::as_int).unwrap_or(0);
                self.repeat_delay = Duration::from_millis(delay.max(0).unsigned_abs().into());
                self.repeat_interval = u32::try_from(rate)
                    .ok()
                    .filter(|&rate| rate > 0)
                    .map(|rate| Duration::from_millis(1000 / u64::from(rate)));
            }
            wl_keyboard::event::KEY => {
                let code = args.get(2).and_then(Arg::as_uint).unwrap_or(0);
                let state = args.get(3).and_then(Arg::as_uint).unwrap_or(0);
                if state == wl_keyboard::key_state::PRESSED {
                    self.serial = args.first().and_then(Arg::as_uint).unwrap_or(self.serial);
                    self.typed(code, out);
                } else if let Ok(code) = u16::try_from(code)
                    && self.repeat.as_ref().is_some_and(|held| held.code == code)
                {
                    self.repeat = None;
                }
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
        let Ok(size) = usize::try_from(args.get(2).and_then(Arg::as_uint).unwrap_or(0)) else {
            return;
        };
        // The descriptor is owned from here: taking it means it is closed
        // when this returns, mapping and all.
        #[expect(
            unsafe_code,
            reason = "AUDIT: the descriptor arrived with this message and is this client's to own"
        )]
        // SAFETY: the wire reader hands over a descriptor nothing else holds.
        let owned = unsafe { OwnedFd::from_raw_fd(fd.0) };
        if size == 0 {
            return;
        }
        // Mapped read-only rather than read: a descriptor that arrived over
        // a socket shares its file offset with the one the compositor sent,
        // so reading it sequentially would move the compositor's offset and
        // leave the next client's keymap empty. The protocol says a client
        // maps this file, and that is why.
        #[expect(
            unsafe_code,
            reason = "AUDIT: mmap is not in std; it maps a descriptor this \
                      process owns at the length the compositor stated, and the \
                      result is checked against MAP_FAILED"
        )]
        // SAFETY: a null hint lets the kernel choose the address.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                owned.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return;
        }
        #[expect(
            unsafe_code,
            reason = "AUDIT: the mapping is this length by construction and is \
                      read as bytes, which any byte pattern is valid for"
        )]
        // SAFETY: `size` bytes were just mapped at `address`.
        let mapped = unsafe { std::slice::from_raw_parts(address.cast::<u8>(), size) };
        // The length counts the terminating NUL, which is not part of the
        // text. A keymap that is not UTF-8 is not one this can read, and
        // leaving `layouts` empty falls back to the default table.
        if let Some(bytes) = mapped.get(..size.saturating_sub(1))
            && let Ok(text) = std::str::from_utf8(bytes)
        {
            self.layouts = compositor_xkb::groups_of(text);
        }
        #[expect(
            unsafe_code,
            reason = "AUDIT: unmapping exactly the mapping made above"
        )]
        // SAFETY: the address and length are the ones just mapped.
        let _ = unsafe { libc::munmap(address, size) };
    }

    /// The table a key is read in: the group in force, or the default.
    fn table(&self) -> Option<&'static compositor_xkb::generated::Layout> {
        self.layouts
            .get(self.group)
            .or_else(|| self.layouts.first())
            .copied()
    }

    /// Send what the key with this evdev code types, and arm it to retype
    /// itself while it is held, if the compositor has said keys repeat.
    fn typed(&mut self, code: u32, out: &mut Writer) {
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
        if self.shortcut(keysym, out) {
            return;
        }
        let control = self.modifiers & CONTROL != 0;
        // A modifier, or a key this terminal has no bytes for: nothing is
        // typed, so nothing should retype itself either.
        let Some(bytes) = crate::keys::bytes(keysym, control) else {
            return;
        };
        // Typing is at the live rows, so a screen looking back comes home.
        self.look_live();
        self.send(&bytes);
        if self.repeat_interval.is_some() {
            self.repeat = Some(Repeating {
                code,
                bytes,
                next: Instant::now() + self.repeat_delay,
            });
        }
    }

    /// Retype the held key if its time has come, and arm the next one.
    ///
    /// Called every pass of `run`'s loop rather than from a timer: the loop
    /// already turns every few milliseconds to read the pseudoterminal, and
    /// a retype that is a few milliseconds late is not one anybody notices.
    fn autorepeat(&mut self) {
        let Some(held) = &self.repeat else {
            return;
        };
        let now = Instant::now();
        if now < held.next {
            return;
        }
        // A rate of zero arrived since this key was armed: stop, rather than
        // guess at a gap the compositor no longer says.
        let Some(interval) = self.repeat_interval else {
            self.repeat = None;
            return;
        };
        let bytes = held.bytes.clone();
        self.send(&bytes);
        if let Some(held) = &mut self.repeat {
            held.next = now + interval;
        }
    }

    /// The terminal's own keys, which the program never sees: control-shift-C
    /// and control-shift-V copy and paste, and shift with Page Up and Page
    /// Down pages through the scrollback. Whether `keysym` was one of them.
    fn shortcut(&mut self, keysym: &str, out: &mut Writer) -> bool {
        if self.modifiers & SHIFT == 0 {
            return false;
        }
        let control = self.modifiers & CONTROL != 0;
        let page = isize::try_from(self.grid.size().1.saturating_sub(1).max(1)).unwrap_or(1);
        match (control, keysym) {
            (true, "C" | "c") => self.copy(out),
            (true, "V" | "v") => self.paste(out),
            (false, "Prior") => self.change(|grid| grid.scroll_view(page)),
            (false, "Next") => self.change(|grid| grid.scroll_view(-page)),
            _ => return false,
        }
        true
    }

    /// Queue bytes for the program.
    fn send(&mut self, bytes: &[u8]) {
        // A program that has stopped reading is not owed an unbounded queue
        // of a held key.
        if self.input.len().saturating_add(bytes.len()) <= PASTE_LIMIT * 2 {
            self.input.extend_from_slice(bytes);
        }
    }

    /// Write what the program will take of what is queued for it.
    fn feed(&mut self) -> Result<(), String> {
        while !self.input.is_empty() {
            let written = self
                .pty
                .write_some(&self.input)
                .map_err(|error| format!("writing the pseudoterminal: {error}"))?;
            if written == 0 {
                break;
            }
            let _ = self.input.drain(..written.min(self.input.len()));
        }
        Ok(())
    }

    /// Change what the screen shows -- where it looks, what is selected --
    /// and damage what that changed.
    fn change(&mut self, change: impl FnOnce(&mut Grid)) {
        let before = self.grid.snapshot();
        change(&mut self.grid);
        self.damaged(&before);
    }

    /// Damage whatever differs from `before`.
    fn damaged(&mut self, before: &Snapshot) {
        if let Some(damage) = self.grid.damage_since(before) {
            self.dirty = Some(self.dirty.map_or(Dirty::Cells(damage), |held| {
                held.joined(Dirty::Cells(damage))
            }));
        }
    }

    /// Look at the live rows, if the screen is looking back.
    fn look_live(&mut self) {
        if self.grid.view() != 0 {
            self.change(Grid::view_live);
        }
    }

    /// What the pointer did.
    fn pointer_event(&mut self, opcode: u16, args: &[Arg<'_>]) {
        let fixed = |at: usize| {
            args.get(at)
                .and_then(Arg::as_fixed)
                .map_or(0.0, compositor_wire::Fixed::to_f64)
        };
        match opcode {
            wl_pointer::event::ENTER => self.mouse.at = (fixed(2), fixed(3)),
            wl_pointer::event::MOTION => {
                self.mouse.at = (fixed(1), fixed(2));
                if self.mouse.dragging {
                    self.drag();
                }
            }
            wl_pointer::event::BUTTON => {
                let button = args.get(2).and_then(Arg::as_uint).unwrap_or(0);
                let state = args.get(3).and_then(Arg::as_uint).unwrap_or(0);
                if button != BTN_LEFT {
                    return;
                }
                if state == wl_pointer::button_state::PRESSED {
                    self.serial = args.first().and_then(Arg::as_uint).unwrap_or(self.serial);
                    self.press();
                } else {
                    self.mouse.dragging = false;
                }
            }
            wl_pointer::event::AXIS
                if args.get(1).and_then(Arg::as_uint)
                    == Some(wl_pointer::axis::VERTICAL_SCROLL) =>
            {
                self.mouse.distance += fixed(2);
            }
            wl_pointer::event::AXIS_DISCRETE
                if args.first().and_then(Arg::as_uint)
                    == Some(wl_pointer::axis::VERTICAL_SCROLL) =>
            {
                let notches = args.get(1).and_then(Arg::as_int).unwrap_or(0);
                self.mouse.notches = self.mouse.notches.saturating_add(notches);
            }
            wl_pointer::event::FRAME => self.wheel(),
            _ => {}
        }
    }

    /// Scroll by what the wheel did in the frame that just ended.
    ///
    /// A wheel says how many notches it turned, and a notch is
    /// [`WHEEL_ROWS`]; a touchpad says only how far, and a row is a cell's
    /// height of that. A positive movement is the content moving up, which
    /// is towards the newest rows.
    fn wheel(&mut self) {
        let rows = if self.mouse.notches != 0 {
            self.mouse.distance = 0.0;
            -self.mouse.notches.saturating_mul(WHEEL_ROWS)
        } else {
            let cell = f64::from(u32::try_from(paint::CELL.1).unwrap_or(24));
            let whole = (self.mouse.distance / cell).trunc();
            self.mouse.distance -= whole * cell;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a frame's scroll is a few rows, truncated to whole ones on purpose"
            )]
            let whole = -whole as i32;
            whole
        };
        self.mouse.notches = 0;
        if rows != 0 {
            let rows = isize::try_from(rows).unwrap_or(0);
            self.change(|grid| grid.scroll_view(rows));
        }
    }

    /// The cell the pointer is over, and whether it is in that cell's right
    /// half. Past the right edge is the right half of the last cell, so a
    /// drag out there takes a line to its end.
    fn under_pointer(&self) -> (usize, usize, bool) {
        let (columns, _) = self.grid.size();
        let (x, y) = (self.mouse.at.0.max(0.0), self.mouse.at.1.max(0.0));
        let width = f64::from(u32::try_from(paint::CELL.0).unwrap_or(12));
        let height = f64::from(u32::try_from(paint::CELL.1).unwrap_or(24));
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "both are clamped at zero above, and a window is far narrower than usize"
        )]
        let (column, row) = ((x / width) as usize, (y / height) as usize);
        if column >= columns {
            return (columns.saturating_sub(1), row, true);
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a column number is far below the 2^52 an f64 holds exactly"
        )]
        let right = x - column as f64 * width >= width / 2.0;
        (column, row, right)
    }

    /// The left button went down: start a selection, by the cell, the word
    /// or the line as this is the first, second or third click in a row.
    fn press(&mut self) {
        let (column, row, right) = self.under_pointer();
        let now = Instant::now();
        let count = match self.mouse.last {
            Some((when, at, count))
                if now.duration_since(when) < MULTI_CLICK && at == (column, row) =>
            {
                count % 3 + 1
            }
            _ => 1,
        };
        self.mouse.last = Some((now, (column, row), count));
        let unit = match count {
            1 => Unit::Cell,
            2 => Unit::Word,
            _ => Unit::Line,
        };
        self.mouse.dragging = true;
        self.change(|grid| {
            let at = grid.point(column, row, right);
            grid.select(at, unit);
        });
    }

    /// The pointer moved with the button held: the selection follows it,
    /// and above or below the window the screen scrolls towards it a row at
    /// a time, so a selection can be longer than the window.
    fn drag(&mut self) {
        let rows = self.grid.size().1;
        let bottom = f64::from(u32::try_from(rows * paint::CELL.1).unwrap_or(u32::MAX));
        let step = if self.mouse.at.1 < 0.0 {
            1
        } else if self.mouse.at.1 >= bottom {
            -1
        } else {
            0
        };
        let (column, row, right) = self.under_pointer();
        self.change(|grid| {
            if step != 0 {
                grid.scroll_view(step);
            }
            let at = grid.point(column, row, right);
            grid.extend(at);
        });
    }

    /// What the data device said: a new offer, the clipboard changing hands,
    /// or a drag passing over (which this terminal takes nothing from).
    fn device_event(&mut self, opcode: u16, args: &[Arg<'_>], out: &mut Writer) {
        match opcode {
            wl_data_device::event::DATA_OFFER => {
                if let Some(offer) = args.first().and_then(Arg::as_object) {
                    let _ = self.clipboard.offers.insert(offer, Vec::new());
                }
            }
            wl_data_device::event::SELECTION => {
                let offer = args
                    .first()
                    .and_then(Arg::as_object)
                    .filter(|offer| !offer.is_null());
                self.clipboard.selection = offer;
                self.forget_offers(offer, out);
            }
            wl_data_device::event::LEAVE => {
                let keep = self.clipboard.selection;
                self.forget_offers(keep, out);
            }
            _ => {}
        }
    }

    /// Destroy every offer but `keep`: a selection that was replaced, or a
    /// drag that has left.
    fn forget_offers(&mut self, keep: Option<ObjectId>, out: &mut Writer) {
        let gone: Vec<ObjectId> = self
            .clipboard
            .offers
            .keys()
            .copied()
            .filter(|offer| Some(*offer) != keep)
            .collect();
        for offer in gone {
            let _ = self.clipboard.offers.remove(&offer);
            request(out, offer, wl_data_offer::request::DESTROY, &[], &[]);
        }
    }

    /// What one of this terminal's sources was asked.
    fn source_event(&mut self, source: ObjectId, opcode: u16, args: &[Arg<'_>], out: &mut Writer) {
        match opcode {
            wl_data_source::event::SEND => {
                let (Some(mime), Some(fd)) = (
                    args.first().and_then(Arg::as_str),
                    args.get(1).and_then(Arg::as_fd),
                ) else {
                    return;
                };
                #[expect(
                    unsafe_code,
                    reason = "AUDIT: the descriptor arrived with this message and is this client's to own"
                )]
                // SAFETY: the wire reader hands over a descriptor nothing else holds.
                let owned = unsafe { OwnedFd::from_raw_fd(fd.0) };
                let text = self.clipboard.sources.get(&source).cloned();
                if let Some(text) = text
                    && TEXT_TYPES.contains(&mime)
                {
                    // On a thread of its own: whoever pastes reads as fast
                    // as it likes, and a large copy is more than a pipe
                    // holds. Writing it here would stop the window until
                    // they had read it all -- and forever, were they
                    // waiting for this window in turn.
                    let _ = std::thread::Builder::new()
                        .name("term-copy".to_owned())
                        .spawn(move || {
                            let mut file = std::fs::File::from(owned);
                            let _ = file.write_all(text.as_bytes());
                        });
                }
            }
            wl_data_source::event::CANCELLED => {
                let _ = self.clipboard.sources.remove(&source);
                if self.clipboard.current == Some(source) {
                    self.clipboard.current = None;
                }
                request(out, source, wl_data_source::request::DESTROY, &[], &[]);
            }
            _ => {}
        }
    }

    /// Put the selected text on the clipboard.
    fn copy(&mut self, out: &mut Writer) {
        if !self.clipboard.device {
            return;
        }
        let Some(text) = self.grid.selected().filter(|text| !text.is_empty()) else {
            return;
        };
        let source = ObjectId(self.clipboard.next);
        self.clipboard.next = self.clipboard.next.saturating_add(1);
        request(
            out,
            id::DATA_MANAGER,
            wl_data_device_manager::request::CREATE_DATA_SOURCE,
            &[ArgType::NewId],
            &[Arg::NewId(source)],
        );
        for mime in TEXT_TYPES {
            request(
                out,
                source,
                wl_data_source::request::OFFER,
                &[ArgType::Str { nullable: false }],
                &[Arg::Str(Some(mime))],
            );
        }
        request(
            out,
            id::DATA_DEVICE,
            wl_data_device::request::SET_SELECTION,
            &[ArgType::Object { nullable: true }, ArgType::Uint],
            &[Arg::Object(source), Arg::Uint(self.serial)],
        );
        // The source this replaces stays until the compositor cancels it,
        // which is when nobody can ask it for anything any more.
        let _ = self.clipboard.sources.insert(source, text);
        self.clipboard.current = Some(source);
    }

    /// Paste the clipboard.
    ///
    /// Text this terminal copied itself is taken from its own hands: asked
    /// through the compositor, it would be this terminal that had to write
    /// into the pipe it was waiting to read. Anyone else's is asked for
    /// through a pipe that [`Terminal::read_paste`] reads a little of on
    /// each pass, so the window keeps drawing however long they take.
    fn paste(&mut self, out: &mut Writer) {
        if self.clipboard.pasting.is_some() {
            return;
        }
        if let Some(text) = self
            .clipboard
            .current
            .and_then(|source| self.clipboard.sources.get(&source))
            .cloned()
        {
            self.deliver(text.as_bytes());
            return;
        }
        let Some(offer) = self.clipboard.selection else {
            return;
        };
        let Some(mime) = self.clipboard.offers.get(&offer).and_then(|types| {
            TEXT_TYPES
                .iter()
                .find(|wanted| types.iter().any(|mime| mime == *wanted))
        }) else {
            return;
        };
        let Ok((read, write)) = pipe() else {
            return;
        };
        request(
            out,
            offer,
            wl_data_offer::request::RECEIVE,
            &[ArgType::Str { nullable: false }, ArgType::Fd],
            &[Arg::Str(Some(mime)), Arg::Fd(Fd(write.as_raw_fd()))],
        );
        self.clipboard.sent.push(write);
        self.clipboard.pasting = Some(Paste {
            pipe: read,
            bytes: Vec::new(),
            started: Instant::now(),
        });
    }

    /// Read what has arrived of a paste, and type it once it is all there.
    fn read_paste(&mut self) {
        let Some(paste) = &mut self.clipboard.pasting else {
            return;
        };
        let mut chunk = [0u8; 4096];
        loop {
            match paste.pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    paste
                        .bytes
                        .extend_from_slice(chunk.get(..read).unwrap_or(&[]));
                    if paste.bytes.len() > PASTE_LIMIT {
                        self.clipboard.pasting = None;
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if paste.started.elapsed() > PASTE_PATIENCE {
                        self.clipboard.pasting = None;
                    }
                    return;
                }
                Err(_) => {
                    self.clipboard.pasting = None;
                    return;
                }
            }
        }
        if let Some(paste) = self.clipboard.pasting.take() {
            self.deliver(&paste.bytes);
        }
    }

    /// Type pasted text at the program, as a terminal does.
    ///
    /// A line break becomes the carriage return the Return key sends. And
    /// when the program asked for bracketed paste the text goes between
    /// `ESC [ 200 ~` and `ESC [ 201 ~`, with every escape inside it taken
    /// out: a paste that could spell the closing bracket itself could end
    /// the paste early and have the rest of it typed as commands.
    fn deliver(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes)
            .replace("\r\n", "\r")
            .replace('\n', "\r");
        if text.is_empty() {
            return;
        }
        self.look_live();
        if self.grid.bracketed_paste() {
            let text = text.replace('\x1B', "");
            self.send(b"\x1B[200~");
            self.send(text.as_bytes());
            self.send(b"\x1B[201~");
        } else {
            self.send(text.as_bytes());
        }
    }

    /// Which interface an object of this client's speaks: one of the fixed
    /// ones, or an offer or a source, which come and go.
    fn interface(&self, id: ObjectId) -> Option<&'static Interface> {
        interface_of(id).or_else(|| {
            if self.clipboard.offers.contains_key(&id) {
                Some(&core::WL_DATA_OFFER)
            } else if self.clipboard.sources.contains_key(&id) {
                Some(&core::WL_DATA_SOURCE)
            } else {
                None
            }
        })
    }

    /// Make the grid the size the window is, and tell the program.
    fn refit(&mut self) {
        let scale = usize::try_from(self.scale.max(1)).unwrap_or(1);
        let (width, height) = (
            usize::try_from(self.width.max(0)).unwrap_or(0) * scale,
            usize::try_from(self.height.max(0)).unwrap_or(0) * scale,
        );
        let (columns, rows) = paint::fits(width, height, scale);
        if self.grid.size() != (columns, rows) {
            self.grid.resize(columns, rows);
            self.pty.resize((
                u16::try_from(columns).unwrap_or(u16::MAX),
                u16::try_from(rows).unwrap_or(u16::MAX),
            ));
        }
        // An output-scale change can leave the cell count unchanged while
        // replacing the shared-memory buffer, which still needs every pixel.
        // A configure that changes nothing -- the same size at the same
        // scale, which is every focus change -- needs none.
        let (Ok(wide), Ok(tall)) = (i32::try_from(width), i32::try_from(height)) else {
            self.dirty = Some(Dirty::Full);
            return;
        };
        if self.shared.is_none() || self.buffer_size != (wide, tall) {
            self.dirty = Some(Dirty::Full);
        }
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

        let recreated = self.shared.is_none() || self.buffer_size != (width, height);
        if recreated {
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

        let dirty = if recreated {
            Dirty::Full
        } else {
            self.dirty.unwrap_or(Dirty::Full)
        };
        if let Some(shared) = self.shared.as_mut() {
            let (wide, tall) = (
                usize::try_from(width).unwrap_or(0),
                usize::try_from(height).unwrap_or(0),
            );
            match dirty {
                Dirty::Full => paint::draw(
                    shared.bytes_mut(),
                    (wide, tall),
                    usize::try_from(stride).unwrap_or(0),
                    &self.grid,
                    &self.colours,
                    usize::try_from(scale).unwrap_or(1),
                ),
                Dirty::Cells(damage) => paint::draw_damage(
                    shared.bytes_mut(),
                    (wide, tall),
                    usize::try_from(stride).unwrap_or(0),
                    &self.grid,
                    &self.colours,
                    usize::try_from(scale).unwrap_or(1),
                    damage,
                ),
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
        let (damage_x, damage_y, damage_width, damage_height) = match dirty {
            Dirty::Full => (0, 0, width, height),
            Dirty::Cells(damage) => {
                let cell_width = i32::try_from(paint::CELL.0)
                    .unwrap_or(0)
                    .saturating_mul(scale);
                let cell_height = i32::try_from(paint::CELL.1)
                    .unwrap_or(0)
                    .saturating_mul(scale);
                (
                    i32::try_from(damage.left)
                        .unwrap_or(0)
                        .saturating_mul(cell_width),
                    i32::try_from(damage.top)
                        .unwrap_or(0)
                        .saturating_mul(cell_height),
                    i32::try_from(damage.width)
                        .unwrap_or(0)
                        .saturating_mul(cell_width),
                    i32::try_from(damage.height)
                        .unwrap_or(0)
                        .saturating_mul(cell_height),
                )
            }
        };
        request(
            out,
            id::SURFACE,
            wl_surface::request::DAMAGE_BUFFER,
            &[ArgType::Int, ArgType::Int, ArgType::Int, ArgType::Int],
            &[
                Arg::Int(damage_x),
                Arg::Int(damage_y),
                Arg::Int(damage_width),
                Arg::Int(damage_height),
            ],
        );
        request(out, id::SURFACE, wl_surface::request::COMMIT, &[], &[]);
        self.drawn = self.drawn.saturating_add(1);
        self.dirty = None;
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
        id::POINTER => &core::WL_POINTER,
        id::DATA_MANAGER => &core::WL_DATA_DEVICE_MANAGER,
        id::DATA_DEVICE => &core::WL_DATA_DEVICE,
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
        // What a full socket left queued still has to go.
        return connection
            .flush()
            .map_err(|error| format!("writing: {error:?}"));
    }
    let (bytes, fds) = out.take();
    connection
        .send(&bytes, &fds)
        .map_err(|error| format!("writing: {error:?}"))
}

/// A pipe for a paste: the read half this client keeps, which never waits,
/// and the write half whoever copied is handed, which does -- they write
/// all of it however slowly this reads.
fn pipe() -> Result<(std::fs::File, OwnedFd), String> {
    let mut ends = [0i32; 2];
    #[expect(
        unsafe_code,
        reason = "AUDIT: pipe2 is not in std; it writes two descriptors into an array this frame \
                  owns and the result is checked"
    )]
    // SAFETY: `ends` is two `int`s, which is what `pipe2` writes.
    let made = unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) };
    if made < 0 {
        return Err(format!("a pipe: {}", std::io::Error::last_os_error()));
    }
    let [read, write] = ends;
    #[expect(
        unsafe_code,
        reason = "AUDIT: pipe2 just made this descriptor and nothing else holds it"
    )]
    // SAFETY: as the reason says; it is owned exactly once from here.
    let read = unsafe { OwnedFd::from_raw_fd(read) };
    #[expect(
        unsafe_code,
        reason = "AUDIT: pipe2 just made this descriptor and nothing else holds it"
    )]
    // SAFETY: as the reason says; it is owned exactly once from here.
    let write = unsafe { OwnedFd::from_raw_fd(write) };
    crate::pty::set_nonblocking(read.as_raw_fd())
        .map_err(|error| format!("a pipe that does not wait: {error}"))?;
    Ok((std::fs::File::from(read), write))
}
