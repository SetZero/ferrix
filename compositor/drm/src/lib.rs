//! `/dev/dri/card0`, through the legacy mode-setting calls.
//!
//! What a compositor needs from a screen and nothing else: find a connected
//! connector and a mode, find a CRTC that can drive it, make dumb buffers,
//! map them, and show one. No atomic commit, no GEM import, no render node --
//! `docs/DISPLAY.md` says why, and what stage 19 adds.
//!
//! It is the one crate in the compositor that only builds on Linux, because
//! `/dev/dri` is Linux's and Ferrix's through its Linux ABI. The parts that
//! are arithmetic rather than ioctls -- choosing a mode, choosing a CRTC,
//! filling a buffer, naming a plane -- are in [`modeset`] and are tested on
//! any host.

#[cfg(target_os = "linux")]
mod card;
mod edid;
pub mod modeset;

#[cfg(target_os = "linux")]
pub use card::{Card, Dumb, Plan, cards, plan, planes, plans, rename, show};
pub use edid::{Edid, registered};

#[cfg(test)]
mod tests;
