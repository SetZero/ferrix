//! The Wayland side of copying and pasting.
//!
//! The objects a clipboard needs and no others: `wl_data_device_manager`,
//! a `wl_seat` to get a device from, a `wl_data_source` to copy with and the
//! `wl_data_offer` the compositor makes to paste from. No surface, no
//! window, no buffer -- a program that copies does not need a screen.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::time::{Duration, Instant};

use compositor_protocol::core::{
    self, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_display,
    wl_registry,
};
use compositor_protocol::primary_selection::{
    self, zwp_primary_selection_device_manager_v1 as primary_manager,
    zwp_primary_selection_device_v1 as primary_device,
    zwp_primary_selection_offer_v1 as primary_offer,
    zwp_primary_selection_source_v1 as primary_source,
};
use compositor_socket::{Connection, RecvError};
use compositor_wire::{Arg, ArgType, Fd, Interface, ObjectId, Reader, Writer};

/// The objects this client makes, at fixed ids.
mod id {
    use compositor_wire::ObjectId;

    pub(super) const DISPLAY: ObjectId = ObjectId(1);
    pub(super) const REGISTRY: ObjectId = ObjectId(2);
    pub(super) const SYNC: ObjectId = ObjectId(3);
    pub(super) const MANAGER: ObjectId = ObjectId(4);
    pub(super) const SEAT: ObjectId = ObjectId(5);
    pub(super) const DEVICE: ObjectId = ObjectId(6);
    pub(super) const SOURCE: ObjectId = ObjectId(7);
}

/// The type a text selection is offered as, which is what every Wayland
/// program agrees on for plain text.
pub const TEXT: &str = "text/plain;charset=utf-8";

/// Which of the two selections a run is about.
///
/// The clipboard is what a copy puts there and a paste takes out; the
/// primary selection is what merely selecting text puts there and a middle
/// click takes out. They are the same protocol twice over, so this program
/// is one program with a flag rather than two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// `wl_data_device`: copy and paste.
    Clipboard,
    /// `zwp_primary_selection_device_v1`: select and middle click.
    Primary,
}

impl Which {
    /// The manager this one binds.
    const fn manager(self) -> &'static str {
        match self {
            Self::Clipboard => "wl_data_device_manager",
            Self::Primary => "zwp_primary_selection_device_manager_v1",
        }
    }

    /// What it is called in a line this program prints.
    const fn name(self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::Primary => "primary",
        }
    }

    /// The version this binds the manager at.
    const fn version(self) -> u32 {
        match self {
            Self::Clipboard => 3,
            Self::Primary => 1,
        }
    }

    /// The four opcodes that differ between the two: making a source,
    /// making a device, offering a type, setting the selection.
    const fn requests(self) -> Requests {
        match self {
            Self::Clipboard => Requests {
                create_source: wl_data_device_manager::request::CREATE_DATA_SOURCE,
                get_device: wl_data_device_manager::request::GET_DATA_DEVICE,
                offer: wl_data_source::request::OFFER,
                set_selection: wl_data_device::request::SET_SELECTION,
                receive: wl_data_offer::request::RECEIVE,
            },
            Self::Primary => Requests {
                create_source: primary_manager::request::CREATE_SOURCE,
                get_device: primary_manager::request::GET_DEVICE,
                offer: primary_source::request::OFFER,
                set_selection: primary_device::request::SET_SELECTION,
                receive: primary_offer::request::RECEIVE,
            },
        }
    }

    /// The four events that differ: the source being asked and cancelled,
    /// the device naming an offer and naming the selection.
    const fn events(self) -> Events {
        match self {
            Self::Clipboard => Events {
                send: wl_data_source::event::SEND,
                cancelled: wl_data_source::event::CANCELLED,
                data_offer: wl_data_device::event::DATA_OFFER,
                selection: wl_data_device::event::SELECTION,
                offered: wl_data_offer::event::OFFER,
            },
            Self::Primary => Events {
                send: primary_source::event::SEND,
                cancelled: primary_source::event::CANCELLED,
                data_offer: primary_device::event::DATA_OFFER,
                selection: primary_device::event::SELECTION,
                offered: primary_offer::event::OFFER,
            },
        }
    }

    /// The interfaces its four objects speak.
    fn interfaces(self) -> [&'static Interface; 4] {
        match self {
            Self::Clipboard => [
                &core::WL_DATA_DEVICE_MANAGER,
                &core::WL_DATA_DEVICE,
                &core::WL_DATA_SOURCE,
                &core::WL_DATA_OFFER,
            ],
            Self::Primary => [
                &primary_selection::ZWP_PRIMARY_SELECTION_DEVICE_MANAGER_V1,
                &primary_selection::ZWP_PRIMARY_SELECTION_DEVICE_V1,
                &primary_selection::ZWP_PRIMARY_SELECTION_SOURCE_V1,
                &primary_selection::ZWP_PRIMARY_SELECTION_OFFER_V1,
            ],
        }
    }
}

