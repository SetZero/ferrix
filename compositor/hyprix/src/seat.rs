//! The seat: what a key or a movement means.
//!
//! This is the part of input that has no descriptor in it. It is handed
//! [`Input`] -- a key, a movement, a button, a scroll, already free of evdev's
//! numbering by the layer below -- and gives back [`Action`]s: run this
//! dispatcher, send this key to the focused window, the pointer is now here.
//! So every rule about what a keybind matches and what a modifier does is
//! host-tested, and running it on Ferrix tests the devices rather than the
//! rules.
//!
//! # What a bind matches
//!
//! Hyprland fires a bind when the key matches *and* the modifiers held are
//! exactly the bind's. Exactly, not "at least": `bind = SUPER, Q` does not
//! fire on `SUPER SHIFT Q`, which is what lets the two be bound to different
//! things. Caps Lock and Num Lock are left out of the comparison, because a
//! keyboard with Caps Lock on would otherwise match nothing.
//!
//! A bind consumes its key: the focused client is not told about it. The `n`
//! flag says not to, and the `i` flag says to fire whatever modifiers are
//! held.

use compositor_config::{Config, Key, Mods};
use compositor_xkb::{Keyboard, Modifiers, generated};

/// What the devices below send up, free of evdev's numbering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// A key went down, came up, or repeated while held.
    Key {
        /// The evdev keycode.
        code: u16,
        /// Down, as opposed to up.
        pressed: bool,
        /// A repeat of a key already held, which evdev sends as value 2.
        repeat: bool,
    },
    /// The pointer moved by this many pixels.
    Motion {
        /// Rightwards.
        dx: f64,
        /// Downwards.
        dy: f64,
    },
    /// The pointer is at this fraction of the screen, from a tablet or a
    /// touchscreen, which report where they are rather than how far they
    /// moved.
    Absolute {
        /// Across, 0 to 1.
        x: f64,
        /// Down, 0 to 1.
        y: f64,
    },
    /// A pointer button, by its evdev code: `BTN_LEFT` is 272, which is what
    /// `wl_pointer.button` carries.
    Button {
        /// The evdev button code.
        button: u32,
        /// Down, as opposed to up.
        pressed: bool,
    },
    /// A scroll: `axis` is a `wl_pointer.axis` value, `value` the distance in
    /// surface coordinates.
    Axis {
        /// `wl_pointer.axis`.
        axis: u32,
        /// The distance.
        value: f64,
    },
}

/// What the compositor should do about an [`Input`].
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Send this key to the focused window.
    Key {
        /// The evdev keycode.
        code: u16,
        /// Down, as opposed to up.
        pressed: bool,
    },
    /// The modifier state changed; tell the focused window.
    Modifiers(Modifiers),
    /// Run a dispatcher, as `hyprctl dispatch` would.
    Dispatch {
        /// Its name, lower-cased.
        name: String,
        /// Its argument, as the bind wrote it.
        argument: String,
    },
    /// The pointer is at `(x, y)` on the screen.
    Pointer {
        /// Across.
        x: f64,
        /// Down.
        y: f64,
    },
    /// A pointer button, for the window under the pointer.
    Button {
        /// The evdev button code.
        button: u32,
        /// Down, as opposed to up.
        pressed: bool,
    },
    /// A scroll, for the window under the pointer.
    Axis {
        /// `wl_pointer.axis`.
        axis: u32,
        /// The distance.
        value: f64,
    },
}

/// The modifiers a bind is compared on.
///
/// Caps Lock and Num Lock are left out: they are locks a person leaves on,
/// and a bind that stopped working because Num Lock was on would look like a
/// broken compositor. Hyprland leaves the same two out.
const COMPARED: u32 = generated::SHIFT
    | generated::CONTROL
    | generated::MOD1
    | generated::MOD3
    | generated::MOD4
    | generated::MOD5;

/// One bind, with its key already resolved to a code.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bound {
    mods: u32,
    trigger: Trigger,
    /// `r`: fire on the release rather than the press.
    release: bool,
    /// `e`: fire again on each repeat while held.
    repeat: bool,
    /// `n`: the key reaches the focused client as well.
    non_consuming: bool,
    /// `i`: fire whatever modifiers are held.
    ignore_mods: bool,
    dispatcher: String,
    argument: String,
}

/// What sets a bind off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    /// A key, by evdev code.
    Key(u16),
    /// A pointer button, by evdev code.
    Button(u32),
    /// The wheel, by `wl_pointer.axis` and the sign of the movement.
    Wheel {
        axis: u32,
        /// Whether the bind wants a positive movement.
        positive: bool,
    },
}

