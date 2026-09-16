//! One client's connection: its objects, and what its requests do.

use std::collections::BTreeMap;

use compositor_protocol::core::{
    self, wl_compositor, wl_display, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use compositor_protocol::xdg_shell::{self, xdg_surface, xdg_toplevel, xdg_wm_base};
use compositor_wire::{
    Arg, ArgType, Error as WireError, Fd, ObjectError, ObjectId, Objects, Reader, Writer,
};

use crate::globals::Globals;
use crate::role::Role;
use crate::shm::{Buffer, FORMATS, Pool};
use crate::surface::{Committed, Rect, Region, Surface};
use crate::xdg::{Toplevel, XdgRole, XdgSurface};

/// What ended a connection.
///
/// Every one of these has been told to the client as `wl_display.error`
/// before it is given back here, except [`Fatal::Unreadable`], which is the
/// client's bytes being unreadable rather than its behaviour being wrong: a
/// message the server cannot decode is a message whose `wl_display.error`
/// would name an object it cannot trust either.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fatal {
    /// A request to an object that is not live, or that takes no requests.
    /// `wl_display.error` with `invalid_object`.
    NoSuchObject(ObjectId),
    /// An opcode the object's interface does not have, or one the version it
    /// was bound at does not have yet. `invalid_method`.
    NoSuchMethod {
        /// The object the request was addressed to.
        object: ObjectId,
        /// The opcode that named nothing.
        opcode: u16,
    },
    /// A `new_id` the client cannot give: zero, one already in use, or one in
    /// the server's half of the id space. `invalid_object`.
    BadNewId(ObjectId),
    /// A `wl_registry.bind` for a name the registry never advertised, or at a
    /// version above the one it advertised. `invalid_object`.
    BadBind {
        /// The name the client asked for.
        name: u32,
        /// The version it asked for.
        version: u32,
    },
    /// The bytes are not a message the signature describes.
    Unreadable(WireError),
    /// A request naming an object of the wrong interface: a `wl_buffer`
    /// argument that is a `wl_surface`, say. `invalid_object`.
    WrongInterface {
        /// The object the argument named.
        object: ObjectId,
        /// What the request wanted it to be.
        wanted: &'static str,
    },
    /// A request an interface's own error codes cover: `wl_shm`'s
    /// `invalid_format` and `invalid_stride`, and the rest as they land.
    /// The object, code and sentence are the interface's, not
    /// `wl_display`'s.
    Interface {
        /// The object the error is on.
        object: ObjectId,
        /// The interface's own code.
        code: u32,
        /// What it says.
        text: String,
    },
}

impl Fatal {
    /// The `wl_display.error` code this is told to the client as.
    #[must_use]
    pub const fn code(&self) -> u32 {
        match self {
            Self::NoSuchObject(_) | Self::BadNewId(_) | Self::BadBind { .. } => {
                wl_display::error::INVALID_OBJECT
            }
            Self::NoSuchMethod { .. } => wl_display::error::INVALID_METHOD,
            Self::Unreadable(_) => wl_display::error::INVALID_METHOD,
            Self::WrongInterface { .. } => wl_display::error::INVALID_OBJECT,
            Self::Interface { code, .. } => *code,
        }
    }

    /// The object the error names, which is the one the request was for when
    /// there is one and `wl_display` otherwise.
    ///
    /// Never the null object: `wl_display.error`'s `object_id` argument is
    /// not nullable, so an error naming zero could not be encoded at all and
    /// the client would see the socket close with nothing said. A client that
    /// sent a `new_id` of zero gets the error on `wl_display`, which is where
    /// libwayland posts one it cannot attribute.
    #[must_use]
    pub const fn object(&self) -> ObjectId {
        let named = match self {
            Self::NoSuchObject(id) | Self::BadNewId(id) => *id,
            Self::NoSuchMethod { object, .. }
            | Self::WrongInterface { object, .. }
            | Self::Interface { object, .. } => *object,
            Self::BadBind { .. } | Self::Unreadable(_) => ObjectId::DISPLAY,
        };
        if named.is_null() {
            ObjectId::DISPLAY
        } else {
            named
        }
    }

    /// The sentence the client is given. libwayland logs it; a person
    /// debugging a client reads it.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NoSuchObject(id) => format!("object {} is not live", id.0),
            Self::NoSuchMethod { object, opcode } => {
                format!("object {} has no request {opcode}", object.0)
            }
            Self::BadNewId(id) => format!("{} is not an id this client may give", id.0),
            Self::BadBind { name, version } => {
                format!("no global {name} at version {version}")
            }
            Self::Unreadable(error) => format!("a message that is not one: {error:?}"),
            Self::WrongInterface { object, wanted } => {
                format!("object {} is not a {wanted}", object.0)
            }
            Self::Interface { text, .. } => text.clone(),
        }
    }
}

