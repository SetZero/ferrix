//! One client's connection: its objects, and what its requests do.

use std::collections::BTreeMap;

use compositor_protocol::core::{self, wl_display, wl_output, wl_registry, wl_seat, wl_shm};
use compositor_protocol::foreign_toplevel::{
    self, zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};
use compositor_protocol::screencopy::{self, zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1};
use compositor_protocol::session_lock::{
    self, ext_session_lock_manager_v1, ext_session_lock_surface_v1, ext_session_lock_v1,
};
use compositor_wire::{
    Arg, ArgType, Error as WireError, Fd, ObjectError, ObjectId, Objects, Reader, Writer,
};

mod buffers;
mod capture;
mod clipboard;
mod compositor;
mod control;
mod desktop;
mod drag;
mod event;
mod frames;
mod hypr;
mod input;
mod layer_shell;
mod outputs;
mod screen;
mod seat;
mod typing;
mod workspaces;
mod xdg_extras;
mod xdg_shell;

pub use capture::{Frame, Source};
pub use control::{Flavour, Manager};
pub use drag::Dragging;
pub use event::Event;
pub use frames::{Hotkey, Listener};
pub use hypr::{Export, Shortcut};
pub use input::{Constraint, Injected};
pub use outputs::Configuration;
pub use outputs::Wanted;
pub use screen::GAMMA_SIZE;
pub use typing::Typed;
pub use workspaces::{Workspace, WorkspaceRequest};

