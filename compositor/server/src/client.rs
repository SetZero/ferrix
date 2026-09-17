//! One client's connection: its objects, and what its requests do.

use std::collections::BTreeMap;

use compositor_protocol::core::{
    self, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
    wl_display, wl_output, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_subcompositor,
    wl_subsurface, wl_surface,
};
use compositor_protocol::cursor_shape::{
    self, wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1,
};
use compositor_protocol::foreign_toplevel::{
    self, zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};
use compositor_protocol::fractional_scale::{
    self, wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
use compositor_protocol::input_method::{self, zwp_input_method_manager_v2, zwp_input_method_v2};
use compositor_protocol::layer_shell::{self, zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use compositor_protocol::primary_selection::{
    self, zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1,
    zwp_primary_selection_offer_v1, zwp_primary_selection_source_v1,
};
use compositor_protocol::screencopy::{self, zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1};
use compositor_protocol::session_lock::{
    self, ext_session_lock_manager_v1, ext_session_lock_surface_v1, ext_session_lock_v1,
};
use compositor_protocol::text_input::{self, zwp_text_input_manager_v3, zwp_text_input_v3};
use compositor_protocol::toplevel_icon::{
    self, xdg_toplevel_icon_manager_v1, xdg_toplevel_icon_v1,
};
use compositor_protocol::viewporter::{self, wp_viewport, wp_viewporter};
use compositor_protocol::xdg_activation::{self, xdg_activation_token_v1, xdg_activation_v1};
use compositor_protocol::xdg_decoration::{
    self, zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use compositor_protocol::xdg_shell::{self, xdg_surface, xdg_toplevel, xdg_wm_base};
use compositor_wire::{
    Arg, ArgType, Error as WireError, Fd, Fixed, ObjectError, ObjectId, Objects, Reader, Writer,
};
use compositor_xkb::Modifiers;

mod control;
mod desktop;
mod input;
mod outputs;
mod screen;
mod workspaces;

pub use control::{Flavour, Manager};
pub use input::{Constraint, Injected};
pub use outputs::Configuration;
pub use outputs::Wanted;
pub use screen::GAMMA_SIZE;
pub use workspaces::{Workspace, WorkspaceRequest};

use crate::globals::Globals;
use crate::layer::{Anchors, Layer, LayerSurface, Margin};
use crate::role::Role;
use crate::shm::{Buffer, FORMATS, Format, Pool};
use crate::surface::{Committed, Output, Rect, Region, Subsurface, Surface};
use crate::xdg::{Popup, Positioner, Toplevel, XdgRole, XdgSurface};

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
    /// `xdg_dialog_v1`: the toplevel said whether it is modal. A modal
    /// dialog floats, which is what Hyprland does with one.
    ToplevelModal {
        /// The `xdg_toplevel`.
        toplevel: ObjectId,
        /// Whether it is modal now.
        modal: bool,
    },
    /// A virtual device asked the seat to do something, which the
    /// compositor hands on as if a real one had reported it.
    Injected(Injected),
    /// `zwlr_gamma_control_v1.set_gamma`: the three ramps on a descriptor,
    /// or `None` where the control was destroyed and the screen goes back
    /// to what it was.
    Gamma {
        /// Which screen, by its place in the outputs.
        output: usize,
        /// The descriptor the ramps are on.
        table: Option<Fd>,
    },
    /// `zwlr_output_power_v1.set_mode`: turn a screen off or on.
    OutputPower {
        /// Which screen.
        output: usize,
        /// Whether it is to be on.
        on: bool,
    },
    /// A clipboard manager made a device, which is owed both selections at
    /// once whether or not it has the keyboard.
    DataControlBound {
        /// The device.
        device: ObjectId,
    },
    /// A clipboard manager put something on a selection.
    DataControlSelection {
        /// Its source, or `None` for giving the selection up.
        source: Option<ObjectId>,
        /// Whether it is the primary selection.
        primary: bool,
        /// The types the source offered.
        mimes: Vec<String>,
    },
    /// A clipboard manager asked for what is on a selection.
    DataControlPaste {
        /// The offer it asked through.
        offer: ObjectId,
        /// The type it asked for.
        mime: String,
        /// The pipe to write it to.
        fd: Fd,
        /// Whether it is the primary selection.
        primary: bool,
    },
    /// A program asked for the screens to be arranged.
    OutputConfigured {
        /// The configuration, which is owed `succeeded` or `failed`.
        configuration: ObjectId,
        /// Whether it only asked whether the arrangement would work.
        testing: bool,
        /// What it asks each screen to become.
        heads: Vec<(usize, Wanted)>,
    },
    /// A bar asked for something to be done to a workspace.
    WorkspaceAsked {
        /// Which workspace, by the number the compositor calls it.
        workspace: i64,
        /// What it asked for.
        what: WorkspaceRequest,
    },
    /// `ext_workspace_manager_v1.commit`: carry out what was asked.
    WorkspacesCommitted,
    /// `xdg_system_bell_v1.ring`: the terminal bell, for the surface that
    /// rang it or for the whole seat.
    Bell {
        /// The `wl_surface`, or `None` for the seat.
        surface: Option<ObjectId>,
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
    /// A surface became a layer surface: a bar, a wallpaper, a launcher.
    /// The compositor has to place it and configure it before the client
    /// may attach a buffer.
    LayerSurfaceCreated {
        /// The `zwlr_layer_surface_v1`.
        layer_surface: ObjectId,
        /// The `wl_surface` under it.
        surface: ObjectId,
    },
    /// A layer surface changed something the compositor places it by: its
    /// anchor, its size, its margin, its zone or its layer.
    LayerSurfaceChanged {
        /// The `zwlr_layer_surface_v1`.
        layer_surface: ObjectId,
    },
    /// A client made a `wl_data_device`. Whatever the selection holds has
    /// to be offered to it, since a client that binds after a copy must
    /// still be able to paste.
    DataDeviceMade {
        /// The `wl_data_device`.
        device: ObjectId,
    },
    /// A client set the selection: it copied something. The compositor
    /// remembers which client and which source, and offers it to the
    /// others.
    SelectionSet {
        /// The `wl_data_source`, or `None` for a selection being cleared.
        source: Option<ObjectId>,
        /// The types it offered, in order.
        mimes: Vec<String>,
    },
    /// A client asked for the selection's data on a descriptor: it pasted.
    /// The compositor passes the descriptor to whoever owns the selection.
    SelectionWanted {
        /// The `wl_data_offer` it asked through.
        offer: ObjectId,
        /// The type it asked for.
        mime: String,
        /// Where the data is to be written.
        fd: Fd,
    },
    /// A program asked for a screenshot of a screen: it is owed the size
    /// and format of the buffer it must make.
    ScreencopyWanted {
        /// The `zwlr_screencopy_frame_v1` it will be given.
        frame: ObjectId,
        /// Which screen, by its place in the outputs the globals advertise.
        output: usize,
        /// The part of it, or `None` for all of it.
        region: Option<Rect>,
    },
    /// A program handed over the buffer its screenshot is to be written
    /// into.
    ScreencopyInto {
        /// The frame it belongs to.
        frame: ObjectId,
        /// The `wl_buffer`.
        buffer: ObjectId,
        /// Which screen, by its place in the outputs.
        output: usize,
        /// The part of it, or `None` for all of it.
        region: Option<Rect>,
        /// Whether `copy_with_damage` was used, which asks for a `damage`
        /// event before `ready`.
        with_damage: bool,
    },
    /// A program locked the session. Everything else stops being drawn and
    /// stops being given input until it unlocks.
    SessionLocked {
        /// The `ext_session_lock_v1` it holds.
        lock: ObjectId,
    },
    /// It made the surface for one screen, which the compositor is to
    /// configure at that screen's size and then draw instead of everything.
    SessionLockSurfaceMade {
        /// The `ext_session_lock_surface_v1`.
        lock_surface: ObjectId,
        /// The `wl_surface` under it.
        surface: ObjectId,
        /// Which screen, by its place in the outputs.
        output: usize,
    },
    /// It unlocked, or it went away while holding the lock. The payload
    /// says which: a lock that was *released* leaves the screen to the
    /// windows again, and one whose client died leaves it locked with
    /// nothing drawn on it, which is what the protocol requires.
    SessionUnlocked {
        /// Whether the client asked, rather than having gone.
        asked: bool,
    },
    /// An application said it wants to be typed into through an input
    /// method, or that it no longer does.
    TextInputEnabled {
        /// Its `zwp_text_input_v3`.
        text_input: ObjectId,
        /// Whether it is enabled now.
        enabled: bool,
    },
    /// It said what is around the cursor, which an input method uses to
    /// guess the next word.
    TextInputSurrounded {
        /// Its `zwp_text_input_v3`.
        text_input: ObjectId,
        /// The text.
        text: String,
        /// Where the cursor is in it, in bytes.
        cursor: i32,
        /// Where the selection's other end is.
        anchor: i32,
    },
    /// It said where the cursor is on the screen, which is where an input
    /// method puts its candidate window.
    TextInputCursorAt {
        /// Its `zwp_text_input_v3`.
        text_input: ObjectId,
        /// The rectangle, in the surface's own coordinates.
        rect: Rect,
    },
    /// It applied everything it has said since the last commit.
    TextInputCommitted {
        /// Its `zwp_text_input_v3`.
        text_input: ObjectId,
    },
    /// A program became the input method for the seat.
    InputMethodMade {
        /// Its `zwp_input_method_v2`.
        method: ObjectId,
    },
    /// The input method typed something.
    InputMethodTyped {
        /// Its `zwp_input_method_v2`.
        method: ObjectId,
        /// What it typed.
        typed: Typed,
    },
    /// The input method went.
    InputMethodGone {
        /// Its `zwp_input_method_v2`.
        method: ObjectId,
    },
    /// A client named the cursor it wants rather than drawing one.
    CursorShaped {
        /// Which of `wp_cursor_shape_device_v1`'s shapes.
        shape: u32,
    },
    /// A client made a `zwp_primary_selection_device_v1`: it can paste the
    /// primary selection, and is owed whatever is in it.
    PrimaryDeviceMade {
        /// The device.
        device: ObjectId,
    },
    /// A client set the primary selection, which is what a middle click
    /// pastes.
    PrimarySet {
        /// The source, or `None` for one being cleared.
        source: Option<ObjectId>,
        /// The types it offered.
        mimes: Vec<String>,
    },
    /// A client asked for the primary selection's data on a descriptor.
    PrimaryWanted {
        /// The offer it asked through.
        offer: ObjectId,
        /// The type it asked for.
        mime: String,
        /// Where the data is to be written.
        fd: Fd,
    },
    /// A client asked for another program's window to be focused, with a
    /// token the compositor gave out.
    ActivationAsked {
        /// The token it was given.
        token: String,
        /// The `wl_surface` it wants raised.
        surface: ObjectId,
    },
    /// A client said what the pointer looks like over its windows.
    CursorSet {
        /// The surface it drew, or `None` for a pointer it wants hidden.
        surface: Option<ObjectId>,
        /// Where in that surface the pointer is.
        hotspot: (i32, i32),
    },
    /// A client made a popup: a menu, a tooltip, a dropdown. It has to be
    /// placed and configured before it may draw anything at all.
    PopupCreated {
        /// The `xdg_popup`.
        popup: ObjectId,
        /// The `wl_surface` its pixels come from.
        surface: ObjectId,
        /// The `xdg_surface` it hangs off.
        parent: ObjectId,
    },
    /// It asked for a grab, which is what makes a menu a menu: the keyboard
    /// is the popup's until it is dismissed, and a click outside dismisses
    /// it.
    PopupGrabbed {
        /// The `xdg_popup`.
        popup: ObjectId,
    },
    /// A popup went.
    PopupGone {
        /// The `xdg_popup` that was destroyed.
        popup: ObjectId,
    },
    /// A bar asked the compositor to do something to a window it does not
    /// own, through `zwlr_foreign_toplevel_handle_v1`.
    ForeignToplevelAsked {
        /// The window, as the compositor numbered it.
        window: u64,
        /// What was asked for.
        what: ForeignRequest,
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

/// What an input method has typed, staged until it commits.
///
/// The three are applied together: a method that replaced a word should not
/// be seen half way through, which is the same reason a `wl_surface` is
/// double-buffered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Typed {
    /// `commit_string`: text to insert.
    pub commit: Option<String>,
    /// `set_preedit_string`: the text being composed, and where the cursor
    /// is inside it.
    pub preedit: Option<(String, i32, i32)>,
    /// `delete_surrounding_text`: how much to take out before and after the
    /// cursor.
    pub delete: Option<(u32, u32)>,
}

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
            serial: 1,
        }
    }

    /// Say what the seat has, before any client binds it.
    ///
    /// A client may only ask for a capability the seat announced, so a
    /// compositor with no input yet announces none rather than handing out a
    /// keyboard that will never send a key.
    pub const fn set_seat_capabilities(&mut self, capabilities: u32) {
        self.capabilities = capabilities;
    }

    /// The keymap every `wl_keyboard` is given: a descriptor and its length.
    ///
    /// The same descriptor goes to every keyboard, which is what libwayland's
    /// own compositors do: the file is read-only and each client maps its own
    /// copy.
    pub const fn set_keymap(&mut self, keymap: Option<(Fd, u32)>) {
        self.keymap = keymap;
    }

    /// Say how a held key repeats: `rate` keys a second, `delay`
    /// milliseconds before the first repeat.
    ///
    /// The compositor sends no repeats of its own -- `wl_keyboard.key` has no
    /// way to say one -- so this is the whole of repeat: the client is told
    /// the numbers and repeats for itself. A rate of zero disables it, which
    /// the protocol says in so many words.
    pub const fn set_repeat_info(&mut self, rate: i32, delay: i32) {
        self.repeat = (rate, delay);
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

    /// The subsurface `id` names, if it is one.
    #[must_use]
    pub fn subsurface(&self, id: ObjectId) -> Option<&Subsurface> {
        self.subsurfaces.get(&id)
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
        top.states = states.to_vec();
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

    // -----------------------------------------------------------------
    // The seat: what the compositor sends when somebody types or points
    //
    // Every one of these goes to each object of the role this client made,
    // because a client may ask its seat for more than one `wl_keyboard` and
    // the protocol says each gets the events. A client that asked for none
    // gets nothing, which is how a compositor sends a key to the focused
    // window without knowing whether that window wanted keys.
    //
    // The serial each returns is this connection's, from `next_serial`: a
    // client quotes it back in `set_cursor`, in a `start_drag`, or in
    // `xdg_toplevel.move`, and the compositor matches it against what it
    // sent.
    // -----------------------------------------------------------------

    /// Give this client the keyboard focus on `surface`.
    ///
    /// `keys` is every key held at the moment focus arrives, in the order
    /// they were pressed, so that a client focused while a key is down knows
    /// it is down.
    ///
    /// `None` when this client has no `wl_keyboard` yet. That is not a
    /// failure and it is not nothing: a window is often mapped in the same
    /// burst of requests that asks the seat for its keyboard, and whichever
    /// the server reads first, the client has to be told it has the focus. So
    /// the caller is told the focus did not arrive, and asks again.
    pub fn keyboard_enter(
        &mut self,
        surface: ObjectId,
        keys: &[u16],
        modifiers: Modifiers,
    ) -> Option<u32> {
        let keyboards = self.objects_with(Role::Keyboard);
        if keyboards.is_empty() {
            return None;
        }
        let serial = self.next_serial();
        let packed: Vec<u8> = keys
            .iter()
            .flat_map(|key| u32::from(*key).to_le_bytes())
            .collect();
        for keyboard in keyboards {
            let _ = self.out.write(
                keyboard,
                core::wl_keyboard::event::ENTER,
                &[
                    ArgType::Uint,
                    ArgType::Object { nullable: false },
                    ArgType::Array,
                ],
                &[Arg::Uint(serial), Arg::Object(surface), Arg::Array(&packed)],
            );
        }
        // The modifiers are not part of `enter`, and a client that is not
        // told them treats every key as unmodified until the next change.
        let _ = self.keyboard_modifiers(modifiers);
        Some(serial)
    }

    /// Take the keyboard focus away from `surface`.
    ///
    /// `None` when this client has no `wl_keyboard`, as in
    /// [`Client::keyboard_enter`].
    pub fn keyboard_leave(&mut self, surface: ObjectId) -> Option<u32> {
        let keyboards = self.objects_with(Role::Keyboard);
        if keyboards.is_empty() {
            return None;
        }
        let serial = self.next_serial();
        for keyboard in keyboards {
            let _ = self.out.write(
                keyboard,
                core::wl_keyboard::event::LEAVE,
                &[ArgType::Uint, ArgType::Object { nullable: false }],
                &[Arg::Uint(serial), Arg::Object(surface)],
            );
        }
        Some(serial)
    }

    /// A key went down or came up, at `time` milliseconds.
    ///
    /// `code` is the evdev keycode, which is what the keymap the client was
    /// given numbers from eight.
    pub fn keyboard_key(&mut self, time: u32, code: u16, pressed: bool) -> Option<u32> {
        let keyboards = self.objects_with(Role::Keyboard);
        if keyboards.is_empty() {
            return None;
        }
        let serial = self.next_serial();
        let state = if pressed {
            core::wl_keyboard::key_state::PRESSED
        } else {
            core::wl_keyboard::key_state::RELEASED
        };
        for keyboard in keyboards {
            let _ = self.out.write(
                keyboard,
                core::wl_keyboard::event::KEY,
                &[ArgType::Uint, ArgType::Uint, ArgType::Uint, ArgType::Uint],
                &[
                    Arg::Uint(serial),
                    Arg::Uint(time),
                    Arg::Uint(u32::from(code)),
                    Arg::Uint(state),
                ],
            );
        }
        Some(serial)
    }

    /// The modifier state changed.
    pub fn keyboard_modifiers(&mut self, modifiers: Modifiers) -> Option<u32> {
        let keyboards = self.objects_with(Role::Keyboard);
        if keyboards.is_empty() {
            return None;
        }
        let serial = self.next_serial();
        for keyboard in keyboards {
            let _ = self.out.write(
                keyboard,
                core::wl_keyboard::event::MODIFIERS,
                &[
                    ArgType::Uint,
                    ArgType::Uint,
                    ArgType::Uint,
                    ArgType::Uint,
                    ArgType::Uint,
                ],
                &[
                    Arg::Uint(serial),
                    Arg::Uint(modifiers.depressed),
                    Arg::Uint(modifiers.latched),
                    Arg::Uint(modifiers.locked),
                    Arg::Uint(modifiers.group),
                ],
            );
        }
        Some(serial)
    }

    /// The pointer came onto `surface` at `(x, y)` in its own coordinates.
    ///
    /// `None` when this client has no `wl_pointer`, as in
    /// [`Client::keyboard_enter`].
    pub fn pointer_enter(&mut self, surface: ObjectId, x: Fixed, y: Fixed) -> Option<u32> {
        let pointers = self.objects_with(Role::Pointer);
        if pointers.is_empty() {
            return None;
        }
        let serial = self.next_serial();
        for pointer in pointers {
            let _ = self.out.write(
                pointer,
                core::wl_pointer::event::ENTER,
                &[
                    ArgType::Uint,
                    ArgType::Object { nullable: false },
                    ArgType::Fixed,
                    ArgType::Fixed,
                ],
                &[
                    Arg::Uint(serial),
                    Arg::Object(surface),
                    Arg::Fixed(x),
                    Arg::Fixed(y),
                ],
            );
        }
        Some(serial)
    }

    /// The pointer left `surface`.
    ///
    /// `None` when this client has no `wl_pointer`.
    pub fn pointer_leave(&mut self, surface: ObjectId) -> Option<u32> {
        let pointers = self.objects_with(Role::Pointer);
        if pointers.is_empty() {
            return None;
        }
        let serial = self.next_serial();
        for pointer in pointers {
            let _ = self.out.write(
                pointer,
                core::wl_pointer::event::LEAVE,
                &[ArgType::Uint, ArgType::Object { nullable: false }],
                &[Arg::Uint(serial), Arg::Object(surface)],
            );
        }
        Some(serial)
    }

    /// The pointer moved to `(x, y)` in the focused surface's coordinates.
    pub fn pointer_motion(&mut self, time: u32, x: Fixed, y: Fixed) {
        for pointer in self.objects_with(Role::Pointer) {
            let _ = self.out.write(
                pointer,
                core::wl_pointer::event::MOTION,
                &[ArgType::Uint, ArgType::Fixed, ArgType::Fixed],
                &[Arg::Uint(time), Arg::Fixed(x), Arg::Fixed(y)],
            );
        }
    }

    /// A pointer button went down or came up. `button` is evdev's code, which
    /// is what the protocol asks for in so many words.
    pub fn pointer_button(&mut self, time: u32, button: u32, pressed: bool) -> Option<u32> {
        let pointers = self.objects_with(Role::Pointer);
        if pointers.is_empty() {
            return None;
        }
        let serial = self.next_serial();
        let state = if pressed {
            core::wl_pointer::button_state::PRESSED
        } else {
            core::wl_pointer::button_state::RELEASED
        };
        for pointer in pointers {
            let _ = self.out.write(
                pointer,
                core::wl_pointer::event::BUTTON,
                &[ArgType::Uint, ArgType::Uint, ArgType::Uint, ArgType::Uint],
                &[
                    Arg::Uint(serial),
                    Arg::Uint(time),
                    Arg::Uint(button),
                    Arg::Uint(state),
                ],
            );
        }
        Some(serial)
    }

    /// A scroll: `axis` is `wl_pointer.axis`, `value` the distance.
    pub fn pointer_axis(&mut self, time: u32, axis: u32, value: Fixed) {
        for pointer in self.objects_with(Role::Pointer) {
            let _ = self.out.write(
                pointer,
                core::wl_pointer::event::AXIS,
                &[ArgType::Uint, ArgType::Uint, ArgType::Fixed],
                &[Arg::Uint(time), Arg::Uint(axis), Arg::Fixed(value)],
            );
        }
    }

    /// End a group of pointer events that belong together.
    ///
    /// Only for version 5 and above; before it, each event stood alone and a
    /// `frame` sent to a version-4 pointer is an opcode it does not have.
    pub fn pointer_frame(&mut self) {
        for pointer in self.objects_with_version(Role::Pointer, 5) {
            let _ = self
                .out
                .write(pointer, core::wl_pointer::event::FRAME, &[], &[]);
        }
    }

    /// Every object this client made with `role`.
    fn objects_with(&self, role: Role) -> Vec<ObjectId> {
        self.objects
            .iter()
            .filter(|(_, entry)| entry.data == role)
            .map(|(id, _)| id)
            .collect()
    }

    /// Every object this client made with `role`, bound at `version` or
    /// above.
    fn objects_with_version(&self, role: Role, version: u32) -> Vec<ObjectId> {
        self.objects
            .iter()
            .filter(|(_, entry)| entry.data == role && entry.version >= version)
            .map(|(id, _)| id)
            .collect()
    }

    /// The layer surface `id` names, if it is one.
    #[must_use]
    pub fn layer_surface(&self, id: ObjectId) -> Option<&LayerSurface> {
        self.layers.get(&id)
    }

    /// Every layer surface, in the order they were created, which is the
    /// order wlroots places them in.
    pub fn layer_surfaces(&self) -> impl Iterator<Item = (ObjectId, &LayerSurface)> {
        self.layers.iter().map(|(id, layer)| (*id, layer))
    }

    /// Tell a layer surface the size it is to be.
    ///
    /// A size of zero on an axis the client is not anchored to both edges of
    /// is `invalid_size`: the protocol says the client must be told a real
    /// number, and a bar that forgot `set_size` finds out here rather than
    /// by drawing nothing.
    pub fn configure_layer(&mut self, layer_surface: ObjectId, width: u32, height: u32) {
        let Some(layer) = self.layers.get(&layer_surface) else {
            return;
        };
        let anchors = Anchors::from_raw(layer.anchor);
        if !layer.size_is_valid(&anchors) {
            self.fail(Fatal::Interface {
                object: layer_surface,
                code: zwlr_layer_surface_v1::error::INVALID_SIZE,
                text: "a surface not anchored to both edges of an axis must set a size on it"
                    .to_owned(),
            });
            return;
        }
        let serial = self.next_serial();
        if let Some(layer) = self.layers.get_mut(&layer_surface) {
            layer.configure_sent(serial, (width, height));
        }
        let _ = self.out.write(
            layer_surface,
            zwlr_layer_surface_v1::event::CONFIGURE,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[Arg::Uint(serial), Arg::Uint(width), Arg::Uint(height)],
        );
    }

    /// Tell a layer surface to close, and forget it.
    ///
    /// `closed` is final, unlike `xdg_toplevel.close`: the protocol says the
    /// surface is no longer shown and the client should destroy it.
    pub fn close_layer(&mut self, layer_surface: ObjectId) {
        if self.layers.remove(&layer_surface).is_none() {
            return;
        }
        let _ = self.out.write(
            layer_surface,
            zwlr_layer_surface_v1::event::CLOSED,
            &[],
            &[],
        );
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
                self.forget_screen(id, other);
                self.forget_control(id, other);
                self.forget_outputs(id, other);
                self.forget_workspaces(id, other);
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
                    || self.workspaces(sender, other, opcode);
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
                if transform > wl_output::transform::FLIPPED_270 {
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

    /// `wl_subcompositor`: `get_subsurface`.
    ///
    /// A subsurface is a surface placed relative to another and committed
    /// with it. Every toolkit makes them -- for a title bar, a shadow, a
    /// cursor -- so a compositor that advertises `wl_subcompositor` and does
    /// not answer this refuses every such client at its first window.
    fn subcompositor(&mut self, opcode: u16, args: &[Arg<'_>]) {
        if opcode != wl_subcompositor::request::GET_SUBSURFACE {
            return;
        }
        let (Some(id), Some(surface), Some(parent)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
            args.get(2).and_then(Arg::as_object),
        ) else {
            return;
        };
        for (object, name) in [(surface, "wl_surface"), (parent, "wl_surface")] {
            if !self.surfaces.contains_key(&object) {
                self.fail(Fatal::WrongInterface {
                    object,
                    wanted: name,
                });
                return;
            }
        }
        if surface == parent {
            self.fail(Fatal::Interface {
                object: id,
                code: wl_subcompositor::error::BAD_SURFACE,
                text: "a surface cannot be its own parent".to_owned(),
            });
            return;
        }
        // A surface that already has a role may not be given another, as for
        // `xdg_surface`.
        if self.subsurfaces.values().any(|sub| sub.surface == surface)
            || self.xdg_surfaces.values().any(|xdg| xdg.surface == surface)
        {
            self.fail(Fatal::Interface {
                object: id,
                code: wl_subcompositor::error::BAD_SURFACE,
                text: "that surface already has a role".to_owned(),
            });
            return;
        }
        if self.make(id, &core::WL_SUBSURFACE, 1, Role::Subsurface) {
            let _ = self.subsurfaces.insert(
                id,
                Subsurface {
                    surface,
                    parent,
                    position: (0, 0),
                    synchronised: true,
                },
            );
        }
    }

    /// `wl_subsurface`: where it sits and how it commits.
    ///
    /// The position is kept and the stacking is not: a subsurface is drawn
    /// with its parent, and this compositor draws a window's own surface
    /// only, so `place_above` and `place_below` change nothing yet. They are
    /// taken rather than refused, since the protocol allows them.
    fn subsurface_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        let Some(sub) = self.subsurfaces.get_mut(&sender) else {
            return;
        };
        match opcode {
            wl_subsurface::request::SET_POSITION => {
                let (Some(x), Some(y)) = (
                    args.first().and_then(Arg::as_int),
                    args.get(1).and_then(Arg::as_int),
                ) else {
                    return;
                };
                sub.position = (x, y);
            }
            wl_subsurface::request::SET_SYNC => sub.synchronised = true,
            wl_subsurface::request::SET_DESYNC => sub.synchronised = false,
            _ => {}
        }
    }

    /// `wl_seat`: the keyboard, the pointer and the touchscreen.
    ///
    /// A client may only ask for a capability the seat announced, and this
    /// one announces what [`Client::set_seat_capabilities`] was told. Asking
    /// for one it did not is `missing_capability`, which is what the protocol
    /// says and what keeps a client from waiting for events that will never
    /// come.
    fn seat(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        let (interface, role, capability, what) = match opcode {
            wl_seat::request::GET_POINTER => (
                &core::WL_POINTER,
                Role::Pointer,
                wl_seat::capability::POINTER,
                "pointer",
            ),
            wl_seat::request::GET_KEYBOARD => (
                &core::WL_KEYBOARD,
                Role::Keyboard,
                wl_seat::capability::KEYBOARD,
                "keyboard",
            ),
            wl_seat::request::GET_TOUCH => (
                &core::WL_TOUCH,
                Role::Touch,
                wl_seat::capability::TOUCH,
                "touch",
            ),
            _ => return,
        };
        if self.capabilities & capability == 0 {
            self.fail(Fatal::Interface {
                object: id,
                code: wl_seat::error::MISSING_CAPABILITY,
                text: format!("this seat has no {what}"),
            });
            return;
        }
        if !self.make(id, interface, version, role) {
            return;
        }
        if role == Role::Keyboard {
            self.send_keymap(id);
            // `repeat_info` arrived in version 4, and a client that does not
            // get it repeats at whatever it chooses -- or, for a toolkit that
            // waits for it, not at all.
            if version >= 4 {
                let (rate, delay) = self.repeat;
                let _ = self.out.write(
                    id,
                    core::wl_keyboard::event::REPEAT_INFO,
                    &[ArgType::Int, ArgType::Int],
                    &[Arg::Int(rate), Arg::Int(delay)],
                );
            }
        }
    }

    /// `zwlr_layer_shell_v1`: `get_layer_surface`.
    ///
    /// The surface must have no buffer and no other role, as every
    /// role-giving request requires, and the layer must be one of the four
    /// the protocol defines. A client that asks for a fifth is refused with
    /// `invalid_layer` rather than being given a surface nothing draws.
    fn layer_shell(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwlr_layer_shell_v1::request::GET_LAYER_SURFACE {
            return;
        }
        let (Some(id), Some(surface)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
        ) else {
            return;
        };
        // The output is nullable: `None` means "you choose", and this
        // compositor has one monitor to choose.
        let output = args.get(2).and_then(Arg::as_object).filter(|id| id.0 != 0);
        let Some(layer) = args.get(3).and_then(Arg::as_uint).and_then(Layer::from_raw) else {
            self.fail(Fatal::Interface {
                object: id,
                code: zwlr_layer_shell_v1::error::INVALID_LAYER,
                text: "that is not one of the four layers".to_owned(),
            });
            return;
        };
        let namespace = args.get(4).and_then(Arg::as_str).unwrap_or("").to_owned();

        if !self.surfaces.contains_key(&surface) {
            self.fail(Fatal::WrongInterface {
                object: surface,
                wanted: "wl_surface",
            });
            return;
        }
        let has_buffer = self
            .surfaces
            .get(&surface)
            .is_some_and(|state| state.is_mapped() || state.pending.buffer.is_some());
        if has_buffer {
            self.fail(Fatal::Interface {
                object: id,
                code: zwlr_layer_shell_v1::error::ALREADY_CONSTRUCTED,
                text: "a surface with a buffer cannot be given a role".to_owned(),
            });
            return;
        }
        let taken = self.xdg_surfaces.values().any(|xdg| xdg.surface == surface)
            || self.layers.values().any(|live| live.surface == surface);
        if taken {
            self.fail(Fatal::Interface {
                object: id,
                code: zwlr_layer_shell_v1::error::ROLE,
                text: "that surface already has a role".to_owned(),
            });
            return;
        }
        if !self.make(
            id,
            &layer_shell::ZWLR_LAYER_SURFACE_V1,
            version,
            Role::LayerSurface,
        ) {
            return;
        }
        let _ = self
            .layers
            .insert(id, LayerSurface::new(surface, output, layer, namespace));
        self.events.push(Event::LayerSurfaceCreated {
            layer_surface: id,
            surface,
        });
    }

    /// `zwlr_layer_surface_v1`: everything a bar says about itself.
    ///
    /// Each request records what the client asked for and tells the
    /// compositor above that the placement has to be worked out again. The
    /// compositor answers with [`Client::configure_layer`], which is where
    /// the size the client is given is decided.
    fn layer_surface_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        let uint = |at: usize| args.get(at).and_then(Arg::as_uint).unwrap_or(0);
        let int = |at: usize| args.get(at).and_then(Arg::as_int).unwrap_or(0);
        match opcode {
            zwlr_layer_surface_v1::request::SET_SIZE => {
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.size = (uint(0), uint(1));
                }
            }
            zwlr_layer_surface_v1::request::SET_ANCHOR => {
                let raw = uint(0);
                if !Anchors::is_valid(raw) {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: zwlr_layer_surface_v1::error::INVALID_ANCHOR,
                        text: "that anchor has a bit the protocol does not define".to_owned(),
                    });
                    return;
                }
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.anchor = raw;
                }
            }
            zwlr_layer_surface_v1::request::SET_EXCLUSIVE_ZONE => {
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.exclusive_zone = int(0);
                }
            }
            zwlr_layer_surface_v1::request::SET_MARGIN => {
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.margin = Margin {
                        top: int(0),
                        right: int(1),
                        bottom: int(2),
                        left: int(3),
                    };
                }
            }
            zwlr_layer_surface_v1::request::SET_KEYBOARD_INTERACTIVITY => {
                let wanted = uint(0);
                if wanted > zwlr_layer_surface_v1::keyboard_interactivity::ON_DEMAND {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: zwlr_layer_surface_v1::error::INVALID_KEYBOARD_INTERACTIVITY,
                        text: "that is not a keyboard interactivity".to_owned(),
                    });
                    return;
                }
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.keyboard_interactivity = wanted;
                }
            }
            zwlr_layer_surface_v1::request::SET_LAYER => {
                let Some(wanted) = Layer::from_raw(uint(0)) else {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: zwlr_layer_shell_v1::error::INVALID_LAYER,
                        text: "that is not one of the four layers".to_owned(),
                    });
                    return;
                };
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.layer = wanted;
                }
            }
            zwlr_layer_surface_v1::request::ACK_CONFIGURE => {
                let serial = uint(0);
                let known = self
                    .layers
                    .get_mut(&sender)
                    .is_some_and(|layer| layer.acked(serial));
                if !known {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_surface::error::INVALID_SERIAL,
                        text: format!("{serial} is not a configure this surface was sent"),
                    });
                }
                return;
            }
            // `get_popup` and `set_exclusive_edge` are read and recorded
            // nowhere: a popup on a layer surface needs popups, which land
            // with `xdg_popup`, and the exclusive edge only matters for a
            // surface anchored to more than one edge with a zone, which
            // `place` does not reserve for anyway.
            _ => return,
        }
        self.events.push(Event::LayerSurfaceChanged {
            layer_surface: sender,
        });
    }

    /// `wl_data_device_manager`: the clipboard's objects.
    ///
    /// The objects are made and nothing is ever offered through them. A
    /// compositor without a clipboard that does not advertise the global at
    /// all is one that toolkits refuse to start on -- which is how this came
    /// to be written -- and one that advertises it and then does not answer
    /// `get_data_device` is worse, because the client only finds out at its
    /// first copy. `wl_data_device.selection` is never sent, which is exactly
    /// what a client sees when no other client has ever copied anything.
    fn data_device_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        match opcode {
            wl_data_device_manager::request::CREATE_DATA_SOURCE => {
                if self.make(id, &core::WL_DATA_SOURCE, version, Role::DataSource) {
                    let _previous = self.sources.insert(id, Vec::new());
                }
            }
            wl_data_device_manager::request::GET_DATA_DEVICE => {
                let Some(seat) = args.get(1).and_then(Arg::as_object) else {
                    return;
                };
                if self
                    .objects
                    .get(seat)
                    .is_none_or(|entry| entry.data != Role::Seat)
                {
                    self.fail(Fatal::WrongInterface {
                        object: seat,
                        wanted: "wl_seat",
                    });
                    return;
                }
                if self.make(id, &core::WL_DATA_DEVICE, version, Role::DataDevice) {
                    self.devices.push(id);
                    self.events.push(Event::DataDeviceMade { device: id });
                }
            }
            _ => {}
        }
    }

    /// `wl_pointer`: `set_cursor`, which is how a client says what the
    /// pointer looks like over its window.
    ///
    /// A text field asks for an I-beam, a link for a hand, a resize edge for
    /// an arrow with two heads: all of them are this one request with a
    /// surface the client drew. A null surface hides the pointer, which is
    /// what a video player full-screen does.
    ///
    /// The serial is not checked. libwayland's own compositors check it
    /// against the last `enter` so that a client cannot change the cursor
    /// while the pointer is somebody else's; this compositor has one pointer
    /// and gives it to the surface under it, so the client that is asking is
    /// the client that has it.
    fn pointer_request(&mut self, opcode: u16, args: &[Arg<'_>]) {
        if opcode != core::wl_pointer::request::SET_CURSOR {
            return;
        }
        let surface = args.get(1).and_then(Arg::as_object);
        let hotspot = (
            args.get(2).and_then(Arg::as_int).unwrap_or(0),
            args.get(3).and_then(Arg::as_int).unwrap_or(0),
        );
        self.cursor = surface
            .filter(|surface| !surface.is_null() && self.surfaces.contains_key(surface))
            .map(|surface| (surface, hotspot));
        self.said_cursor = true;
        self.events.push(Event::CursorSet {
            surface: self.cursor.map(|(surface, _)| surface),
            hotspot,
        });
    }

    /// What this client last asked the pointer to look like over its
    /// windows: the surface and where in it the pointer is.
    ///
    /// `None` is a client that has asked for no cursor at all, which is what
    /// hides the pointer.
    #[must_use]
    pub const fn cursor(&self) -> Option<(ObjectId, (i32, i32))> {
        self.cursor
    }

    /// Whether this client has ever said anything about the cursor.
    ///
    /// A client that has not is drawn the compositor's own arrow; one that
    /// has asked for nothing is drawn none, and the two are different
    /// things.
    #[must_use]
    pub const fn said_cursor(&self) -> bool {
        self.said_cursor
    }

    /// `wp_cursor_shape_manager_v1`: `get_pointer` and `get_tablet_tool_v2`.
    ///
    /// A device object is made for each; the tablet tool's is made and never
    /// spoken to, because this compositor has no tablet tool to name a
    /// cursor for.
    fn cursor_shape_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if !matches!(
            opcode,
            wp_cursor_shape_manager_v1::request::GET_POINTER
                | wp_cursor_shape_manager_v1::request::GET_TABLET_TOOL_V2
        ) {
            return;
        }
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        let _ = self.make(
            id,
            &cursor_shape::WP_CURSOR_SHAPE_DEVICE_V1,
            version,
            Role::CursorShapeDevice,
        );
    }

    /// `wp_cursor_shape_device_v1`: `set_shape`.
    ///
    /// The client names a cursor instead of drawing one, which is what a
    /// toolkit would rather do: it has no idea what the person's theme
    /// looks like and the compositor does. This one draws its own arrow for
    /// every shape it is given -- there is one shape and no theme to pick
    /// another from -- and says which was asked for, so that a client is
    /// answered rather than refused and the log records what a real toolkit
    /// wanted.
    fn cursor_shape_device(&mut self, opcode: u16, args: &[Arg<'_>]) {
        if opcode != wp_cursor_shape_device_v1::request::SET_SHAPE {
            return;
        }
        let shape = args.get(1).and_then(Arg::as_uint).unwrap_or(0);
        if shape == 0 || shape > wp_cursor_shape_device_v1::shape::ALL_SCROLL {
            self.fail(Fatal::Interface {
                object: ObjectId::DISPLAY,
                code: wp_cursor_shape_device_v1::error::INVALID_SHAPE,
                text: format!("{shape} is not a cursor shape"),
            });
            return;
        }
        self.cursor = None;
        self.said_cursor = true;
        self.events.push(Event::CursorShaped { shape });
    }

    /// `zwp_primary_selection_device_manager_v1`: `create_source` and
    /// `get_device`.
    fn primary_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        match opcode {
            zwp_primary_selection_device_manager_v1::request::CREATE_SOURCE => {
                if self.make(
                    id,
                    &primary_selection::ZWP_PRIMARY_SELECTION_SOURCE_V1,
                    version,
                    Role::PrimarySource,
                ) {
                    let _ = self.primary_sources.insert(id, Vec::new());
                }
            }
            zwp_primary_selection_device_manager_v1::request::GET_DEVICE => {
                if !self.make(
                    id,
                    &primary_selection::ZWP_PRIMARY_SELECTION_DEVICE_V1,
                    version,
                    Role::PrimaryDevice,
                ) {
                    return;
                }
                self.primary_devices.push(id);
                self.events.push(Event::PrimaryDeviceMade { device: id });
            }
            _ => {}
        }
    }

    /// `zwp_primary_selection_source_v1`: the types it is offering.
    fn primary_source(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_primary_selection_source_v1::request::OFFER {
            return;
        }
        let Some(mime) = args.first().and_then(Arg::as_str) else {
            return;
        };
        if let Some(mimes) = self.primary_sources.get_mut(&sender) {
            mimes.push(mime.to_owned());
        }
    }

    /// `zwp_primary_selection_device_v1`: `set_selection`.
    ///
    /// The primary selection is what a middle click pastes, and it is set by
    /// *selecting* rather than by asking: no serial is checked here for the
    /// same reason the clipboard's is not.
    fn primary_device(&mut self, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_primary_selection_device_v1::request::SET_SELECTION {
            return;
        }
        let source = args.first().and_then(Arg::as_object);
        let mimes = source
            .and_then(|source| self.primary_sources.get(&source))
            .cloned()
            .unwrap_or_default();
        self.events.push(Event::PrimarySet {
            source: source.filter(|source| !source.is_null()),
            mimes,
        });
    }

    /// `zwp_primary_selection_offer_v1`: `receive`, which is a paste.
    fn primary_offer(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_primary_selection_offer_v1::request::RECEIVE {
            return;
        }
        let (Some(mime), Some(fd)) = (
            args.first().and_then(Arg::as_str),
            args.get(1).and_then(Arg::as_fd),
        ) else {
            return;
        };
        self.events.push(Event::PrimaryWanted {
            offer: sender,
            mime: mime.to_owned(),
            fd,
        });
    }

    /// `xdg_activation_v1`: `get_activation_token` and `activate`.
    ///
    /// A program that wants another raised asks for a token, hands it over
    /// by whatever means it has -- an environment variable, a command line
    /// -- and the other program passes it to `activate`. The token is a
    /// string the compositor makes and only it can make, which is what stops
    /// any program stealing the focus whenever it likes.
    fn activation(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        match opcode {
            xdg_activation_v1::request::GET_ACTIVATION_TOKEN => {
                let Some(id) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                let _ = self.make(
                    id,
                    &xdg_activation::XDG_ACTIVATION_TOKEN_V1,
                    version,
                    Role::ActivationToken,
                );
            }
            xdg_activation_v1::request::ACTIVATE => {
                let (Some(token), Some(surface)) = (
                    args.first().and_then(Arg::as_str),
                    args.get(1).and_then(Arg::as_object),
                ) else {
                    return;
                };
                self.events.push(Event::ActivationAsked {
                    token: token.to_owned(),
                    surface,
                });
            }
            _ => {}
        }
    }

    /// `xdg_activation_token_v1`: `commit` is where the token is handed
    /// back.
    ///
    /// `set_serial`, `set_app_id` and `set_surface` say who is asking and
    /// why; they are read and the token is given whatever they said, because
    /// this compositor grants an activation to whoever has a token it made.
    fn activation_token(&mut self, sender: ObjectId, opcode: u16, _args: &[Arg<'_>]) {
        if opcode != xdg_activation_token_v1::request::COMMIT {
            return;
        }
        // One token a request, made here and never twice: a client that
        // could guess another's token could steal the focus.
        self.token = self.token.wrapping_add(1);
        let token = format!("hyprix-{}-{}", self.serial, self.token);
        let _ = self.tokens.insert(token.clone());
        let _ = self.out.write(
            sender,
            xdg_activation_token_v1::event::DONE,
            &[ArgType::Str { nullable: false }],
            &[Arg::Str(Some(&token))],
        );
    }

    /// Whether `token` is one this client was given and has not used.
    #[must_use]
    pub fn takes_token(&mut self, token: &str) -> bool {
        self.tokens.remove(token)
    }

    /// `wp_viewporter`: `get_viewport`.
    fn viewporter(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != wp_viewporter::request::GET_VIEWPORT {
            return;
        }
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
        if self.viewports.values().any(|held| *held == surface) {
            self.fail(Fatal::Interface {
                object: surface,
                code: wp_viewporter::error::VIEWPORT_EXISTS,
                text: "that surface already has a viewport".to_owned(),
            });
            return;
        }
        if self.make(id, &viewporter::WP_VIEWPORT, version, Role::Viewport) {
            let _ = self.viewports.insert(id, surface);
        }
    }

    /// `wp_viewport`: `set_source` and `set_destination`.
    ///
    /// The crop and the scale a surface's buffer is drawn with. Both are
    /// recorded on the surface and applied where the surface's size is
    /// worked out, which is the one place a buffer's size and a window's
    /// stop being the same number.
    fn viewport(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        let Some(surface) = self.viewports.get(&sender).copied() else {
            return;
        };
        match opcode {
            wp_viewport::request::SET_SOURCE => {
                let numbers: Vec<Fixed> = args.iter().filter_map(Arg::as_fixed).collect();
                let [x, y, width, height] = numbers.as_slice() else {
                    return;
                };
                if let Some(state) = self.surfaces.get_mut(&surface) {
                    state.pending.viewport_source = (width.to_f64() > 0.0)
                        .then(|| (x.to_f64(), y.to_f64(), width.to_f64(), height.to_f64()));
                }
            }
            wp_viewport::request::SET_DESTINATION => {
                let numbers: Vec<i32> = args.iter().filter_map(Arg::as_int).collect();
                let [width, height] = numbers.as_slice() else {
                    return;
                };
                if *width <= 0 && *width != -1 {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: wp_viewport::error::BAD_VALUE,
                        text: format!("a destination of {width}x{height}"),
                    });
                    return;
                }
                if let Some(state) = self.surfaces.get_mut(&surface) {
                    state.pending.viewport_size = (*width > 0).then_some((*width, *height));
                }
            }
            wp_viewport::request::DESTROY => {
                let _ = self.viewports.remove(&sender);
            }
            _ => {}
        }
    }

    /// `wp_fractional_scale_manager_v1`: `get_fractional_scale`.
    ///
    /// The scale is sent at once, as the protocol allows: this compositor's
    /// monitor scales are whole numbers, so the preferred scale is that
    /// number in the protocol's 120ths and a client that asked is told
    /// rather than left waiting.
    fn fractional_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != wp_fractional_scale_manager_v1::request::GET_FRACTIONAL_SCALE {
            return;
        }
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
        if !self.make(
            id,
            &fractional_scale::WP_FRACTIONAL_SCALE_V1,
            version,
            Role::FractionalScale,
        ) {
            return;
        }
        let _ = self.fractional.insert(id, surface);
        let scale = self.outputs.first().map_or(1, |output| output.scale.max(1));
        let _ = self.out.write(
            id,
            wp_fractional_scale_v1::event::PREFERRED_SCALE,
            &[ArgType::Uint],
            // 120ths, which is the protocol's own unit.
            &[Arg::Uint(
                u32::try_from(scale).unwrap_or(1).saturating_mul(120),
            )],
        );
    }

    /// `xdg_toplevel_icon_manager_v1`: `create_icon` and `set_icon`.
    ///
    /// The sizes are announced at bind and the icon is taken as given: what
    /// a compositor does with one is draw it in a taskbar, and this one has
    /// no taskbar of its own -- the bar is a client, and what it draws is
    /// its own business. Answering rather than refusing is what matters: a
    /// toolkit that finds no manager logs a warning on every start.
    fn icon_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        match opcode {
            xdg_toplevel_icon_manager_v1::request::CREATE_ICON => {
                let Some(id) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                let _ = self.make(
                    id,
                    &toplevel_icon::XDG_TOPLEVEL_ICON_V1,
                    version,
                    Role::Icon,
                );
            }
            xdg_toplevel_icon_manager_v1::request::SET_ICON => {
                let (Some(toplevel), icon) = (
                    args.first().and_then(Arg::as_object),
                    args.get(1).and_then(Arg::as_object),
                ) else {
                    return;
                };
                let name = icon
                    .and_then(|icon| self.icons.get(&icon))
                    .cloned()
                    .unwrap_or_default();
                if let Some(top) = self.toplevels.get_mut(&toplevel) {
                    top.icon = name;
                }
            }
            _ => {}
        }
    }

    /// `xdg_toplevel_icon_v1`: `set_name` and `add_buffer`.
    ///
    /// The name is kept, which is what a taskbar looks up in an icon theme.
    /// The buffers are accepted and not kept: this compositor draws no icon
    /// itself, and holding a client's pixels for something nobody draws is
    /// memory nobody asked for.
    fn icon(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != xdg_toplevel_icon_v1::request::SET_NAME {
            return;
        }
        if let Some(name) = args.first().and_then(Arg::as_str) {
            let _ = self.icons.insert(sender, name.to_owned());
        }
    }

    /// `zwp_text_input_manager_v3`: `get_text_input`.
    fn text_input_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_text_input_manager_v3::request::GET_TEXT_INPUT {
            return;
        }
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        if self.make(id, &text_input::ZWP_TEXT_INPUT_V3, version, Role::TextInput) {
            self.text_inputs.push(id);
        }
    }

    /// `zwp_text_input_v3`: an application saying it wants to be typed
    /// into, and what it is being typed into.
    ///
    /// `enable` and `disable` are the two that matter to the compositor:
    /// they are what an input method is told about, as `activate` and
    /// `deactivate`. The rest -- the surrounding text, the content type,
    /// where the cursor is on the screen -- is passed on so that an input
    /// method can put its candidate window in the right place and guess the
    /// right word.
    fn text_input(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        use zwp_text_input_v3::request;
        match opcode {
            request::ENABLE => self.events.push(Event::TextInputEnabled {
                text_input: sender,
                enabled: true,
            }),
            request::DISABLE => self.events.push(Event::TextInputEnabled {
                text_input: sender,
                enabled: false,
            }),
            request::SET_SURROUNDING_TEXT => {
                let text = args.first().and_then(Arg::as_str).unwrap_or("").to_owned();
                let cursor = args.get(1).and_then(Arg::as_int).unwrap_or(0);
                let anchor = args.get(2).and_then(Arg::as_int).unwrap_or(0);
                self.events.push(Event::TextInputSurrounded {
                    text_input: sender,
                    text,
                    cursor,
                    anchor,
                });
            }
            request::SET_CURSOR_RECTANGLE => {
                let numbers: Vec<i32> = args.iter().filter_map(Arg::as_int).collect();
                let [x, y, width, height] = numbers.as_slice() else {
                    return;
                };
                self.events.push(Event::TextInputCursorAt {
                    text_input: sender,
                    rect: Rect {
                        x: *x,
                        y: *y,
                        width: *width,
                        height: *height,
                    },
                });
            }
            request::COMMIT => self
                .events
                .push(Event::TextInputCommitted { text_input: sender }),
            request::DESTROY => {
                self.text_inputs.retain(|held| *held != sender);
                self.events.push(Event::TextInputEnabled {
                    text_input: sender,
                    enabled: false,
                });
            }
            _ => {}
        }
    }

    /// `zwp_input_method_manager_v2`: `get_input_method`.
    ///
    /// One input method a seat: a second is made and told `unavailable` at
    /// once, which is what the protocol says and what stops two programs
    /// both believing they are the keyboard.
    fn input_method_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_input_method_manager_v2::request::GET_INPUT_METHOD {
            return;
        }
        let Some(id) = args.get(1).and_then(Arg::as_object) else {
            return;
        };
        if !self.make(
            id,
            &input_method::ZWP_INPUT_METHOD_V2,
            version,
            Role::InputMethod,
        ) {
            return;
        }
        self.input_method = Some(id);
        self.events.push(Event::InputMethodMade { method: id });
    }

    /// `zwp_input_method_v2`: what the input method has typed.
    ///
    /// `commit_string`, `set_preedit_string` and `delete_surrounding_text`
    /// are staged and applied by `commit`, which is the same double-buffered
    /// shape a `wl_surface` has and for the same reason: a method that
    /// replaced a word should not be seen half way through.
    fn input_method(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        use zwp_input_method_v2::request;
        match opcode {
            request::COMMIT_STRING => {
                self.typed.commit = args.first().and_then(Arg::as_str).map(str::to_owned);
            }
            request::SET_PREEDIT_STRING => {
                let text = args.first().and_then(Arg::as_str).unwrap_or("").to_owned();
                let begin = args.get(1).and_then(Arg::as_int).unwrap_or(0);
                let end = args.get(2).and_then(Arg::as_int).unwrap_or(0);
                self.typed.preedit = Some((text, begin, end));
            }
            request::DELETE_SURROUNDING_TEXT => {
                let before = args.first().and_then(Arg::as_uint).unwrap_or(0);
                let after = args.get(1).and_then(Arg::as_uint).unwrap_or(0);
                self.typed.delete = Some((before, after));
            }
            request::COMMIT => {
                let typed = std::mem::take(&mut self.typed);
                self.events.push(Event::InputMethodTyped {
                    method: sender,
                    typed,
                });
            }
            request::DESTROY => {
                if self.input_method == Some(sender) {
                    self.input_method = None;
                }
                self.events.push(Event::InputMethodGone { method: sender });
            }
            _ => {}
        }
    }

    /// Tell an input method that a text field has been focused, or has gone.
    pub fn input_method_active(&mut self, method: ObjectId, active: bool) {
        let opcode = if active {
            zwp_input_method_v2::event::ACTIVATE
        } else {
            zwp_input_method_v2::event::DEACTIVATE
        };
        let _ = self.out.write(method, opcode, &[], &[]);
        let _ = self
            .out
            .write(method, zwp_input_method_v2::event::DONE, &[], &[]);
    }

    /// Tell an input method it will never be the seat's: another already is.
    pub fn input_method_unavailable(&mut self, method: ObjectId) {
        let _ = self
            .out
            .write(method, zwp_input_method_v2::event::UNAVAILABLE, &[], &[]);
    }

    /// Give a text field what an input method typed.
    ///
    /// The order is the protocol's: what is deleted, then the preedit, then
    /// the commit, then `done` -- which is what applies all three at once.
    pub fn text_input_typed(&mut self, text_input: ObjectId, typed: &Typed, serial: u32) {
        if let Some((before, after)) = typed.delete {
            let _ = self.out.write(
                text_input,
                zwp_text_input_v3::event::DELETE_SURROUNDING_TEXT,
                &[ArgType::Uint, ArgType::Uint],
                &[Arg::Uint(before), Arg::Uint(after)],
            );
        }
        if let Some((text, begin, end)) = typed.preedit.as_ref() {
            let _ = self.out.write(
                text_input,
                zwp_text_input_v3::event::PREEDIT_STRING,
                &[ArgType::Str { nullable: true }, ArgType::Int, ArgType::Int],
                &[Arg::Str(Some(text)), Arg::Int(*begin), Arg::Int(*end)],
            );
        }
        if let Some(text) = typed.commit.as_ref() {
            let _ = self.out.write(
                text_input,
                zwp_text_input_v3::event::COMMIT_STRING,
                &[ArgType::Str { nullable: true }],
                &[Arg::Str(Some(text))],
            );
        }
        let _ = self.out.write(
            text_input,
            zwp_text_input_v3::event::DONE,
            &[ArgType::Uint],
            &[Arg::Uint(serial)],
        );
    }

    /// Tell a text field that an input method is there, or is gone.
    pub fn text_input_focus(&mut self, text_input: ObjectId, surface: ObjectId, entered: bool) {
        let opcode = if entered {
            zwp_text_input_v3::event::ENTER
        } else {
            zwp_text_input_v3::event::LEAVE
        };
        let _ = self.out.write(
            text_input,
            opcode,
            &[ArgType::Object { nullable: false }],
            &[Arg::Object(surface)],
        );
    }

    /// Every `zwp_text_input_v3` this client has made.
    #[must_use]
    pub fn text_inputs(&self) -> &[ObjectId] {
        &self.text_inputs
    }

    /// The `zwp_input_method_v2` this client holds, if it is the input
    /// method.
    #[must_use]
    pub const fn input_method_object(&self) -> Option<ObjectId> {
        self.input_method
    }

    /// `xdg_positioner`: the numbers a popup is placed with.
    ///
    /// Each is recorded and none is acted on: where a popup goes is worked
    /// out once, when `get_popup` reads the positioner, because the protocol
    /// says a positioner is a value copied at that moment and a change after
    /// it moves nothing.
    fn positioner(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        use xdg_shell::xdg_positioner::request;
        let numbers: Vec<i32> = args.iter().filter_map(Arg::as_int).collect();
        let Some(held) = self.positioners.get_mut(&sender) else {
            return;
        };
        match opcode {
            request::SET_SIZE => {
                let [width, height] = numbers.as_slice() else {
                    return;
                };
                if *width <= 0 || *height <= 0 {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_shell::xdg_positioner::error::INVALID_INPUT,
                        text: format!("a popup of {width}x{height}"),
                    });
                    return;
                }
                held.size = (*width, *height);
            }
            request::SET_ANCHOR_RECT => {
                let [x, y, width, height] = numbers.as_slice() else {
                    return;
                };
                if *width <= 0 || *height <= 0 {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_shell::xdg_positioner::error::INVALID_INPUT,
                        text: format!("an anchor rectangle of {width}x{height}"),
                    });
                    return;
                }
                held.anchor_rect = (*x, *y, *width, *height);
            }
            request::SET_ANCHOR | request::SET_GRAVITY => {
                let Some(value) = args.first().and_then(Arg::as_uint) else {
                    return;
                };
                if value > 8 {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_shell::xdg_positioner::error::INVALID_INPUT,
                        text: format!("{value} is not an anchor or a gravity"),
                    });
                    return;
                }
                if opcode == request::SET_ANCHOR {
                    held.anchor = value;
                } else {
                    held.gravity = value;
                }
            }
            request::SET_CONSTRAINT_ADJUSTMENT => {
                held.adjust = args.first().and_then(Arg::as_uint).unwrap_or(0);
            }
            request::SET_OFFSET => {
                let [x, y] = numbers.as_slice() else {
                    return;
                };
                held.offset = (*x, *y);
            }
            request::SET_REACTIVE => held.reactive = true,
            // `set_parent_size` and `set_parent_configure` are for a
            // reactive popup whose parent is being resized: this compositor
            // places a popup against the parent's geometry as it is, so both
            // are read and nothing is kept.
            _ => {}
        }
    }

    /// `xdg_popup`: `grab`, `reposition` and `destroy`.
    fn popup_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        use xdg_shell::xdg_popup::request;
        match opcode {
            request::GRAB => {
                if let Some(popup) = self.popups.get_mut(&sender) {
                    popup.grabbed = true;
                }
                self.events.push(Event::PopupGrabbed { popup: sender });
            }
            request::REPOSITION => {
                let held = args
                    .first()
                    .and_then(Arg::as_object)
                    .and_then(|id| self.positioners.get(&id))
                    .copied();
                let token = args.get(1).and_then(Arg::as_uint).unwrap_or(0);
                if let (Some(held), Some(popup)) = (held, self.popups.get_mut(&sender)) {
                    popup.positioner = held;
                    popup.placed = None;
                }
                // `repositioned` goes before the `configure` the compositor
                // will send, which is what tells the client the configure
                // that follows is the answer to this request and not to
                // something else.
                let _ = self.out.write(
                    sender,
                    xdg_shell::xdg_popup::event::REPOSITIONED,
                    &[ArgType::Uint],
                    &[Arg::Uint(token)],
                );
                let (surface, parent) = self
                    .popups
                    .get(&sender)
                    .map_or((ObjectId::NULL, ObjectId::NULL), |popup| {
                        (popup.surface, popup.parent)
                    });
                self.events.push(Event::PopupCreated {
                    popup: sender,
                    surface,
                    parent,
                });
            }
            request::DESTROY => {
                let _ = self.popups.remove(&sender);
                self.events.push(Event::PopupGone { popup: sender });
            }
            _ => {}
        }
    }

    /// Tell a popup where it is, and its `xdg_surface` that the state is
    /// whole.
    ///
    /// `x` and `y` are in the parent's surface-local coordinates, which is
    /// what the protocol says `xdg_popup.configure` carries.
    pub fn configure_popup(&mut self, popup: ObjectId, at: (i32, i32, i32, i32)) {
        let Some(held) = self.popups.get_mut(&popup) else {
            return;
        };
        held.placed = Some(at);
        let xdg_surface = held.xdg_surface;
        let (x, y, width, height) = at;
        let _ = self.out.write(
            popup,
            xdg_shell::xdg_popup::event::CONFIGURE,
            &[ArgType::Int, ArgType::Int, ArgType::Int, ArgType::Int],
            &[Arg::Int(x), Arg::Int(y), Arg::Int(width), Arg::Int(height)],
        );
        let serial = self.next_serial();
        let _ = self.out.write(
            xdg_surface,
            xdg_surface::event::CONFIGURE,
            &[ArgType::Uint],
            &[Arg::Uint(serial)],
        );
        if let Some(surface) = self.xdg_surfaces.get_mut(&xdg_surface) {
            surface.configure_sent(serial);
        }
    }

    /// Tell a popup it has been dismissed.
    ///
    /// The client is to destroy it: a menu that was clicked away is gone,
    /// and the compositor says so rather than waiting to be told.
    pub fn popup_done(&mut self, popup: ObjectId) {
        if !self.popups.contains_key(&popup) {
            return;
        }
        let _ = self
            .out
            .write(popup, xdg_shell::xdg_popup::event::POPUP_DONE, &[], &[]);
    }

    /// One popup, if this client has it.
    #[must_use]
    pub fn popup(&self, popup: ObjectId) -> Option<&Popup> {
        self.popups.get(&popup)
    }

    /// Every popup it has, in the order they were made.
    pub fn popups(&self) -> impl Iterator<Item = (ObjectId, &Popup)> {
        self.popups.iter().map(|(id, popup)| (*id, popup))
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

    /// `zxdg_decoration_manager_v1`: `get_toplevel_decoration`.
    ///
    /// A tiling compositor draws the border and the client draws nothing, so
    /// the answer is always `server_side` and it is sent at once rather than
    /// waited for: the protocol says a compositor may configure a decoration
    /// as soon as it is made, and a client that asked for `client_side` and
    /// is told `server_side` draws no title bar, which is the point of
    /// offering this at all.
    ///
    /// Offering it matters more than it looks. A toolkit that finds no
    /// `zxdg_decoration_manager_v1` assumes client-side decorations and
    /// draws a title bar, a shadow and a resize border into its own surface
    /// -- inside the rectangle the tiling gave it.
    fn decoration_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zxdg_decoration_manager_v1::request::GET_TOPLEVEL_DECORATION {
            return;
        }
        let (Some(id), Some(toplevel)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
        ) else {
            return;
        };
        if !self.toplevels.contains_key(&toplevel) {
            self.fail(Fatal::WrongInterface {
                object: toplevel,
                wanted: "xdg_toplevel",
            });
            return;
        }
        if !self.make(
            id,
            &xdg_decoration::ZXDG_TOPLEVEL_DECORATION_V1,
            version,
            Role::ToplevelDecoration,
        ) {
            return;
        }
        self.configure_decoration(id);
    }

    /// `zxdg_toplevel_decoration_v1`: `set_mode` and `unset_mode`.
    ///
    /// Both are answered the same way, because the answer does not depend on
    /// what was asked: this compositor draws the decorations.
    fn toplevel_decoration(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        use zxdg_toplevel_decoration_v1::request;
        match opcode {
            request::SET_MODE => {
                let mode = args.first().and_then(Arg::as_uint).unwrap_or(0);
                if mode != zxdg_toplevel_decoration_v1::mode::CLIENT_SIDE
                    && mode != zxdg_toplevel_decoration_v1::mode::SERVER_SIDE
                {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: zxdg_toplevel_decoration_v1::error::INVALID_MODE,
                        text: format!("{mode} is not a decoration mode"),
                    });
                    return;
                }
                self.configure_decoration(sender);
            }
            request::UNSET_MODE => self.configure_decoration(sender),
            _ => {}
        }
    }

    /// Tell a decoration it is the compositor's to draw.
    fn configure_decoration(&mut self, decoration: ObjectId) {
        let _ = self.out.write(
            decoration,
            zxdg_toplevel_decoration_v1::event::CONFIGURE,
            &[ArgType::Uint],
            &[Arg::Uint(zxdg_toplevel_decoration_v1::mode::SERVER_SIDE)],
        );
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

    /// `wl_data_source`: the types a client is offering, and the object
    /// going away.
    ///
    /// A source is made before it is offered and offered before it is set as
    /// the selection, so the types are collected here and read when
    /// `set_selection` arrives.
    fn data_source_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        // `set_actions` is drag-and-drop's, which this compositor does not
        // do: a client that asks is not refused, since the protocol allows
        // the request on a selection source too.
        if opcode != wl_data_source::request::OFFER {
            return;
        }
        let Some(mime) = args.first().and_then(Arg::as_str) else {
            return;
        };
        let offered = self.sources.entry(sender).or_default();
        if !offered.iter().any(|known| known == mime) {
            offered.push(mime.to_owned());
        }
    }

    /// `wl_data_device`: the selection, and the drag this compositor does
    /// not do.
    fn data_device_request(&mut self, opcode: u16, args: &[Arg<'_>]) {
        // `start_drag` is drag-and-drop's. Nothing is dragged here, and a
        // client that asks is answered with nothing rather than an error:
        // the protocol's own answer to a drag that does not start is no
        // `enter` and no `drop`.
        if opcode != wl_data_device::request::SET_SELECTION {
            return;
        }
        // A null source clears the selection, which is what a client sends
        // when it no longer owns what it copied.
        let source = args.first().and_then(Arg::as_object);
        let mimes = source
            .and_then(|source| self.sources.get(&source).cloned())
            .unwrap_or_default();
        self.events.push(Event::SelectionSet {
            source: source.filter(|source| !source.is_null()),
            mimes,
        });
    }

    /// `wl_data_offer`: what a client does with the selection it was told
    /// about.
    fn data_offer_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        // `accept` and `finish` are the drag's; `set_actions` too.
        if opcode != wl_data_offer::request::RECEIVE {
            return;
        }
        let (Some(mime), Some(fd)) = (
            args.first().and_then(Arg::as_str),
            args.get(1).and_then(Arg::as_fd),
        ) else {
            return;
        };
        self.events.push(Event::SelectionWanted {
            offer: sender,
            mime: mime.to_owned(),
            fd,
        });
    }

    /// `xdg_wm_base`: `create_positioner`, `get_xdg_surface` and `pong`.
    fn xdg_wm_base(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        match opcode {
            xdg_wm_base::request::CREATE_POSITIONER => {
                let Some(id) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                // A positioner is a bag of numbers a popup reads, and it
                // starts empty: the protocol requires `set_size` and
                // `set_anchor_rect` before `get_popup` uses it.
                if self.make(id, &xdg_shell::XDG_POSITIONER, version, Role::XdgPositioner) {
                    let _ = self.positioners.insert(id, Positioner::default());
                }
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
                // The parent is nullable in the protocol -- an
                // `xdg_positioner` with a parent set by another extension
                // may carry it -- and this compositor has no such extension,
                // so a null parent is a popup with nothing to hang off.
                let (parent, positioner) = (
                    args.get(1).and_then(Arg::as_object),
                    args.get(2).and_then(Arg::as_object),
                );
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
                let Some(parent) = parent.filter(|parent| !parent.is_null()) else {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_wm_base::error::INVALID_POPUP_PARENT,
                        text: "a popup with no parent".to_owned(),
                    });
                    return;
                };
                if !self.xdg_surfaces.contains_key(&parent) {
                    self.fail(Fatal::WrongInterface {
                        object: parent,
                        wanted: "xdg_surface",
                    });
                    return;
                }
                let Some(held) = positioner.and_then(|id| self.positioners.get(&id)).copied()
                else {
                    self.fail(Fatal::WrongInterface {
                        object: positioner.unwrap_or(ObjectId::NULL),
                        wanted: "xdg_positioner",
                    });
                    return;
                };
                // A positioner without a size or an anchor rectangle is the
                // one error a popup can earn before it exists.
                if !held.is_complete() {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_wm_base::error::INVALID_POSITIONER,
                        text: "the positioner has no size or no anchor rectangle".to_owned(),
                    });
                    return;
                }
                if !self.make(id, &xdg_shell::XDG_POPUP, version, Role::XdgPopup) {
                    return;
                }
                if let Some(xdg) = self.xdg_surfaces.get_mut(&sender) {
                    xdg.role = Some(XdgRole::Popup(id));
                }
                let _ = self.popups.insert(
                    id,
                    Popup {
                        surface,
                        xdg_surface: sender,
                        parent,
                        positioner: held,
                        placed: None,
                        grabbed: false,
                    },
                );
                self.events.push(Event::PopupCreated {
                    popup: id,
                    surface,
                    parent,
                });
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
                Arg::Int(wl_output::transform::NORMAL.cast_signed()),
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
                (
                    wl_output::event::DESCRIPTION,
                    "the Ferrix compositor's output",
                ),
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

    /// Tell this client what the selection holds.
    ///
    /// The server makes a `wl_data_offer` of its own -- an id out of the
    /// server's half of the space, which is what that half is for -- says
    /// which types it has, and then names it as the selection. That is the
    /// order `wl_data_device`'s description gives, and a client that reads
    /// them in any other order sees an offer for a selection it has not been
    /// told about.
    ///
    /// An empty `mimes` clears the selection, which is what a client sees
    /// when whoever copied has gone.
    pub fn offer_selection(&mut self, mimes: &[String]) {
        if self.devices.is_empty() {
            return;
        }
        // The offer this client had is replaced, as the protocol says a new
        // selection replaces the last.
        let offer = if mimes.is_empty() {
            None
        } else {
            let version = self
                .devices
                .first()
                .and_then(|device| self.objects.get(*device))
                .map_or(3, |entry| entry.version);
            let Ok(offer) = self
                .objects
                .create(&core::WL_DATA_OFFER, version, Role::DataOffer)
            else {
                return;
            };
            Some(offer)
        };
        let devices = self.devices.clone();
        for device in devices {
            if let Some(offer) = offer {
                let _ = self.out.write(
                    device,
                    wl_data_device::event::DATA_OFFER,
                    &[ArgType::NewId],
                    &[Arg::NewId(offer)],
                );
                for mime in mimes {
                    let _ = self.out.write(
                        offer,
                        wl_data_offer::event::OFFER,
                        &[ArgType::Str { nullable: false }],
                        &[Arg::Str(Some(mime))],
                    );
                }
            }
            let _ = self.out.write(
                device,
                wl_data_device::event::SELECTION,
                &[ArgType::Object { nullable: true }],
                &[Arg::Object(offer.unwrap_or(ObjectId::NULL))],
            );
        }
        self.offer = offer;
    }

    /// Whether `offer` is the offer this client was last given.
    #[must_use]
    pub fn holds_offer(&self, offer: ObjectId) -> bool {
        self.offer == Some(offer)
    }

    /// Ask this client's source for the selection's data on `fd`.
    ///
    /// The client writes what it copied and closes the descriptor; whoever
    /// pasted reads until end of file. Nothing here touches the data.
    pub fn send_selection(&mut self, source: ObjectId, mime: &str, fd: Fd) {
        let _ = self.out.write(
            source,
            wl_data_source::event::SEND,
            &[ArgType::Str { nullable: false }, ArgType::Fd],
            &[Arg::Str(Some(mime)), Arg::Fd(fd)],
        );
    }

    /// Tell this client's source that it is no longer the selection.
    pub fn cancel_selection(&mut self, source: ObjectId) {
        let _ = self
            .out
            .write(source, wl_data_source::event::CANCELLED, &[], &[]);
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

    /// Tell this client what the primary selection holds.
    ///
    /// The same shape as [`Client::offer_selection`], because the primary
    /// selection is the same protocol with a different name: an offer is
    /// made, its types are sent, and it is then named as the selection.
    pub fn offer_primary(&mut self, mimes: &[String]) {
        if self.primary_devices.is_empty() {
            return;
        }
        let offer = if mimes.is_empty() {
            None
        } else {
            let Ok(offer) = self.objects.create(
                &primary_selection::ZWP_PRIMARY_SELECTION_OFFER_V1,
                1,
                Role::PrimaryOffer,
            ) else {
                return;
            };
            Some(offer)
        };
        let devices = self.primary_devices.clone();
        for device in devices {
            if let Some(offer) = offer {
                let _ = self.out.write(
                    device,
                    zwp_primary_selection_device_v1::event::DATA_OFFER,
                    &[ArgType::NewId],
                    &[Arg::NewId(offer)],
                );
                for mime in mimes {
                    let _ = self.out.write(
                        offer,
                        zwp_primary_selection_offer_v1::event::OFFER,
                        &[ArgType::Str { nullable: false }],
                        &[Arg::Str(Some(mime))],
                    );
                }
            }
            let _ = self.out.write(
                device,
                zwp_primary_selection_device_v1::event::SELECTION,
                &[ArgType::Object { nullable: true }],
                &[Arg::Object(offer.unwrap_or(ObjectId::NULL))],
            );
        }
        self.primary_offer = offer;
    }

    /// Whether `offer` is the primary offer this client was last given.
    #[must_use]
    pub fn holds_primary_offer(&self, offer: ObjectId) -> bool {
        self.primary_offer == Some(offer)
    }

    /// Ask this client's primary source for its data on `fd`.
    pub fn send_primary(&mut self, source: ObjectId, mime: &str, fd: Fd) {
        let _ = self.out.write(
            source,
            zwp_primary_selection_source_v1::event::SEND,
            &[ArgType::Str { nullable: false }, ArgType::Fd],
            &[Arg::Str(Some(mime)), Arg::Fd(fd)],
        );
    }

    /// Tell this client's primary source that it is no longer the
    /// selection.
    pub fn cancel_primary(&mut self, source: ObjectId) {
        let _ = self.out.write(
            source,
            zwp_primary_selection_source_v1::event::CANCELLED,
            &[],
            &[],
        );
    }

    /// Whether this client can paste the primary selection.
    #[must_use]
    pub fn has_primary_device(&self) -> bool {
        !self.primary_devices.is_empty()
    }

    /// Whether this client has a `wl_data_device`, which is what a client
    /// that can paste has.
    #[must_use]
    pub fn has_data_device(&self) -> bool {
        !self.devices.is_empty()
    }

    /// Give a fresh `wl_keyboard` the keymap, or say there is none.
    ///
    /// `wl_keyboard.keymap` must be sent before anything else, and a client
    /// that is given `no_keymap` knows it will be told raw keycodes it cannot
    /// name. That is what a compositor with no keymap yet should say, rather
    /// than sending a descriptor that is not one.
    fn send_keymap(&mut self, id: ObjectId) {
        let signature = &[ArgType::Uint, ArgType::Fd, ArgType::Uint];
        match self.keymap {
            Some((fd, size)) => {
                let _ = self.out.write(
                    id,
                    core::wl_keyboard::event::KEYMAP,
                    signature,
                    &[
                        Arg::Uint(core::wl_keyboard::keymap_format::XKB_V1),
                        Arg::Fd(fd),
                        Arg::Uint(size),
                    ],
                );
            }
            None => {
                // The protocol has no way to send nothing, so `no_keymap`
                // goes with a descriptor the client will not map and a size
                // of zero. libwayland's own compositors do the same.
                let _ = self.out.write(
                    id,
                    core::wl_keyboard::event::KEYMAP,
                    signature,
                    &[
                        Arg::Uint(core::wl_keyboard::keymap_format::NO_KEYMAP),
                        Arg::Fd(Fd(-1)),
                        Arg::Uint(0),
                    ],
                );
            }
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
