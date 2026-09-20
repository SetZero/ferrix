//! The guest's side: the selection, over `ext-data-control`.
//!
//! # Why `ext-data-control` and not `wl_data_device`
//!
//! `/bin/clip` copies and pastes through `wl_data_device`, which is the
//! protocol a program with a window uses. An agent has no window and no
//! keyboard focus, and `wl_data_device`'s selection is the focused client's
//! to set -- a compositor entitled to check would refuse it.
//! `ext-data-control` is the protocol written for exactly this: a clipboard
//! manager that owns and watches the selection without ever being focused.
//! The compositor already carries it (`compositor/server/src/client/control.rs`).
//!
//! # One connection, two directions, for ever
//!
//! `compositor/clip` makes a connection, does one thing and exits. This one
//! lives as long as the compositor: it holds a source whenever the host owns
//! the selection, answers every `send` from it out of the host's clipboard,
//! and watches `selection` for a guest program taking the selection back.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;

use compositor_protocol::core::{self, wl_display, wl_registry};
use compositor_protocol::ext_data_control::{
    self, ext_data_control_device_v1 as device, ext_data_control_manager_v1 as manager,
    ext_data_control_offer_v1 as offer, ext_data_control_source_v1 as source,
};
use compositor_socket::{Connection, RecvError};
use compositor_wire::{Arg, ArgType, Fd, Interface, ObjectId, Reader, Writer};

/// The type a text selection is offered as, which every Wayland program
/// agrees on for plain text. The same constant `/bin/clip` uses.
pub(crate) use compositor_clip::client::TEXT;

/// The objects this client makes at fixed ids. Sources are numbered above
/// them, because one is made and destroyed for every grab.
mod id {
    use compositor_wire::ObjectId;

    pub(super) const DISPLAY: ObjectId = ObjectId(1);
    pub(super) const REGISTRY: ObjectId = ObjectId(2);
    pub(super) const SYNC: ObjectId = ObjectId(3);
    pub(super) const MANAGER: ObjectId = ObjectId(4);
    pub(super) const SEAT: ObjectId = ObjectId(5);
    pub(super) const DEVICE: ObjectId = ObjectId(6);
    /// The first source, and the lowest id a grab may take.
    pub(super) const FIRST_SOURCE: u32 = 16;
}

/// Something the compositor did that the host must hear about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    /// A guest program owns the selection and offers text.
    Grabbed,
    /// Nothing owns the selection any more.
    Released,
    /// The compositor wants the host's clipboard written to this descriptor.
    /// It is this program's to close.
    Wanted(i32),
    /// The source this agent set is no longer the selection: a guest program
    /// took it. The descriptors of any unanswered `send` are already closed.
    Cancelled,
}

/// The connection, and what is on the selection.
#[derive(Debug)]
pub(crate) struct Wayland {
    /// The connection to the compositor.
    connection: Connection,
    /// Requests queued to go out.
    out: Writer,
    /// What the registry advertised.
    globals: BTreeMap<String, (u32, u32)>,
    /// Whether the registry's answer has been acted on.
    bound: bool,
    /// The offer the compositor last made, and the types it carries.
    offer: Option<ObjectId>,
    /// The MIME types that offer named.
    offers: Vec<String>,
    /// The source this agent set, when the host owns the selection.
    source: Option<ObjectId>,
    /// The next source id.
    next_source: u32,
    /// Whether the next `selection` event is this agent's own grab coming
    /// back.
    ///
    /// A client that sets the selection is told about it like every other,
    /// and an agent that believed it would grab on the host what the host
    /// had just grabbed here, for ever. The flag is cleared by the first
    /// `selection` after a `set_selection`, which is the one the compositor
    /// sends for it.
    echo: bool,
}

impl Wayland {
    /// Connect and ask for the registry.
    ///
    /// # Errors
    ///
    /// A sentence saying what could not be done.
    pub(crate) fn connect(socket: &Path) -> Result<Wayland, String> {
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
        let mut wayland = Wayland {
            connection,
            out,
            globals: BTreeMap::new(),
            bound: false,
            offer: None,
            offers: Vec::new(),
            source: None,
            next_source: id::FIRST_SOURCE,
            echo: false,
        };
        wayland.flush()?;
        Ok(wayland)
    }

