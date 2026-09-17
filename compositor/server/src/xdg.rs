//! `xdg_shell`: how a surface becomes a window.
//!
//! A `wl_surface` on its own is a rectangle of pixels with nowhere to be.
//! `xdg_wm_base.get_xdg_surface` gives it the beginnings of a window, and
//! `xdg_surface.get_toplevel` makes it one; from then on the compositor and
//! the client agree on its size by a conversation rather than either one
//! deciding.
//!
//! # The configure conversation
//!
//! The compositor sends `xdg_toplevel.configure` with a size and a set of
//! states, then `xdg_surface.configure` with a serial. The client draws at
//! that size, sends `ack_configure` with the serial back, and commits. Until
//! it has acked and committed once, the surface may not carry a buffer at
//! all: attaching one to an unconfigured `xdg_surface` is
//! `unconfigured_buffer`, and it is the check that stops a client painting
//! at a size the compositor never agreed to.
//!
//! Serials go up and are never reused. A client may ack an old one -- it may
//! be several configures behind -- so acking anything the compositor has sent
//! and not yet seen acked is accepted, and anything else is `invalid_serial`.
//!
//! # A tiling compositor configures, it does not negotiate
//!
//! Hyprland tells a window its size; `set_max_size` and `set_min_size` are
//! recorded and otherwise ignored for a tiled window, as Hyprland's own
//! `CXDGToplevelResource` records them and its layout ignores them. The size
//! in a configure is the layout's.

use compositor_wire::ObjectId;

/// What a client has said about a toplevel window.
///
/// The title and the app id are what `hyprctl clients` prints and what
/// `windowrule` matches on, so they are kept even though nothing is drawn
/// from them yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Toplevel {
    /// The `xdg_surface` it was made from.
    pub xdg_surface: ObjectId,
    /// The `wl_surface` under that.
    pub surface: ObjectId,
    /// `set_title`.
    pub title: String,
    /// `set_app_id`, which is what a `windowrule` matches on.
    pub app_id: String,
    /// `xdg_toplevel_icon_v1.set_name`: what a taskbar looks up in an icon
    /// theme to draw beside this window's name.
    pub icon: String,
    /// `set_parent`, for a dialog.
    pub parent: Option<ObjectId>,
    /// `set_min_size`, recorded and not obeyed for a tiled window.
    pub min_size: (i32, i32),
    /// `set_max_size`, the same.
    pub max_size: (i32, i32),
    /// Whether the client asked to be maximized.
    pub maximized: bool,
    /// Whether it asked to be fullscreen.
    pub fullscreen: bool,
    /// The last size the compositor configured it at.
    pub configured: (i32, i32),
    /// `xdg_dialog_v1.set_modal`: a dialog the application will not let
    /// you look past, which is what floats it here.
    pub modal: bool,
    /// `xdg_toplevel_tag_manager_v1.set_toplevel_tag`: a name the window
    /// keeps across restarts.
    pub tag: String,
    /// `set_toplevel_description`: a sentence about it, for a session
    /// manager and for `hyprctl clients`.
    pub description: String,
    /// The states the last configure carried, in the order they were sent.
    ///
    /// Kept beside the size because a configure is both: a window that has
    /// just been focused is the same size and a different state, and a client
    /// that is not told has a title bar that never lights up.
    pub states: Vec<u32>,
}

/// A `wl_surface` that has been given the beginnings of a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XdgSurface {
    /// The `wl_surface` it wraps.
    pub surface: ObjectId,
    /// The role it was given, once it has one. A surface may be given one
    /// role and never another.
    pub role: Option<XdgRole>,
    /// `set_window_geometry`: the part of the surface that is the window,
    /// leaving out shadows a client draws outside it. `None` until the
    /// client says, in which case the window is the whole surface.
    pub geometry: Option<(i32, i32, i32, i32)>,
    /// Serials sent and not yet acked, oldest first.
    pub unacked: Vec<u32>,
    /// Whether the client has acked a configure and committed since.
    pub configured: bool,
    /// Whether a configure has been sent at all.
    pub sent_configure: bool,
}

/// What an `xdg_surface` was made into.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum XdgRole {
    /// `xdg_toplevel`: a window.
    Toplevel(ObjectId),
    /// `xdg_popup`: a menu, anchored to another surface.
    Popup(ObjectId),
}

/// What an `xdg_positioner` holds, as the wire carries it.
///
/// The numbers and nothing else: where a popup *goes* is arithmetic, and it
/// lives in `compositor/layout` with the rest of the geometry. This crate
/// records what the client said and hands it on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Positioner {
    /// `set_size`.
    pub size: (i32, i32),
    /// `set_anchor_rect`, in the parent's surface-local coordinates.
    pub anchor_rect: (i32, i32, i32, i32),
    /// `set_anchor`.
    pub anchor: u32,
    /// `set_gravity`.
    pub gravity: u32,
    /// `set_constraint_adjustment`.
    pub adjust: u32,
    /// `set_offset`.
    pub offset: (i32, i32),
    /// `set_reactive`.
    pub reactive: bool,
}

impl Positioner {
    /// Whether `set_size` and `set_anchor_rect` have both been given, which
    /// the protocol requires before `get_popup`.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.size.0 > 0 && self.size.1 > 0 && self.anchor_rect.2 > 0 && self.anchor_rect.3 > 0
    }
}

/// One `xdg_popup`: a menu, a tooltip, a dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Popup {
    /// The `wl_surface` its pixels come from.
    pub surface: ObjectId,
    /// The `xdg_surface` it was made from.
    pub xdg_surface: ObjectId,
    /// The `xdg_surface` it hangs off: a window's, or another popup's.
    pub parent: ObjectId,
    /// The numbers it was placed with.
    pub positioner: Positioner,
    /// Where the compositor put it, in the parent's surface-local
    /// coordinates, once it has said.
    pub placed: Option<(i32, i32, i32, i32)>,
    /// Whether the client asked for a grab, which is what makes a menu a
    /// menu: it takes the keyboard until it is dismissed.
    pub grabbed: bool,
}

impl XdgSurface {
    /// A surface just given the beginnings of a window.
    #[must_use]
    pub const fn new(surface: ObjectId) -> Self {
        Self {
            surface,
            role: None,
            geometry: None,
            unacked: Vec::new(),
            configured: false,
            sent_configure: false,
        }
    }

    /// Record a configure the compositor is about to send.
    pub fn configure_sent(&mut self, serial: u32) {
        self.unacked.push(serial);
        self.sent_configure = true;
    }

    /// Take a client's `ack_configure`.
    ///
    /// A client may be several configures behind, so acking any serial still
    /// outstanding is right and drops every older one with it, as
    /// `xdg_surface.ack_configure`'s description says: "If the client
    /// receives multiple configure events before it can respond to one, it
    /// only has to ack the last configure event."
    ///
    /// `false` when the serial is not one that is waiting, which is
    /// `invalid_serial`.
    pub fn ack(&mut self, serial: u32) -> bool {
        let Some(index) = self.unacked.iter().position(|sent| *sent == serial) else {
            return false;
        };
        let _ = self.unacked.drain(..=index);
        self.configured = true;
        true
    }
}
