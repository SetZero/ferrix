//! What is held, what is locked, and what that makes `wl_keyboard.modifiers`.

use crate::{Key, key};

/// The four masks `wl_keyboard.modifiers` carries.
///
/// A client compares them against the indices its own copy of the keymap
/// gives, so they mean what [`crate::generated`]'s constants mean and nothing
/// else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Held down now.
    pub depressed: u32,
    /// Latched: always zero here. See the crate's documentation.
    pub latched: u32,
    /// Locked, as `Caps Lock` and `Num Lock` leave them.
    pub locked: u32,
    /// The layout group: always zero, since there is one layout.
    pub group: u32,
}

/// The keyboard's state: which keys are down and which locks are on.
#[derive(Clone, Debug, Default)]
pub struct Keyboard {
    /// The evdev codes held, in the order they were pressed, which is the
    /// order `wl_keyboard.enter` must carry them in.
    held: Vec<u16>,
    locked: u32,
}

impl Keyboard {
    /// A keyboard with nothing held and nothing locked.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            held: Vec::new(),
            locked: 0,
        }
    }

    /// Take a key press or release, and say whether it changed anything.
    ///
    /// A repeat -- a press of a key already held, which evdev sends as value
    /// 2 and this takes as another `true` -- changes nothing and is reported
    /// as such, because `wl_keyboard.key` has no repeat: the client repeats
    /// for itself from `wl_keyboard.repeat_info`.
    pub fn key(&mut self, code: u16, down: bool) -> bool {
        let at = self.held.iter().position(|held| *held == code);
        match (down, at) {
            (true, None) => {
                self.held.push(code);
                // A lock toggles on the press, as libxkbcommon's state
                // machine has it: the probe measured `Caps Lock` leaving its
                // bit locked after a press and a release, and the release is
                // where it would be undone if it toggled twice.
                if let Some(locked) = key(code).map(|key| key.locked)
                    && locked != 0
                {
                    self.locked ^= locked;
                }
                true
            }
            (true, Some(_)) => false,
            (false, Some(at)) => {
                let _ = self.held.remove(at);
                true
            }
            (false, None) => false,
        }
    }

    /// Whether `code` is held.
    #[must_use]
    pub fn is_held(&self, code: u16) -> bool {
        self.held.contains(&code)
    }

    /// Every key held, in the order they were pressed.
    #[must_use]
    pub fn pressed(&self) -> &[u16] {
        &self.held
    }

    /// The masks to send.
    #[must_use]
    pub fn modifiers(&self) -> Modifiers {
        Modifiers {
            depressed: self
                .held
                .iter()
                .filter_map(|code| key(*code))
                .map(|key: &Key| key.held)
                .fold(0, |mask, held| mask | held),
            latched: 0,
            locked: self.locked,
            group: 0,
        }
    }

    /// Forget everything, as a compositor does when it loses the devices.
    pub fn clear(&mut self) {
        self.held.clear();
        self.locked = 0;
    }
}
