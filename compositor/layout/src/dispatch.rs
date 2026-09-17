//! The dispatchers a binding names, parsed from the strings the
//! configuration's `Bind` carries.
//!
//! Names and argument forms follow Hyprland's `KeybindManager.cpp`. Only
//! the forms listed on each variant are accepted; anything else, such as a
//! window selector after a comma or a named workspace, is an
//! [`Error::BadArgument`] rather than a guess.

use crate::{Error, WorkspaceId};

/// A direction, from `l`, `r`, `u` or `t`, and `d` or `b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `l`.
    Left,
    /// `r`.
    Right,
    /// `u` or `t`.
    Up,
    /// `d` or `b`.
    Down,
}

impl Direction {
    /// Parse a direction argument.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "l" => Some(Self::Left),
            "r" => Some(Self::Right),
            "u" | "t" => Some(Self::Up),
            "d" | "b" => Some(Self::Down),
            _ => None,
        }
    }
}

/// Which workspace a `workspace` or `movetoworkspace` argument names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceTarget {
    /// `N`: workspace N; 0 means 1, as Hyprland clamps it.
    Id(WorkspaceId),
    /// `+N` or `-N`: the workspace N numbers after or before the current
    /// one, never below 1, created if need be.
    Relative(i64),
    /// `e+N` or `e-N`: N places along the workspaces that exist on every
    /// monitor, in number order, wrapping around (`m+N`, the same on the
    /// focused monitor only, is not accepted).
    Open(i64),
    /// `special` or `special:NAME`: a workspace shown over the monitor's own
    /// rather than instead of it. Bare `special` is Hyprland's
    /// `special:special`, which is what `togglespecialworkspace` with no
    /// argument toggles.
    Special(String),
}

impl WorkspaceTarget {
    /// Parse a workspace argument.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let signed = |rest: &str| {
            if rest.starts_with(['+', '-']) {
                rest.parse::<i64>().ok()
            } else {
                None
            }
        };
        // `special` before `e`, because `special` does not start with one
        // and a name might.
        if text == "special" {
            return Some(Self::Special("special".to_owned()));
        }
        if let Some(name) = text.strip_prefix("special:") {
            return (!name.is_empty()).then(|| Self::Special(name.to_owned()));
        }
        if let Some(rest) = text.strip_prefix('e') {
            return signed(rest).map(Self::Open);
        }
        if let Some(offset) = signed(text) {
            return Some(Self::Relative(offset));
        }
        if !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse::<i64>()
            .ok()
            .map(|id| Self::Id(WorkspaceId(id.max(1))))
    }
}

/// How a window is fullscreen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullscreenMode {
    /// `fullscreen, 0`: the whole monitor, no gaps and no border.
    Fullscreen,
    /// `fullscreen, 1`: the workspace's work area, as a lone tiled window
    /// would have it, gaps and border kept. As in Hyprland, the other
    /// windows on the workspace are hidden in this mode too.
    Maximized,
}

/// A dispatcher and its argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatcher {
    /// `movefocus`: focus the neighbour in a direction, or the monitor
    /// there if there is no window, or else, unless
    /// `general:no_focus_fallback` is set, the window at the far edge of
    /// the focused monitor.
    MoveFocus(Direction),
    /// `movewindow`: move the focused tiled window past its neighbour in a
    /// direction, the way the layout does it, or to the monitor there if
    /// there is no window.
    MoveWindow(Direction),
    /// `workspace`: show a workspace, creating it if need be.
    Workspace(WorkspaceTarget),
    /// `movetoworkspace`: move the focused window to a workspace and follow
    /// it there.
    MoveToWorkspace(WorkspaceTarget),
    /// `movetoworkspacesilent`: move the focused window to a workspace and
    /// stay.
    MoveToWorkspaceSilent(WorkspaceTarget),
    /// `killactive`: ask the focused window to close.
    KillActive,
    /// `togglefloating`: float the focused window, or tile it again.
    ToggleFloating,
    /// `fullscreen`: toggle the focused window's fullscreen in a mode.
    Fullscreen(FullscreenMode),
    /// `togglespecialworkspace`: show the named special workspace over the
    /// monitor's own, or hide it if it is already showing.
    ToggleSpecialWorkspace(String),
}

impl Dispatcher {
    /// Parse a dispatcher from its name, in any case, and its argument.
    pub fn parse(name: &str, arg: &str) -> Result<Self, Error> {
        let name = name.trim().to_ascii_lowercase();
        let arg = arg.trim();
        let bad = || Error::BadArgument {
            dispatcher: name.clone(),
            arg: arg.to_owned(),
        };
        let direction = || Direction::parse(arg).ok_or_else(bad);
        let workspace = || WorkspaceTarget::parse(arg).ok_or_else(bad);
        match name.as_str() {
            "movefocus" => direction().map(Self::MoveFocus),
            "movewindow" => direction().map(Self::MoveWindow),
            "workspace" => workspace().map(Self::Workspace),
            "movetoworkspace" => workspace().map(Self::MoveToWorkspace),
            "movetoworkspacesilent" => workspace().map(Self::MoveToWorkspaceSilent),
            // Hyprland's killactive ignores its argument.
            "killactive" => Ok(Self::KillActive),
            "togglefloating" => match arg {
                "" | "active" => Ok(Self::ToggleFloating),
                _ => Err(bad()),
            },
            "togglespecialworkspace" => Ok(Self::ToggleSpecialWorkspace(if arg.is_empty() {
                "special".to_owned()
            } else {
                arg.to_owned()
            })),
            "fullscreen" => match arg {
                "" | "0" => Ok(Self::Fullscreen(FullscreenMode::Fullscreen)),
                "1" => Ok(Self::Fullscreen(FullscreenMode::Maximized)),
                _ => Err(bad()),
            },
            _ => Err(Error::UnknownDispatcher(name.clone())),
        }
    }
}