/// The request opcodes that differ between the two selections.
#[derive(Clone, Copy, Debug)]
struct Requests {
    create_source: u16,
    get_device: u16,
    offer: u16,
    set_selection: u16,
    receive: u16,
}

/// The event opcodes that differ.
#[derive(Clone, Copy, Debug)]
struct Events {
    send: u16,
    cancelled: u16,
    data_offer: u16,
    selection: u16,
    offered: u16,
}

/// How long a paste waits for the selection before giving up.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long a copy stays alive after it has been asked for its data.
///
/// A program that copied has to stay to answer; one that exits at once is a
/// clipboard whose contents vanish. `wl-copy` forks into the background and
/// stays for ever, which a test cannot do: this one leaves once it has
/// answered, and a moment later in case something else asks.
const LINGER: Duration = Duration::from_secs(2);

/// Copy `text`, and stay until it has been asked for or `patience` is up.
///
/// # Errors
///
/// A sentence saying what could not be done.
pub fn copy(
    socket: &std::path::Path,
    which: Which,
    text: &str,
    patience: Duration,
) -> Result<String, String> {
    let mut session = Session::connect(socket, which, Want::Copy(text.to_owned()))?;
    session.run(patience)?;
    Ok(format!(
        "clip: copied {} bytes to the {}, asked for {} times",
        text.len(),
        which.name(),
        session.answered
    ))
}

/// Paste, and give back what the selection held.
///
/// # Errors
///
/// A sentence saying what could not be done, including nothing having been
/// copied before the wait was up.
pub fn paste(socket: &std::path::Path, which: Which) -> Result<String, String> {
    let mut session = Session::connect(socket, which, Want::Paste)?;
    session.run(PATIENCE)?;
    session
        .pasted
        .clone()
        .ok_or_else(|| "nothing was on the clipboard".to_owned())
}

/// Which half this is.
#[derive(Clone, Debug)]
enum Want {
    /// Copy this text.
    Copy(String),
    /// Paste whatever is there.
    Paste,
}

/// One connection, doing one of the two things.
struct Session {
    connection: Connection,
    out: Writer,
    which: Which,
    want: Want,
    globals: BTreeMap<String, (u32, u32)>,
    bound: bool,
    /// The offer the compositor made, and the types it carries.
    offer: Option<ObjectId>,
    offers: Vec<String>,
    /// Whether the selection has been asked for.
    asked: bool,
    /// What a paste read.
    pasted: Option<String>,
    /// How many times a copy was asked for its data.
    answered: u32,
    /// When the last answer was given, so a copy can leave.
    answered_at: Option<Instant>,
    /// Whether the compositor said this source is no longer the selection.
    cancelled: bool,
}