/// The seat.
#[derive(Clone, Debug, Default)]
pub struct Seat {
    keyboard: Keyboard,
    binds: Vec<Bound>,
    /// Where the pointer is on the screen, in pixels.
    pointer: (f64, f64),
    /// The screen, which the pointer may not leave.
    screen: (f64, f64),
    /// Binds that have not been resolved, with the reason, for the log.
    unresolved: Vec<String>,
    /// Keys whose press a bind ate, so that their release is eaten too.
    ///
    /// Without this a client is told a key came up that it was never told
    /// went down, and a toolkit that keeps its own idea of what is held --
    /// every toolkit does -- has a key stuck down for ever. The modifiers
    /// have usually been let go by the time the key is, so the bind no longer
    /// matches and the release cannot be judged on its own.
    eaten: Vec<u16>,
}

impl Seat {
    /// A seat for a screen of `width` by `height`, with `config`'s binds.
    #[must_use]
    pub fn new(config: &Config, width: u32, height: u32) -> Self {
        let mut seat = Self {
            keyboard: Keyboard::new(),
            binds: Vec::new(),
            // The pointer starts in the middle, as Hyprland's does.
            pointer: (f64::from(width) / 2.0, f64::from(height) / 2.0),
            screen: (f64::from(width), f64::from(height)),
            unresolved: Vec::new(),
            eaten: Vec::new(),
        };
        seat.set_binds(config);
        seat
    }

    /// Take the binds from `config`, forgetting the ones before.
    pub fn set_binds(&mut self, config: &Config) {
        self.binds.clear();
        self.unresolved.clear();
        for bind in &config.binds {
            // A bind in a submap is not in the global map, and submaps are
            // not entered yet; one bound here would fire when it should not.
            if bind.submap.is_some() {
                continue;
            }
            let Some(trigger) = trigger_of(&bind.key) else {
                self.unresolved
                    .push(format!("{} is not a key this keymap has", bind.key));
                continue;
            };
            self.binds.push(Bound {
                mods: bind.mods.0 & COMPARED,
                trigger,
                release: bind.flags.release,
                repeat: bind.flags.repeat,
                non_consuming: bind.flags.non_consuming,
                ignore_mods: bind.flags.ignore_mods,
                dispatcher: bind.dispatcher.clone(),
                argument: bind.arg.clone(),
            });
        }
    }

    /// How many binds are live, and what could not be resolved.
    #[must_use]
    pub fn binds(&self) -> (usize, &[String]) {
        (self.binds.len(), &self.unresolved)
    }

    /// The keys held and the locks on, for `wl_keyboard.enter`.
    #[must_use]
    pub const fn keyboard(&self) -> &Keyboard {
        &self.keyboard
    }

    /// Where the pointer is.
    #[must_use]
    pub const fn pointer(&self) -> (f64, f64) {
        self.pointer
    }

    /// Take one input, and say what to do about it.
    pub fn input(&mut self, input: Input) -> Vec<Action> {
        match input {
            Input::Key {
                code,
                pressed,
                repeat,
            } => self.key(code, pressed, repeat),
            Input::Motion { dx, dy } => self.move_to(self.pointer.0 + dx, self.pointer.1 + dy),
            Input::Absolute { x, y } => self.move_to(x * self.screen.0, y * self.screen.1),
            Input::Button { button, pressed } => {
                let mut actions = self.fired(Trigger::Button(button), pressed, false);
                actions.push(Action::Button { button, pressed });
                actions
            }
            Input::Axis { axis, value } => {
                let mut actions = self.fired(
                    Trigger::Wheel {
                        axis,
                        positive: value > 0.0,
                    },
                    true,
                    false,
                );
                actions.push(Action::Axis { axis, value });
                actions
            }
        }
    }

