//! What the compositor tells the IPC about itself.
//!
//! A snapshot rather than a borrow of the compositor's own state: an answer
//! is written whole, and a request that arrived halfway through a frame
//! should describe the frame before it rather than half of the next.

/// One window, with the fields `hyprctl clients` prints.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    /// Hyprland prints an address, which programs use as an opaque handle.
    /// This is the compositor's own window id, printed the same way.
    pub address: u64,
    /// Whether it is showing a buffer.
    pub mapped: bool,
    /// Whether it is on the workspace its monitor shows.
    pub visible: bool,
    /// Its client area's top-left, in the space all monitors share.
    pub at: (i32, i32),
    /// Its client area's size.
    pub size: (i32, i32),
    /// The workspace it is on.
    pub workspace: i32,
    /// That workspace's name.
    pub workspace_name: String,
    /// Whether it floats.
    pub floating: bool,
    /// Whether it is fullscreen.
    pub fullscreen: bool,
    /// Which monitor it is on.
    pub monitor: i32,
    /// `xdg_toplevel.set_app_id`, which Hyprland calls the class and which
    /// `windowrule` matches on.
    pub class: String,
    /// `xdg_toplevel.set_title`.
    pub title: String,
    /// The process, when the compositor started it; zero otherwise.
    pub pid: i32,
    /// How recently it was focused: 0 is the focused window.
    pub focus_history: i32,
}

/// One workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Workspace {
    /// Its number.
    pub id: i32,
    /// Its name, which for a numbered workspace is its number.
    pub name: String,
    /// The monitor showing it.
    pub monitor: String,
    /// How many windows are on it.
    pub windows: u32,
    /// Whether any of them is fullscreen.
    pub has_fullscreen: bool,
}

/// One monitor.
///
/// Not `Eq`: the refresh rate and the scale are what `hyprctl monitors`
/// prints, and Hyprland prints them as floats.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Monitor {
    /// Its number.
    pub id: i32,
    /// Its name, as `wl_output.name` carries it.
    pub name: String,
    /// In pixels.
    pub width: i32,
    /// In pixels.
    pub height: i32,
    /// In hertz, as `hyprctl monitors` prints it.
    pub refresh: f64,
    /// Where it is in the space all monitors share.
    pub at: (i32, i32),
    /// The workspace it is showing.
    pub active_workspace: i32,
    /// That workspace's name.
    pub active_workspace_name: String,
    /// The special workspace shown over it, if one is: its id and its name.
    /// Hyprland prints `special workspace: 0 ()` for a monitor showing none,
    /// which is what `None` becomes.
    pub special_workspace: Option<(i32, String)>,
    /// Buffer pixels per logical pixel.
    pub scale: f64,
    /// Whether it holds the focus.
    pub focused: bool,
}

/// Everything an answer is written from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// The monitors.
    pub monitors: Vec<Monitor>,
    /// The workspaces that exist.
    pub workspaces: Vec<Workspace>,
    /// The windows, in the order the compositor holds them.
    pub windows: Vec<Window>,
    /// The focused window's address, if one is focused.
    pub active_window: Option<u64>,
    /// The workspace the focused monitor shows.
    pub active_workspace: i32,
}

impl Snapshot {
    /// The focused window, if there is one.
    #[must_use]
    pub fn active(&self) -> Option<&Window> {
        let address = self.active_window?;
        self.windows.iter().find(|window| window.address == address)
    }
}
