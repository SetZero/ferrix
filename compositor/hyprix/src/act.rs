//! The dispatchers that are the compositor's rather than the tiling's.
//!
//! `compositor/layout` answers everything that moves a window: it holds the
//! monitors, the workspaces and the tree, and `movefocus` means nothing
//! without them. The rest of Hyprland's one dispatcher table reaches past
//! the layout -- it starts a program, signals one, turns a screen off, moves
//! the pointer, writes a line on the event socket or ends the session -- and
//! that is what this module is.
//!
//! [`Around`] is everything such a dispatcher may touch. It is a struct and
//! not a dozen arguments because Hyprland's table is one function taking one
//! string and the whole compositor behind it, and this is that compositor.
//!
//! # What is not here
//!
//! `forceidle` and `releaseinputcapture` are answered with a line saying
//! they do nothing: this compositor has neither an idle protocol nor input
//! capture, and a dispatcher that quietly did nothing would be worse than
//! one that says so. `toggleswallow` keeps its flag and says the same:
//! swallowing a terminal is not implemented.

use std::collections::BTreeMap;

use compositor_layout::{Move, State, WindowId};

use crate::frame::Source;
use crate::seat::{Action, Seat};
use crate::select::Selector;
use crate::state::Slot;

/// A drag with the mouse: `bindm = SUPER, mouse:272, movewindow`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drag {
    /// Which window is being dragged.
    pub window: WindowId,
    /// Whether it is being resized rather than moved.
    pub resizing: bool,
    /// Where the pointer was when it started.
    pub from: (i64, i64),
}

/// Everything a dispatcher may reach besides the layout.
pub struct Around<'a> {
    /// The connections.
    pub slots: &'a mut [Slot],
    /// Which client and surface each window's pixels come from.
    pub sources: &'a BTreeMap<WindowId, Source>,
    /// Where the Wayland socket is, for a program that is started.
    pub socket: &'a std::path::Path,
    /// The keyboard and the pointer.
    pub seat: &'a mut Seat,
    /// The plugins, which are the table's last entry.
    pub plugins: &'a mut crate::plugins::Plugins,
    /// What a rule gave each window to be drawn with, which `setprop`
    /// changes by hand.
    pub rules: &'a mut crate::rules::Rules,
    /// The event socket, for `event`.
    pub events: &'a mut Option<crate::control::Events>,
    /// Whether each screen is turned off, by the name `dpms` names it.
    pub dpms: &'a mut BTreeMap<String, bool>,
    /// The screens' names and rectangles, for `dpms` and for
    /// `movecursortocorner`.
    pub screens: &'a [(String, compositor_layout::Rect)],
    /// Windows a client asked to have raised and that have not been looked
    /// at, which is what `focusurgentorlast` looks for.
    pub urgent: &'a mut Vec<WindowId>,
    /// A drag with the mouse, while one is going on.
    pub drag: &'a mut Option<Drag>,
    /// Actions a dispatcher caused, which the caller delivers.
    pub pending: &'a mut Vec<Action>,
    /// Set when a dispatcher asked the compositor to end.
    pub quit: &'a mut bool,
    /// Whether a terminal that opened a window is hidden: `toggleswallow`.
    pub swallow: &'a mut bool,
    /// What to say.
    pub report: &'a mut dyn FnMut(&str),
}

impl core::fmt::Debug for Around<'_> {
    /// What a dispatcher changed, and not the parts of the compositor it
    /// borrowed to change them: a connection and a closure have nothing to
    /// print, and the loop's own report says what happened anyway.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Around")
            .field("screens", &self.screens)
            .field("dpms", &self.dpms)
            .field("urgent", &self.urgent)
            .field("drag", &self.drag)
            .field("quit", &self.quit)
            .field("swallow", &self.swallow)
            .finish_non_exhaustive()
    }
}

impl Around<'_> {
    /// Say something, the way the compositor says everything else.
    pub fn say(&mut self, what: &str) {
        (self.report)(what);
    }
}

