//! The options the compositor reads, with their types and defaults.
//!
//! Names are Hyprland's, written as `hyprctl getoption` writes them:
//! category, colon, name. The table holds the options the compositor
//! implements, not every option Hyprland has. A configuration naming any
//! other gets Hyprland's "does not exist" diagnostic and the rest of the file
//! still applies. The defaults follow Hyprland's `ConfigManager.cpp`; the
//! differential harness compares them with `hyprctl getoption` on a real
//! Hyprland.

use core::fmt;

use crate::value::{self, Color, Gaps, Gradient};

/// An option's value.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionValue {
    /// An integer, which is also how Hyprland stores booleans and colours.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string, kept as written.
    Str(String),
    /// A border gradient.
    Gradient(Gradient),
    /// Gaps on four sides.
    Gaps(Gaps),
}

impl fmt::Display for OptionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value:.6}"),
            Self::Str(value) => f.write_str(value),
            Self::Gradient(value) => write!(f, "{value}"),
            Self::Gaps(value) => write!(f, "{value}"),
        }
    }
}

/// An option's default, in a form a `static` table can hold.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Initial {
    Int(i64),
    Float(f64),
    Str(&'static str),
    Gradient(u32),
    Gaps(i64),
}

impl Initial {
    /// The value this default stands for.
    pub(crate) fn value(self) -> OptionValue {
        match self {
            Self::Int(value) => OptionValue::Int(value),
            Self::Float(value) => OptionValue::Float(value),
            Self::Str(value) => OptionValue::Str(value.to_owned()),
            Self::Gradient(color) => OptionValue::Gradient(Gradient::solid(Color(color))),
            Self::Gaps(gap) => OptionValue::Gaps(Gaps::all(gap)),
        }
    }

    /// Parse `text` as a value of this default's type.
    pub(crate) fn parse(self, text: &str) -> Result<OptionValue, String> {
        match self {
            Self::Int(_) => value::parse_int(text).map(OptionValue::Int),
            Self::Float(_) => value::parse_float(text).map(OptionValue::Float),
            Self::Str(_) => Ok(OptionValue::Str(text.trim().to_owned())),
            Self::Gradient(_) => value::parse_gradient(text).map(OptionValue::Gradient),
            Self::Gaps(_) => value::parse_gaps(text).map(OptionValue::Gaps),
        }
    }
}

use Initial::{Float, Gaps as GapsOf, Gradient as GradientOf, Int, Str};

/// Every option the compositor implements, sorted by name.
pub(crate) static OPTIONS: &[(&str, Initial)] = &[
    ("animations:enabled", Int(1)),
    ("binds:allow_workspace_cycles", Int(0)),
    ("binds:workspace_back_and_forth", Int(0)),
    ("decoration:active_opacity", Float(1.0)),
    ("decoration:blur:enabled", Int(1)),
    ("decoration:blur:passes", Int(1)),
    ("decoration:blur:size", Int(8)),
    ("decoration:dim_inactive", Int(0)),
    ("decoration:dim_strength", Float(0.5)),
    ("decoration:fullscreen_opacity", Float(1.0)),
    ("decoration:inactive_opacity", Float(1.0)),
    ("decoration:rounding", Int(0)),
    ("decoration:shadow:color", GradientOf(0xee1a_1a1a)),
    ("decoration:shadow:enabled", Int(1)),
    // A vector, which this configuration has no type for; it is read as the
    // two numbers `compositor/render` parses out of it, as Hyprland reads
    // `0 0` and `0, 0` alike.
    ("decoration:shadow:offset", Str("0 0")),
    ("decoration:shadow:range", Int(4)),
    ("decoration:shadow:render_power", Int(3)),
    ("dwindle:default_split_ratio", Float(1.0)),
    ("dwindle:force_split", Int(0)),
    ("dwindle:permanent_direction_override", Int(0)),
    ("dwindle:preserve_split", Int(0)),
    ("dwindle:pseudotile", Int(0)),
    ("dwindle:smart_resizing", Int(1)),
    ("dwindle:smart_split", Int(0)),
    ("dwindle:special_scale_factor", Float(1.0)),
    ("dwindle:split_bias", Int(0)),
    ("dwindle:split_width_multiplier", Float(1.0)),
    ("dwindle:use_active_for_splits", Int(1)),
    ("general:border_size", Int(1)),
    ("general:col.active_border", GradientOf(0xffff_ffff)),
    ("general:col.inactive_border", GradientOf(0xff44_4444)),
    ("general:extend_border_grab_area", Int(15)),
    ("general:gaps_in", GapsOf(5)),
    ("general:gaps_out", GapsOf(20)),
    ("general:gaps_workspaces", Int(0)),
    ("general:layout", Str("dwindle")),
    ("general:no_focus_fallback", Int(0)),
    ("general:resize_on_border", Int(0)),
    ("input:accel_profile", Str("")),
    ("input:follow_mouse", Int(1)),
    ("input:kb_file", Str("")),
    ("input:kb_layout", Str("us")),
    ("input:kb_model", Str("")),
    ("input:kb_options", Str("")),
    ("input:kb_rules", Str("")),
    ("input:kb_variant", Str("")),
    ("input:left_handed", Int(0)),
    ("input:natural_scroll", Int(0)),
    ("input:numlock_by_default", Int(0)),
    ("input:repeat_delay", Int(600)),
    ("input:repeat_rate", Int(25)),
    ("input:sensitivity", Float(0.0)),
    ("master:allow_small_split", Int(0)),
    ("master:drop_at_cursor", Int(1)),
    ("master:inherit_fullscreen", Int(1)),
    ("master:mfact", Float(0.55)),
    ("master:new_on_active", Str("none")),
    ("master:new_on_top", Int(0)),
    ("master:new_status", Str("slave")),
    ("master:orientation", Str("left")),
    ("master:smart_resizing", Int(1)),
    ("master:special_scale_factor", Float(1.0)),
    ("misc:disable_hyprland_logo", Int(0)),
    ("misc:disable_splash_rendering", Int(0)),
    // Off, as in Hyprland: a program that asks for another's window to be
    // raised makes it urgent rather than taking the focus away.
    ("misc:focus_on_activate", Int(0)),
    ("misc:force_default_wallpaper", Int(-1)),
];

/// What the table says an option starts as, if the compositor has it.
///
/// `hyprctl getoption` prints a `set` flag, which is whether the
/// configuration said anything about the option rather than leaving it at
/// this.
#[must_use]
pub fn default_of(name: &str) -> Option<OptionValue> {
    find(name).map(Initial::value)
}

/// The table entry for `name`, if the compositor has that option.
pub(crate) fn find(name: &str) -> Option<Initial> {
    OPTIONS
        .binary_search_by(|(entry, _)| (*entry).cmp(name))
        .ok()
        .and_then(|index| OPTIONS.get(index))
        .map(|&(_, default)| default)
}
