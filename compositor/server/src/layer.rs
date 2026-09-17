//! `zwlr_layer_shell_v1`: the surfaces that are not windows.
//!
//! A bar, a wallpaper, a notification and a launcher are not windows: they
//! are not tiled, they are not in the focus order, and they sit at a fixed
//! place on a fixed layer. `wlr-layer-shell-unstable-v1` is how every
//! wlroots-shaped compositor -- Hyprland included -- lets a client say so,
//! and it is what `waybar`, `hyprpaper`, `mako`, `wofi` and every other bar
//! and launcher is written against. Without it a Hyprland user's setup does
//! not start at all.
//!
//! # The four layers
//!
//! `background`, `bottom`, `top` and `overlay`, drawn in that order with the
//! windows between `bottom` and `top`. A wallpaper is `background`, a bar is
//! `top`, a lock screen is `overlay`.
//!
//! # The configure conversation
//!
//! The same shape as `xdg_surface`'s, and stricter about size. A client says
//! which edges it is anchored to and how big it wants to be; the compositor
//! works out the rest and sends `configure` with a serial and the size it
//! decided. A width or a height of zero means "you choose", and the client
//! *must* be told a real number for any axis it is not anchored to both
//! edges of -- a zero on such an axis is `invalid_size`, which is the
//! protocol's own rule and the one that catches a bar that forgot to call
//! `set_size`.
//!
//! # The exclusive zone
//!
//! A bar that reserves space takes it out of the area windows tile in. The
//! zone is a number of pixels on the edge the surface is anchored to: a
//! positive one reserves that many, zero reserves nothing but is still moved
//! out of other exclusive zones' way, and −1 means "put me under everything
//! and reserve nothing", which is what a wallpaper asks for.

use compositor_wire::ObjectId;

/// Which layer a surface is on, in the order they are drawn.
///
/// The numbers are `zwlr_layer_shell_v1.layer`'s own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Layer {
    /// Under everything: a wallpaper.
    Background,
    /// Above the wallpaper, under the windows.
    Bottom,
    /// Above the windows: a bar.
    Top,
    /// Above everything: a lock screen.
    Overlay,
}

impl Layer {
    /// The layer a `zwlr_layer_shell_v1.layer` value names.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            0 => Self::Background,
            1 => Self::Bottom,
            2 => Self::Top,
            3 => Self::Overlay,
            _ => return None,
        })
    }

    /// Its `zwlr_layer_shell_v1.layer` value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Background => 0,
            Self::Bottom => 1,
            Self::Top => 2,
            Self::Overlay => 3,
        }
    }

    /// Whether this layer is drawn above the windows.
    #[must_use]
    pub const fn above_windows(self) -> bool {
        matches!(self, Self::Top | Self::Overlay)
    }
}

/// The margins a surface asked for, in the protocol's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Margin {
    /// From the top edge.
    pub top: i32,
    /// From the right edge.
    pub right: i32,
    /// From the bottom edge.
    pub bottom: i32,
    /// From the left edge.
    pub left: i32,
}

/// One `zwlr_layer_surface_v1`, and what its client has said about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerSurface {
    /// The `wl_surface` it gives a role to.
    pub surface: ObjectId,
    /// The `wl_output` it asked for, or `None` for "you choose", which is
    /// this compositor's only monitor.
    pub output: Option<ObjectId>,
    /// Which layer it is on.
    pub layer: Layer,
    /// What the client called itself: `waybar`, `hyprpaper`, `wallpaper`.
    /// Hyprland's `layerrule` matches on it and its `openlayer` event
    /// carries it.
    pub namespace: String,
    /// The size it asked for; zero on an axis means "you choose".
    pub size: (u32, u32),
    /// `zwlr_layer_surface_v1.anchor` bits.
    pub anchor: u32,
    /// How many pixels it takes out of the area windows tile in. −1 asks to
    /// be left out of the reckoning altogether.
    pub exclusive_zone: i32,
    /// The margins from the edges it is anchored to.
    pub margin: Margin,
    /// `zwlr_layer_surface_v1.keyboard_interactivity`.
    pub keyboard_interactivity: u32,
    /// The size the last configure carried.
    pub configured: (u32, u32),
    /// Serials sent and not yet acked, oldest first.
    pub unacked: Vec<u32>,
    /// Whether the client has acked a configure and committed since.
    pub committed: bool,
    /// Whether a configure has been sent at all.
    pub sent_configure: bool,
}

impl LayerSurface {
    /// A fresh surface, before its client has said anything about it.
    #[must_use]
    pub fn new(
        surface: ObjectId,
        output: Option<ObjectId>,
        layer: Layer,
        namespace: String,
    ) -> Self {
        Self {
            surface,
            output,
            layer,
            namespace,
            size: (0, 0),
            anchor: 0,
            exclusive_zone: 0,
            margin: Margin::default(),
            keyboard_interactivity: 0,
            configured: (0, 0),
            unacked: Vec::new(),
            committed: false,
            sent_configure: false,
        }
    }

    /// Whether the size the client asked for can be satisfied.
    ///
    /// A zero on an axis means "you choose", and the protocol only lets a
    /// client say that for an axis it is anchored to *both* edges of --
    /// otherwise the compositor has nothing to choose from. Anything else is
    /// `invalid_size`.
    #[must_use]
    pub const fn size_is_valid(&self, anchor: &Anchors) -> bool {
        (self.size.0 != 0 || (anchor.left && anchor.right))
            && (self.size.1 != 0 || (anchor.top && anchor.bottom))
    }

    /// Record that a configure was sent.
    pub fn configure_sent(&mut self, serial: u32, size: (u32, u32)) {
        self.unacked.push(serial);
        self.configured = size;
        self.sent_configure = true;
    }

    /// Take an `ack_configure`, saying whether the serial was one that was
    /// sent.
    ///
    /// A client may be several configures behind, so acking anything still
    /// outstanding is accepted and everything older than it is forgotten.
    pub fn acked(&mut self, serial: u32) -> bool {
        let Some(at) = self.unacked.iter().position(|sent| *sent == serial) else {
            return false;
        };
        let _ = self.unacked.drain(..=at);
        self.committed = true;
        true
    }
}

/// The four anchor bits, taken apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Anchors {
    /// Anchored to the top edge.
    pub top: bool,
    /// To the bottom edge.
    pub bottom: bool,
    /// To the left edge.
    pub left: bool,
    /// To the right edge.
    pub right: bool,
}

impl Anchors {
    /// The bits `zwlr_layer_surface_v1.set_anchor` carries.
    ///
    /// Anything above the four defined bits is `invalid_anchor`, which the
    /// caller answers; this only takes the ones that are there apart.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self {
            top: raw & 1 != 0,
            bottom: raw & 2 != 0,
            left: raw & 4 != 0,
            right: raw & 8 != 0,
        }
    }

    /// Whether `raw` has a bit the protocol does not define.
    #[must_use]
    pub const fn is_valid(raw: u32) -> bool {
        raw & !0xF == 0
    }
}
