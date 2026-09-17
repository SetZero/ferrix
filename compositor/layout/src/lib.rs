//! Hyprland's window management as a pure state machine: monitors,
//! workspaces, the dwindle and master layouts, and the dispatchers a key
//! binding runs.
//!
//! Nothing here is a Wayland object, a socket or a device. A window is a
//! [`WindowId`] the protocol server chose, a monitor is a rectangle in
//! logical pixels, and the answer to "where does everything go" is
//! [`State::layout`]: for the workspace each monitor shows, every visible
//! window's rectangle and whether it has focus. The protocol server turns
//! those into configure events and the renderer into borders, so this crate
//! is tested with arithmetic alone and carries over whichever way the Smithay
//! decision in `docs/BACKLOG.md` goes.
//!
//! Hyprland is the reference: the dwindle tree follows `DwindleLayout.cpp`,
//! the master layout `MasterLayout.cpp`, the neighbour search
//! `CCompositor::getWindowInDirection`, and the gap and border arithmetic
//! `applyNodeDataToWindow`, as of Hyprland 0.53. Where 0.54 moved these into
//! `src/layout/algorithm` and changed what they do, as it did for
//! `movewindow`, this crate follows 0.54. Each module says where it departs.
//!
//! Every change goes through [`State`]'s methods, and each returns the
//! [`Change`]s it caused, so the caller redraws, reconfigures or emits IPC
//! events for exactly those. A dispatcher that cannot do anything, such as
//! `movefocus` with nowhere to go, returns no changes; a dispatcher that is
//! not understood is an [`Error`], never a panic.
//!
//! What is not handled yet: `dwindle:pseudotile` (the option is left unread
//! and every tiled window fills its slot), the master layout's `center`
//! orientation (it lays out as `left`), `master:new_on_active`,
//! `binds:workspace_back_and_forth`, `binds:window_direction_monitor_fallback`
//! (always on, its default), `binds:movefocus_cycles_fullscreen`, special and
//! named workspaces, window selectors as dispatcher arguments, resizing
//! splits, and `movewindow` on a floating window, which in Hyprland pushes it
//! against the monitor's edge and here does nothing.

#![forbid(unsafe_code)]

mod dispatch;
mod dwindle;
mod geometry;
pub mod layers;
mod master;
mod settings;
mod state;

#[cfg(test)]
mod tests;

use core::fmt;

pub use compositor_config::Gaps;
pub use dispatch::{Direction, Dispatcher, FullscreenMode, GroupMember, Locking, WorkspaceTarget};
pub use settings::{
    DwindleSettings, ForceSplit, Layout, MasterSettings, NewStatus, Orientation, Settings,
};
pub use state::{Change, Group, MonitorLayout, Placed, State};

/// A window, by the id the protocol server gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(pub u64);

/// A workspace, by number. Hyprland's workspace ids are signed, with the
/// negative ones for special workspaces; the ones this crate creates count
/// from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceId(pub i64);

/// A monitor, by the id the backend gave its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorId(pub u32);

/// A rectangle in logical pixels, in the global space all monitors share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// The left edge.
    pub x: i64,
    /// The top edge.
    pub y: i64,
    /// The width, not negative.
    pub width: i64,
    /// The height, not negative.
    pub height: i64,
}

impl Rect {
    /// A rectangle from its position and size.
    #[must_use]
    pub const fn new(x: i64, y: i64, width: i64, height: i64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The first column right of the rectangle.
    #[must_use]
    pub const fn right(self) -> i64 {
        self.x.saturating_add(self.width)
    }

    /// The first row below the rectangle.
    #[must_use]
    pub const fn bottom(self) -> i64 {
        self.y.saturating_add(self.height)
    }

    /// The rectangle moved by `dx` and `dy`.
    #[must_use]
    pub const fn translate(self, dx: i64, dy: i64) -> Self {
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
            width: self.width,
            height: self.height,
        }
    }
}

/// A monitor as the layouts see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// Its id.
    pub id: MonitorId,
    /// What it is called: the connector's name, as Hyprland names a monitor
    /// and as a `monitor =` line, a `workspace` rule and `focusmonitor`
    /// name one. Empty for a monitor nothing has named.
    pub name: String,
    /// Where it is and how big, in logical pixels.
    pub rect: Rect,
    /// The strips along its edges that layer-shell surfaces such as bars
    /// reserve, which tiled windows stay out of: Hyprland's
    /// `vecReservedTopLeft` and `vecReservedBottomRight`.
    pub reserved: Gaps,
}

/// What a request to the layouts could not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A dispatcher name this crate does not know.
    UnknownDispatcher(String),
    /// A dispatcher it knows, with an argument it cannot take.
    BadArgument {
        /// The dispatcher's name.
        dispatcher: String,
        /// The argument as given.
        arg: String,
    },
    /// A window was to open, and there is no monitor to put it on.
    NoMonitor,
    /// A monitor was added with an id already in use.
    DuplicateMonitor(MonitorId),
    /// A monitor id that is not present.
    UnknownMonitor(MonitorId),
    /// A window was opened with an id already in use.
    DuplicateWindow(WindowId),
    /// A window id that is not present.
    UnknownWindow(WindowId),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDispatcher(name) => write!(f, "Invalid dispatcher {name}"),
            Self::BadArgument { dispatcher, arg } => {
                write!(f, "Invalid argument for {dispatcher}: {arg}")
            }
            Self::NoMonitor => f.write_str("No monitor"),
            Self::DuplicateMonitor(id) => write!(f, "Monitor {} already exists", id.0),
            Self::UnknownMonitor(id) => write!(f, "No monitor {}", id.0),
            Self::DuplicateWindow(id) => write!(f, "Window {} already exists", id.0),
            Self::UnknownWindow(id) => write!(f, "No window {}", id.0),
        }
    }
}

impl core::error::Error for Error {}
