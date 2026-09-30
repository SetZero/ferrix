//! hyprlock, for Ferrix.
//!
//! Upstream hyprlock (`hyprwm/hyprlock` 0.9.6) is a lock screen: it takes
//! the session lock through `ext-session-lock-v1`, draws the widgets its
//! configuration describes on a surface for every screen, reads a password
//! and hands it to PAM, and lets the lock go when PAM says yes. This is the
//! same program for Ferrix, reading the same `hyprlock.conf` unchanged.
//! `docs/DESKTOP-CLIENTS.md` §5 says what of the file works and what cannot.
//!
//! Everything here is a pure function of what it is given -- the file, the
//! screen, the clock, the keys -- so each rule is a host test: [`config`]
//! reads the file through `compositor/hyprlang`, [`scene`] draws one screen's
//! widgets with [`paint`] and [`blur`], [`session`] is the field's key rules,
//! and [`auth`] the one interface a password goes through.

pub mod auth;
pub mod blur;
pub mod config;
pub mod format;
pub mod layout;
pub mod paint;
pub mod scene;
pub mod session;
pub mod tween;

/// Say a line on standard error, as upstream's log does: every diagnostic
/// this program has, one line each.
pub fn say(line: &str) {
    use std::io::Write as _;
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{line}");
}
