//! The generated interface tables, one module per vendored protocol.
//!
//! Written by `scripts/gen-wayland-protocol.py`; `cargo xtask check` runs it
//! with `--check`, so an edit here that the XML does not justify fails the
//! gate. This file is the only hand-written one in the directory.

pub mod core;
pub mod foreign_toplevel;
pub mod layer_shell;
pub mod screencopy;
pub mod session_lock;
pub mod xdg_decoration;
pub mod xdg_shell;
