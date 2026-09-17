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
    /// The layout group in force: which of the keymap's layouts a key means.
    ///
    /// A client is told the index and reads the group out of the keymap it
    /// already holds, which is why a switch sends no new keymap.
    pub group: u32,
}

/// The keyboard's state: which keys are down, which locks are on, and which
/// layout group is in force.
///
/// The group belongs to the keyboard and not to the seat, as Hyprland's does
/// (`IKeyboard::m_modifiersState.group`): two keyboards may sit on different
/// layouts, and what clients are told is the group of whichever one was last
/// typed on.
#[derive(Clone, Debug)]
pub struct Keyboard {
    /// The evdev codes held, in the order they were pressed, which is the
    /// order `wl_keyboard.enter` must carry them in.
    held: Vec<u16>,
    locked: u32,
    /// How many layouts the keymap has, never zero.
    groups: u32,
    /// Which of them is in force, always below `groups`.
    group: u32,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Keyboard {
    /// A keyboard with nothing held, nothing locked, and one layout.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            held: Vec::new(),
            locked: 0,
            groups: 1,
            group: 0,
        }
    }

    /// Say how many layouts the keymap has, keeping the group inside it.
    ///
    /// Called when the keymap changes, which is when a configuration is read
    /// or reloaded. A keymap with no layouts is not a thing a client could be
    /// told about, so zero is read as one.
    pub fn set_groups(&mut self, groups: u32) {
        self.groups = groups.max(1);
        self.group %= self.groups;
    }

    /// How many layouts the keymap has.
    #[must_use]
    pub const fn groups(&self) -> u32 {
        self.groups
    }

    /// Which layout is in force.
    #[must_use]
    pub const fn group(&self) -> u32 {
        self.group
    }

    /// Put `group` in force, and say whether that changed anything.
    ///
    /// An index beyond the last layout comes back round by modulus, which is
    /// what libxkbcommon does with an effective layout out of range
    /// (`xkbcommon.h`: "brought back into range" by the number of groups) and
    /// what `hyprctl switchxkblayout next` depends on -- Hyprland asks for
    /// the index after the last one and lets the wrap answer it. A caller
    /// that wants to refuse an out-of-range index must check before calling,
    /// as Hyprland's numeric case does.
    pub fn set_group(&mut self, group: u32) -> bool {
        let wanted = group % self.groups;
        let changed = wanted != self.group;
        self.group = wanted;
        changed
    }

    /// Put the next layout in force, wrapping at the last.
    pub fn next_group(&mut self) -> bool {
        self.set_group(self.group.saturating_add(1))
    }

    /// Put the previous layout in force, wrapping at the first.
    pub fn previous_group(&mut self) -> bool {
        self.set_group(self.group + self.groups.saturating_sub(1))
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
            group: self.group,
        }
    }

    /// Forget what is held and locked, as a compositor does when it loses
    /// the devices.
    ///
    /// The group and the keymap's layout count stay: they are the
    /// configuration's, not the fingers'.
    pub fn clear(&mut self) {
        self.held.clear();
        self.locked = 0;
    }
}
