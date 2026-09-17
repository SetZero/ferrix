//! Who gets the key, and who gets the click.
//!
//! The seat says what happened; this says who to tell. Two focuses, because
//! Wayland has two:
//!
//! * **The keyboard's** is the layout's focused window, whatever the pointer
//!   is over. That is what a tiling compositor means by focus, and what
//!   `movefocus` changes.
//! * **The pointer's** is the window under the pointer. A client is told
//!   `enter` when the pointer comes onto its surface and `leave` when it
//!   goes, and the coordinates it is given are its own, not the screen's.
//!
//! A window that is not on the screen -- another workspace, or a client that
//! has not committed a buffer -- has neither.
//!
//! # `follow_mouse`
//!
//! Hyprland's `input:follow_mouse` decides whether moving the pointer onto a
//! window focuses it. 1, the default, is "yes"; 0 is "no". That is the only
//! value read here, since 2 and 3 are about which of the keyboard and the
//! pointer follows the other and both need a click to be acted on.

use std::collections::BTreeMap;

use compositor_layout::{State, WindowId};
use compositor_wire::{Fixed, ObjectId};

use crate::frame::Source;
use crate::seat::Action;
use crate::state::Slot;

/// Where a window is on the screen, and which client's surface it is.
#[derive(Clone, Copy, Debug)]
struct Placement {
    window: WindowId,
    client: usize,
    surface: ObjectId,
    rect: (i64, i64, i64, i64),
}

/// What has the keyboard and what has the pointer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Focus {
    keyboard: Option<(usize, ObjectId)>,
    pointer: Option<(usize, ObjectId)>,
}

impl Focus {
    /// Nothing focused, which is a compositor with no windows.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            keyboard: None,
            pointer: None,
        }
    }

    /// The window the keyboard is on, if any.
    #[must_use]
    pub const fn keyboard(&self) -> Option<(usize, ObjectId)> {
        self.keyboard
    }

    /// A connection went, so every slot after it moved: `places[old]` is
    /// where the client that was at `old` is now, or `None` if it is the one
    /// that went.
    ///
    /// The focus holds a client by its place in the list, and a place that
    /// is no longer that client is a `wl_keyboard.leave` sent to a stranger.
    pub fn renumber(&mut self, places: &[Option<usize>]) {
        moved(&mut self.keyboard, places);
        moved(&mut self.pointer, places);
    }

    /// Give the keyboard to whatever the layout says is focused.
    ///
    /// Called every time round the loop. A focus that has not moved sends
    /// nothing: `wl_keyboard.leave` and `enter` to the same surface would
    /// make a toolkit drop what it was typing.
    ///
    /// The new focus is remembered only if the `enter` reached a
    /// `wl_keyboard`. A window is usually mapped in the same burst of
    /// requests that asks the seat for its keyboard, and whichever the server
    /// reads first, the client has to end up with the focus; remembering an
    /// `enter` that went nowhere would leave that window unable to be typed
    /// into for as long as it lived.
    pub fn follow_layout(
        &mut self,
        state: &State,
        slots: &mut [Slot],
        sources: &BTreeMap<WindowId, Source>,
        held: &[u16],
        modifiers: compositor_xkb::Modifiers,
    ) {
        let wanted = state
            .focused_window()
            .and_then(|window| sources.get(&window))
            .map(|source| (source.client, source.surface));
        if wanted == self.keyboard {
            return;
        }
        if let Some((client, surface)) = self.keyboard
            && let Some(slot) = slots.get_mut(client)
        {
            let _ = slot.client_mut().keyboard_leave(surface);
        }
        self.keyboard = None;
        let Some((client, surface)) = wanted else {
            return;
        };
        if let Some(slot) = slots.get_mut(client)
            && slot
                .client_mut()
                .keyboard_enter(surface, held, modifiers)
                .is_some()
        {
            self.keyboard = wanted;
        }
    }

    /// Everything the pointer left or arrived on, and the pointer's position
    /// inside whatever it is on now.
    fn move_pointer(
        &mut self,
        placements: &[Placement],
        slots: &mut [Slot],
        at: (f64, f64),
    ) -> Option<(f64, f64)> {
        let over = placements.iter().find(|placed| {
            let (x, y, width, height) = placed.rect;
            #[expect(
                clippy::cast_precision_loss,
                reason = "a window's edge is at most a screen's width; the loss is beyond any pixel"
            )]
            let (left, top, right, bottom) =
                (x as f64, y as f64, (x + width) as f64, (y + height) as f64);
            at.0 >= left && at.0 < right && at.1 >= top && at.1 < bottom
        });
        let wanted = over.map(|placed| (placed.client, placed.surface));
        let local = over.map(|placed| {
            let (x, y, _, _) = placed.rect;
            #[expect(
                clippy::cast_precision_loss,
                reason = "as above: the origin is a screen coordinate"
            )]
            (at.0 - x as f64, at.1 - y as f64)
        });

        if wanted != self.pointer {
            if let Some((client, surface)) = self.pointer
                && let Some(slot) = slots.get_mut(client)
            {
                let _ = slot.client_mut().pointer_leave(surface);
                slot.client_mut().pointer_frame();
            }
            // As in `follow_layout`: remembered only if it arrived.
            self.pointer = None;
            if let Some(((client, surface), (x, y))) = wanted.zip(local)
                && let Some(slot) = slots.get_mut(client)
                && slot
                    .client_mut()
                    .pointer_enter(surface, fixed(x), fixed(y))
                    .is_some()
            {
                slot.client_mut().pointer_frame();
                self.pointer = wanted;
            }
        }
        local
    }
}