/// Something the compositor above has to act on, which the protocol alone
/// cannot answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// The client bound a global. The compositor learns of every binding so
    /// it can send what a fresh object is owed -- `wl_shm`'s formats, a
    /// seat's capabilities, an output's mode.
    Bound {
        /// The object the client made.
        object: ObjectId,
        /// What it is.
        role: Role,
        /// The version it was bound at, which is at most the global's.
        version: u32,
    },
    /// The client destroyed an object.
    Destroyed {
        /// The object that is gone.
        object: ObjectId,
        /// What it was.
        role: Role,
    },
    /// A surface's pending state became current. What it shows may have
    /// changed, and so may the region it takes input in.
    SurfaceCommitted {
        /// The surface.
        surface: ObjectId,
        /// What the commit did.
        change: Committed,
    },
    /// A pool was made over a descriptor the client sent. The compositor
    /// above maps it; nothing here touches it.
    PoolCreated {
        /// The `wl_shm_pool` object.
        pool: ObjectId,
        /// The pool's descriptor and size.
        memory: Pool,
    },
    /// A surface became a window. The layout has to place it, and the
    /// compositor has to configure it before the client may attach a buffer.
    ToplevelCreated {
        /// The `xdg_toplevel`.
        toplevel: ObjectId,
        /// The `wl_surface` under it.
        surface: ObjectId,
    },
    /// A window's title or app id changed, which `hyprctl clients` prints
    /// and `windowrule` matches on.
    ToplevelRenamed {
        /// The `xdg_toplevel`.
        toplevel: ObjectId,
    },
    /// A window asked for a state a tiling compositor answers by
    /// configuring: `set_maximized`, `set_fullscreen` and their opposites.
    ToplevelAsked {
        /// The `xdg_toplevel`.
        toplevel: ObjectId,
        /// Whether it asked to be maximized.
        maximized: bool,
        /// Whether it asked to be fullscreen.
        fullscreen: bool,
    },
    /// A pool grew. Whatever mapped it has to map it again.
    PoolResized {
        /// The `wl_shm_pool` object.
        pool: ObjectId,
        /// Its size now.
        size: i32,
    },
}

/// The bytes and descriptors a connection has to send.
#[derive(Clone, Debug, Default)]
pub struct Outgoing {
    /// The bytes, whole messages only.
    pub bytes: Vec<u8>,
    /// The descriptors to send beside them, in order.
    pub descriptors: Vec<Fd>,
}

/// One client.
#[derive(Debug)]
pub struct Client {
    objects: Objects<Role>,
    out: Writer,
    events: Vec<Event>,
    fatal: Option<Fatal>,
    globals: Globals,
    surfaces: BTreeMap<ObjectId, Surface>,
    regions: BTreeMap<ObjectId, Region>,
    pools: BTreeMap<ObjectId, Pool>,
    buffers: BTreeMap<ObjectId, Buffer>,
    xdg_surfaces: BTreeMap<ObjectId, XdgSurface>,
    toplevels: BTreeMap<ObjectId, Toplevel>,
    /// The next configure serial. Serials go up and are never reused, so a
    /// client's `ack_configure` names one configure and no other.
    serial: u32,
}

impl Client {
    /// A client that has just connected: `wl_display` is object 1 and nothing
    /// else is live, which is exactly the state libwayland starts a
    /// connection in.
    ///
    /// `globals` is what its registry will advertise.
    #[must_use]
    pub fn new(globals: Globals) -> Self {
        let mut objects = Objects::new();
        // wl_display is version 1 and the only object that is never created
        // by a request, so this cannot fail; if it somehow did, every request
        // would be answered `invalid_object`, which is the safe direction.
        let _ = objects.insert(ObjectId::DISPLAY, &core::WL_DISPLAY, 1, Role::Display);
        Self {
            objects,
            out: Writer::new(),
            events: Vec::new(),
            fatal: None,
            globals,
            surfaces: BTreeMap::new(),
            regions: BTreeMap::new(),
            pools: BTreeMap::new(),
            buffers: BTreeMap::new(),
            xdg_surfaces: BTreeMap::new(),
            toplevels: BTreeMap::new(),
            serial: 1,
        }
    }

    /// The `xdg_surface` `id` names, if it is one.
    #[must_use]
    pub fn xdg_surface(&self, id: ObjectId) -> Option<&XdgSurface> {
        self.xdg_surfaces.get(&id)
    }

    /// The toplevel `id` names, if it is one.
    #[must_use]
    pub fn toplevel(&self, id: ObjectId) -> Option<&Toplevel> {
        self.toplevels.get(&id)
    }

    /// Every toplevel, in id order: the windows this client has.
    pub fn toplevels(&self) -> impl Iterator<Item = (ObjectId, &Toplevel)> {
        self.toplevels.iter().map(|(id, top)| (*id, top))
    }

    /// The next serial, which is also this connection's serial for input
    /// events. Wayland has one serial space per connection.
    pub fn next_serial(&mut self) -> u32 {
        let serial = self.serial;
        self.serial = self.serial.wrapping_add(1).max(1);
        serial
    }

    /// Tell a toplevel what size to be and what state it is in.
    ///
    /// This is the compositor's half of the configure conversation: the
    /// layout decides a size, the client draws at it and acks. `states` is
    /// `xdg_toplevel.state` values, which for a tiling compositor is mostly
    /// `activated` and the `tiled_*` edges.
    pub fn configure_toplevel(
        &mut self,
        toplevel: ObjectId,
        width: i32,
        height: i32,
        states: &[u32],
    ) {
        let Some(top) = self.toplevels.get_mut(&toplevel) else {
            return;
        };
        top.configured = (width, height);
        let xdg = top.xdg_surface;
        let packed: Vec<u8> = states
            .iter()
            .flat_map(|state| state.to_le_bytes())
            .collect();
        let _ = self.out.write(
            toplevel,
            xdg_toplevel::event::CONFIGURE,
            &[ArgType::Int, ArgType::Int, ArgType::Array],
            &[Arg::Int(width), Arg::Int(height), Arg::Array(&packed)],
        );
        let serial = self.next_serial();
        let _ = self.out.write(
            xdg,
            xdg_surface::event::CONFIGURE,
            &[ArgType::Uint],
            &[Arg::Uint(serial)],
        );
        if let Some(surface) = self.xdg_surfaces.get_mut(&xdg) {
            surface.configure_sent(serial);
        }
    }