impl Session {
    /// Connect and ask for the registry.
    fn connect(socket: &std::path::Path, which: Which, want: Want) -> Result<Self, String> {
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
            which,
            want,
            globals: BTreeMap::new(),
            bound: false,
            offer: None,
            offers: Vec::new(),
            asked: false,
            pasted: None,
            answered: 0,
            answered_at: None,
            cancelled: false,
        })
    }

    /// Read and answer until the work is done or the time is up.
    fn run(&mut self, patience: Duration) -> Result<(), String> {
        let started = Instant::now();
        self.flush()?;
        while started.elapsed() < patience {
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
            if self.finished() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.finished() {
            Ok(())
        } else {
            Err(match self.want {
                Want::Copy(_) => "nobody asked for what was copied".to_owned(),
                Want::Paste => "nothing was copied".to_owned(),
            })
        }
    }

    /// Whether there is nothing left to do.
    fn finished(&self) -> bool {
        match self.want {
            Want::Copy(_) => {
                self.cancelled || self.answered_at.is_some_and(|when| when.elapsed() > LINGER)
            }
            Want::Paste => self.pasted.is_some(),
        }
    }

    /// Send whatever is queued.
    fn flush(&mut self) -> Result<(), String> {
        if self.out.is_empty() {
            return Ok(());
        }
        let (bytes, fds) = self.out.take();
        self.connection
            .send(&bytes, &fds)
            .map_err(|error| format!("writing: {error:?}"))
    }

    /// Read whatever arrived, and answer it.
    ///
    /// Gives how many bytes whole events took and how many of the
    /// descriptors beside them they claimed: a descriptor an event carried
    /// is this program's from then on, and the connection must be told so it
    /// does not close it a second time.
    fn read(&mut self) -> Result<(usize, usize), String> {
        let fds = self.connection.fds();
        let bytes = self.connection.bytes().to_vec();
        let mut reader = Reader::new(&bytes, &fds);
        while !reader.is_done() {
            let Ok(header) = reader.peek() else {
                break;
            };
            let Some(interface) = self.interface_of(header.sender) else {
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

    /// Which interface an object speaks.
    fn interface_of(&self, id: ObjectId) -> Option<&'static Interface> {
        let [manager, device, source, offer] = self.which.interfaces();
        if self.offer == Some(id) || id.is_server() {
            return Some(offer);
        }
        Some(match id {
            id::DISPLAY => &core::WL_DISPLAY,
            id::REGISTRY => &core::WL_REGISTRY,
            id::SYNC => &core::WL_CALLBACK,
            id::MANAGER => manager,
            id::SEAT => &core::WL_SEAT,
            id::DEVICE => device,
            id::SOURCE => source,
            _ => return None,
        })
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
                let _ = self.globals.insert(interface.to_owned(), (name, version));
            }
            id::SYNC if opcode == core::wl_callback::event::DONE => self.bind()?,
            id::SOURCE if opcode == self.which.events().send => {
                let (Some(mime), Some(fd)) = (
                    args.first().and_then(Arg::as_str),
                    args.get(1).and_then(Arg::as_fd),
                ) else {
                    return Ok(());
                };
                self.answer(mime, fd);
            }
            id::SOURCE if opcode == self.which.events().cancelled => self.cancelled = true,
            id::DEVICE if opcode == self.which.events().data_offer => {
                self.offer = args.first().and_then(Arg::as_object);
                self.offers.clear();
            }
            id::DEVICE if opcode == self.which.events().selection => {
                let named = args.first().and_then(Arg::as_object);
                if named.is_none_or(ObjectId::is_null) {
                    self.offer = None;
                    return Ok(());
                }
                self.take()?;
            }
            other if self.offer == Some(other) && opcode == self.which.events().offered => {
                if let Some(mime) = args.first().and_then(Arg::as_str) {
                    self.offers.push(mime.to_owned());
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Bind what this half needs, and say what it is doing.
    fn bind(&mut self) -> Result<(), String> {
        if self.bound {
            return Ok(());
        }
        self.bound = true;
        for (interface, id, want) in [
            (self.which.manager(), id::MANAGER, self.which.version()),
            ("wl_seat", id::SEAT, 7),
        ] {
            let (name, offered) = self
                .globals
                .get(interface)
                .copied()
                .ok_or_else(|| format!("the compositor offers no {interface}"))?;
            self.out
                .write(
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
            &mut self.out,
            id::MANAGER,
            self.which.requests().get_device,
            &[ArgType::NewId, ArgType::Object { nullable: false }],
            &[Arg::NewId(id::DEVICE), Arg::Object(id::SEAT)],
        );
        if let Want::Copy(_) = self.want {
            request(
                &mut self.out,
                id::MANAGER,
                self.which.requests().create_source,
                &[ArgType::NewId],
                &[Arg::NewId(id::SOURCE)],
            );
            request(
                &mut self.out,
                id::SOURCE,
                self.which.requests().offer,
                &[ArgType::Str { nullable: false }],
                &[Arg::Str(Some(TEXT))],
            );
            request(
                &mut self.out,
                id::DEVICE,
                self.which.requests().set_selection,
                &[ArgType::Object { nullable: true }, ArgType::Uint],
                // The serial of the event that made this the client's to
                // copy. Nothing here has a serial to give: a compositor that
                // checked would refuse, and this one does not, which is what
                // `wl-copy --primary` relies on too.
                &[Arg::Object(id::SOURCE), Arg::Uint(0)],
            );
        }
        Ok(())
    }

    /// Ask for the selection's text through a pipe, and read it.
    fn take(&mut self) -> Result<(), String> {
        if self.asked || !matches!(self.want, Want::Paste) {
            return Ok(());
        }
        let Some(offer) = self.offer else {
            return Ok(());
        };
        if !self.offers.iter().any(|mime| mime == TEXT) {
            return Err(format!(
                "the selection is not text: it offers {}",
                self.offers.join(", ")
            ));
        }
        self.asked = true;
        let (read, write) = pipe()?;
        request_with_fd(
            &mut self.out,
            offer,
            self.which.requests().receive,
            &[ArgType::Str { nullable: false }, ArgType::Fd],
            &[Arg::Str(Some(TEXT)), Arg::Fd(Fd(write))],
        );
        // The compositor has to see the request before anything is written,
        // and the write half has to be closed here or the read never ends.
        self.flush()?;
        close(write);
        let mut file = unsafe_file(read);
        let mut text = String::new();
        let _read = file
            .read_to_string(&mut text)
            .map_err(|error| format!("reading the selection: {error}"))?;
        self.pasted = Some(text);
        Ok(())
    }

    /// Write what was copied to the descriptor whoever pasted sent.
    fn answer(&mut self, mime: &str, fd: Fd) {
        let Want::Copy(text) = &self.want else {
            close(fd.0);
            return;
        };
        if mime != TEXT {
            close(fd.0);
            return;
        }
        let mut file = unsafe_file(fd.0);
        let _ = file.write_all(text.as_bytes());
        let _ = file.flush();
        // Dropping the file closes the descriptor, which is what tells the
        // reader there is no more.
        drop(file);
        self.answered = self.answered.saturating_add(1);
        self.answered_at = Some(Instant::now());
    }
}

/// A pipe, as `wl-paste` makes one: the read half for this program and the
/// write half for the compositor to pass on.
fn pipe() -> Result<(i32, i32), String> {
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
    Ok((ends[0], ends[1]))
}

/// A descriptor as a file, so that `read` and `write` are `std`'s.
fn unsafe_file(fd: i32) -> std::fs::File {
    #[expect(
        unsafe_code,
        reason = "AUDIT: the descriptor is this process's own -- one end of a pipe it made, or one \
                  the compositor sent it -- and the File owns it from here"
    )]
    // SAFETY: as the comment says; nothing else holds it.
    unsafe {
        use std::os::fd::FromRawFd;
        std::fs::File::from_raw_fd(fd)
    }
}

/// Close a descriptor.
fn close(fd: i32) {
    #[expect(
        unsafe_code,
        reason = "AUDIT: close is not in std for a raw descriptor; this one is this process's own"
    )]
    // SAFETY: a descriptor this process owns.
    let _ = unsafe { libc::close(fd) };
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
