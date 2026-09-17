//! The generated interface tables, one module per vendored protocol.
//!
//! Written by `scripts/gen-wayland-protocol.py`; `cargo xtask check` runs it
//! with `--check`, so an edit here that the XML does not justify fails the
//! gate. This file is the only hand-written one in the directory.

pub mod core;
pub mod cursor_shape;
pub mod foreign_toplevel;
pub mod fractional_scale;
pub mod input_method;
pub mod layer_shell;
pub mod primary_selection;
pub mod screencopy;
pub mod session_lock;
pub mod text_input;
pub mod toplevel_icon;
pub mod viewporter;
pub mod xdg_activation;
pub mod xdg_decoration;
pub mod xdg_shell;
