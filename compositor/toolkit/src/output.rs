//! Screens, as `wl_output` describes them.

/// A screen, by the runtime's own number for it.
///
/// Not the `wl_output`'s object id and not the registry name: a screen that
/// is unplugged and plugged back in is a new global with a new name, and a
/// program that kept an id across that would be pointing at nothing.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct OutputId(pub u32);

/// How a screen is turned, from `wl_output.geometry`'s `transform`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Transform {
    /// Upright.
    #[default]
    Normal,
    /// A quarter turn anticlockwise.
    Rotated90,
    /// Upside down.
    Rotated180,
    /// Three quarters.
    Rotated270,
    /// Mirrored, then each of the above.
    Flipped,
    /// Mirrored and a quarter turn.
    Flipped90,
    /// Mirrored and a half turn.
    Flipped180,
    /// Mirrored and three quarters.
    Flipped270,
}

impl Transform {
    /// From the protocol's number; an unknown one is upright.
    #[must_use]
    pub const fn from_wire(value: i32) -> Self {
        match value {
            1 => Self::Rotated90,
            2 => Self::Rotated180,
            3 => Self::Rotated270,
            4 => Self::Flipped,
            5 => Self::Flipped90,
            6 => Self::Flipped180,
            7 => Self::Flipped270,
            _ => Self::Normal,
        }
    }

    /// Whether width and height trade places.
    #[must_use]
    pub const fn is_sideways(self) -> bool {
        matches!(
            self,
            Self::Rotated90 | Self::Rotated270 | Self::Flipped90 | Self::Flipped270
        )
    }
}

/// What the compositor has said about one screen.
///
/// Every field is the last value sent; [`Output::done`] says whether a
/// `wl_output.done` has closed the first batch, before which a program
/// should not decide anything about the screen. waybar and hyprlock match a
/// screen by [`Output::description`] (`"desc:…"` in hyprlock, the whole
/// string in waybar), so that is kept exactly as it arrived.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    /// The runtime's number for it.
    pub id: Option<OutputId>,
    /// `wl_output.name` (version 4): the connector, `DP-1`, `HDMI-A-1`.
    pub name: String,
    /// `wl_output.description` (version 4): make, model and serial as the
    /// compositor words them, `"Dell Inc. DELL U2415 XKV0P9BE2GLU"`.
    pub description: String,
    /// `wl_output.geometry`'s make.
    pub make: String,
    /// `wl_output.geometry`'s model.
    pub model: String,
    /// Where the screen sits in the compositor's space, in its pixels.
    pub position: (i32, i32),
    /// The physical size in millimetres, which is what a DPI is worked out
    /// from; `(0, 0)` where the compositor does not know.
    pub physical_mm: (i32, i32),
    /// The current mode in the screen's own pixels.
    pub mode: (i32, i32),
    /// The current mode's refresh in millihertz.
    pub refresh_mhz: i32,
    /// `wl_output.scale`: the integer scale a buffer should be drawn at.
    pub scale: i32,
    /// `wl_output.geometry`'s transform.
    pub transform: Transform,
    /// `zxdg_output_v1.name`, where the compositor offers
    /// `zxdg_output_manager_v1`; empty otherwise.
    pub xdg_name: String,
    /// `zxdg_output_v1.description`. On Hyprland this is the
    /// `wl_output.description` with `" (NAME)"` after it, which is why
    /// waybar cuts that off before it compares.
    pub xdg_description: String,
    /// `zxdg_output_v1.logical_position`: where it is in the compositor's
    /// logical space.
    pub logical_position: Option<(i32, i32)>,
    /// `zxdg_output_v1.logical_size`: its size in logical pixels, which a
    /// bar lays itself out in.
    pub logical_extent: Option<(i32, i32)>,
    /// Whether the first `wl_output.done` has arrived.
    pub done: bool,
}

impl Output {
    /// The size a surface covering the whole screen has, in logical pixels:
    /// `zxdg_output_v1.logical_size` where it was sent, else the mode,
    /// turned, divided by the scale.
    #[must_use]
    pub fn logical_size(&self) -> (i32, i32) {
        if let Some(extent) = self.logical_extent {
            return extent;
        }
        let (width, height) = if self.transform.is_sideways() {
            (self.mode.1, self.mode.0)
        } else {
            self.mode
        };
        let scale = self.scale.max(1);
        (width / scale, height / scale)
    }

    /// Dots per inch across, from the mode and the physical width, or
    /// `None` where the compositor gave no physical size.
    #[must_use]
    pub fn dpi(&self) -> Option<f64> {
        let (width, _) = if self.transform.is_sideways() {
            (self.mode.1, self.mode.0)
        } else {
            self.mode
        };
        let (mm, _) = if self.transform.is_sideways() {
            (self.physical_mm.1, self.physical_mm.0)
        } else {
            self.physical_mm
        };
        (mm > 0 && width > 0).then(|| f64::from(width) * 25.4 / f64::from(mm))
    }

    /// hyprlock's rule for `monitor =` (`Renderer::getOrCreateWidgetsFor`):
    /// empty matches every screen; otherwise the connector name exactly, or
    /// a prefix of the description, written bare or after `desc:`. Not
    /// trimmed, as upstream does not trim.
    #[must_use]
    pub fn matches_hyprland(&self, monitor: &str) -> bool {
        monitor.is_empty()
            || monitor == self.name
            || self.description.starts_with(monitor)
            || format!("desc:{}", self.description).starts_with(monitor)
    }
}
