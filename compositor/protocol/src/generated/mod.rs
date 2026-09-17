//! The generated interface tables, one module per vendored protocol.
//!
//! Written by `scripts/gen-wayland-protocol.py`; `cargo xtask check` runs it
//! with `--check`, so an edit here that the XML does not justify fails the
//! gate. This file is the only hand-written one in the directory.

pub mod alpha_modifier;
pub mod content_type;
pub mod core;
pub mod cursor_shape;
pub mod data_control;
pub mod ext_data_control;
pub mod ext_workspace;
pub mod foreign_list;
pub mod foreign_toplevel;
pub mod fractional_scale;
pub mod gamma_control;
pub mod idle_inhibit;
pub mod idle_notify;
pub mod input_method;
pub mod kde_decoration;
pub mod layer_shell;
pub mod output_management;
pub mod output_power;
pub mod pointer_constraints;
pub mod pointer_gestures;
pub mod presentation;
pub mod primary_selection;
pub mod relative_pointer;
pub mod screencopy;
pub mod session_lock;
pub mod shortcuts_inhibit;
pub mod single_pixel;
pub mod system_bell;
pub mod text_input;
pub mod toplevel_icon;
pub mod toplevel_tag;
pub mod viewporter;
pub mod virtual_keyboard;
pub mod virtual_pointer;
pub mod xdg_activation;
pub mod xdg_decoration;
pub mod xdg_dialog;
pub mod xdg_output;
pub mod xdg_shell;