    /// Read whatever arrived and answer it. Gives what the host must hear.
    ///
    /// # Errors
    ///
    /// A sentence, for a compositor that refused this client or a connection
    /// that cannot be read.
    pub(crate) fn turn(&mut self) -> Result<Vec<Event>, String> {
        match self.connection.receive() {
            Ok(_) | Err(RecvError::WouldBlock) => {}
            Err(RecvError::Closed) => return Err("the compositor closed the connection".to_owned()),
            Err(error) => return Err(format!("reading: {error:?}")),
        }
        let mut events = Vec::new();
        let (consumed, claimed) = self.read(&mut events)?;
        if consumed > 0 {
            self.connection.consume(consumed, claimed);
        }
        self.flush()?;
        Ok(events)
    }

    /// Take the selection for the host, offering text.
    ///
    /// The old source is destroyed first: `ext-data-control` refuses a source
    /// used twice, so every grab is a new one.
    ///
    /// # Errors
    ///
    /// A sentence, for a request that would not encode.
    pub(crate) fn grab(&mut self) -> Result<(), String> {
        self.drop_source();
        let made = ObjectId(self.next_source);
        self.next_source = self.next_source.saturating_add(1);
        request(
            &mut self.out,
            id::MANAGER,
            manager::request::CREATE_DATA_SOURCE,
            &[ArgType::NewId],
            &[Arg::NewId(made)],
        );
        request(
            &mut self.out,
            made,
            source::request::OFFER,
            &[ArgType::Str { nullable: false }],
            &[Arg::Str(Some(TEXT))],
        );
        request(
            &mut self.out,
            id::DEVICE,
            device::request::SET_SELECTION,
            &[ArgType::Object { nullable: true }],
            &[Arg::Object(made)],
        );
        self.source = Some(made);
        self.echo = true;
        self.flush()
    }

    /// Give the selection up, which a host that released its clipboard
    /// means.
    ///
    /// # Errors
    ///
    /// A sentence, for a request that would not encode.
    pub(crate) fn release(&mut self) -> Result<(), String> {
        if self.source.is_none() {
            return Ok(());
        }
        request(
            &mut self.out,
            id::DEVICE,
            device::request::SET_SELECTION,
            &[ArgType::Object { nullable: true }],
            &[Arg::Object(ObjectId(0))],
        );
        self.drop_source();
        self.echo = true;
        self.flush()
    }

    /// Ask the guest's selection for its text and read it.
    ///
    /// The read is blocking, as `/bin/clip`'s is: the program being asked is
    /// another process, and the pipe's write end is closed here so that the
    /// read ends when it has answered.
    ///
    /// # Errors
    ///
    /// A sentence. `Ok(None)` is nothing to give, which is a selection that
    /// is not text or not there -- the answer to that is
    /// `CLIPBOARD`/`NONE` and not a failure.
    pub(crate) fn read_selection(&mut self, limit: usize) -> Result<Option<Vec<u8>>, String> {
        let Some(offered) = self.offer else {
            return Ok(None);
        };
        if !self.offers.iter().any(|mime| mime == TEXT) {
            return Ok(None);
        }
        let (read, write) = pipe()?;
        request(
            &mut self.out,
            offered,
            offer::request::RECEIVE,
            &[ArgType::Str { nullable: false }, ArgType::Fd],
            &[Arg::Str(Some(TEXT)), Arg::Fd(Fd(write))],
        );
        // The compositor has to see the request before anything is written,
        // and this end of the pipe has to be closed here or the read never
        // ends.
        self.flush()?;
        close(write);
        let mut file = unsafe_file(read);
        let mut bytes = Vec::new();
        // One byte over the limit is enough to know it is too large, and
        // stops a hostile guest program filling this process's memory.
        let mut taken = (&mut file).take(limit as u64 + 1);
        let _ = taken
            .read_to_end(&mut bytes)
            .map_err(|error| format!("reading the selection: {error}"))?;
        if bytes.len() > limit {
            return Ok(None);
        }
        Ok(Some(bytes))
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

    /// Forget the source, having destroyed it.
    fn drop_source(&mut self) {
        let Some(made) = self.source.take() else {
            return;
        };
        request(&mut self.out, made, source::request::DESTROY, &[], &[]);
    }

    /// Read whatever arrived, and answer it.
    ///
    /// Gives how many bytes whole events took and how many of the
    /// descriptors beside them they claimed: a descriptor an event carried
    /// is this program's from then on, and the connection must be told so it
    /// does not close it a second time.
    fn read(&mut self, events: &mut Vec<Event>) -> Result<(usize, usize), String> {
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
            self.event(header.sender, header.opcode, &args, events)?;
        }
        Ok((reader.consumed(), reader.descriptors_taken()))
    }