/// Run `name` if it is one of the compositor's own dispatchers.
///
/// Gives `None` when the name belongs to the layout, so the caller hands it
/// on; `Some(changed)` when it was answered here, and whether the screen has
/// to be drawn again.
pub fn compositor(
    name: &str,
    argument: &str,
    state: &mut State,
    around: &mut Around<'_>,
) -> Option<bool> {
    match name {
        // Hyprland's `exec` goes through a shell and `execr` does not; this
        // compositor never went through one, so the two are the same call
        // and `execr` is the honest name for what both do.
        "exec" | "execr" => Some(start(argument, around)),
        "exit" => {
            *around.quit = true;
            around.say("hyprix: asked to end");
            Some(false)
        }
        "submap" => Some(submap(argument, around)),
        "forcerendererreload" => {
            // Nothing is cached between frames, so there is nothing to
            // throw away: drawing again is the whole of it.
            Some(true)
        }
        "event" => Some(event(argument, around)),
        "global" => Some(global(argument, around)),
        "dpms" => Some(dpms(argument, around)),
        "movecursor" => Some(move_cursor(argument, around)),
        "movecursortocorner" => Some(move_cursor_to_corner(argument, state, around)),
        "mouse" => Some(mouse(argument, state, around)),
        "forcekillactive" => Some(signal(argument_or(argument, "9"), None, state, around)),
        "signal" => Some(signal(argument, None, state, around)),
        "signalwindow" => {
            let (which, number) = argument.split_once(',')?;
            Some(signal(number.trim(), Some(which.trim()), state, around))
        }
        "setprop" => Some(set_prop(argument, state, around)),
        "focusurgentorlast" => Some(focus_urgent_or_last(state, around)),
        "toggleswallow" => {
            *around.swallow = !*around.swallow;
            around.say("hyprix: swallowing a terminal is not implemented");
            Some(false)
        }
        "forceidle" => {
            around.say("hyprix: there is no idle protocol to notify");
            Some(false)
        }
        "releaseinputcapture" => {
            around.say("hyprix: nothing has captured the input");
            Some(false)
        }
        _ => None,
    }
}

/// An empty argument means this instead, which is how `forcekillactive` is
/// `signal 9`.
fn argument_or<'a>(argument: &'a str, instead: &'a str) -> &'a str {
    if argument.trim().is_empty() {
        instead
    } else {
        argument
    }
}

/// `exec`: start a program with this compositor's socket in its
/// environment.
fn start(command: &str, around: &mut Around<'_>) -> bool {
    match crate::state::start(command, around.socket) {
        Ok(pid) => around.say(&format!("hyprix: started {command} as {pid}")),
        Err(error) => around.say(&format!("hyprix: {command} did not start: {error}")),
    }
    false
}

/// `submap`: which set of binds is in force.
fn submap(name: &str, around: &mut Around<'_>) -> bool {
    match around.seat.enter_submap(name) {
        Ok(true) => {
            let name = around.seat.submap();
            if name.is_empty() {
                around.say("hyprix: the global keymap");
            } else {
                around.say(&format!("hyprix: submap {name}"));
            }
        }
        Ok(false) => {}
        Err(why) => around.say(&format!("hyprix: {why}")),
    }
    false
}

/// `event`: a line of a program's own on the event socket, which Hyprland
/// writes as `custom>>`.
fn event(data: &str, around: &mut Around<'_>) -> bool {
    match around.events.as_mut() {
        Some(events) => events.say(&format!("custom>>{data}\n")),
        None => around.say("hyprix: no event socket to write on"),
    }
    false
}

/// `global`: a shortcut a program registered.
///
/// `hyprland-global-shortcuts-v1` is not among the protocols this
/// compositor offers, so the only thing that can have registered one is a
/// plugin, and that is where the name goes.
fn global(name: &str, around: &mut Around<'_>) -> bool {
    if around.plugins.dispatch("global", name) {
        return false;
    }
    around.say(&format!("hyprix: nothing has registered the shortcut {name}"));
    false
}

