//! The password being typed and what became of the last one: upstream's
//! `CHyprlock::onKey`/`handleKeySym` and `CAuth`, with the clock passed in
//! so that every rule is a host test.
//!
//! * Typed text is appended; `BackSpace` and `Delete` take one character
//!   off (the last UTF-8 character, not the last byte) and are the only
//!   keys that repeat while held; `Escape`, `Ctrl+U`, `Ctrl+A` and
//!   `Ctrl+BackSpace` clear.
//! * `Return` or `KP_Enter` submits, unless the field is empty and
//!   `general:ignore_empty_input` is set. The field empties at once and the
//!   check runs; keys are ignored until it answers.
//! * The check holds a refusal for as long as its policy says (`authd`'s
//!   `FailDelaySec`, as upstream's PAM holds one), and the field shows
//!   `check_color` until it answers. The failure's text then shows for
//!   `general:fail_timeout` milliseconds, or until the next key, and no new
//!   attempt is submitted until its `retry_after_ms` is over.

use crate::auth::Verdict;

/// What a key did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Keyed {
    /// Nothing that changes a picture.
    Nothing,
    /// The field or the lock state changed: draw again.
    Redraw,
    /// The field was submitted: check this password.
    Submit(String),
    /// A key during `--grace`: unlock without a password.
    Grace,
}

/// The lock screen's state between keys.
#[derive(Clone, Debug, Default)]
pub struct Session {
    buffer: String,
    /// Failed attempts so far: `$ATTEMPTS`.
    pub attempts: usize,
    /// The current failure's text: `$FAIL`.
    pub fail_text: String,
    /// The password check's last failure: `$PAMFAIL`.
    pub pam_fail: Option<String>,
    /// Whether the fail text and colour are showing, and until when.
    fail_until: Option<u64>,
    /// When the check submitted last answers, while it is running.
    checking: bool,
    /// A refusal to land at the next [`Session::tick`].
    refused: Option<Verdict>,
    /// Before when `Return` submits nothing: the last refusal's
    /// `retry_after_ms`.
    retry_at: u64,
    /// Caps Lock, as the field's colours read it.
    pub caps_lock: bool,
    /// Num Lock.
    pub num_lock: bool,
    /// Whether the password was accepted.
    pub accepted: bool,
    /// Keys held down, by evdev code: a release of a key never pressed is
    /// ignored, as upstream ignores a stray release.
    held: Vec<u32>,
    /// Until when a key unlocks with no password (`--grace`).
    grace_until: Option<u64>,
    /// `general:ignore_empty_input`.
    ignore_empty: bool,
    /// `general:fail_timeout`.
    fail_timeout: u64,
}

/// One key event, as the toolkit reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPress<'a> {
    /// The evdev code.
    pub code: u32,
    /// Its keysym's name.
    pub keysym: &'a str,
    /// What it types.
    pub text: &'a str,
    /// Down or up.
    pub pressed: bool,
    /// The runtime retyping a held key.
    pub repeat: bool,
    /// Control held.
    pub control: bool,
    /// Caps Lock on, as the keyboard's modifiers say.
    pub caps_lock: bool,
    /// Num Lock on.
    pub num_lock: bool,
}

impl Session {
    /// A session reading `general:ignore_empty_input` and
    /// `general:fail_timeout`, with `grace` milliseconds from `now` in which
    /// any key unlocks.
    #[must_use]
    pub fn new(ignore_empty: bool, fail_timeout: i64, grace: u64, now: u64) -> Self {
        Self {
            ignore_empty,
            fail_timeout: u64::try_from(fail_timeout).unwrap_or(0),
            grace_until: (grace > 0).then(|| now.saturating_add(grace)),
            ..Self::default()
        }
    }

    /// How many characters are in the field, which is how many dots show.
    #[must_use]
    pub fn length(&self) -> usize {
        self.buffer.chars().count()
    }

    /// Whether a submitted password is being checked: `checkWaiting`.
    #[must_use]
    pub const fn checking(&self) -> bool {
        self.checking
    }

    /// Whether the fail text shows: `m_bDisplayFailText`.
    #[must_use]
    pub const fn failing(&self) -> bool {
        self.fail_until.is_some()
    }

