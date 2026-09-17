//! The windows a compositor has, listed from outside it.
//!
//! `lswt` is Leon Henrik Plickat's "list wayland toplevels", and this is the
//! same idea in the smallest honest form: bind
//! `zwlr_foreign_toplevel_manager_v1`, take the handle the compositor makes
//! for each window, read its title, its application id and its states, and
//! print a line each.
//!
//! What that protocol is *for* is a taskbar -- waybar's `wlr/taskbar`, eww's
//! window list, every panel that shows what is open -- and none of them is on
//! Ferrix. A program that prints the same list is the part of a bar that can
//! be tested without a screen: if the compositor tells this one the truth, it
//! is telling waybar the truth.
//!
//! It is also how a window is *acted on* from outside.
//! `lswt activate <title>` focuses one and `lswt close <title>` asks it to
//! close, which are the two things a click on a taskbar entry does.

/// The Wayland client.
pub mod client;

pub use client::{Toplevel, Want, run};
