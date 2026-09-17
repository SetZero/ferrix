//! A screenshot, taken from outside the compositor.
//!
//! `grim` is what a person runs on wlroots, and `hyprshot` is Hyprland's
//! wrapper round it; both go through `zwlr_screencopy_v1` and nothing else.
//! Neither is on Ferrix, so this is the same protocol in the smallest honest
//! form: bind the manager and a `wl_output`, ask for a frame, make the
//! `wl_shm` buffer the compositor says to make, hand it over, and read back
//! what was written into it.
//!
//! What comes out is the screen as the compositor composed it, pixel for
//! pixel -- which is the useful thing about testing it: the picture a
//! screenshot gives can be compared against the picture `compositor/render`
//! blesses, and the two are reached by completely different paths.
//!
//! The digest is FNV-1a over the red, green and blue of each pixel in row
//! order, so that a program on the guest and a test on the host can compare
//! a whole screen through a serial port.

/// The Wayland client.
pub mod client;

pub use client::{Shot, digest, take};