    /// Ask a toplevel to close, as `killactive` does.
    ///
    /// It is a request, not an order: a client may put up "save your work?"
    /// and never close. Hyprland's `killactive` sends this and nothing else.
    pub fn close_toplevel(&mut self, toplevel: ObjectId) {
        if !self.toplevels.contains_key(&toplevel) {
            return;
        }
        let _ = self
            .out
            .write(toplevel, xdg_toplevel::event::CLOSE, &[], &[]);
    }

    /// The surface `id` names, if it is one.
    #[must_use]
    pub fn surface(&self, id: ObjectId) -> Option<&Surface> {
        self.surfaces.get(&id)
    }

    /// The surface `id` names, to be changed by the compositor above.
    pub fn surface_mut(&mut self, id: ObjectId) -> Option<&mut Surface> {
        self.surfaces.get_mut(&id)
    }

    /// Every surface, in id order.
    pub fn surfaces(&self) -> impl Iterator<Item = (ObjectId, &Surface)> {
        self.surfaces.iter().map(|(id, surface)| (*id, surface))
    }

    /// The region `id` names, if it is one.
    #[must_use]
    pub fn region(&self, id: ObjectId) -> Option<&Region> {
        self.regions.get(&id)
    }

    /// The pool `id` names, if it is one.
    #[must_use]
    pub fn pool(&self, id: ObjectId) -> Option<&Pool> {
        self.pools.get(&id)
    }

    /// The buffer `id` names, if it is one.
    #[must_use]
    pub fn buffer(&self, id: ObjectId) -> Option<&Buffer> {
        self.buffers.get(&id)
    }

    /// Tell the client a buffer is its own again.
    ///
    /// The compositor above calls this once it has finished reading a buffer
    /// a commit replaced. Until it does, the client may not draw into that
    /// memory, so a compositor that forgets is a client that stalls.
    pub fn release_buffer(&mut self, buffer: ObjectId) {
        if !self.buffers.contains_key(&buffer) {
            return;
        }
        let _ = self
            .out
            .write(buffer, core::wl_buffer::event::RELEASE, &[], &[]);
    }

    /// Fire a surface's frame callbacks with `time`, and take them.
    ///
    /// `wl_callback.done`'s argument is milliseconds with an undefined base,
    /// which is what every client treats it as.
    pub fn fire_frame_callbacks(&mut self, surface: ObjectId, time: u32) {
        let Some(state) = self.surfaces.get_mut(&surface) else {
            return;
        };
        for callback in state.take_frame_callbacks() {
            let _ = self.out.write(
                callback,
                core::wl_callback::event::DONE,
                &[ArgType::Uint],
                &[Arg::Uint(time)],
            );
            self.destroy(callback, Role::FrameCallback);
        }
    }

    /// Why the connection ended, once it has.
    #[must_use]
    pub const fn fatal(&self) -> Option<&Fatal> {
        self.fatal.as_ref()
    }

