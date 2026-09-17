//! The Wayland protocols the compositor speaks.
//!
//! Every interface here is a table generated from the protocol's own XML by
//! `scripts/gen-wayland-protocol.py`, which `cargo xtask check` runs with
//! `--check` so a hand edit cannot drift from the file it came from. The XML
//! is vendored under `protocols/`, not read from the machine: a table built
//! from whatever `wayland-protocols` the builder happened to have installed
//! would change under the compositor without a commit.
//!
//! What the tables are *for* is `compositor_wire`: the wire format is
//! untyped, so a reader has to be told the signature of what it is about to
//! read, and [`Interface`] is where that comes from.
//!
//! # What is here
//!
//! * **`core`** is `wayland.xml`: `wl_display`, `wl_registry`,
//!   `wl_compositor` and `wl_surface`, `wl_shm`, `wl_seat` with its
//!   keyboard and pointer, `wl_output`, `wl_subcompositor` and the data
//!   device.
//! * **`xdg_shell`** is how an application gets a window.
//! * **`xdg_decoration`** is who draws the title bar, which for a tiling
//!   compositor is always the client-side answer of "neither".
//! * **`layer_shell`** is `zwlr_layer_shell_v1`, which is how a bar or a
//!   wallpaper places itself. Hyprland's own bars use it.
//! * **`foreign_toplevel`** is `zwlr_foreign_toplevel_management_v1`: the
//!   list of windows, and the four things a bar does with one. It is what
//!   fills a taskbar, and it is the other half of a bar's job -- layer-shell
//!   puts the bar on the screen and this tells it what to draw.
//! * **`screencopy`** is `zwlr_screencopy_v1`: a screenshot. `grim`,
//!   `hyprshot` and every screen recorder on wlroots go through it.
//! * **`cursor_shape`** is `wp_cursor_shape_v1`: a client naming the cursor
//!   it wants -- an I-beam, a hand, a resize arrow -- rather than drawing
//!   one, which is what a toolkit prefers and what a compositor with a
//!   theme is for.
//! * **`primary_selection`** is the middle-click paste, which is the
//!   selection's older and simpler sibling.
//! * **`xdg_activation`** is one program asking the compositor to focus
//!   another: a link opened from a chat window raising the browser.
//! * **`viewporter`** is a client saying its buffer is to be cropped or
//!   scaled into its surface, and **`fractional_scale`** is the compositor
//!   telling it a scale that is not a whole number.
//! * **`toplevel_icon`** is the icon a taskbar draws beside a window's
//!   name.
//! * **`text_input`** and **`input_method`** are the two halves of typing
//!   through an input method: the application says it wants text, the
//!   method says what was typed, and the compositor is what joins them.
//! * **`session_lock`** is `ext-session-lock-v1`: the screen lock.
//!   `hyprlock` and `swaylock` speak it, and it is the one protocol whose
//!   whole point is that the compositor stops drawing everything else.
//!
//! The list of protocols is `FILES` in the generator. Adding one is vendoring
//! its XML, adding a line there and a module to `generated/mod.rs`, and
//! naming its interfaces in `probe/interfaces.c` and `probe/interfaces.sh`,
//! so that the new tables are compared against libwayland's compiled ones as
//! every other table here is.

mod generated;

pub use compositor_wire::Interface;
pub use generated::{
    core, cursor_shape, foreign_toplevel, fractional_scale, input_method, layer_shell,
    primary_selection, screencopy, session_lock, text_input, toplevel_icon, viewporter,
    xdg_activation, xdg_decoration, xdg_shell,
};

/// Every interface the compositor offers as a global, with the version it
/// offers, in the order `wl_registry.global` announces them.
///
/// `wl_display` and `wl_registry` are not here: a connection starts with the
/// first and asks for the second, and neither is ever bound.
pub const GLOBALS: &[&Interface] = &[
    &core::WL_COMPOSITOR,
    &core::WL_SUBCOMPOSITOR,
    &core::WL_SHM,
    &core::WL_SEAT,
    &core::WL_OUTPUT,
    &core::WL_DATA_DEVICE_MANAGER,
    &xdg_shell::XDG_WM_BASE,
    &xdg_decoration::ZXDG_DECORATION_MANAGER_V1,
    &layer_shell::ZWLR_LAYER_SHELL_V1,
    &foreign_toplevel::ZWLR_FOREIGN_TOPLEVEL_MANAGER_V1,
    &screencopy::ZWLR_SCREENCOPY_MANAGER_V1,
    &session_lock::EXT_SESSION_LOCK_MANAGER_V1,
    &cursor_shape::WP_CURSOR_SHAPE_MANAGER_V1,
    &primary_selection::ZWP_PRIMARY_SELECTION_DEVICE_MANAGER_V1,
    &xdg_activation::XDG_ACTIVATION_V1,
    &viewporter::WP_VIEWPORTER,
    &fractional_scale::WP_FRACTIONAL_SCALE_MANAGER_V1,
    &toplevel_icon::XDG_TOPLEVEL_ICON_MANAGER_V1,
    &text_input::ZWP_TEXT_INPUT_MANAGER_V3,
    &input_method::ZWP_INPUT_METHOD_MANAGER_V2,
];

#[cfg(test)]
mod tests;