use crate::globals::Globals;
use crate::layer::LayerSurface;
use crate::role::Role;
use crate::shm::{Buffer, FORMATS, Format, Pool};
use crate::surface::{Output, Rect, Region, Subsurface, Surface};
use crate::xdg::{Popup, Positioner, Toplevel, XdgSurface};

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
    /// How many pools this connection has made: the last [`PoolKey`](crate::shm::PoolKey) given.
    pools_made: u64,
    buffers: BTreeMap<ObjectId, Buffer>,
    xdg_surfaces: BTreeMap<ObjectId, XdgSurface>,
    toplevels: BTreeMap<ObjectId, Toplevel>,
    subsurfaces: BTreeMap<ObjectId, Subsurface>,
    layers: BTreeMap<ObjectId, LayerSurface>,
    /// What the seat announces: `wl_seat.capability` bits.
    capabilities: u32,
    /// The keymap every `wl_keyboard` is sent, if there is one.
    keymap: Option<(Fd, u32)>,
    /// What every `wl_keyboard` of version 4 or above is told about repeat:
    /// keys a second, then the delay before the first repeat in
    /// milliseconds. Hyprland's `input:repeat_rate` and `input:repeat_delay`
    /// defaults.
    repeat: (i32, i32),
    /// The types each of this client's `wl_data_source`s has offered, in
    /// the order it offered them.
    sources: BTreeMap<ObjectId, Vec<String>>,
    /// The `wl_data_device`s it has made, which is where a selection is
    /// announced.
    devices: Vec<ObjectId>,
    /// The `wl_data_offer` this client was last given, if it still has one:
    /// a new selection replaces it, as the protocol says.
    offer: Option<ObjectId>,
    /// What each `wl_output` says its screen is, one to a monitor and in
    /// the order the globals advertise them.
    outputs: Vec<Output>,
    /// The monitor each bound `wl_output` object names, by its place in
    /// `outputs`: a layer surface and a `wl_surface.enter` both name a
    /// screen by its object.
    output_objects: BTreeMap<ObjectId, usize>,
    /// The `zwlr_foreign_toplevel_manager_v1`s this client has bound and not
    /// stopped, which is what makes it a bar.
    managers: Vec<ObjectId>,
    /// The handle this client holds for each window, and the window each
    /// handle names. A handle is the server's object, so the map is the
    /// only way back from a request to a window.
    handles: BTreeMap<u64, ObjectId>,
    /// What each of those handles was last told, so that nothing is sent
    /// twice and `done` is sent only when something changed.
    told: BTreeMap<u64, ForeignToplevel>,
    /// Each screenshot being taken: which screen, and whether the buffer
    /// has been handed over already. A frame may be copied into once, which
    /// is `zwlr_screencopy_frame_v1`'s `already_used`.
    frames: BTreeMap<ObjectId, Capture>,
    /// The `zwp_text_input_v3`s it has made: an application's text fields.
    text_inputs: Vec<ObjectId>,
    /// The `zwp_input_method_v2` it holds, if it is the input method.
    input_method: Option<ObjectId>,
    /// What the input method has staged and not yet committed.
    typed: Typed,
    /// The `zwp_primary_selection_source_v1`s it has made, and the types
    /// each offered.
    primary_sources: BTreeMap<ObjectId, Vec<String>>,
    /// The `zwp_primary_selection_device_v1`s it has made.
    primary_devices: Vec<ObjectId>,
    /// The primary offer it was last given, if it still has one.
    primary_offer: Option<ObjectId>,
    /// The activation tokens this client was given and has not used.
    tokens: std::collections::BTreeSet<String>,
    /// How many tokens it has been given, which makes each one different.
    token: u32,
    /// Which surface each `wp_viewport` is on.
    viewports: BTreeMap<ObjectId, ObjectId>,
    /// Which surface each `wp_fractional_scale_v1` is on.
    fractional: BTreeMap<ObjectId, ObjectId>,
    /// What each `xdg_toplevel_icon_v1` is called.
    icons: BTreeMap<ObjectId, String>,
    /// What the client last asked the pointer to look like: the surface and
    /// the hotspot, or `None` for a pointer it asked to be hidden.
    cursor: Option<(ObjectId, (i32, i32))>,
    /// Whether it has ever asked, which is not the same as having asked for
    /// nothing.
    said_cursor: bool,
    /// Each `xdg_positioner` this client made, and what it has set on it.
    positioners: BTreeMap<ObjectId, Positioner>,
    /// Each `xdg_popup` it has made.
    popups: BTreeMap<ObjectId, Popup>,
    /// The `ext_session_lock_v1` this client holds, if it locked the
    /// session, and the lock surfaces it has made, by screen.
    lock: Option<ObjectId>,
    lock_surfaces: BTreeMap<ObjectId, usize>,
    /// Which screen each `zxdg_output_v1` names, by its place in
    /// `outputs`.
    xdg_outputs: BTreeMap<ObjectId, usize>,
    /// The `wp_presentation_feedback`s owed for each surface's next frame.
    feedback: BTreeMap<ObjectId, Vec<ObjectId>>,
    /// Each `ext_idle_notification_v1` this client is waiting on.
    idles: BTreeMap<ObjectId, desktop::Idle>,
    /// The surface each `zwp_idle_inhibitor_v1` holds idling off for.
    inhibitors: BTreeMap<ObjectId, ObjectId>,
    /// Which surface each `wp_content_type_v1` is on.
    contents: BTreeMap<ObjectId, ObjectId>,
    /// Which surface each `wp_alpha_modifier_surface_v1` is on.
    alphas: BTreeMap<ObjectId, ObjectId>,
    /// Which toplevel each `xdg_dialog_v1` is on.
    dialogs: BTreeMap<ObjectId, ObjectId>,
    /// Which surface each `org_kde_kwin_server_decoration` is on.
    kde_decorations: BTreeMap<ObjectId, ObjectId>,
    /// The `zwp_relative_pointer_v1`s this client has made.
    relative_pointers: Vec<ObjectId>,
    /// The pointer constraints it holds, by object.
    constraints: BTreeMap<ObjectId, Constraint>,
    /// The surface each `zwp_keyboard_shortcuts_inhibitor_v1` covers.
    shortcut_inhibitors: BTreeMap<ObjectId, ObjectId>,
    /// The `zwp_virtual_keyboard_v1`s it has made.
    virtual_keyboards: Vec<ObjectId>,
    /// The `zwlr_virtual_pointer_v1`s it has made.
    virtual_pointers: Vec<ObjectId>,
    /// The `ext_foreign_toplevel_list_v1`s it has bound and not stopped.
    lists: Vec<ObjectId>,
    /// The handle it holds for each window in that list.
    list_handles: BTreeMap<u64, ObjectId>,
    /// What each of those handles was last told.
    list_told: BTreeMap<u64, ForeignToplevel>,
    /// Which screen each `zwlr_gamma_control_v1` is on.
    gammas: BTreeMap<ObjectId, usize>,
    /// Which screen each `zwlr_output_power_v1` is on.
    powers: BTreeMap<ObjectId, usize>,
    /// The data-control devices it has made, with the offers each holds.
    control_devices: BTreeMap<ObjectId, Manager>,
    /// The types each data-control source has offered.
    control_sources: BTreeMap<ObjectId, Vec<String>>,
    /// The `zwlr_output_manager_v1`s it has bound and not stopped.
    output_managers: Vec<ObjectId>,
    /// The head it holds for each screen.
    heads: BTreeMap<usize, ObjectId>,
    /// The arrangements it is building.
    configurations: BTreeMap<ObjectId, Configuration>,
    /// Which configuration and screen each configuration head is for.
    configuration_heads: BTreeMap<ObjectId, (ObjectId, usize)>,
    /// The serial the screens were last published with. A configuration
    /// made against an older one is refused, which is this protocol's one
    /// safety rule.
    output_serial: u32,
    /// The `ext_workspace_manager_v1`s it has bound and not stopped.
    workspace_managers: Vec<ObjectId>,
    /// The group it holds for each monitor.
    workspace_groups: BTreeMap<usize, ObjectId>,
    /// The handle it holds for each workspace.
    workspace_handles: BTreeMap<i64, ObjectId>,
    /// What each of those was last told.
    workspace_told: BTreeMap<i64, Workspace>,
    /// The global shortcuts this client has registered.
    shortcuts: BTreeMap<ObjectId, Shortcut>,
    /// The focus grabs it holds, and what each covers.
    grabs: BTreeMap<ObjectId, Vec<ObjectId>>,
    /// The `hyprland_lock_notification_v1`s it is waiting on.
    lock_notifications: Vec<ObjectId>,
    /// Which surface each `hyprland_surface_v1` is on.
    hyprland_surfaces: BTreeMap<ObjectId, ObjectId>,
    /// The window captures being taken.
    exports: BTreeMap<ObjectId, Export>,
    /// What each `wl_data_source` of this client said it can do in a drag.
    source_actions: BTreeMap<ObjectId, u32>,
    /// The drag this client is under, while one is over it.
    dragging: Dragging,
    /// Which surface each of the four per-surface presentation objects is
    /// for: a background effect, a tearing control, a fifo and a timer.
    for_surfaces: BTreeMap<ObjectId, ObjectId>,
    /// The listening sockets sandboxes handed over.
    contexts: BTreeMap<ObjectId, Listener>,
    /// What each of those sandboxes calls itself: the engine, the
    /// application and the instance.
    sandboxes: BTreeMap<ObjectId, (String, String, String)>,
    /// The keys a launcher asked for by keysym.
    hotkeys: BTreeMap<ObjectId, Hotkey>,
    /// What each `ext_image_capture_source_v1` is looking at.
    capture_sources: BTreeMap<ObjectId, Source>,
    /// What each capture session is looking at.
    sessions: BTreeMap<ObjectId, Source>,
    /// Each frame being copied out of one.
    frames_taken: BTreeMap<ObjectId, Frame>,
    /// The next configure serial. Serials go up and are never reused, so a
    /// client's `ack_configure` names one configure and no other.
    serial: u32,
}

/// One screenshot being taken.
#[derive(Clone, Copy, Debug)]
struct Capture {
    /// Which screen, by its place in the outputs.
    output: usize,
    /// The part of it, or `None` for all of it.
    region: Option<Rect>,
    /// Whether the buffer has been handed over.
    used: bool,
}