/// What delivering the seat's actions did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// The dispatchers to run, in order, which the caller hands the layout.
    pub dispatch: Vec<(String, String)>,
    /// The window `follow_mouse` asks to be focused, if the pointer moved
    /// onto one that is not.
    ///
    /// Not a dispatcher, because Hyprland's `focuswindow` takes a window
    /// *rule* -- `class:foot`, `title:...` -- and not an id, so there is no
    /// dispatcher that says "this one". The layout has `focus_window`, and
    /// the caller calls it.
    pub focus: Option<WindowId>,
}

/// Send the seat's actions to whoever they belong to.
///
/// The dispatchers are not run here: they change the layout, and the layout
/// is the caller's. Everything else is sent as it goes.
pub fn deliver(
    actions: &[Action],
    focus: &mut Focus,
    state: &State,
    slots: &mut [Slot],
    sources: &BTreeMap<WindowId, Source>,
    time: u32,
    follow_mouse: bool,
) -> Done {
    let placements = placements(state, sources);
    let mut done = Done::default();
    for action in actions {
        match action {
            Action::Dispatch { name, argument } => {
                done.dispatch.push((name.clone(), argument.clone()));
            }
            Action::Key { code, pressed } => {
                if let Some((client, _)) = focus.keyboard
                    && let Some(slot) = slots.get_mut(client)
                {
                    let _ = slot.client_mut().keyboard_key(time, *code, *pressed);
                }
            }
            Action::Modifiers(modifiers) => {
                if let Some((client, _)) = focus.keyboard
                    && let Some(slot) = slots.get_mut(client)
                {
                    let _ = slot.client_mut().keyboard_modifiers(*modifiers);
                }
            }
            Action::Pointer { x, y } => {
                if let Some((local_x, local_y)) = focus.move_pointer(&placements, slots, (*x, *y))
                    && let Some((client, _)) = focus.pointer
                    && let Some(slot) = slots.get_mut(client)
                {
                    slot.client_mut()
                        .pointer_motion(time, fixed(local_x), fixed(local_y));
                    slot.client_mut().pointer_frame();
                }
                // Moving onto a window focuses it, which is what
                // `follow_mouse` turns off.
                if follow_mouse
                    && let Some((client, surface)) = focus.pointer
                    && let Some(window) = placements
                        .iter()
                        .find(|placed| placed.client == client && placed.surface == surface)
                        .map(|placed| placed.window)
                    && state.focused_window() != Some(window)
                {
                    done.focus = Some(window);
                }
            }
            Action::Button { button, pressed } => {
                if let Some((client, _)) = focus.pointer
                    && let Some(slot) = slots.get_mut(client)
                {
                    let _ = slot.client_mut().pointer_button(time, *button, *pressed);
                    slot.client_mut().pointer_frame();
                }
            }
            Action::Axis { axis, value } => {
                if let Some((client, _)) = focus.pointer
                    && let Some(slot) = slots.get_mut(client)
                {
                    slot.client_mut().pointer_axis(time, *axis, fixed(*value));
                    slot.client_mut().pointer_frame();
                }
            }
        }
    }
    done
}

/// Every window on a screen now, with where it is and whose it is.
fn placements(state: &State, sources: &BTreeMap<WindowId, Source>) -> Vec<Placement> {
    let mut out = Vec::new();
    for output in state.layout() {
        for placed in &output.windows {
            let Some(source) = sources.get(&placed.window) else {
                continue;
            };
            out.push(Placement {
                window: placed.window,
                client: source.client,
                surface: source.surface,
                rect: (
                    placed.rect.x,
                    placed.rect.y,
                    placed.rect.width,
                    placed.rect.height,
                ),
            });
        }
    }
    // The last window drawn is the one on top, so the pointer finds it first.
    out.reverse();
    out
}

/// A pixel position as Wayland's 24.8 fixed point.
fn fixed(value: f64) -> Fixed {
    Fixed::from_f64(value)
}

/// One held `(client, surface)` pair, moved to where its client is now or
/// forgotten if that client has gone.
fn moved(held: &mut Option<(usize, ObjectId)>, places: &[Option<usize>]) {
    if let Some((client, surface)) = *held {
        *held = places
            .get(client)
            .copied()
            .flatten()
            .map(|at| (at, surface));
    }
}
