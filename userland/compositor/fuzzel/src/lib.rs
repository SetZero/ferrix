//! fuzzel, the application launcher, for Ferrix.
//!
//! A port of Daniel Eklöf's fuzzel (codeberg.org/dnkl/fuzzel, 1.12): it
//! reads `~/.config/fuzzel/fuzzel.ini` as fuzzel does, lists the `.desktop`
//! entries fuzzel would list, ranks what is typed against them by fuzzel's
//! rules, and starts the one chosen. Each module names the part of fuzzel's
//! source it follows.
//!
//! The parts here hold no socket and draw no pixel, so they are host-tested
//! against the behaviour fuzzel's source defines.

pub mod cache;
pub mod cli;
pub mod config;
pub mod desktop;
pub mod dmenu;
pub mod exec;
pub mod geometry;
pub mod icon;
pub mod keys;
pub mod keysym;
pub mod launcher;
pub mod matching;
pub mod prompt;

#[cfg(test)]
mod testdir;