    /// The screen changed size, so the pointer's limits did.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.screen = (f64::from(width), f64::from(height));
        let _ = self.move_to(self.pointer.0, self.pointer.1);
    }

    fn key(&mut self, code: u16, pressed: bool, repeat: bool) -> Vec<Action> {
        let changed = self.keyboard.key(code, pressed);
        let mut actions = Vec::new();
        if changed && is_modifier(code) {
            actions.push(Action::Modifiers(self.keyboard.modifiers()));
        }
        let fired = self.fired(Trigger::Key(code), pressed, repeat);
        let mut consumed = !fired.is_empty() && !self.consumes_nothing(Trigger::Key(code));
        actions.extend(fired);

        // The release of a key whose press was eaten is eaten as well.
        if pressed && consumed && !repeat && !self.eaten.contains(&code) {
            self.eaten.push(code);
        }
        if !pressed && let Some(at) = self.eaten.iter().position(|held| *held == code) {
            let _ = self.eaten.remove(at);
            consumed = true;
        }

        // A repeat is not sent: `wl_keyboard.key` has no way to say one, and
        // the client repeats for itself from `repeat_info`.
        if !consumed && !repeat {
            actions.push(Action::Key { code, pressed });
        }
        actions
    }

    /// The dispatchers the binds on `trigger` ask for.
    fn fired(&self, trigger: Trigger, pressed: bool, repeat: bool) -> Vec<Action> {
        let held = self.keyboard.modifiers().depressed & COMPARED;
        self.binds
            .iter()
            .filter(|bind| bind.trigger == trigger)
            .filter(|bind| bind.ignore_mods || bind.mods == held)
            .filter(|bind| if bind.release { !pressed } else { pressed })
            .filter(|bind| !repeat || bind.repeat)
            .map(|bind| Action::Dispatch {
                name: bind.dispatcher.clone(),
                argument: bind.argument.clone(),
            })
            .collect()
    }

    /// Whether every bind that matched `trigger` lets the key through.
    fn consumes_nothing(&self, trigger: Trigger) -> bool {
        self.binds
            .iter()
            .filter(|bind| bind.trigger == trigger)
            .all(|bind| bind.non_consuming)
    }

    fn move_to(&mut self, x: f64, y: f64) -> Vec<Action> {
        // The pointer may not leave the screen, and a NaN from a device that
        // reported nonsense must not become the position.
        let hold = |value: f64, limit: f64| {
            if value.is_nan() {
                return 0.0;
            }
            value.clamp(0.0, (limit - 1.0).max(0.0))
        };
        self.pointer = (hold(x, self.screen.0), hold(y, self.screen.1));
        vec![Action::Pointer {
            x: self.pointer.0,
            y: self.pointer.1,
        }]
    }
}

/// What sets `key` off, or `None` for a key this keymap does not have.
fn trigger_of(key: &Key) -> Option<Trigger> {
    Some(match key {
        Key::Sym(name) => Trigger::Key(compositor_xkb::code_of(name)?),
        // `code:NN` is the keycode as `xev` prints it, which is XKB's and so
        // eight above evdev's. Hyprland reads it the same way.
        Key::Code(code) => {
            Trigger::Key(u16::try_from(code.checked_sub(compositor_xkb::XKB_OFFSET)?).ok()?)
        }
        Key::Mouse(button) => Trigger::Button(*button),
        Key::Wheel(name) => {
            let (axis, positive) = match name.as_str() {
                // Scrolling up is a negative movement along the vertical
                // axis, as `wl_pointer.axis` has it: the value is how far the
                // surface's content moved, not the finger.
                "mouse_up" => (
                    compositor_protocol::core::wl_pointer::axis::VERTICAL_SCROLL,
                    false,
                ),
                "mouse_down" => (
                    compositor_protocol::core::wl_pointer::axis::VERTICAL_SCROLL,
                    true,
                ),
                "mouse_left" => (
                    compositor_protocol::core::wl_pointer::axis::HORIZONTAL_SCROLL,
                    false,
                ),
                "mouse_right" => (
                    compositor_protocol::core::wl_pointer::axis::HORIZONTAL_SCROLL,
                    true,
                ),
                _ => return None,
            };
            Trigger::Wheel { axis, positive }
        }
    })
}

/// Whether this key is one that changes the modifier state.
fn is_modifier(code: u16) -> bool {
    compositor_xkb::key(code).is_some_and(|key| key.held != 0 || key.locked != 0)
}

/// What Hyprland's modifier names mean here, so that a `Mods` from the
/// configuration and a mask from the keymap are the same bits.
///
/// They are, and this says so rather than leaving it to be noticed: XKB
/// declares `Shift`, `Lock`, `Control` and `Mod1` to `Mod5` in that order,
/// and `compositor/config`'s `Mods` numbers them the same way.
const _: () = {
    assert!(Mods::SHIFT == generated::SHIFT);
    assert!(Mods::CAPS == generated::LOCK);
    assert!(Mods::CTRL == generated::CONTROL);
    assert!(Mods::ALT == generated::MOD1);
    assert!(Mods::MOD2 == generated::MOD2);
    assert!(Mods::MOD3 == generated::MOD3);
    assert!(Mods::LOGO == generated::MOD4);
    assert!(Mods::MOD5 == generated::MOD5);
};

#[cfg(test)]
mod tests;
