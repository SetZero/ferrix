//! The clipboard from a program's side.
//!
//! `wl-copy` and `wl-paste` are what a person uses on Wayland, and neither
//! is on Ferrix. This is both, in the smallest honest form:
//!
//! * `clip copy <text>` offers the text as `text/plain;charset=utf-8`, sets
//!   it as the selection, and stays alive to answer whoever pastes -- which
//!   it must, because Wayland's clipboard is a promise and not a buffer: the
//!   data lives in the program that copied it.
//! * `clip paste` waits for the compositor to say what the selection holds,
//!   asks for the text through a pipe, and prints what comes back.
//!
//! Between them they are a test of the compositor's clipboard that needs no
//! window and no screen: what is copied in one process comes out of another.

/// The Wayland client both halves are.
pub mod client;

pub use client::{copy, paste};