    /// Which interface an object speaks.
    fn interface_of(&self, object: ObjectId) -> Option<&'static Interface> {
        if self.offer == Some(object) || object.is_server() {
            return Some(&ext_data_control::EXT_DATA_CONTROL_OFFER_V1);
        }
        if self.source == Some(object) {
            return Some(&ext_data_control::EXT_DATA_CONTROL_SOURCE_V1);
        }
        Some(match object {
            id::DISPLAY => &core::WL_DISPLAY,
            id::REGISTRY => &core::WL_REGISTRY,
            id::SYNC => &core::WL_CALLBACK,
            id::MANAGER => &ext_data_control::EXT_DATA_CONTROL_MANAGER_V1,
            id::SEAT => &core::WL_SEAT,
            id::DEVICE => &ext_data_control::EXT_DATA_CONTROL_DEVICE_V1,
            // A source destroyed a moment ago may still have an event on its
            // way; it speaks what it always did.
            other if other.0 >= id::FIRST_SOURCE => &ext_data_control::EXT_DATA_CONTROL_SOURCE_V1,
            _ => return None,
        })
    }

    /// Answer one event.
    fn event(
        &mut self,
        sender: ObjectId,
        opcode: u16,
        args: &[Arg<'_>],
        events: &mut Vec<Event>,
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
            id::SYNC if opcode == core::wl_callback::event::DONE => self.bind()?,
            id::DEVICE if opcode == device::event::DATA_OFFER => {
                self.offer = args.first().and_then(Arg::as_object);
                self.offers.clear();
            }
            id::DEVICE if opcode == device::event::SELECTION => {
                let named = args.first().and_then(Arg::as_object);
                let taken = self.echo;
                self.echo = false;
                if named.is_none_or(ObjectId::is_null) {
                    self.offer = None;
                    self.offers.clear();
                    if !taken {
                        events.push(Event::Released);
                    }
                    return Ok(());
                }
                if !taken && self.source.is_none() {
                    events.push(Event::Grabbed);
                }
            }
            // The device is gone: the compositor is going away.
            id::DEVICE if opcode == device::event::FINISHED => {
                return Err("the compositor took the data control device away".to_owned());
            }
            other if self.source == Some(other) && opcode == source::event::SEND => {
                let (Some(mime), Some(fd)) = (
                    args.first().and_then(Arg::as_str),
                    args.get(1).and_then(Arg::as_fd),
                ) else {
                    return Ok(());
                };
                if mime == TEXT {
                    events.push(Event::Wanted(fd.0));
                } else {
                    close(fd.0);
                }
            }
            other if self.source == Some(other) && opcode == source::event::CANCELLED => {
                self.drop_source();
                events.push(Event::Cancelled);
            }
            other if self.offer == Some(other) && opcode == offer::event::OFFER => {
                if let Some(mime) = args.first().and_then(Arg::as_str) {
                    self.offers.push(mime.to_owned());
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Bind the manager and the seat, and take a device from them.
    fn bind(&mut self) -> Result<(), String> {
        if self.bound {
            return Ok(());
        }
        self.bound = true;
        for (interface, object, want) in [
            ("ext_data_control_manager_v1", id::MANAGER, 1),
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
                            id: object,
                        },
                    ],
                )
                .map_err(|error| format!("binding {interface}: {error:?}"))?;
        }
        request(
            &mut self.out,
            id::MANAGER,
            manager::request::GET_DATA_DEVICE,
            &[ArgType::NewId, ArgType::Object { nullable: false }],
            &[Arg::NewId(id::DEVICE), Arg::Object(id::SEAT)],
        );
        Ok(())
    }
}

/// A pipe: the read half for this program and the write half for whoever is
/// asked to fill it.
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
pub(crate) fn unsafe_file(fd: i32) -> std::fs::File {
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
pub(crate) fn close(fd: i32) {
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
