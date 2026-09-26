//! hypridle, for Ferrix.
//!
//! Upstream hypridle (0.1.8, `/var/cache/hyprland-build/src/hypridle`) reads
//! `~/.config/hypr/hypridle.conf`, asks the compositor through
//! `ext-idle-notify-v1` to be told when the seat has gone unused for each
//! `listener`'s `timeout`, and runs the listener's `on-timeout` then and its
//! `on-resume` at the next input. It also runs `lock_cmd` when logind says
//! the session is to be locked, and `on_lock_cmd` when the compositor says,
//! through `hyprland-lock-notify-v1`, that it is.
//!
//! This is the same program, reading the same file unchanged. What Ferrix
//! lacks -- D-Bus, logind, a suspend -- is said at start, one line an option
//! ([`config::ferrix_notes`]), and logind's part in `loginctl lock-session`
//! is played by a socket of hypridle's own, which Ferrix's `loginctl`
//! writes to ([`session`]).

/// Reading hyprlang.
pub mod conf;

/// The options and their defaults.
pub mod config;

/// What happens on each event.
pub mod idle;

/// Upstream's log lines.
pub mod log;

/// The socket `loginctl` reaches hypridle through.
pub mod session;

/// The Wayland connection and the loop.
pub mod client;

#[cfg(test)]
mod tests;