/// The version of `zwlr_foreign_toplevel_handle_v1` a handle is made at.
///
/// A handle is the server's object, so its version is not inherited from a
/// request the way every client-made object's is: the compositor picks it,
/// and it is the manager's, which is what wlroots does.
const FOREIGN_TOPLEVEL_VERSION: u32 = 3;

/// What a bar asked the compositor to do to somebody else's window.
///
/// `set_rectangle` is left out: it says where the window's icon is on the
/// bar, for a minimise animation to fly to, and this compositor has neither.
/// So is `set_minimized`, which Hyprland answers by moving the window to the
/// special workspace; that is `movetoworkspacesilent special:minimized` and
/// is the compositor's to decide, so it comes through as the request and the
/// compositor chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignRequest {
    /// `activate`: focus it.
    Activate,
    /// `close`: ask it to close, as `killactive` does.
    Close,
    /// `set_fullscreen` or `unset_fullscreen`.
    Fullscreen(bool),
    /// `set_maximized` or `unset_maximized`.
    Maximized(bool),
    /// `set_minimized` or `unset_minimized`.
    Minimized(bool),
}

/// What one window looks like to a bar.
///
/// The four states `zwlr_foreign_toplevel_handle_v1.state` has, and the two
/// names every taskbar draws.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignToplevel {
    /// The window, as the compositor numbered it.
    pub window: u64,
    /// Its title.
    pub title: String,
    /// Its application id.
    pub app_id: String,
    /// Whether it is the focused window.
    pub activated: bool,
    /// Whether it is fullscreen.
    pub fullscreen: bool,
    /// Whether it is maximized.
    pub maximized: bool,
    /// Whether it is minimized, which here means on a workspace nothing
    /// shows.
    pub minimized: bool,
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
            pools_made: 0,
            buffers: BTreeMap::new(),
            xdg_surfaces: BTreeMap::new(),
            toplevels: BTreeMap::new(),
            subsurfaces: BTreeMap::new(),
            layers: BTreeMap::new(),
            capabilities: 0,
            keymap: None,
            repeat: (25, 600),
            sources: BTreeMap::new(),
            devices: Vec::new(),
            offer: None,
            outputs: vec![Output::default()],
            output_objects: BTreeMap::new(),
            managers: Vec::new(),
            handles: BTreeMap::new(),
            told: BTreeMap::new(),
            frames: BTreeMap::new(),
            text_inputs: Vec::new(),
            input_method: None,
            typed: Typed::default(),
            primary_sources: BTreeMap::new(),
            primary_devices: Vec::new(),
            primary_offer: None,
            tokens: std::collections::BTreeSet::new(),
            token: 0,
            viewports: BTreeMap::new(),
            fractional: BTreeMap::new(),
            icons: BTreeMap::new(),
            cursor: None,
            said_cursor: false,
            positioners: BTreeMap::new(),
            popups: BTreeMap::new(),
            lock: None,
            lock_surfaces: BTreeMap::new(),
            xdg_outputs: BTreeMap::new(),
            feedback: BTreeMap::new(),
            idles: BTreeMap::new(),
            inhibitors: BTreeMap::new(),
            contents: BTreeMap::new(),
            alphas: BTreeMap::new(),
            dialogs: BTreeMap::new(),
            kde_decorations: BTreeMap::new(),
            relative_pointers: Vec::new(),
            constraints: BTreeMap::new(),
            shortcut_inhibitors: BTreeMap::new(),
            virtual_keyboards: Vec::new(),
            virtual_pointers: Vec::new(),
            lists: Vec::new(),
            list_handles: BTreeMap::new(),
            list_told: BTreeMap::new(),
            gammas: BTreeMap::new(),
            powers: BTreeMap::new(),
            control_devices: BTreeMap::new(),
            control_sources: BTreeMap::new(),
            output_managers: Vec::new(),
            heads: BTreeMap::new(),
            configurations: BTreeMap::new(),
            configuration_heads: BTreeMap::new(),
            output_serial: 0,
            workspace_managers: Vec::new(),
            workspace_groups: BTreeMap::new(),
            workspace_handles: BTreeMap::new(),
            workspace_told: BTreeMap::new(),
            shortcuts: BTreeMap::new(),
            grabs: BTreeMap::new(),
            lock_notifications: Vec::new(),
            hyprland_surfaces: BTreeMap::new(),
            exports: BTreeMap::new(),
            source_actions: BTreeMap::new(),
            dragging: Dragging::default(),
            for_surfaces: BTreeMap::new(),
            contexts: BTreeMap::new(),
            sandboxes: BTreeMap::new(),
            hotkeys: BTreeMap::new(),
            capture_sources: BTreeMap::new(),
            sessions: BTreeMap::new(),
            frames_taken: BTreeMap::new(),
            serial: 1,
        }
    }

    /// Say what the screen is, before any client binds `wl_output`.
    pub fn set_output(&mut self, output: Output) {
        self.outputs = vec![output];
    }

    /// Say what every screen is, in the order the `wl_output` globals were
    /// added: the first global describes the first monitor and so on, which
    /// is how a client tells two screens apart.
    pub fn set_outputs(&mut self, outputs: Vec<Output>) {
        if !outputs.is_empty() {
            self.outputs = outputs;
        }
    }

    /// The monitor a bound `wl_output` object names, by its place in the
    /// list [`Client::set_outputs`] was given.
    #[must_use]
    pub fn output_of(&self, object: ObjectId) -> Option<usize> {
        self.output_objects.get(&object).copied()
    }

    /// The next serial, which is also this connection's serial for input
    /// events. Wayland has one serial space per connection.
    pub fn next_serial(&mut self) -> u32 {
        let serial = self.serial;
        self.serial = self.serial.wrapping_add(1).max(1);
        serial
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
                // A layer surface whose `wl_surface` went has nothing to
                // show; the protocol calls destroying them in that order
                // undefined, and dropping the record is the reading that
                // leaves nothing pointing at a surface that is gone.
                self.layers.retain(|_, layer| layer.surface != id);
            }
            Role::Region => {
                let _ = self.regions.remove(&id);
            }
            Role::Subsurface => {
                // The surface keeps its buffer and loses its place: the
                // protocol's "the wl_surface is unmapped".
                let _ = self.subsurfaces.remove(&id);
            }
            Role::ShmPool => {
                // The protocol keeps the pool's memory alive while buffers
                // cut from it live: "the mmapped memory will be released
                // when all buffers that have been created from this pool are
                // gone". So the object goes and the memory does not, and the
                // compositor above unmaps it when the last buffer does --
                // told by the pool's key, since the id may be the client's
                // next object's before then.
                if let Some(pool) = self.pools.remove(&id) {
                    self.events.push(Event::PoolRetired { pool: pool.key });
                }
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
            Role::LayerSurface => {
                // The surface keeps its buffer and loses its place, as a
                // subsurface does: the protocol's "the wl_surface is
                // unmapped". A client that destroys the layer surface and
                // makes another gets a fresh configure conversation.
                let _ = self.layers.remove(&id);
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
            other => {
                self.forget_desktop(id, other);
                self.forget_input(id, other);
                self.forget_drag(id, other);
                self.forget_screen(id, other);
                self.forget_control(id, other);
                self.forget_outputs(id, other);
                self.forget_workspaces(id, other);
                self.forget_hypr(id, other);
                self.forget_frames(id, other);
                self.forget_capture(id, other);
            }
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
            Role::Subcompositor => self.subcompositor(opcode, args),
            Role::Subsurface => self.subsurface_request(sender, opcode, args),
            Role::Seat => self.seat(version, opcode, args),
            Role::DataDeviceManager => self.data_device_manager(version, opcode, args),
            Role::DataSource => self.data_source_request(sender, opcode, args),
            Role::DataDevice => self.data_device_request(opcode, args),
            Role::DataOffer => self.data_offer_request(sender, opcode, args),
            Role::XdgWmBase => self.xdg_wm_base(version, opcode, args),
            Role::XdgSurface => self.xdg_surface_request(sender, version, opcode, args),
            Role::XdgToplevel => self.xdg_toplevel_request(sender, opcode, args),
            Role::DecorationManager => self.decoration_manager(version, opcode, args),
            Role::ToplevelDecoration => self.toplevel_decoration(sender, opcode, args),
            Role::Pointer => self.pointer_request(opcode, args),
            Role::CursorShapeManager => self.cursor_shape_manager(version, opcode, args),
            Role::CursorShapeDevice => self.cursor_shape_device(opcode, args),
            Role::PrimaryManager => self.primary_manager(version, opcode, args),
            Role::PrimaryDevice => self.primary_device(opcode, args),
            Role::PrimarySource => self.primary_source(sender, opcode, args),
            Role::PrimaryOffer => self.primary_offer(sender, opcode, args),
            Role::Activation => self.activation(version, opcode, args),
            Role::ActivationToken => self.activation_token(sender, opcode, args),
            Role::Viewporter => self.viewporter(version, opcode, args),
            Role::Viewport => self.viewport(sender, opcode, args),
            Role::FractionalScaleManager => self.fractional_manager(version, opcode, args),
            Role::IconManager => self.icon_manager(version, opcode, args),
            Role::Icon => self.icon(sender, opcode, args),
            Role::TextInputManager => self.text_input_manager(version, opcode, args),
            Role::TextInput => self.text_input(sender, opcode, args),
            Role::InputMethodManager => self.input_method_manager(version, opcode, args),
            Role::InputMethod => self.input_method(sender, opcode, args),
            Role::XdgPositioner => self.positioner(sender, opcode, args),
            Role::XdgPopup => self.popup_request(sender, opcode, args),
            Role::SessionLockManager => self.lock_manager(version, opcode, args),
            Role::SessionLock => self.session_lock(sender, version, opcode, args),
            Role::SessionLockSurface => self.lock_surface(sender, opcode, args),
            Role::ScreencopyManager => self.screencopy_manager(version, opcode, args),
            Role::ScreencopyFrame => self.screencopy_frame(sender, opcode, args),
            Role::ForeignToplevelManager => self.toplevel_manager(sender, opcode),
            Role::ForeignToplevel => self.toplevel_handle(sender, opcode),
            Role::LayerShell => self.layer_shell(version, opcode, args),
            Role::LayerSurface => self.layer_surface_request(sender, opcode, args),
            // The protocols a desktop session asks for beyond a window and
            // a bar, which are their own module.
            //
            // What is left after that is `wl_buffer`, whose only request is
            // `destroy` and whose destructor flag handles it, and the
            // globals whose roles have no requests. A bound object's
            // requests are read, decoded and dropped rather than refused,
            // because refusing would be a protocol error for a request the
            // protocol allows.
            other => {
                let _ = self.desktop(sender, other, version, opcode, args)
                    || self.input(sender, other, version, opcode, args)
                    || self.screen(sender, other, version, opcode, args)
                    || self.control(sender, other, version, opcode, args)
                    || self.outputs(sender, other, version, opcode, args)
                    || self.workspaces(sender, other, opcode)
                    || self.hypr(sender, other, version, opcode, args)
                    || self.frames(sender, other, version, opcode, args)
                    || self.capture(sender, other, version, opcode, args);
            }
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
        if global.role == Role::Seat {
            // libwayland's own seats send these from the bind handler, and
            // every toolkit gathers them during its first roundtrip.
            let _ = self.out.write(
                id,
                wl_seat::event::CAPABILITIES,
                &[ArgType::Uint],
                &[Arg::Uint(self.capabilities)],
            );
            if version >= 2 {
                let _ = self.out.write(
                    id,
                    wl_seat::event::NAME,
                    &[ArgType::Str { nullable: false }],
                    &[Arg::Str(Some("seat0"))],
                );
            }
        }
        if global.role == Role::Output {
            // Which screen this global is: the outputs are advertised in the
            // order `set_outputs` was given them.
            let which = self
                .globals
                .all()
                .iter()
                .filter(|other| other.role == Role::Output)
                .position(|other| other.name == *name)
                .unwrap_or(0);
            let _ = self.output_objects.insert(id, which);
            self.describe_output(id, version, which);
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
        if global.role == Role::ForeignToplevelManager {
            // A bar that has just bound is owed a handle for every window
            // that already exists. Nothing is pushed for it: the compositor
            // hands the whole list to every watching client each pass, and
            // `show_toplevels` works out that this one has been told
            // nothing yet.
            self.managers.push(id);
        }
        // The three lists the server itself fills, each owed everything
        // that already exists the moment it is bound.
        match global.role {
            // A client that was told nothing must assume the compositor can
            // do no background effect at all, so the capabilities go out at
            // bind.
            Role::BackgroundEffectManager => self.background_capabilities(id),
            Role::ForeignList => self.lists.push(id),
            Role::WorkspaceManager => self.workspace_managers.push(id),
            Role::OutputManager => {
                self.output_managers.push(id);
                // `zwlr_output_manager_v1` has no roundtrip of its own: a
                // manager that binds and hears nothing waits for ever, so
                // the screens go out at once. The compositor publishes
                // them again whenever they change.
                let outputs = self.outputs.clone();
                self.publish_outputs(&outputs);
            }
            _ => {}
        }
        self.events.push(Event::Bound {
            object: id,
            role: global.role,
            version,
        });
    }

    /// `ext_session_lock_manager_v1`: `lock`.
    ///
    /// The lock object is made at once and the *compositor* decides when to
    /// send `locked`: the protocol says that event means every screen is
    /// covered by a lock surface the client has drawn, and nothing but the
    /// compositor knows when that is true. Until then the client must
    /// assume the screen still shows what it did.
    fn lock_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != ext_session_lock_manager_v1::request::LOCK {
            return;
        }
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        if !self.make(
            id,
            &session_lock::EXT_SESSION_LOCK_V1,
            version,
            Role::SessionLock,
        ) {
            return;
        }
        self.lock = Some(id);
        self.events.push(Event::SessionLocked { lock: id });
    }

    /// `ext_session_lock_v1`: `get_lock_surface`, `unlock_and_destroy` and
    /// `destroy`.
    ///
    /// `destroy` on a lock that was never unlocked is `invalid_destroy`, and
    /// `unlock_and_destroy` on one that was never told it was locked is
    /// `invalid_unlock`. Both are protocol errors because both leave a
    /// screen nobody is drawing: the client believes it is done and the
    /// compositor believes the screen is covered.
    fn session_lock(&mut self, sender: ObjectId, version: u32, opcode: u16, args: &[Arg<'_>]) {
        use ext_session_lock_v1::request;
        match opcode {
            request::GET_LOCK_SURFACE => {
                let (Some(id), Some(surface), Some(output)) = (
                    args.first().and_then(Arg::as_object),
                    args.get(1).and_then(Arg::as_object),
                    args.get(2).and_then(Arg::as_object),
                ) else {
                    return;
                };
                let Some(which) = self.output_objects.get(&output).copied() else {
                    self.fail(Fatal::WrongInterface {
                        object: output,
                        wanted: "wl_output",
                    });
                    return;
                };
                if self.lock_surfaces.values().any(|held| *held == which) {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: ext_session_lock_v1::error::DUPLICATE_OUTPUT,
                        text: "that screen already has a lock surface".to_owned(),
                    });
                    return;
                }
                if !self.surfaces.contains_key(&surface) {
                    self.fail(Fatal::WrongInterface {
                        object: surface,
                        wanted: "wl_surface",
                    });
                    return;
                }
                if !self.make(
                    id,
                    &session_lock::EXT_SESSION_LOCK_SURFACE_V1,
                    version,
                    Role::SessionLockSurface,
                ) {
                    return;
                }
                let _ = self.lock_surfaces.insert(id, which);
                self.events.push(Event::SessionLockSurfaceMade {
                    lock_surface: id,
                    surface,
                    output: which,
                });
            }
            request::UNLOCK_AND_DESTROY => {
                self.lock = None;
                self.lock_surfaces.clear();
                self.events.push(Event::SessionUnlocked { asked: true });
            }
            // Destroying a lock that is still held is the error the
            // protocol names, because it would leave the screen locked with
            // nothing to draw on it and no way back.
            request::DESTROY if self.lock == Some(sender) => {
                self.fail(Fatal::Interface {
                    object: sender,
                    code: ext_session_lock_v1::error::INVALID_DESTROY,
                    text: "the lock was destroyed without being unlocked".to_owned(),
                });
            }
            _ => {}
        }
    }

    /// `ext_session_lock_surface_v1`: `ack_configure` and `destroy`.
    fn lock_surface(&mut self, sender: ObjectId, opcode: u16, _args: &[Arg<'_>]) {
        if opcode == ext_session_lock_surface_v1::request::DESTROY {
            let _ = self.lock_surfaces.remove(&sender);
        }
    }

    /// Tell a lock surface how large the screen it covers is.
    ///
    /// The client may not commit a buffer before it has acknowledged one of
    /// these, and the buffer it commits must be exactly this size.
    pub fn configure_lock_surface(&mut self, lock_surface: ObjectId, size: (u32, u32)) {
        let serial = self.serial;
        self.serial = self.serial.wrapping_add(1);
        let (width, height) = size;
        let _ = self.out.write(
            lock_surface,
            ext_session_lock_surface_v1::event::CONFIGURE,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[Arg::Uint(serial), Arg::Uint(width), Arg::Uint(height)],
        );
    }

    /// Tell the client the screen is covered by what it drew.
    ///
    /// Sent when every screen has a lock surface with a buffer on it, which
    /// is the protocol's own condition and the compositor's to judge.
    pub fn session_is_locked(&mut self) {
        let Some(lock) = self.lock else {
            return;
        };
        let _ = self
            .out
            .write(lock, ext_session_lock_v1::event::LOCKED, &[], &[]);
    }

    /// Tell the client it will never be told the screen is covered.
    ///
    /// `finished` is what a compositor sends when it refuses the lock -- a
    /// second program asking while one is held -- and the client is then to
    /// destroy the object and stop.
    pub fn session_lock_refused(&mut self, lock: ObjectId) {
        let _ = self
            .out
            .write(lock, ext_session_lock_v1::event::FINISHED, &[], &[]);
    }

    /// Whether this client holds the lock.
    #[must_use]
    pub const fn holds_lock(&self) -> bool {
        self.lock.is_some()
    }

    /// The lock surface for `output`, if this client has made one.
    #[must_use]
    pub fn lock_surface_on(&self, output: usize) -> Option<ObjectId> {
        self.lock_surfaces
            .iter()
            .find(|(_, which)| **which == output)
            .map(|(id, _)| *id)
    }

    /// `zwlr_screencopy_manager_v1`: `capture_output` and
    /// `capture_output_region`.
    ///
    /// The frame object is the client's id, made here; which screen it names
    /// is worked out from the `wl_output` it was given, since a screenshot
    /// program binds every output and asks for the one it wants. The size
    /// and format it must make a buffer of are the compositor's to say, so
    /// this only records the request and reports it.
    ///
    /// `overlay_cursor` is read and ignored: there is no cursor drawn into
    /// the frame to leave out.
    fn screencopy_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let region = match opcode {
            zwlr_screencopy_manager_v1::request::CAPTURE_OUTPUT => None,
            zwlr_screencopy_manager_v1::request::CAPTURE_OUTPUT_REGION => {
                let value = |at: usize| args.get(at).and_then(Arg::as_int).unwrap_or(0);
                Some(Rect {
                    x: value(3),
                    y: value(4),
                    width: value(5),
                    height: value(6),
                })
            }
            _ => return,
        };
        let (Some(frame), Some(output)) = (
            args.first().and_then(Arg::as_object),
            args.get(2).and_then(Arg::as_object),
        ) else {
            return;
        };
        let Some(which) = self.output_objects.get(&output).copied() else {
            // A `wl_output` this client never bound: the frame is made and
            // failed at once, which is what the protocol has for a capture
            // that cannot be done.
            if self.make(
                frame,
                &screencopy::ZWLR_SCREENCOPY_FRAME_V1,
                version,
                Role::ScreencopyFrame,
            ) {
                self.screencopy_failed(frame);
            }
            return;
        };
        if !self.make(
            frame,
            &screencopy::ZWLR_SCREENCOPY_FRAME_V1,
            version,
            Role::ScreencopyFrame,
        ) {
            return;
        }
        let _ = self.frames.insert(
            frame,
            Capture {
                output: which,
                region,
                used: false,
            },
        );
        self.events.push(Event::ScreencopyWanted {
            frame,
            output: which,
            region,
        });
    }

    /// `zwlr_screencopy_frame_v1`: `copy`, `copy_with_damage` and `destroy`.
    ///
    /// A frame may be copied into once. A second `copy` is
    /// `already_used`, which is a protocol error and so the end of the
    /// connection: the client has lost track of an object it owns.
    fn screencopy_frame(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        use zwlr_screencopy_frame_v1::request;
        if opcode == request::DESTROY {
            let _ = self.frames.remove(&sender);
            return;
        }
        let with_damage = match opcode {
            request::COPY => false,
            request::COPY_WITH_DAMAGE => true,
            _ => return,
        };
        let Some(buffer) = args.first().and_then(Arg::as_object) else {
            return;
        };
        let Some(capture) = self.frames.get_mut(&sender) else {
            return;
        };
        if capture.used {
            self.fail(Fatal::Interface {
                object: sender,
                code: zwlr_screencopy_frame_v1::error::ALREADY_USED,
                text: "the frame has already been used to copy".to_owned(),
            });
            return;
        }
        capture.used = true;
        let (output, region) = (capture.output, capture.region);
        self.events.push(Event::ScreencopyInto {
            frame: sender,
            buffer,
            output,
            region,
            with_damage,
        });
    }

    /// `zwlr_foreign_toplevel_manager_v1`: `stop`.
    ///
    /// `stop` is a farewell, not a destroy: the protocol has the compositor
    /// answer with `finished`, after which neither side uses the manager
    /// again. The handles it made stay valid until each is destroyed, which
    /// is why they are not taken away here.
    fn toplevel_manager(&mut self, sender: ObjectId, opcode: u16) {
        if opcode != zwlr_foreign_toplevel_manager_v1::request::STOP {
            return;
        }
        if let Some(at) = self.managers.iter().position(|held| *held == sender) {
            let _ = self.managers.remove(at);
        }
        let _ = self.out.write(
            sender,
            zwlr_foreign_toplevel_manager_v1::event::FINISHED,
            &[],
            &[],
        );
    }

    /// `zwlr_foreign_toplevel_handle_v1`: what a bar does with a window.
    ///
    /// Every one of them is the compositor's to carry out, so each becomes
    /// an event. `set_rectangle` is accepted and dropped: it says where the
    /// window's icon is on the bar so a minimise can animate towards it, and
    /// there is no such animation here.
    fn toplevel_handle(&mut self, sender: ObjectId, opcode: u16) {
        use zwlr_foreign_toplevel_handle_v1::request;
        if opcode == request::DESTROY {
            self.forget_handle(sender);
            return;
        }
        let what = match opcode {
            request::ACTIVATE => ForeignRequest::Activate,
            request::CLOSE => ForeignRequest::Close,
            request::SET_FULLSCREEN => ForeignRequest::Fullscreen(true),
            request::UNSET_FULLSCREEN => ForeignRequest::Fullscreen(false),
            request::SET_MAXIMIZED => ForeignRequest::Maximized(true),
            request::UNSET_MAXIMIZED => ForeignRequest::Maximized(false),
            request::SET_MINIMIZED => ForeignRequest::Minimized(true),
            request::UNSET_MINIMIZED => ForeignRequest::Minimized(false),
            _ => return,
        };
        let window = self
            .handles
            .iter()
            .find(|(_, handle)| **handle == sender)
            .map(|(window, _)| *window);
        if let Some(window) = window {
            self.events
                .push(Event::ForeignToplevelAsked { window, what });
        }
    }

    /// Forget the handle `id`, whichever window it named.
    fn forget_handle(&mut self, id: ObjectId) {
        let window = self
            .handles
            .iter()
            .find(|(_, handle)| **handle == id)
            .map(|(window, _)| *window);
        if let Some(window) = window {
            let _ = self.handles.remove(&window);
            let _ = self.told.remove(&window);
        }
    }

    /// Tell a fresh `wl_output` what the screen is.
    ///
    /// Every client reads these: a toolkit with no mode has no size to scale
    /// against, and foot reports `(null): 0x0+0x0@0Hz` for an output that
    /// sent none. The `done` at the end is what says the description is
    /// whole, and a client waits for it.
    fn describe_output(&mut self, id: ObjectId, version: u32, which: usize) {
        let Some(mode) = self.outputs.get(which).cloned() else {
            return;
        };
        let _ = self.out.write(
            id,
            wl_output::event::GEOMETRY,
            &[
                ArgType::Int,
                ArgType::Int,
                ArgType::Int,
                ArgType::Int,
                ArgType::Int,
                ArgType::Str { nullable: false },
                ArgType::Str { nullable: false },
                ArgType::Int,
            ],
            &[
                Arg::Int(mode.x),
                Arg::Int(mode.y),
                // A size in millimetres. Nothing here has a physical screen,
                // and a zero is what every headless compositor sends: a
                // client reads it as "unknown" and uses the scale instead.
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(wl_output::subpixel::UNKNOWN.cast_signed()),
                Arg::Str(Some("Ferrix")),
                Arg::Str(Some("hyprix")),
                // How the monitor is turned, which Hyprland sends as its
                // `m_transform`: the transform that makes the buffer the
                // picture a person reads.
                Arg::Int(mode.transform),
            ],
        );
        let _ = self.out.write(
            id,
            wl_output::event::MODE,
            &[ArgType::Uint, ArgType::Int, ArgType::Int, ArgType::Int],
            &[
                Arg::Uint(wl_output::mode::CURRENT | wl_output::mode::PREFERRED),
                Arg::Int(mode.width),
                Arg::Int(mode.height),
                // Millihertz, as the protocol counts it.
                Arg::Int(mode.refresh),
            ],
        );
        if version >= 2 {
            let _ = self.out.write(
                id,
                wl_output::event::SCALE,
                &[ArgType::Int],
                &[Arg::Int(mode.scale)],
            );
        }
        if version >= 4 {
            for (opcode, text) in [
                (wl_output::event::NAME, mode.name.as_str()),
                (wl_output::event::DESCRIPTION, mode.description.as_str()),
            ] {
                let _ = self.out.write(
                    id,
                    opcode,
                    &[ArgType::Str { nullable: false }],
                    &[Arg::Str(Some(text))],
                );
            }
        }
        if version >= 2 {
            let _ = self.out.write(id, wl_output::event::DONE, &[], &[]);
        }
    }

    /// Tell a screenshot program what buffer to make: the format, the size
    /// and the stride of the screen it asked for.
    ///
    /// `buffer_done` follows at version 3, which is what says the list of
    /// formats is complete; at 1 and 2 there is no such event and the client
    /// takes the single `buffer` as the whole answer.
    pub fn screencopy_offer(&mut self, frame: ObjectId, format: Format, size: (u32, u32)) {
        let version = self.objects.get(frame).map_or(1, |entry| entry.version);
        let (width, height) = size;
        let _ = self.out.write(
            frame,
            zwlr_screencopy_frame_v1::event::BUFFER,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[
                Arg::Uint(format.to_wl_shm()),
                Arg::Uint(width),
                Arg::Uint(height),
                Arg::Uint(width.saturating_mul(4)),
            ],
        );
        if version >= 3 {
            let _ = self.out.write(
                frame,
                zwlr_screencopy_frame_v1::event::BUFFER_DONE,
                &[],
                &[],
            );
        }
    }

    /// The screenshot is in the client's buffer: `flags`, then the damage it
    /// asked for, then `ready` at `when`.
    ///
    /// `when` is the presentation time as `clock_gettime(CLOCK_MONOTONIC)`
    /// gives it, split the way the protocol splits it: the seconds in two
    /// halves so they do not overflow a `uint` until the machine has been up
    /// for longer than it will be.
    pub fn screencopy_ready(&mut self, frame: ObjectId, when: (u64, u32), damaged: Option<Rect>) {
        let version = self.objects.get(frame).map_or(1, |entry| entry.version);
        // No flags: the frame is written top row first, so `y_invert` is not
        // set, which is what a client reads to know which way up it is.
        let _ = self.out.write(
            frame,
            zwlr_screencopy_frame_v1::event::FLAGS,
            &[ArgType::Uint],
            &[Arg::Uint(0)],
        );
        if version >= 2
            && let Some(rect) = damaged
        {
            let at = |value: i32| Arg::Uint(u32::try_from(value).unwrap_or(0));
            let _ = self.out.write(
                frame,
                zwlr_screencopy_frame_v1::event::DAMAGE,
                &[ArgType::Uint, ArgType::Uint, ArgType::Uint, ArgType::Uint],
                &[at(rect.x), at(rect.y), at(rect.width), at(rect.height)],
            );
        }
        let (seconds, nanos) = when;
        let _ = self.out.write(
            frame,
            zwlr_screencopy_frame_v1::event::READY,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[
                Arg::Uint(u32::try_from(seconds >> 32).unwrap_or(0)),
                Arg::Uint(u32::try_from(seconds & 0xFFFF_FFFF).unwrap_or(0)),
                Arg::Uint(nanos),
            ],
        );
    }

    /// The screenshot could not be taken.
    ///
    /// The frame is dead from here: the protocol says the client must
    /// destroy it and ask again, which is what `grim` does.
    pub fn screencopy_failed(&mut self, frame: ObjectId) {
        let _ = self
            .out
            .write(frame, zwlr_screencopy_frame_v1::event::FAILED, &[], &[]);
    }

    /// Whether this client is a bar: it bound the toplevel manager and has
    /// not stopped it.
    #[must_use]
    pub fn watches_toplevels(&self) -> bool {
        !self.managers.is_empty()
    }

    /// Tell this client what every window is now, making and taking away
    /// handles as the list changes.
    ///
    /// The compositor calls this with the whole list each pass rather than
    /// with what changed, because the compositor is where the windows are
    /// and this is where it is known what each client was last told. A
    /// window whose fields are what this client already has is not written
    /// to at all: a bar redrawing on every frame of an animation because the
    /// compositor said `done` is a bar that burns a core.
    pub fn show_toplevels(&mut self, windows: &[ForeignToplevel]) {
        if self.managers.is_empty() {
            return;
        }
        // Gone first, so that a bar is never told about more windows than
        // there are.
        let living: Vec<u64> = windows.iter().map(|window| window.window).collect();
        let closed: Vec<u64> = self
            .handles
            .keys()
            .copied()
            .filter(|window| !living.contains(window))
            .collect();
        for window in closed {
            if let Some(handle) = self.handles.remove(&window) {
                let _ = self.out.write(
                    handle,
                    zwlr_foreign_toplevel_handle_v1::event::CLOSED,
                    &[],
                    &[],
                );
                // The handle is the client's to destroy, and it will: until
                // then it is live and may still be sent requests.
                let _ = self.told.remove(&window);
            }
        }
        for window in windows {
            self.show_toplevel(window);
        }
    }

    /// One window, made or brought up to date.
    fn show_toplevel(&mut self, window: &ForeignToplevel) {
        let fresh = !self.handles.contains_key(&window.window);
        if fresh {
            let Ok(handle) = self.objects.create(
                &foreign_toplevel::ZWLR_FOREIGN_TOPLEVEL_HANDLE_V1,
                FOREIGN_TOPLEVEL_VERSION,
                Role::ForeignToplevel,
            ) else {
                return;
            };
            let _ = self.handles.insert(window.window, handle);
            let managers = self.managers.clone();
            for manager in managers {
                let _ = self.out.write(
                    manager,
                    zwlr_foreign_toplevel_manager_v1::event::TOPLEVEL,
                    &[ArgType::NewId],
                    &[Arg::NewId(handle)],
                );
            }
        } else if self.told.get(&window.window) == Some(window) {
            return;
        }
        let Some(handle) = self.handles.get(&window.window).copied() else {
            return;
        };
        let before = self.told.get(&window.window).cloned().unwrap_or_default();
        if fresh || before.title != window.title {
            let _ = self.out.write(
                handle,
                zwlr_foreign_toplevel_handle_v1::event::TITLE,
                &[ArgType::Str { nullable: false }],
                &[Arg::Str(Some(&window.title))],
            );
        }
        if fresh || before.app_id != window.app_id {
            let _ = self.out.write(
                handle,
                zwlr_foreign_toplevel_handle_v1::event::APP_ID,
                &[ArgType::Str { nullable: false }],
                &[Arg::Str(Some(&window.app_id))],
            );
        }
        // The states go as one array, which is what the protocol says: a
        // `state` event replaces the set rather than adding to it.
        let mut states = Vec::new();
        for (on, value) in [
            (
                window.maximized,
                zwlr_foreign_toplevel_handle_v1::state::MAXIMIZED,
            ),
            (
                window.minimized,
                zwlr_foreign_toplevel_handle_v1::state::MINIMIZED,
            ),
            (
                window.activated,
                zwlr_foreign_toplevel_handle_v1::state::ACTIVATED,
            ),
            (
                window.fullscreen,
                zwlr_foreign_toplevel_handle_v1::state::FULLSCREEN,
            ),
        ] {
            if on {
                states.extend_from_slice(&value.to_ne_bytes());
            }
        }
        let _ = self.out.write(
            handle,
            zwlr_foreign_toplevel_handle_v1::event::STATE,
            &[ArgType::Array],
            &[Arg::Array(&states)],
        );
        // Everything above is one atomic change, and `done` is what says so.
        let _ = self.out.write(
            handle,
            zwlr_foreign_toplevel_handle_v1::event::DONE,
            &[],
            &[],
        );
        let _ = self.told.insert(window.window, window.clone());
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
        // A request's new object takes its parent's version, and a protocol
        // may declare the child older than the parent: pointer-gestures has
        // its manager at 3 and its swipe and pinch at 2. libwayland makes the
        // child at the parent's version all the same; here it is made at the
        // most its interface has, which is every request and event it can
        // ever carry. Chrome ended its connection on this before.
        let version = version.min(interface.version);
        match self.objects.insert(id, interface, version, role) {
            Ok(()) => true,
            Err(ObjectError::Version { .. }) => {
                // Zero: `bind` refuses a global asked for at version zero
                // first, so only a request's parent could carry it here.
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