/// `dpms on|off|toggle [monitor]`: turn a screen off, or every screen.
fn dpms(argument: &str, around: &mut Around<'_>) -> bool {
    let mut words = argument.split_whitespace();
    let what = words.next().unwrap_or("");
    let named = words.next();
    let mut changed = false;
    for (name, _) in around.screens.iter() {
        if named.is_some_and(|wanted| wanted != name) {
            continue;
        }
        let now = around.dpms.get(name).copied().unwrap_or(false);
        // Hyprland reads anything that is not `on` or `toggle` as `off`.
        let off = match what {
            "on" => false,
            "toggle" => !now,
            _ => true,
        };
        if off != now {
            let _ = around.dpms.insert(name.clone(), off);
            changed = true;
        }
    }
    changed
}

/// `movecursor <x> <y>`: put the pointer somewhere.
fn move_cursor(argument: &str, around: &mut Around<'_>) -> bool {
    let mut numbers = argument.split_whitespace();
    let read = |text: Option<&str>| text.and_then(|text| text.parse::<f64>().ok());
    let (Some(x), Some(y)) = (read(numbers.next()), read(numbers.next())) else {
        around.say("hyprix: movecursor takes two numbers");
        return false;
    };
    around.pending.extend(around.seat.warp(x, y));
    true
}

/// `movecursortocorner <0-3>`: to a corner of the focused window, counting
/// anticlockwise from the bottom left, as Hyprland counts them.
fn move_cursor_to_corner(argument: &str, state: &State, around: &mut Around<'_>) -> bool {
    let Ok(corner) = argument.trim().parse::<u8>() else {
        around.say("hyprix: movecursortocorner takes a number from 0 to 3");
        return false;
    };
    // The focused window, or the focused screen when nothing is focused.
    let rect = state
        .focused_window()
        .and_then(|window| {
            state
                .layout()
                .iter()
                .flat_map(|output| output.windows.iter())
                .find(|placed| placed.window == window)
                .map(|placed| placed.rect)
        })
        .or_else(|| around.screens.first().map(|(_, rect)| *rect));
    let Some(rect) = rect else {
        return false;
    };
    // The last column and row inside the window, so the pointer lands on it
    // rather than one pixel past it.
    let (left, top) = (rect.x, rect.y);
    let (right, bottom) = (rect.right().saturating_sub(1), rect.bottom().saturating_sub(1));
    let (x, y) = match corner {
        0 => (left, bottom),
        1 => (right, bottom),
        2 => (right, top),
        3 => (left, top),
        _ => {
            around.say("hyprix: movecursortocorner takes a number from 0 to 3");
            return false;
        }
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "a screen's pixels are far inside f64's exact range"
    )]
    around
        .pending
        .extend(around.seat.warp(x as f64, y as f64));
    true
}

/// `mouse +action` and `-action`: a drag begins and ends.
fn mouse(argument: &str, state: &mut State, around: &mut Around<'_>) -> bool {
    let (starting, action) = match argument.split_at_checked(1) {
        Some(("+", rest)) => (true, rest),
        Some(("-", rest)) => (false, rest),
        _ => {
            around.say("hyprix: mouse takes +action or -action");
            return false;
        }
    };
    if !starting {
        *around.drag = None;
        return false;
    }
    let resizing = match action {
        "movewindow" => false,
        "resizewindow" => true,
        _ => {
            around.say(&format!("hyprix: there is no mouse action {action}"));
            return false;
        }
    };
    let Some(window) = state.focused_window() else {
        return false;
    };
    // A tiled window has no rectangle of its own to drag; Hyprland floats it
    // first, and so does this.
    if !state.is_floating(window) {
        let _ = state.dispatch_str("togglefloating", "");
    }
    let (x, y) = around.seat.pointer();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the pointer is held inside the screen, which is far inside i64"
    )]
    let from = (x as i64, y as i64);
    *around.drag = Some(Drag {
        window,
        resizing,
        from,
    });
    true
}