    /// A key.
    pub fn key(&mut self, key: &KeyPress<'_>, now: u64) -> Keyed {
        if key.pressed && self.grace_until.is_some_and(|until| now < until) {
            return Keyed::Grace;
        }
        if key.repeat {
            // Only the keys upstream repeats itself.
            if !key.pressed || !matches!(key.keysym, "BackSpace" | "Delete") || self.checking {
                return Keyed::Nothing;
            }
            return self.keysym(key, now);
        }
        if key.pressed {
            if self.held.contains(&key.code) {
                return Keyed::Nothing;
            }
            self.held.push(key.code);
        } else {
            let Some(at) = self.held.iter().position(|code| *code == key.code) else {
                return Keyed::Nothing;
            };
            let _ = self.held.remove(at);
        }
        if self.checking {
            return Keyed::Redraw;
        }
        self.fail_until = None;
        if !key.pressed {
            return Keyed::Redraw;
        }
        self.caps_lock = key.caps_lock;
        self.num_lock = key.num_lock;
        self.keysym(key, now)
    }

    /// `handleKeySym`.
    fn keysym(&mut self, key: &KeyPress<'_>, now: u64) -> Keyed {
        match key.keysym {
            "Escape" => self.buffer.clear(),
            "u" | "a" | "BackSpace" if key.control => self.buffer.clear(),
            "Return" | "KP_Enter" => {
                if (self.buffer.is_empty() && self.ignore_empty) || now < self.retry_at {
                    return Keyed::Nothing;
                }
                self.checking = true;
                return Keyed::Submit(std::mem::take(&mut self.buffer));
            }
            "BackSpace" | "Delete" => {
                let _ = self.buffer.pop();
            }
            "Caps_Lock" => self.caps_lock = !self.caps_lock,
            "Num_Lock" => self.num_lock = !self.num_lock,
            _ => self.buffer.push_str(key.text),
        }
        Keyed::Redraw
    }

    /// The check answered at `now`. A refusal lands at the next
    /// [`Session::tick`], and holds the next attempt back for its
    /// `retry_after_ms`.
    pub fn answered(&mut self, outcome: Verdict, now: u64) {
        match &outcome {
            Verdict::Accepted => {
                self.checking = false;
                self.accepted = true;
                self.pam_fail = Some(outcome.fail_text().to_owned());
                return;
            }
            Verdict::Failed { retry_after_ms, .. } => {
                self.retry_at = now.saturating_add(*retry_after_ms);
            }
            Verdict::Unavailable(_) => {}
        }
        self.refused = Some(outcome);
    }

    /// The backend asked another question in the same conversation: the
    /// field takes the next answer.
    pub fn prompted(&mut self) {
        self.checking = false;
    }

    /// Let time pass: a refusal whose delay is over lands, and a fail text
    /// whose time is up goes. Whether anything changed.
    pub fn tick(&mut self, now: u64) -> bool {
        let mut changed = false;
        if let Some(outcome) = self.refused.take() {
            let text = outcome.fail_text().to_owned();
            self.checking = false;
            self.attempts += 1;
            self.fail_text.clone_from(&text);
            self.pam_fail = Some(text);
            self.fail_until = Some(now.saturating_add(self.fail_timeout));
            changed = true;
        }
        if self.fail_until.is_some_and(|until| now >= until) {
            self.fail_until = None;
            changed = true;
        }
        changed
    }

