//! A terminal emulator: a pseudoterminal, a character grid, and a window to
//! draw it in.
//!
//! `docs/ROADMAP.md` stage 18's exit asks for a terminal on the compositor,
//! and this is it. The parts are apart on purpose:
//!
//! * [`grid`] is the screen a program writes at -- the cells, the cursor and
//!   the escape sequences -- and is a pure function of the bytes, so every
//!   rule it has is host-tested.
//! * [`paint`] draws a grid into a buffer with the Hack faces in `font`,
//!   rasterised into coverage cells beside it.
//! * [`pty`] is `/dev/ptmx`, the slave, and the program on it.
//! * [`keys`] turns what the compositor says about the keyboard into the
//!   bytes a terminal sends.
//!
//! The binary is the Wayland client that holds them together.

/// The window the grid is drawn in.
#[cfg(target_os = "linux")]
pub mod client;
/// The font, as coverage cells: generated, and read only by [`paint`].
mod font;
/// The character grid and its escape sequences.
pub mod grid;
/// What a key sends.
pub mod keys;
/// Drawing a grid into a buffer.
pub mod paint;
/// The pseudoterminal, and the program on it.
#[cfg(target_os = "linux")]
pub mod pty;

#[cfg(test)]
mod tests;