/// Carry a drag on: the pointer has moved to `(x, y)`.
///
/// Gives whether the window moved, and is called from the loop rather than
/// from a dispatcher: a drag is one bind and then every pointer movement
/// until the button comes back up.
pub fn dragged(drag: &mut Drag, state: &mut State, (x, y): (i64, i64)) -> bool {
    let by = Move {
        x: x.saturating_sub(drag.from.0),
        y: y.saturating_sub(drag.from.1),
        exact: false,
    };
    if by.x == 0 && by.y == 0 {
        return false;
    }
    drag.from = (x, y);
    let moved = if drag.resizing {
        state.resize_window_pixel(drag.window, &by)
    } else {
        state.move_window_pixel(drag.window, &by)
    };
    moved.is_ok_and(|changes| !changes.is_empty())
}

/// `signal`, `signalwindow` and `forcekillactive`: send a signal to the
/// process on the other end of a window's connection.
fn signal(
    number: &str,
    which: Option<&str>,
    state: &State,
    around: &mut Around<'_>,
) -> bool {
    let Ok(number) = number.trim().parse::<i32>() else {
        around.say("hyprix: a signal is a number");
        return false;
    };
    let Some(window) = pick(which, state, around) else {
        return false;
    };
    let Some(pid) = pid_of(window, around) else {
        around.say("hyprix: the window's connection has no process");
        return false;
    };
    #[expect(
        unsafe_code,
        reason = "AUDIT: kill is not in std; the pid came from SO_PEERCRED on this compositor's own socket"
    )]
    // SAFETY: `kill` takes two integers and touches no memory.
    let sent = unsafe { libc::kill(pid, number) };
    if sent < 0 {
        around.say(&format!(
            "hyprix: signal {number} to {pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    false
}

/// `setprop <window> <property> <value>`: change what one window is drawn
/// with, the way a `windowrule` would have.
fn set_prop(argument: &str, state: &State, around: &mut Around<'_>) -> bool {
    let mut words = argument.split_whitespace();
    let (Some(which), Some(property)) = (words.next(), words.next()) else {
        around.say("hyprix: setprop takes a window, a property and a value");
        return false;
    };
    let value: String = words.collect::<Vec<&str>>().join(" ");
    let Some(window) = pick(Some(which), state, around) else {
        return false;
    };
    if around.rules.set_property(window, property, &value) {
        return true;
    }
    around.say(&format!("hyprix: there is no window property {property}"));
    false
}

/// `focusurgentorlast`: the window that asked to be raised, or else the one
/// focused before this.
fn focus_urgent_or_last(state: &mut State, around: &mut Around<'_>) -> bool {
    while let Some(window) = around.urgent.pop() {
        // A window that has gone is not one to focus.
        if state.focus_window(window).is_ok() {
            return true;
        }
    }
    state
        .dispatch_str("focuscurrentorlast", "")
        .is_ok_and(|changes| !changes.is_empty())
}

/// The window an expression names, or the focused one when there is no
/// expression.
pub fn pick(which: Option<&str>, state: &State, around: &mut Around<'_>) -> Option<WindowId> {
    let Some(which) = which.filter(|text| !text.is_empty()) else {
        return state.focused_window();
    };
    let seen = crate::state::as_seen(state, around.slots, around.sources);
    let window = Selector::parse(which)?.pick(&seen, state.focused_window());
    if window.is_none() {
        around.say(&format!("hyprix: no window answers {which:?}"));
    }
    window
}

/// The process on the other end of a window's connection.
fn pid_of(window: WindowId, around: &Around<'_>) -> Option<i32> {
    let source = around.sources.get(&window)?;
    let slot = around.slots.get(source.client)?;
    Some(slot.pid()).filter(|pid| *pid > 0)
}