    /// The next moment [`Session::tick`] has something to do, if any.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        if self.refused.is_some() {
            return Some(0);
        }
        self.fail_until
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyPress, Keyed, Session};

    /// `authd`'s hold on a refusal, which comes before the answer here.
    const FAIL_DELAY_MS: u64 = 2000;
    use crate::auth::{REJECTED, Verdict};

    fn refused() -> Verdict {
        Verdict::Failed {
            text: REJECTED.to_owned(),
            retry_after_ms: FAIL_DELAY_MS,
        }
    }

    fn press(code: u32, keysym: &'static str, text: &'static str) -> KeyPress<'static> {
        KeyPress {
            code,
            keysym,
            text,
            pressed: true,
            repeat: false,
            control: false,
            caps_lock: false,
            num_lock: false,
        }
    }

    fn tap(
        session: &mut Session,
        code: u32,
        keysym: &'static str,
        text: &'static str,
        now: u64,
    ) -> Keyed {
        let down = session.key(&press(code, keysym, text), now);
        let _ = session.key(
            &KeyPress {
                pressed: false,
                ..press(code, keysym, text)
            },
            now,
        );
        down
    }

    fn typed(session: &mut Session, word: &'static str, now: u64) {
        for (at, _) in word.char_indices() {
            let one = word.get(at..at + 1).unwrap_or("");
            let _ = tap(session, 100 + u32::try_from(at).unwrap_or(0), one, one, now);
        }
    }

    #[test]
    fn typing_backspace_escape_and_enter() {
        let mut session = Session::new(true, 2000, 0, 0);
        typed(&mut session, "ferrjx", 0);
        assert_eq!(session.length(), 6);
        let _ = tap(&mut session, 14, "BackSpace", "", 0);
        let _ = tap(&mut session, 14, "BackSpace", "", 0);
        typed(&mut session, "ix", 0);
        assert_eq!(
            tap(&mut session, 28, "Return", "", 0),
            Keyed::Submit("ferrix".to_owned())
        );
        assert_eq!(session.length(), 0);
        assert!(session.checking());
        let mut other = Session::new(true, 2000, 0, 0);
        typed(&mut other, "abc", 0);
        let _ = tap(&mut other, 1, "Escape", "", 0);
        assert_eq!(other.length(), 0);
        // Empty input is ignored with `ignore_empty_input`.
        assert_eq!(tap(&mut other, 28, "Return", "", 0), Keyed::Nothing);
        assert!(!other.checking());
    }

    #[test]
    fn a_wrong_password_shows_its_failure_then_goes() {
        let mut session = Session::new(true, 2000, 0, 0);
        typed(&mut session, "wrong", 0);
        let _ = tap(&mut session, 28, "Return", "", 0);
        assert!(session.checking());
        session.answered(refused(), 10 + FAIL_DELAY_MS);
        assert!(session.tick(10 + FAIL_DELAY_MS));
        assert!(!session.checking());
        assert!(session.failing());
        assert_eq!(session.attempts, 1);
        assert_eq!(session.pam_fail.as_deref(), Some("Authentication failed"));
        // `fail_timeout` later, it goes.
        assert!(session.tick(10 + FAIL_DELAY_MS + 2000));
        assert!(!session.failing());
    }

    #[test]
    fn a_refusal_holds_the_next_attempt_for_its_retry_after() {
        let mut session = Session::new(true, 2000, 0, 0);
        typed(&mut session, "a", 0);
        let _ = tap(&mut session, 28, "Return", "", 0);
        session.answered(
            Verdict::Failed {
                text: "wait 16 s".to_owned(),
                retry_after_ms: 16_000,
            },
            100,
        );
        let _ = session.tick(100);
        assert_eq!(session.pam_fail.as_deref(), Some("wait 16 s"));
        typed(&mut session, "b", 200);
        assert_eq!(tap(&mut session, 28, "Return", "", 200), Keyed::Nothing);
        assert_eq!(session.length(), 1);
        assert_eq!(
            tap(&mut session, 28, "Return", "", 16_100),
            Keyed::Submit("b".to_owned())
        );
    }

    #[test]
    fn a_key_hides_the_failure_and_keys_wait_for_the_check() {
        let mut session = Session::new(false, 2000, 0, 0);
        let _ = tap(&mut session, 28, "Return", "", 0);
        // Keys while checking type nothing.
        typed(&mut session, "x", 1);
        assert_eq!(session.length(), 0);
        session.answered(Verdict::Unavailable("no service".to_owned()), 1);
        let _ = session.tick(1);
        assert_eq!(session.fail_text, "no service");
        assert!(session.failing());
        typed(&mut session, "y", 2);
        assert!(!session.failing());
        assert_eq!(session.length(), 1);
    }

    #[test]
    fn only_backspace_repeats_and_characters_come_off_whole() {
        let mut session = Session::new(false, 2000, 0, 0);
        let _ = tap(&mut session, 40, "adiaeresis", "ä", 0);
        let _ = tap(&mut session, 30, "a", "a", 0);
        let repeat = |keysym, text| KeyPress {
            repeat: true,
            ..press(30, keysym, text)
        };
        assert_eq!(session.key(&repeat("a", "a"), 0), Keyed::Nothing);
        assert_eq!(session.length(), 2);
        assert_eq!(session.key(&repeat("BackSpace", ""), 0), Keyed::Redraw);
        assert_eq!(session.key(&repeat("BackSpace", ""), 0), Keyed::Redraw);
        assert_eq!(session.length(), 0);
    }

    #[test]
    fn control_u_clears_and_grace_unlocks() {
        let mut session = Session::new(false, 2000, 0, 0);
        typed(&mut session, "abc", 0);
        let control_u = KeyPress {
            control: true,
            ..press(22, "u", "u")
        };
        let _ = session.key(&control_u, 0);
        assert_eq!(session.length(), 0);
        let mut grace = Session::new(false, 2000, 5000, 1000);
        assert_eq!(grace.key(&press(30, "a", "a"), 3000), Keyed::Grace);
        assert_ne!(grace.key(&press(31, "s", "s"), 7000), Keyed::Grace);
    }
}
