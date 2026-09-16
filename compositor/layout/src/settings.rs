//! The options the layouts read, taken out of the configuration once so the
//! arithmetic does not look them up by name for every window.
//!
//! Each field falls back to Hyprland's default when the configuration does
//! not hold the option with the expected type, and each is clamped the way
//! Hyprland clamps it where it reads it.

use compositor_config::{Config, Gaps};

/// Which tiling layout workspaces use: `general:layout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// `dwindle`, the default, and what an unknown name falls back to.
    #[default]
    Dwindle,
    /// `master`.
    Master,
}

/// Which side of a split a new dwindle window takes: `dwindle:force_split`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ForceSplit {
    /// `0`: Hyprland follows the cursor, putting the new window on the half
    /// of the split the cursor is over. The layouts have no cursor, so this
    /// behaves as [`ForceSplit::Second`].
    #[default]
    Auto,
    /// `1`: always left or top.
    First,
    /// `2`: always right or bottom.
    Second,
}

/// Where the master layout puts its master: `master:orientation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    /// `left`, the default, and what `center` and unknown names fall back
    /// to.
    #[default]
    Left,
    /// `right`.
    Right,
    /// `top`.
    Top,
    /// `bottom`.
    Bottom,
}

/// What a new window becomes in the master layout: `master:new_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewStatus {
    /// `slave`, the default: it joins the stack.
    #[default]
    Slave,
    /// `master`: it becomes the master and the old master joins the stack.
    Master,
    /// `inherit`: master if the focused window is the master.
    Inherit,
}

/// The dwindle layout's options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DwindleSettings {
    /// `dwindle:preserve_split`: keep each split's direction when its box
    /// changes shape, rather than choosing it again from the box.
    pub preserve_split: bool,
    /// `dwindle:force_split`.
    pub force_split: ForceSplit,
    /// `dwindle:split_width_multiplier`: a box splits side by side when its
    /// width exceeds its height times this.
    pub split_width_multiplier: f64,
    /// `dwindle:default_split_ratio`, clamped to 0.1 to 1.9 as Hyprland
    /// does: the first child of a split gets this times half the box.
    pub default_split_ratio: f64,
}

/// The master layout's options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MasterSettings {
    /// `master:mfact`, clamped to 0.05 to 0.95 as Hyprland does: the part of
    /// the workspace the master takes when there is a stack.
    pub mfact: f64,
    /// `master:new_status`.
    pub new_status: NewStatus,
    /// `master:new_on_top`: new windows go to the top of the stack.
    pub new_on_top: bool,
    /// `master:orientation`.
    pub orientation: Orientation,
}

/// Everything the layouts read from the configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// `general:layout`.
    pub layout: Layout,
    /// `general:gaps_in`: the gap on each window edge that faces another
    /// window, so two neighbours are twice this apart.
    pub gaps_in: Gaps,
    /// `general:gaps_out`: the gap on each window edge that faces the
    /// monitor's edge.
    pub gaps_out: Gaps,
    /// `general:border_size`, not negative: reserved inside the gap on every
    /// edge of a window that is not fullscreen.
    pub border_size: i64,
    /// `general:no_focus_fallback`: when `movefocus` finds no window and no
    /// monitor in its direction, do nothing rather than wrap around to the
    /// far edge of the monitor.
    pub no_focus_fallback: bool,
    /// The dwindle layout's options.
    pub dwindle: DwindleSettings,
    /// The master layout's options.
    pub master: MasterSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

/// `value` if it is a finite number, else `default`.
fn finite(value: Option<f64>, default: f64) -> f64 {
    value.filter(|value| value.is_finite()).unwrap_or(default)
}

impl Settings {
    /// Read the options from `config`.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let layout = match config.str("general:layout") {
            Some(name) if name.eq_ignore_ascii_case("master") => Layout::Master,
            _ => Layout::Dwindle,
        };
        let force_split = match config.int("dwindle:force_split") {
            Some(1) => ForceSplit::First,
            Some(2) => ForceSplit::Second,
            _ => ForceSplit::Auto,
        };
        let orientation = match config.str("master:orientation") {
            Some("right") => Orientation::Right,
            Some("top") => Orientation::Top,
            Some("bottom") => Orientation::Bottom,
            _ => Orientation::Left,
        };
        let new_status = match config.str("master:new_status") {
            Some("master") => NewStatus::Master,
            Some("inherit") => NewStatus::Inherit,
            _ => NewStatus::Slave,
        };
        Self {
            layout,
            gaps_in: config.gaps("general:gaps_in").unwrap_or(Gaps::all(5)),
            gaps_out: config.gaps("general:gaps_out").unwrap_or(Gaps::all(20)),
            border_size: config.int("general:border_size").unwrap_or(1).max(0),
            no_focus_fallback: config.bool("general:no_focus_fallback").unwrap_or(false),
            dwindle: DwindleSettings {
                preserve_split: config.bool("dwindle:preserve_split").unwrap_or(false),
                force_split,
                split_width_multiplier: finite(config.float("dwindle:split_width_multiplier"), 1.0),
                default_split_ratio: finite(config.float("dwindle:default_split_ratio"), 1.0)
                    .clamp(0.1, 1.9),
            },
            master: MasterSettings {
                mfact: finite(config.float("master:mfact"), 0.55).clamp(0.05, 0.95),
                new_status,
                new_on_top: config.bool("master:new_on_top").unwrap_or(false),
                orientation,
            },
        }
    }
}
