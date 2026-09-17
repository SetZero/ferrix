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
    /// `left`, the default, and what an unknown name falls back to.
    #[default]
    Left,
    /// `right`.
    Right,
    /// `top`.
    Top,
    /// `bottom`.
    Bottom,
    /// `center`: the master in the middle of the screen with the stack in
    /// two columns beside it, once there are
    /// [`MasterSettings::slave_count_for_center`] of them. Below that many
    /// it is [`MasterSettings::center_fallback`] instead, which is what
    /// keeps one window from being a narrow strip in the middle of an empty
    /// screen.
    Center,
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
    /// `dwindle:split_bias = 1`, which Hyprland calls `current`: the split
    /// favours the window that was already there rather than whichever of
    /// the two ends up first.
    pub split_bias_current: bool,
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
    /// `master:slave_count_for_center_master`: how many windows there have
    /// to be in the stack before `center` centres the master.
    pub slave_count_for_center: usize,
    /// `master:center_master_fallback`: which orientation `center` is until
    /// there are that many. `right` also decides which column takes the odd
    /// window when the stack is an odd number.
    pub center_fallback: Orientation,
    /// `master:always_keep_position`: one window alone keeps the master's
    /// share of the screen rather than filling it.
    pub always_keep_position: bool,
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
    /// `binds:workspace_back_and_forth`: asking for the workspace that is
    /// already shown goes to the one before it instead, which is what makes
    /// one key both there and back.
    pub workspace_back_and_forth: bool,
    /// `binds:hide_special_on_workspace_change`: the scratchpad goes away
    /// when the workspace under it changes.
    pub hide_special_on_workspace_change: bool,
    /// `misc:close_special_on_empty`: a special workspace whose last window
    /// has gone stops being shown.
    pub close_special_on_empty: bool,
    /// `dwindle:special_scale_factor` and `master:special_scale_factor`: a
    /// window on a special workspace is drawn this much of the size the
    /// layout gave it, centred in that slot, so the scratchpad looks like
    /// something over the screen rather than another workspace.
    ///
    /// One number rather than two, because only the layout in force is ever
    /// asked and a person who sets one sets the other.
    pub special_scale_factor: f64,
    /// `binds:allow_pin_fullscreen`: a fullscreen window can be pinned.
    /// Without it `pin` takes only on a floating window, which is what
    /// Hyprland's own handler does with one.
    pub allow_pin_fullscreen: bool,
    /// `binds:movefocus_cycles_fullscreen`: `movefocus` on a fullscreen
    /// window walks to the next window on the workspace rather than looking
    /// for one in that direction -- there is nothing beside a window that
    /// covers the screen, so without this the key does nothing.
    pub movefocus_cycles_fullscreen: bool,
    /// `binds:window_direction_monitor_fallback`: `movefocus` with no window
    /// in that direction moves to the monitor there.
    pub window_direction_monitor_fallback: bool,
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
    /// The orientation `name` names, `left` for anything else.
    ///
    /// Hyprland's `defaultOrientation`: the four sides and `center`, and
    /// every other word is `left`.
    #[must_use]
    pub fn orientation_of(name: &str) -> Orientation {
        match name.trim() {
            "right" => Orientation::Right,
            "top" => Orientation::Top,
            "bottom" => Orientation::Bottom,
            "center" => Orientation::Center,
            _ => Orientation::Left,
        }
    }

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
        let named = |name: Option<&str>| name.map_or(Orientation::Left, Self::orientation_of);
        let orientation = named(config.str("master:orientation"));
        // `center` is not a fallback for itself: Hyprland's own list of
        // fallbacks is the four sides, and anything else is `left`.
        let center_fallback = match config.str("master:center_master_fallback") {
            Some("center") | None => Orientation::Left,
            other => named(other),
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
            workspace_back_and_forth: config
                .bool("binds:workspace_back_and_forth")
                .unwrap_or(false),
            hide_special_on_workspace_change: config
                .bool("binds:hide_special_on_workspace_change")
                .unwrap_or(false),
            close_special_on_empty: config.bool("misc:close_special_on_empty").unwrap_or(true),
            allow_pin_fullscreen: config.bool("binds:allow_pin_fullscreen").unwrap_or(false),
            movefocus_cycles_fullscreen: config
                .bool("binds:movefocus_cycles_fullscreen")
                .unwrap_or(false),
            window_direction_monitor_fallback: config
                .bool("binds:window_direction_monitor_fallback")
                .unwrap_or(true),
            special_scale_factor: finite(
                config.float(match layout {
                    Layout::Master => "master:special_scale_factor",
                    Layout::Dwindle => "dwindle:special_scale_factor",
                }),
                1.0,
            )
            .clamp(0.0, 1.0),
            dwindle: DwindleSettings {
                preserve_split: config.bool("dwindle:preserve_split").unwrap_or(false),
                force_split,
                split_width_multiplier: finite(config.float("dwindle:split_width_multiplier"), 1.0),
                split_bias_current: config.int("dwindle:split_bias").unwrap_or(0) == 1,
                default_split_ratio: finite(config.float("dwindle:default_split_ratio"), 1.0)
                    .clamp(0.1, 1.9),
            },
            master: MasterSettings {
                mfact: finite(config.float("master:mfact"), 0.55).clamp(0.05, 0.95),
                new_status,
                new_on_top: config.bool("master:new_on_top").unwrap_or(false),
                orientation,
                slave_count_for_center: usize::try_from(
                    config
                        .int("master:slave_count_for_center_master")
                        .unwrap_or(2),
                )
                .unwrap_or(2),
                center_fallback,
                always_keep_position: config.bool("master:always_keep_position").unwrap_or(false),
            },
        }
    }
}
