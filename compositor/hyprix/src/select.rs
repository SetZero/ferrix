//! Which window a dispatcher names.
//!
//! `dispatch focuswindow class:^(foot)$` and `dispatch closewindow title:vim`
//! do not name a window by number: they carry one of Hyprland's window
//! expressions, and the compositor finds the window it picks out. Hyprland
//! reads them in `CViewQuery::bySelector`, and this is that function.
//!
//! It is here and not in `compositor/layout` because the layout holds a
//! window's place and nothing about what it is called: a title, an
//! application id and a process are the compositor's, so the selecting is
//! too.
//!
//! # The expressions
//!
//! * `active` -- the focused window, and anything starting with it.
//! * `floating` and `tiled` -- the first such window on the focused
//!   window's workspace.
//! * `class:`, `initialclass:`, `title:`, `initialtitle:`, `tag:` -- a
//!   regular expression, which has to match the whole of the field.
//! * `address:` and `stableid:` -- the handle `hyprctl clients` prints.
//! * `pid:` -- the process on the other end of the client's socket.
//! * anything else -- a regular expression matched against the class, which
//!   is what `focuswindowbyclass` is and what a bare expression means.

use compositor_layout::{WindowId, WorkspaceId};
use compositor_regex::Regex;

/// A window, as a selector sees it.
#[derive(Clone, Copy, Debug)]
pub struct Seen<'a> {
    /// Which window.
    pub window: WindowId,
    /// `xdg_toplevel.set_app_id`, which Hyprland calls the class.
    pub class: &'a str,
    /// `xdg_toplevel.set_title`.
    pub title: &'a str,
    /// The class it mapped with.
    pub initial_class: &'a str,
    /// The title it mapped with.
    pub initial_title: &'a str,
    /// What `tagwindow` has given it.
    pub tags: &'a [String],
    /// The process on the other end of its connection, or 0 where the
    /// kernel would not say.
    pub pid: i32,
    /// Whether it floats.
    pub floating: bool,
    /// Which workspace it is on.
    pub workspace: Option<WorkspaceId>,
}

/// One of Hyprland's window expressions, read.
#[derive(Debug)]
pub enum Selector {
    /// `active`: whichever window has the focus.
    Active,
    /// `floating` or `tiled`, on the focused window's workspace.
    Floating(bool),
    /// A field matched whole against a regular expression.
    Matching(Field, Regex),
    /// `address:` or `stableid:`: the handle `hyprctl clients` prints.
    Address(u64),
    /// `pid:`: the process that opened the connection.
    Pid(i32),
}

/// Which of a window's names a `Selector::Matching` matches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    /// `class:`, and a bare expression.
    Class,
    /// `initialclass:`.
    InitialClass,
    /// `title:`.
    Title,
    /// `initialtitle:`.
    InitialTitle,
    /// `tag:`, which matches when any one tag does.
    Tag,
}

impl Selector {
    /// Read an expression, or `None` when it is not one -- which is what a
    /// regular expression that does not compile and a `pid:` that is not a
    /// number both are.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.starts_with("active") {
            return Some(Self::Active);
        }
        if text.starts_with("floating") {
            return Some(Self::Floating(true));
        }
        if text.starts_with("tiled") {
            return Some(Self::Floating(false));
        }
        for (prefix, field) in [
            ("class:", Field::Class),
            ("initialclass:", Field::InitialClass),
            ("title:", Field::Title),
            ("initialtitle:", Field::InitialTitle),
            ("tag:", Field::Tag),
        ] {
            if let Some(rest) = text.strip_prefix(prefix) {
                return Regex::new(rest).ok().map(|re| Self::Matching(field, re));
            }
        }
        for prefix in ["address:", "stableid:"] {
            if let Some(rest) = text.strip_prefix(prefix) {
                let rest = rest.strip_prefix("0x").unwrap_or(rest);
                return u64::from_str_radix(rest, 16).ok().map(Self::Address);
            }
        }
        if let Some(rest) = text.strip_prefix("pid:") {
            return rest.trim().parse().ok().map(Self::Pid);
        }
        // Hyprland's default: the whole expression against the class, which
        // is what `focuswindowbyclass` means and what a bare one does.
        Regex::new(text).ok().map(|re| Self::Matching(Field::Class, re))
    }

    /// The first window this picks out, in the order the compositor holds
    /// them -- which is the order Hyprland walks its own window list in, so
    /// an expression two windows answer picks the older of the two.
    #[must_use]
    pub fn pick(&self, windows: &[Seen<'_>], focused: Option<WindowId>) -> Option<WindowId> {
        match self {
            Self::Active => focused,
            Self::Floating(wanted) => {
                // Hyprland's own rule: with nothing focused there is no
                // workspace to look on, so there is no answer either.
                let here = focused?;
                let workspace = windows
                    .iter()
                    .find(|seen| seen.window == here)
                    .and_then(|seen| seen.workspace)?;
                windows
                    .iter()
                    .find(|seen| seen.floating == *wanted && seen.workspace == Some(workspace))
                    .map(|seen| seen.window)
            }
            Self::Address(address) => windows
                .iter()
                .find(|seen| seen.window.0 == *address)
                .map(|seen| seen.window),
            Self::Pid(pid) => windows
                .iter()
                .find(|seen| seen.pid == *pid)
                .map(|seen| seen.window),
            Self::Matching(field, pattern) => windows
                .iter()
                .find(|seen| matches(*field, pattern, seen))
                .map(|seen| seen.window),
        }
    }
}

/// Whether one window answers a field's pattern.
fn matches(field: Field, pattern: &Regex, seen: &Seen<'_>) -> bool {
    match field {
        Field::Class => pattern.matches(seen.class),
        Field::InitialClass => pattern.matches(seen.initial_class),
        Field::Title => pattern.matches(seen.title),
        Field::InitialTitle => pattern.matches(seen.initial_title),
        Field::Tag => seen.tags.iter().any(|tag| pattern.matches(tag)),
    }
}

#[cfg(test)]
mod tests;