    /// Whether the connection is finished and nothing more should be read.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.fatal.is_some()
    }

    /// The live objects, for the compositor above and for tests.
    #[must_use]
    pub const fn objects(&self) -> &Objects<Role> {
        &self.objects
    }

    /// What the compositor above has to act on, taken.
    #[must_use]
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// The bytes and descriptors to send, taken.
    #[must_use]
    pub fn take_outgoing(&mut self) -> Outgoing {
        let (bytes, descriptors) = self.out.take();
        Outgoing { bytes, descriptors }
    }

    /// Answer every whole message in `bytes`, taking descriptors from `fds`
    /// as `fd` arguments call for them.
    ///
    /// Gives how many bytes were used; what is left is a message that has not
    /// all arrived, which the caller keeps for the next read. A connection
    /// that has already failed uses nothing.
    pub fn read(&mut self, bytes: &[u8], fds: &[Fd]) -> usize {
        if self.is_finished() {
            return 0;
        }
        let mut reader = Reader::new(bytes, fds);
        while !reader.is_done() {
            let Ok(header) = reader.peek() else {
                break;
            };
            // Find the object and its request before reading the arguments:
            // the signature is what says how to read them.
            let Some(entry) = self.objects.get(header.sender) else {
                self.fail(Fatal::NoSuchObject(header.sender));
                break;
            };
            let (role, interface, version) = (entry.data, entry.interface, entry.version);
            if !role.takes_requests() {
                self.fail(Fatal::NoSuchObject(header.sender));
                break;
            }
            let Some(method) = interface.request(header.opcode) else {
                self.fail(Fatal::NoSuchMethod {
                    object: header.sender,
                    opcode: header.opcode,
                });
                break;
            };
            // A request added in a later version of the interface is not one
            // this object has: the client asked for the version it got.
            if method.since > version {
                self.fail(Fatal::NoSuchMethod {
                    object: header.sender,
                    opcode: header.opcode,
                });
                break;
            }
            let (_, args) = match reader.read(method.signature) {
                Ok(read) => read,
                Err(WireError::Incomplete { .. }) => break,
                Err(error) => {
                    self.fail(Fatal::Unreadable(error));
                    break;
                }
            };
            self.dispatch(header.sender, role, version, header.opcode, &args);
            if method.destructor {
                self.destroy(header.sender, role);
            }
            if self.is_finished() {
                break;
            }
        }
        reader.consumed()
    }

    /// End the connection, telling the client why.
    ///
    /// Only the first failure is told: after one, `wl_display.error` has
    /// already named an object and nothing else the client sent will be
    /// answered.
    pub fn fail(&mut self, reason: Fatal) {
        if self.fatal.is_some() {
            return;
        }
        let message = reason.message();
        let args = [
            Arg::Object(reason.object()),
            Arg::Uint(reason.code()),
            Arg::Str(Some(&message)),
        ];
        // The only way this can fail is a sentence longer than the wire
        // format allows, which none of `Fatal`'s is, or a null object, which
        // `Fatal::object` is written to rule out. Either way the client is
        // about to be disconnected; the assertion is what keeps a future
        // `Fatal` from silently losing its error event.
        let wrote = self.out.write(
            ObjectId::DISPLAY,
            wl_display::event::ERROR,
            error_signature(),
            &args,
        );
        debug_assert!(
            wrote.is_ok(),
            "wl_display.error could not be encoded: {wrote:?}"
        );
        self.fatal = Some(reason);
    }

    /// Drop an object and tell the client it may reuse the number.
    fn destroy(&mut self, id: ObjectId, role: Role) {
        if self.objects.remove(id).is_err() {
            return;
        }
        match role {
            Role::Surface => {
                let _ = self.surfaces.remove(&id);
            }
            Role::Region => {
                let _ = self.regions.remove(&id);
            }
            Role::ShmPool => {
                // The protocol keeps the pool's memory alive while buffers
                // cut from it live: "the mmapped memory will be released
                // when all buffers that have been created from this pool are
                // gone". So the object goes and the memory does not, and the
                // compositor above unmaps it when the last buffer does.
                let _ = self.pools.remove(&id);
            }
            Role::XdgSurface => {
                // xdg_surface.destroy with a role object still live is
                // `defunct_role_object`; the client is supposed to destroy
                // the toplevel first. Dropping the toplevel here as well
                // would hide the client's mistake, so it is refused instead.
                let _ = self.xdg_surfaces.remove(&id);
            }
            Role::XdgToplevel => {
                if let Some(top) = self.toplevels.remove(&id)
                    && let Some(xdg) = self.xdg_surfaces.get_mut(&top.xdg_surface)
                {
                    xdg.role = None;
                    // A toplevel that is destroyed and made again has to be
                    // configured again before it may attach a buffer.
                    xdg.configured = false;
                    xdg.sent_configure = false;
                    xdg.unacked.clear();
                }
            }
            Role::Buffer => {
                let _ = self.buffers.remove(&id);
                // A buffer a surface is showing that the client destroys
                // leaves the surface showing nothing, which is what
                // `wl_buffer`'s description says: "destroying the
                // wl_buffer... the surface contents become undefined". Every
                // compositor treats that as unmapped rather than as garbage.
                for surface in self.surfaces.values_mut() {
                    if surface.current.buffer == Some(id) {
                        surface.current.buffer = None;
                    }
                    if surface.pending.buffer == Some(id) {
                        surface.pending.buffer = None;
                    }
                }
            }
            _ => {}
        }
        // wl_display.delete_id is what lets a client reuse an id without
        // racing the server. libwayland sends it for every object the client
        // made, and only for those: a server-made object's id is the
        // server's to reuse.
        if id.is_client() {
            let _ = self.out.write(
                ObjectId::DISPLAY,
                wl_display::event::DELETE_ID,
                &[ArgType::Uint],
                &[Arg::Uint(id.0)],
            );
        }
        self.events.push(Event::Destroyed { object: id, role });
    }

    /// Answer one decoded request.
    fn dispatch(
        &mut self,
        sender: ObjectId,
        role: Role,
        version: u32,
        opcode: u16,
        args: &[Arg<'_>],
    ) {
        match role {
            Role::Display => self.display(opcode, args),
            Role::Registry => self.registry(sender, opcode, args),
            Role::Compositor => self.compositor(version, opcode, args),
            Role::Surface => self.surface_request(sender, opcode, args),
            Role::Region => self.region_request(sender, opcode, args),
            Role::Shm => self.shm(opcode, args),
            Role::ShmPool => self.shm_pool(sender, opcode, args),
            Role::XdgWmBase => self.xdg_wm_base(version, opcode, args),
            Role::XdgSurface => self.xdg_surface_request(sender, version, opcode, args),
            Role::XdgToplevel => self.xdg_toplevel_request(sender, opcode, args),
            // wl_buffer's only request is `destroy`, which the destructor
            // flag handles; the rest are globals whose roles land after
            // this. A bound object's requests are read, decoded and dropped
            // rather than refused, because refusing would be a protocol
            // error for a request the protocol allows.
            _ => {}
        }
    }

    /// `wl_display`: `sync` and `get_registry`.
    fn display(&mut self, opcode: u16, args: &[Arg<'_>]) {
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        match opcode {
            wl_display::request::SYNC => {
                // The callback is made, fired and destroyed in one go: its
                // data is undefined and the protocol says to ignore it, but
                // libwayland sends the serial, so this does too.
                if !self.make(id, &core::WL_CALLBACK, 1, Role::Callback) {
                    return;
                }
                let _ = self.out.write(
                    id,
                    core::wl_callback::event::DONE,
                    &[ArgType::Uint],
                    &[Arg::Uint(0)],
                );
                self.destroy(id, Role::Callback);
            }
            wl_display::request::GET_REGISTRY => {
                if !self.make(id, &core::WL_REGISTRY, 1, Role::Registry) {
                    return;
                }
                self.announce(id);
            }
            _ => {}
        }
    }

    /// `wl_registry`: `bind`.
    fn registry(&mut self, _sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != wl_registry::request::BIND {
            return;
        }
        let (
            Some(Arg::Uint(name)),
            Some(&Arg::AnyNewId {
                interface,
                version,
                id,
            }),
        ) = (args.first(), args.get(1))
        else {
            return;
        };
        // Copied out: `make` below takes the whole client, and a global is
        // three words.
        let global = self.globals.get(*name).copied();
        let Some(global) = global else {
            self.fail(Fatal::BadBind {
                name: *name,
                version,
            });
            return;
        };
        // libwayland refuses a version above what was advertised and a name
        // whose interface the client got wrong, both as `invalid_object`. The
        // interface check matters: a client binding `wl_shm`'s name while
        // saying `wl_seat` would otherwise get a `wl_shm` answering seat
        // requests.
        if version == 0 || version > global.version || interface != global.interface.name {
            self.fail(Fatal::BadBind {
                name: *name,
                version,
            });
            return;
        }
        if !self.make(id, global.interface, version, global.role) {
            return;
        }
        if global.role == Role::Shm {
            // libwayland's wl_shm sends its formats from the bind handler,
            // before the client has had a chance to ask, and every toolkit
            // gathers them during its first roundtrip.
            for format in FORMATS {
                let _ = self.out.write(
                    id,
                    wl_shm::event::FORMAT,
                    &[ArgType::Uint],
                    &[Arg::Uint(format.to_wl_shm())],
                );
            }
        }
        self.events.push(Event::Bound {
            object: id,
            role: global.role,
            version,
        });
    }

    /// `wl_compositor`: `create_surface` and `create_region`.
    fn compositor(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        match opcode {
            wl_compositor::request::CREATE_SURFACE => {
                // A surface inherits its `wl_compositor`'s version: the
                // protocol's rule for every object made by another, and the
                // reason a client bound at 4 is never sent
                // `preferred_buffer_scale`, which arrived in 6.
                if self.make(id, &core::WL_SURFACE, version, Role::Surface) {
                    let _ = self.surfaces.insert(id, Surface::new());
                }
            }
            // A wl_region is version 1 whatever its compositor was bound
            // at, because the interface has only ever had one.
            wl_compositor::request::CREATE_REGION
                if self.make(id, &core::WL_REGION, 1, Role::Region) =>
            {
                let _ = self.regions.insert(id, Region::new());
            }
            _ => {}
        }
    }

    /// `wl_surface`: everything a client says about what it is drawing.
    fn surface_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        match opcode {
            wl_surface::request::ATTACH => {
                let Some(buffer) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                if !buffer.is_null() && !self.buffers.contains_key(&buffer) {
                    self.fail(Fatal::WrongInterface {
                        object: buffer,
                        wanted: "wl_buffer",
                    });
                    return;
                }
                let (x, y) = (
                    args.get(1).and_then(Arg::as_int).unwrap_or(0),
                    args.get(2).and_then(Arg::as_int).unwrap_or(0),
                );
                let Some(surface) = self.surfaces.get_mut(&sender) else {
                    return;
                };
                surface.pending.buffer = (!buffer.is_null()).then_some(buffer);
                // Before version 5 `attach` carries the offset; from 5 it
                // must be zero and `offset` carries it. A client bound below
                // 5 that sends one is obeyed.
                if x != 0 || y != 0 {
                    surface.pending.offset = (x, y);
                }
            }
            wl_surface::request::DAMAGE | wl_surface::request::DAMAGE_BUFFER => {
                let rect = Rect::new(
                    args.first().and_then(Arg::as_int).unwrap_or(0),
                    args.get(1).and_then(Arg::as_int).unwrap_or(0),
                    args.get(2).and_then(Arg::as_int).unwrap_or(0),
                    args.get(3).and_then(Arg::as_int).unwrap_or(0),
                );
                let Some(surface) = self.surfaces.get_mut(&sender) else {
                    return;
                };
                if let Some(rect) = rect {
                    if opcode == wl_surface::request::DAMAGE {
                        surface.pending.damage.push(rect);
                    } else {
                        surface.pending.buffer_damage.push(rect);
                    }
                }
            }
            wl_surface::request::FRAME => {
                let Some(id) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                if !self.make(id, &core::WL_CALLBACK, 1, Role::FrameCallback) {
                    return;
                }
                if let Some(surface) = self.surfaces.get_mut(&sender) {
                    surface.frame_callbacks.push(id);
                }
            }
            wl_surface::request::SET_OPAQUE_REGION | wl_surface::request::SET_INPUT_REGION => {
                let Some(id) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                let region = if id.is_null() {
                    None
                } else {
                    match self.regions.get(&id) {
                        Some(region) => Some(region.clone()),
                        None => {
                            self.fail(Fatal::WrongInterface {
                                object: id,
                                wanted: "wl_region",
                            });
                            return;
                        }
                    }
                };
                let Some(surface) = self.surfaces.get_mut(&sender) else {
                    return;
                };
                if opcode == wl_surface::request::SET_OPAQUE_REGION {
                    surface.pending.opaque = region;
                } else {
                    surface.pending.input = region;
                }
            }
            wl_surface::request::COMMIT => {
                // A surface that has been given an `xdg_surface` may not
                // carry a buffer until it has acked a configure. That is the
                // rule that stops a client painting at a size the compositor
                // never agreed to: `xdg_surface`'s description has the
                // client commit once with nothing attached, take the
                // configure, ack it, and only then attach.
                let wants_buffer = self
                    .surfaces
                    .get(&sender)
                    .is_some_and(|state| state.pending.buffer.is_some());
                let unconfigured = self
                    .xdg_surfaces
                    .iter()
                    .find(|(_, xdg)| xdg.surface == sender)
                    .filter(|(_, xdg)| !xdg.configured)
                    .map(|(id, _)| *id);
                if let Some(xdg) = unconfigured
                    && wants_buffer
                {
                    self.fail(Fatal::Interface {
                        object: xdg,
                        code: xdg_surface::error::UNCONFIGURED_BUFFER,
                        text: "a buffer was attached before a configure was acked".to_owned(),
                    });
                    return;
                }
                let Some(surface) = self.surfaces.get_mut(&sender) else {
                    return;
                };
                let change = surface.commit();
                self.events.push(Event::SurfaceCommitted {
                    surface: sender,
                    change,
                });
            }
            wl_surface::request::SET_BUFFER_TRANSFORM => {
                let Some(transform) = args.first().and_then(Arg::as_int) else {
                    return;
                };
                // wl_surface.error.invalid_transform is the protocol's answer
                // to one that is not a wl_output.transform value.
                let Ok(transform) = u32::try_from(transform) else {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: wl_surface::error::INVALID_TRANSFORM,
                        text: "a buffer transform that is not one".to_owned(),
                    });
                    return;
                };
                if transform > core::wl_output::transform::FLIPPED_270 {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: wl_surface::error::INVALID_TRANSFORM,
                        text: format!("{transform} is not a wl_output transform"),
                    });
                    return;
                }
                if let Some(surface) = self.surfaces.get_mut(&sender) {
                    surface.pending.transform = transform;
                }
            }
            wl_surface::request::SET_BUFFER_SCALE => {
                let Some(scale) = args.first().and_then(Arg::as_int) else {
                    return;
                };
                if scale < 1 {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: wl_surface::error::INVALID_SCALE,
                        text: format!("a buffer scale of {scale}"),
                    });
                    return;
                }
                if let Some(surface) = self.surfaces.get_mut(&sender) {
                    surface.pending.scale = scale;
                }
            }
            wl_surface::request::OFFSET => {
                let (Some(x), Some(y)) = (
                    args.first().and_then(Arg::as_int),
                    args.get(1).and_then(Arg::as_int),
                ) else {
                    return;
                };
                if let Some(surface) = self.surfaces.get_mut(&sender) {
                    surface.pending.offset = (x, y);
                }
            }
            _ => {}
        }
    }

    /// `wl_region`: `add` and `subtract`.
    fn region_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        let rect = Rect::new(
            args.first().and_then(Arg::as_int).unwrap_or(0),
            args.get(1).and_then(Arg::as_int).unwrap_or(0),
            args.get(2).and_then(Arg::as_int).unwrap_or(0),
            args.get(3).and_then(Arg::as_int).unwrap_or(0),
        );
        let Some(region) = self.regions.get_mut(&sender) else {
            return;
        };
        let Some(rect) = rect else {
            return;
        };
        match opcode {
            wl_region::request::ADD => region.add(rect),
            wl_region::request::SUBTRACT => region.subtract(rect),
            _ => {}
        }
    }

    /// `wl_shm`: `create_pool`.
    fn shm(&mut self, opcode: u16, args: &[Arg<'_>]) {
        if opcode != wl_shm::request::CREATE_POOL {
            return;
        }
        let (Some(id), Some(fd), Some(size)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_fd),
            args.get(2).and_then(Arg::as_int),
        ) else {
            return;
        };
        if size <= 0 {
            // wl_shm has no error for it, and libwayland's mmap of a
            // zero-length pool fails, which it answers with invalid_fd.
            self.fail(Fatal::Interface {
                object: id,
                code: wl_shm::error::INVALID_FD,
                text: format!("a pool of {size} bytes"),
            });
            return;
        }
        if !self.make(id, &core::WL_SHM_POOL, 1, Role::ShmPool) {
            return;
        }
        let memory = Pool::new(fd, size);
        let _ = self.pools.insert(id, memory);
        self.events.push(Event::PoolCreated { pool: id, memory });
    }

    /// `wl_shm_pool`: `create_buffer` and `resize`.
    fn shm_pool(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        match opcode {
            wl_shm_pool::request::CREATE_BUFFER => {
                let Some(id) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                let numbers: Vec<i32> = (1..5)
                    .filter_map(|index| args.get(index).and_then(Arg::as_int))
                    .collect();
                let (Some(pool), [offset, width, height, stride], Some(format)) = (
                    self.pools.get(&sender),
                    numbers.as_slice(),
                    args.get(5).and_then(Arg::as_uint),
                ) else {
                    return;
                };
                match pool.buffer(sender, *offset, *width, *height, *stride, format) {
                    Ok(buffer) => {
                        if self.make(id, &core::WL_BUFFER, 1, Role::Buffer) {
                            let _ = self.buffers.insert(id, buffer);
                        }
                    }
                    Err(error) => self.fail(Fatal::Interface {
                        object: sender,
                        code: error.code(),
                        text: error.message(),
                    }),
                }
            }
            wl_shm_pool::request::RESIZE => {
                let Some(size) = args.first().and_then(Arg::as_int) else {
                    return;
                };
                let Some(pool) = self.pools.get_mut(&sender) else {
                    return;
                };
                if pool.resize(size) {
                    self.events.push(Event::PoolResized { pool: sender, size });
                }
            }
            _ => {}
        }
    }

    /// `xdg_wm_base`: `create_positioner`, `get_xdg_surface` and `pong`.
    fn xdg_wm_base(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        match opcode {
            xdg_wm_base::request::CREATE_POSITIONER => {
                let Some(id) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                // A positioner is a bag of numbers a popup reads. Popups are
                // not placed yet, so it is made and kept alive -- destroying
                // it would be a protocol error the client did not earn --
                // and nothing reads it.
                let _ = self.make(id, &xdg_shell::XDG_POSITIONER, version, Role::XdgPositioner);
            }
            xdg_wm_base::request::GET_XDG_SURFACE => {
                let (Some(id), Some(surface)) = (
                    args.first().and_then(Arg::as_object),
                    args.get(1).and_then(Arg::as_object),
                ) else {
                    return;
                };
                if !self.surfaces.contains_key(&surface) {
                    self.fail(Fatal::WrongInterface {
                        object: surface,
                        wanted: "wl_surface",
                    });
                    return;
                }
                // A surface that already has a buffer may not be given a
                // role: `xdg_surface`'s description says it must be unmapped
                // and have no buffer attached or committed.
                let has_buffer = self
                    .surfaces
                    .get(&surface)
                    .is_some_and(|state| state.is_mapped() || state.pending.buffer.is_some());
                if has_buffer {
                    self.fail(Fatal::Interface {
                        object: id,
                        code: xdg_surface::error::UNCONFIGURED_BUFFER,
                        text: "a surface with a buffer cannot be given a role".to_owned(),
                    });
                    return;
                }
                if self.xdg_surfaces.values().any(|xdg| xdg.surface == surface) {
                    self.fail(Fatal::Interface {
                        object: id,
                        code: xdg_wm_base::error::ROLE,
                        text: "that surface already has an xdg_surface".to_owned(),
                    });
                    return;
                }
                if self.make(id, &xdg_shell::XDG_SURFACE, version, Role::XdgSurface) {
                    let _ = self.xdg_surfaces.insert(id, XdgSurface::new(surface));
                }
            }
            // `pong` answers the `ping` that asks whether a client is still
            // there. Nothing pings yet, so an unsolicited pong is ignored
            // rather than refused: the protocol gives no error for one.
            xdg_wm_base::request::PONG => {}
            _ => {}
        }
    }

    /// `xdg_surface`: the role requests, the window geometry and the ack.
    fn xdg_surface_request(
        &mut self,
        sender: ObjectId,
        version: u32,
        opcode: u16,
        args: &[Arg<'_>],
    ) {
        match opcode {
            xdg_surface::request::GET_TOPLEVEL => {
                let Some(id) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                let Some(xdg) = self.xdg_surfaces.get(&sender) else {
                    return;
                };
                if xdg.role.is_some() {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_surface::error::ALREADY_CONSTRUCTED,
                        text: "this xdg_surface already has a role".to_owned(),
                    });
                    return;
                }
                let surface = xdg.surface;
                if !self.make(id, &xdg_shell::XDG_TOPLEVEL, version, Role::XdgToplevel) {
                    return;
                }
                if let Some(xdg) = self.xdg_surfaces.get_mut(&sender) {
                    xdg.role = Some(XdgRole::Toplevel(id));
                }
                let _ = self.toplevels.insert(
                    id,
                    Toplevel {
                        xdg_surface: sender,
                        surface,
                        ..Toplevel::default()
                    },
                );
                self.events.push(Event::ToplevelCreated {
                    toplevel: id,
                    surface,
                });
            }
            xdg_surface::request::GET_POPUP => {
                let Some(id) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                // Popups are not placed yet. The object is made so the
                // client's ids stay in step and it is told nothing, rather
                // than the connection being ended for using a protocol the
                // compositor advertised.
                let Some(xdg) = self.xdg_surfaces.get(&sender) else {
                    return;
                };
                if xdg.role.is_some() {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_surface::error::ALREADY_CONSTRUCTED,
                        text: "this xdg_surface already has a role".to_owned(),
                    });
                    return;
                }
                if self.make(id, &xdg_shell::XDG_POPUP, version, Role::XdgPopup)
                    && let Some(xdg) = self.xdg_surfaces.get_mut(&sender)
                {
                    xdg.role = Some(XdgRole::Popup(id));
                }
            }
            xdg_surface::request::SET_WINDOW_GEOMETRY => {
                let numbers: Vec<i32> = (0..4)
                    .filter_map(|index| args.get(index).and_then(Arg::as_int))
                    .collect();
                let [x, y, width, height] = numbers.as_slice() else {
                    return;
                };
                if *width <= 0 || *height <= 0 {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_surface::error::INVALID_SIZE,
                        text: format!("a window geometry of {width}x{height}"),
                    });
                    return;
                }
                if let Some(xdg) = self.xdg_surfaces.get_mut(&sender) {
                    xdg.geometry = Some((*x, *y, *width, *height));
                }
            }
            xdg_surface::request::ACK_CONFIGURE => {
                let Some(serial) = args.first().and_then(Arg::as_uint) else {
                    return;
                };
                let acked = self
                    .xdg_surfaces
                    .get_mut(&sender)
                    .is_some_and(|xdg| xdg.ack(serial));
                if !acked {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_surface::error::INVALID_SERIAL,
                        text: format!("serial {serial} was never sent or is already acked"),
                    });
                }
            }
            _ => {}
        }
    }

    /// `xdg_toplevel`: what a client says about its window.
    fn xdg_toplevel_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        match opcode {
            xdg_toplevel::request::SET_TITLE | xdg_toplevel::request::SET_APP_ID => {
                let text = args.first().and_then(Arg::as_str).unwrap_or("").to_owned();
                let Some(top) = self.toplevels.get_mut(&sender) else {
                    return;
                };
                if opcode == xdg_toplevel::request::SET_TITLE {
                    top.title = text;
                } else {
                    top.app_id = text;
                }
                self.events
                    .push(Event::ToplevelRenamed { toplevel: sender });
            }
            xdg_toplevel::request::SET_PARENT => {
                let Some(parent) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                if !parent.is_null() && !self.toplevels.contains_key(&parent) {
                    self.fail(Fatal::WrongInterface {
                        object: parent,
                        wanted: "xdg_toplevel",
                    });
                    return;
                }
                if let Some(top) = self.toplevels.get_mut(&sender) {
                    top.parent = (!parent.is_null()).then_some(parent);
                }
            }
            xdg_toplevel::request::SET_MAX_SIZE | xdg_toplevel::request::SET_MIN_SIZE => {
                let (Some(width), Some(height)) = (
                    args.first().and_then(Arg::as_int),
                    args.get(1).and_then(Arg::as_int),
                ) else {
                    return;
                };
                if width < 0 || height < 0 {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_toplevel::error::INVALID_SIZE,
                        text: format!("a size of {width}x{height}"),
                    });
                    return;
                }
                let Some(top) = self.toplevels.get_mut(&sender) else {
                    return;
                };
                if opcode == xdg_toplevel::request::SET_MAX_SIZE {
                    top.max_size = (width, height);
                } else {
                    top.min_size = (width, height);
                }
            }
            xdg_toplevel::request::SET_MAXIMIZED
            | xdg_toplevel::request::UNSET_MAXIMIZED
            | xdg_toplevel::request::SET_FULLSCREEN
            | xdg_toplevel::request::UNSET_FULLSCREEN => {
                let Some(top) = self.toplevels.get_mut(&sender) else {
                    return;
                };
                match opcode {
                    xdg_toplevel::request::SET_MAXIMIZED => top.maximized = true,
                    xdg_toplevel::request::UNSET_MAXIMIZED => top.maximized = false,
                    xdg_toplevel::request::SET_FULLSCREEN => top.fullscreen = true,
                    _ => top.fullscreen = false,
                }
                let (maximized, fullscreen) = (top.maximized, top.fullscreen);
                self.events.push(Event::ToplevelAsked {
                    toplevel: sender,
                    maximized,
                    fullscreen,
                });
            }
            // `show_window_menu`, `move`, `resize` and `set_minimized` ask
            // for things a tiling compositor does not do. The protocol says
            // a compositor may ignore each, and Hyprland ignores the first
            // three for a tiled window.
            _ => {}
        }
    }

    /// Announce every global to a fresh registry.
    fn announce(&mut self, registry: ObjectId) {
        for global in self.globals.all() {
            let _ = self.out.write(
                registry,
                wl_registry::event::GLOBAL,
                &[
                    ArgType::Uint,
                    ArgType::Str { nullable: false },
                    ArgType::Uint,
                ],
                &[
                    Arg::Uint(global.name),
                    Arg::Str(Some(global.interface.name)),
                    Arg::Uint(global.version),
                ],
            );
        }
    }

    /// Make the object a `new_id` argument named, or end the connection.
    ///
    /// `false` when the connection is now finished.
    fn make(
        &mut self,
        id: ObjectId,
        interface: &'static compositor_protocol::Interface,
        version: u32,
        role: Role,
    ) -> bool {
        match self.objects.insert(id, interface, version, role) {
            Ok(()) => true,
            Err(ObjectError::Version { .. }) => {
                // Asked for above what the interface offers, which `bind`
                // checks against the global first, so this is a request whose
                // protocol version is wrong rather than the client's choice.
                self.fail(Fatal::BadBind { name: 0, version });
                false
            }
            Err(_) => {
                self.fail(Fatal::BadNewId(id));
                false
            }
        }
    }
}

/// `wl_display.error`'s arguments: the object, the code and the sentence.
const fn error_signature() -> &'static [ArgType] {
    &[
        ArgType::Object { nullable: false },
        ArgType::Uint,
        ArgType::Str { nullable: false },
    ]
}
