//! Monitors, workspaces and windows, and what the dispatchers do to them.
//!
//! Workspaces are created on demand, as Hyprland's are: switching to a
//! number that does not exist makes it on the focused monitor, and a
//! workspace that is neither shown nor holding a window is removed. Each
//! workspace belongs to one monitor and keeps its tiled windows in the
//! configured layout, its floating windows in stacking order with their own
//! rectangles, and at most one fullscreen window.
//!
//! Focus follows Hyprland's focus history: one list of windows, most
//! recently focused last. The focused window of a workspace is the most
//! recent one on it, and the focused window overall is that of the
//! workspace the focused monitor shows. Closing a window therefore hands
//! focus back to the one focused before it, and `movefocus` breaks ties
//! between several neighbours by recency, as `binds:focus_preferred_method`
//! 0 does.
//!
//! A fullscreen window, in either mode, hides every other window on its
//! workspace, as Hyprland's `setFullscreenFadeAnimation` fades them to
//! nothing. Hyprland leaves visible the floating windows opened or raised
//! over it (`m_createdOverFullscreen`); here they stay hidden, and a new
//! window opens behind the fullscreen one. The direction searches skip the
//! hidden windows, where Hyprland's find them on other monitors.
//!
//! Every public method that changes anything returns the [`Change`]s it
//! caused. The ones only a caller can act on (a close request, a window
//! moved, floated or made fullscreen) are reported as they happen; which
//! monitor has focus, what each monitor shows, whose geometry changed and
//! which window has focus are found by comparing the state before and
//! after, so none can be forgotten.

use std::collections::BTreeMap;
use std::f64::consts::{FRAC_PI_2, PI};

use compositor_config::{Bind, Config, Gaps};

use crate::dispatch::{Direction, Dispatcher, FullscreenMode, WorkspaceTarget};
use crate::dwindle::Dwindle;
use crate::geometry::{self, Area, overlap, sticks};
use crate::master::Master;
use crate::settings::{Layout, Settings};
use crate::{Error, Monitor, MonitorId, Rect, WindowId, WorkspaceId};

/// Something a change to the state did that the caller acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The window should be asked to close. It stays until the caller
    /// reports it gone with [`State::window_gone`].
    Close(WindowId),
    /// The window moved to another workspace.
    MoveToWorkspace {
        /// The window.
        window: WindowId,
        /// The workspace it is on now.
        workspace: WorkspaceId,
    },
    /// The window started or stopped floating.
    Floating {
        /// The window.
        window: WindowId,
        /// Whether it floats now.
        floating: bool,
    },
    /// The window became fullscreen, changed mode, or stopped being
    /// fullscreen.
    Fullscreen {
        /// The window.
        window: WindowId,
        /// Its mode now, `None` if it is not fullscreen.
        mode: Option<FullscreenMode>,
    },
    /// Another monitor has focus.
    FocusMonitor(MonitorId),
    /// A monitor shows another workspace.
    Workspace {
        /// The monitor.
        monitor: MonitorId,
        /// The workspace it shows now.
        workspace: WorkspaceId,
    },
    /// What a monitor shows changed geometry: a window appeared, went, moved
    /// or resized.
    Layout(MonitorId),
    /// Another window has focus, or none.
    Focus(Option<WindowId>),
}

/// A visible window's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    /// The window.
    pub window: WindowId,
    /// Its client area: what the client is configured to, with the border
    /// drawn outside it.
    pub rect: Rect,
    /// The border's width on every side, zero when fullscreen.
    pub border: i64,
    /// Whether it is the focused window, which draws the active border.
    pub focused: bool,
    /// Whether it floats.
    pub floating: bool,
    /// Whether it is fullscreen or maximized.
    pub fullscreen: bool,
}

/// What one monitor shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorLayout {
    /// The monitor.
    pub monitor: MonitorId,
    /// The workspace it shows.
    pub workspace: WorkspaceId,
    /// The visible windows, bottom to top: tiled windows, then floating ones
    /// in stacking order; only the fullscreen window when there is one.
    pub windows: Vec<Placed>,
}

/// A workspace's tiled windows, in the layout it was made with.
#[derive(Debug, Clone, PartialEq)]
enum Tiling {
    Dwindle(Dwindle),
    Master(Master),
}

impl Tiling {
    fn new(layout: Layout) -> Self {
        match layout {
            Layout::Dwindle => Self::Dwindle(Dwindle::default()),
            Layout::Master => Self::Master(Master::default()),
        }
    }

    /// Add `new`, beside or after `focused`, the workspace's most recently
    /// focused tiled window.
    fn insert(
        &mut self,
        new: WindowId,
        focused: Option<WindowId>,
        area: Area,
        settings: &Settings,
    ) {
        match self {
            Self::Dwindle(dwindle) => dwindle.insert(new, focused, area, settings),
            Self::Master(master) => master.insert(new, focused, settings),
        }
    }

    /// Add `new` at a point, as the dwindle layout puts back a window
    /// `movewindow` took out; the master layout adds it as it would a new
    /// one.
    fn insert_at(
        &mut self,
        new: WindowId,
        point: (f64, f64),
        toward: Option<Direction>,
        area: Area,
        settings: &Settings,
    ) {
        match self {
            Self::Dwindle(dwindle) => dwindle.insert_at(new, point, toward, area, settings),
            Self::Master(master) => master.insert(new, None, settings),
        }
    }

    fn remove(&mut self, window: WindowId) {
        match self {
            Self::Dwindle(dwindle) => dwindle.remove(window),
            Self::Master(master) => master.remove(window),
        }
    }

    /// Exchange two windows' places in the master layout; the dwindle
    /// layout never exchanges windows.
    fn swap(&mut self, a: WindowId, b: WindowId) {
        if let Self::Master(master) = self {
            master.swap(a, b);
        }
    }

    fn contains(&self, window: WindowId) -> bool {
        match self {
            Self::Dwindle(dwindle) => dwindle.contains(window),
            Self::Master(master) => master.contains(window),
        }
    }

    fn windows(&self) -> Vec<WindowId> {
        match self {
            Self::Dwindle(dwindle) => dwindle.windows(),
            Self::Master(master) => master.windows(),
        }
    }

    fn slots(&self, area: Area, settings: &Settings) -> Vec<(WindowId, Area)> {
        match self {
            Self::Dwindle(dwindle) => dwindle.slots(area, settings),
            Self::Master(master) => master.slots(area, settings),
        }
    }

    fn settle(&mut self, area: Area, settings: &Settings) {
        if let Self::Dwindle(dwindle) = self {
            dwindle.settle(area, settings);
        }
    }
}

/// A workspace.
#[derive(Debug, Clone, PartialEq)]
struct Workspace {
    /// The monitor it belongs to, which may have been unplugged.
    monitor: MonitorId,
    tiling: Tiling,
    /// Floating windows, bottom to top.
    floating: Vec<WindowId>,
    fullscreen: Option<(WindowId, FullscreenMode)>,
}

impl Workspace {
    fn is_empty(&self) -> bool {
        self.floating.is_empty() && self.tiling.windows().is_empty()
    }
}

/// A monitor, the workspace it shows, and the special workspace over it.
///
/// A special workspace is not shown *instead of* the normal one: it is drawn
/// over it, which is what makes Hyprland's scratchpad a scratchpad. A monitor
/// shows at most one at a time.
#[derive(Debug, Clone, PartialEq)]
struct Output {
    monitor: Monitor,
    active: WorkspaceId,
    special: Option<WorkspaceId>,
}

/// The part of the state [`Change`]s are derived from.
struct Snapshot {
    focus: Option<WindowId>,
    monitor: Option<MonitorId>,
    layout: Vec<MonitorLayout>,
}

/// A window a direction search can find.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    window: WindowId,
    workspace: WorkspaceId,
    /// Where the layout put it: its slot if tiled, its rectangle if
    /// floating, the monitor if fullscreen. Hyprland's `m_position` and
    /// `m_size`.
    placed: Rect,
    /// `placed` as the searches compare it; see [`ideal_box`].
    ideal: Rect,
    floating: bool,
    fullscreen: bool,
}

/// Monitors, workspaces, windows and focus.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    settings: Settings,
    /// In the order they were added.
    outputs: Vec<Output>,
    focused_monitor: Option<MonitorId>,
    workspaces: BTreeMap<WorkspaceId, Workspace>,
    /// Which workspace each window is on.
    windows: BTreeMap<WindowId, WorkspaceId>,
    /// Each window's floating rectangle, relative to its monitor's origin
    /// so it moves with the window between monitors, kept while the window
    /// is tiled so floating it again puts it back.
    floating_rects: BTreeMap<WindowId, Rect>,
    /// Every window, most recently focused last.
    history: Vec<WindowId>,
    /// The name of each workspace that has one, which is the special ones:
    /// a numbered workspace's name is its number.
    names: BTreeMap<WorkspaceId, String>,
}

/// The lowest id a special workspace has, `SPECIAL_WORKSPACE_START` in
/// Hyprland's `macros.hpp`. Every special workspace's id is between it and
/// −2.
const SPECIAL_START: i64 = -99;

impl State {
    /// No monitors and no windows.
    #[must_use]
    pub const fn new(settings: Settings) -> Self {
        Self {
            settings,
            outputs: Vec::new(),
            focused_monitor: None,
            workspaces: BTreeMap::new(),
            windows: BTreeMap::new(),
            floating_rects: BTreeMap::new(),
            history: Vec::new(),
            names: BTreeMap::new(),
        }
    }

    /// No monitors and no windows, with the options `config` sets.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self::new(Settings::from_config(config))
    }

    // -- Queries --------------------------------------------------------------

    /// The options in force.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The monitors, in the order they were added.
    pub fn monitors(&self) -> impl Iterator<Item = &Monitor> {
        self.outputs.iter().map(|output| &output.monitor)
    }

    /// The focused monitor.
    #[must_use]
    pub const fn focused_monitor(&self) -> Option<MonitorId> {
        self.focused_monitor
    }

    /// The workspace `monitor` shows.
    #[must_use]
    pub fn active_workspace(&self, monitor: MonitorId) -> Option<WorkspaceId> {
        self.output(monitor).map(|output| output.active)
    }

    /// The workspace the focused monitor shows.
    #[must_use]
    pub fn current_workspace(&self) -> Option<WorkspaceId> {
        self.focused_monitor
            .and_then(|monitor| self.active_workspace(monitor))
    }

    /// The focused window.
    #[must_use]
    pub fn focused_window(&self) -> Option<WindowId> {
        // A monitor showing a scratchpad has two workspaces on it, and the
        // focus may be on either: the most recently focused window of the
        // two is the focused one.
        let monitor = self.focused_monitor?;
        let special = self.special_on(monitor).and_then(|id| self.focused_on(id));
        let own = self
            .active_workspace(monitor)
            .and_then(|id| self.focused_on(id));
        match (special, own) {
            (Some(one), Some(other)) => {
                let at = |window: WindowId| self.history.iter().rposition(|id| *id == window);
                if at(one) >= at(other) {
                    Some(one)
                } else {
                    Some(other)
                }
            }
            (found, None) | (None, found) => found,
        }
    }

    /// The workspaces that exist, in number order.
    pub fn workspaces(&self) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.workspaces.keys().copied()
    }

    /// The monitor a workspace belongs to.
    #[must_use]
    pub fn workspace_monitor(&self, workspace: WorkspaceId) -> Option<MonitorId> {
        self.workspaces.get(&workspace).map(|ws| ws.monitor)
    }

    /// The windows on a workspace: tiled ones in layout order, then floating
    /// ones bottom to top.
    #[must_use]
    pub fn windows(&self, workspace: WorkspaceId) -> Vec<WindowId> {
        self.workspaces
            .get(&workspace)
            .map(|ws| {
                let mut windows = ws.tiling.windows();
                windows.extend(ws.floating.iter().copied());
                windows
            })
            .unwrap_or_default()
    }

    /// The workspace a window is on.
    #[must_use]
    pub fn workspace_of(&self, window: WindowId) -> Option<WorkspaceId> {
        self.windows.get(&window).copied()
    }

    /// Whether a window floats.
    #[must_use]
    pub fn is_floating(&self, window: WindowId) -> bool {
        self.workspace_of(window)
            .and_then(|workspace| self.workspaces.get(&workspace))
            .is_some_and(|ws| ws.floating.contains(&window))
    }

    /// A workspace's fullscreen window and its mode.
    #[must_use]
    pub fn fullscreen(&self, workspace: WorkspaceId) -> Option<(WindowId, FullscreenMode)> {
        self.workspaces.get(&workspace).and_then(|ws| ws.fullscreen)
    }

    /// For the workspace each monitor shows, in monitor order, every visible
    /// window's place.
    #[must_use]
    pub fn layout(&self) -> Vec<MonitorLayout> {
        let focus = self.focused_window();
        self.outputs
            .iter()
            .map(|output| MonitorLayout {
                monitor: output.monitor.id,
                workspace: output.active,
                windows: self.with_special(output, focus),
            })
            .collect()
    }

    /// A monitor's windows: its workspace's, with its special workspace's
    /// over them.
    fn with_special(&self, output: &Output, focus: Option<WindowId>) -> Vec<Placed> {
        let mut windows = self.placements(output, focus);
        if let Some(special) = output.special {
            let over = Output {
                monitor: output.monitor,
                active: special,
                special: None,
            };
            windows.extend(self.placements(&over, focus));
        }
        windows
    }

    // -- Monitors -------------------------------------------------------------

    /// Say what a monitor has reserved, and re-tile it.
    ///
    /// The strips a layer surface's exclusive zone takes off the monitor:
    /// a bar across the top means the windows start below it. Nothing else
    /// sets them, and `compositor/layout` works none of them out -- where a
    /// layer surface goes is [`crate::layers`]' and what it reserves is that
    /// module's answer, because it is the protocol's rule and not the
    /// tiling's.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownMonitor`] for a monitor that is not there.
    pub fn set_reserved(
        &mut self,
        monitor: MonitorId,
        reserved: Gaps,
    ) -> Result<Vec<Change>, Error> {
        if self.output(monitor).is_none() {
            return Err(Error::UnknownMonitor(monitor));
        }
        Ok(self.run(|state| {
            if let Some(output) = state
                .outputs
                .iter_mut()
                .find(|output| output.monitor.id == monitor)
            {
                output.monitor.reserved = reserved;
            }
            Vec::new()
        }))
    }

    /// Add a monitor. It shows the lowest-numbered workspace left without a
    /// monitor by an unplug, taking all of those, or else the lowest number
    /// not in use. The first monitor gets focus.
    pub fn add_monitor(&mut self, monitor: Monitor) -> Result<Vec<Change>, Error> {
        if self.output(monitor.id).is_some() {
            return Err(Error::DuplicateMonitor(monitor.id));
        }
        Ok(self.run(|state| {
            let orphans: Vec<WorkspaceId> = state
                .workspaces
                .iter()
                .filter(|(_, ws)| state.output(ws.monitor).is_none())
                .map(|(id, _)| *id)
                .collect();
            for id in &orphans {
                if let Some(ws) = state.workspaces.get_mut(id) {
                    ws.monitor = monitor.id;
                }
            }
            let active = orphans
                .first()
                .copied()
                .unwrap_or_else(|| state.first_free_workspace());
            state.ensure_workspace(active, monitor.id);
            state.outputs.push(Output {
                monitor,
                active,
                special: None,
            });
            if state.focused_monitor.is_none() {
                state.focused_monitor = Some(monitor.id);
            }
            Vec::new()
        }))
    }

    /// Remove a monitor. Its workspaces go to the first monitor left, or
    /// wait for the next one added if none is.
    pub fn remove_monitor(&mut self, id: MonitorId) -> Result<Vec<Change>, Error> {
        if self.output(id).is_none() {
            return Err(Error::UnknownMonitor(id));
        }
        Ok(self.run(|state| {
            state.outputs.retain(|output| output.monitor.id != id);
            let fallback = state.outputs.first().map(|output| output.monitor.id);
            if let Some(fallback) = fallback {
                state
                    .workspaces
                    .values_mut()
                    .filter(|ws| ws.monitor == id)
                    .for_each(|ws| ws.monitor = fallback);
            }
            if state.focused_monitor == Some(id) {
                state.focused_monitor = fallback;
            }
            Vec::new()
        }))
    }

    // -- Windows --------------------------------------------------------------

    /// A new window, tiled on the workspace the focused monitor shows, and
    /// focused unless a fullscreen window is there: Hyprland's default
    /// `misc:new_window_takes_over_fullscreen` of 0 opens it behind.
    pub fn open_window(&mut self, window: WindowId) -> Result<Vec<Change>, Error> {
        self.open(window, None)
    }

    /// A new window, floating at `rect` on the workspace the focused monitor
    /// shows, and focused as [`State::open_window`] would.
    pub fn open_floating(&mut self, window: WindowId, rect: Rect) -> Result<Vec<Change>, Error> {
        self.open(window, Some(rect))
    }

    fn open(&mut self, window: WindowId, floating: Option<Rect>) -> Result<Vec<Change>, Error> {
        if self.windows.contains_key(&window) {
            return Err(Error::DuplicateWindow(window));
        }
        let Some(workspace) = self.current_workspace() else {
            return Err(Error::NoMonitor);
        };
        Ok(self.run(|state| {
            if let Some(rect) = floating {
                let (x, y) = state.origin(workspace);
                let _previous = state.floating_rects.insert(
                    window,
                    rect.translate(x.saturating_neg(), y.saturating_neg()),
                );
            }
            state.attach(window, workspace, floating.is_some());
            match state.fullscreen(workspace) {
                Some((fullscreen, _)) => {
                    let at = state
                        .history
                        .iter()
                        .position(|id| *id == fullscreen)
                        .unwrap_or(state.history.len());
                    state.history.insert(at, window);
                }
                None => state.history.push(window),
            }
            Vec::new()
        }))
    }

    /// A window is gone: its client closed it or died. Focus falls back to
    /// the window focused before it.
    pub fn window_gone(&mut self, window: WindowId) -> Result<Vec<Change>, Error> {
        if !self.windows.contains_key(&window) {
            return Err(Error::UnknownWindow(window));
        }
        Ok(self.run(|state| {
            state.detach(window);
            state.history.retain(|id| *id != window);
            let _rect = state.floating_rects.remove(&window);
            Vec::new()
        }))
    }

    /// Focus a window, as a click or `focuswindow` does: its monitor gets
    /// focus and shows its workspace, and a floating window is raised.
    pub fn focus_window(&mut self, window: WindowId) -> Result<Vec<Change>, Error> {
        if !self.windows.contains_key(&window) {
            return Err(Error::UnknownWindow(window));
        }
        Ok(self.run(|state| {
            state.focus(window);
            Vec::new()
        }))
    }

    /// Change the options. A different `general:layout` lays every
    /// workspace's tiled windows out again in the new layout, in their old
    /// order.
    pub fn set_settings(&mut self, settings: Settings) -> Vec<Change> {
        self.run(|state| {
            let old = state.settings.layout;
            state.settings = settings;
            if old != settings.layout {
                state.retile();
            }
            Vec::new()
        })
    }

    /// Lay every workspace's tiled windows out again in the configured
    /// layout, in their old order.
    fn retile(&mut self) {
        let outputs = &self.outputs;
        for ws in self.workspaces.values_mut() {
            let area = Area::of(work_area(outputs, ws.monitor, &self.settings));
            let windows = ws.tiling.windows();
            ws.tiling = Tiling::new(self.settings.layout);
            let mut previous = None;
            for window in windows {
                ws.tiling.insert(window, previous, area, &self.settings);
                previous = Some(window);
            }
        }
    }

    // -- Dispatchers ----------------------------------------------------------

    /// Run a dispatcher.
    pub fn dispatch(&mut self, dispatcher: &Dispatcher) -> Vec<Change> {
        let dispatcher = dispatcher.clone();
        self.run(|state| match dispatcher {
            Dispatcher::MoveFocus(direction) => state.move_focus(direction),
            Dispatcher::MoveWindow(direction) => state.move_window(direction),
            Dispatcher::Workspace(target) => state.switch_workspace(target),
            Dispatcher::MoveToWorkspace(target) => state.move_to_workspace(target, true),
            Dispatcher::MoveToWorkspaceSilent(target) => state.move_to_workspace(target, false),
            Dispatcher::KillActive => state
                .focused_window()
                .map(Change::Close)
                .into_iter()
                .collect(),
            Dispatcher::ToggleFloating => state.toggle_floating(),
            Dispatcher::Fullscreen(mode) => state.toggle_fullscreen(mode),
            Dispatcher::ToggleSpecialWorkspace(name) => state.toggle_special(&name),
        })
    }

    /// Parse and run a dispatcher, as `hyprctl dispatch` does.
    pub fn dispatch_str(&mut self, name: &str, arg: &str) -> Result<Vec<Change>, Error> {
        let dispatcher = Dispatcher::parse(name, arg)?;
        Ok(self.dispatch(&dispatcher))
    }

    /// Run the dispatcher a binding names.
    pub fn dispatch_bind(&mut self, bind: &Bind) -> Result<Vec<Change>, Error> {
        self.dispatch_str(&bind.dispatcher, &bind.arg)
    }

    /// Hyprland's `moveFocusTo`.
    fn move_focus(&mut self, direction: Direction) -> Vec<Change> {
        let Some(window) = self.focused_window() else {
            if let Some(monitor) = self.monitor_towards(direction) {
                self.focused_monitor = Some(monitor);
            }
            return Vec::new();
        };
        if let Some(target) = self.window_in_direction(window, direction) {
            self.focus(target);
        } else if let Some(monitor) = self.monitor_towards(direction) {
            self.focused_monitor = Some(monitor);
        } else if !self.settings.no_focus_fallback
            && let Some(target) = self.wrapped(window, direction)
        {
            self.focus(target);
        }
        Vec::new()
    }

    /// Hyprland's `moveActiveTo`, for a tiled window.
    fn move_window(&mut self, direction: Direction) -> Vec<Change> {
        let Some(window) = self.focused_window() else {
            return Vec::new();
        };
        let Some(from) = self.workspace_of(window) else {
            return Vec::new();
        };
        if self.is_floating(window) || self.fullscreen(from).is_some() {
            return Vec::new();
        }
        if let Some(other) = self.window_in_direction(window, direction) {
            return self.move_past(window, other, direction);
        }
        let Some(to) = self
            .monitor_towards(direction)
            .and_then(|monitor| self.active_workspace(monitor))
        else {
            return Vec::new();
        };
        self.move_window_to(window, to);
        self.focus(window);
        vec![Change::MoveToWorkspace {
            window,
            workspace: to,
        }]
    }

    /// Move the tiled `window` in `direction`, where the neighbour search
    /// found `other`: the layouts' `moveWindowTo`, `moveTargetInDirection`
    /// from Hyprland 0.54.
    ///
    /// The master layout exchanges the two on one workspace and sends the
    /// window to the other's workspace across two. The dwindle layout takes
    /// the window out and puts it back in at a point one pixel past its
    /// edge, on the active workspace of whichever monitor that point is on.
    fn move_past(
        &mut self,
        window: WindowId,
        other: WindowId,
        direction: Direction,
    ) -> Vec<Change> {
        let (Some(from), Some(beyond)) = (self.workspace_of(window), self.workspace_of(other))
        else {
            return Vec::new();
        };
        let lone_sibling = match self.workspaces.get(&from).map(|ws| &ws.tiling) {
            Some(Tiling::Dwindle(dwindle)) => dwindle.faces_lone_sibling(window, direction),
            _ => {
                if self.fullscreen(beyond).is_some() {
                    return Vec::new();
                }
                if from == beyond {
                    if let Some(ws) = self.workspaces.get_mut(&from) {
                        ws.tiling.swap(window, other);
                    }
                    return Vec::new();
                }
                self.move_window_to(window, beyond);
                self.focus(window);
                return vec![Change::MoveToWorkspace {
                    window,
                    workspace: beyond,
                }];
            }
        };
        let Some(ideal) = self
            .candidates()
            .into_iter()
            .find(|candidate| candidate.window == window)
            .map(|candidate| candidate.ideal)
        else {
            return Vec::new();
        };
        let point = focal_point(ideal, direction);
        let Some(to) = self
            .monitor_at(point)
            .and_then(|monitor| self.active_workspace(monitor))
        else {
            return Vec::new();
        };
        if self.fullscreen(to).is_some() {
            return Vec::new();
        }
        let toward = (from == to && lone_sibling).then_some(direction);
        let area = Area::of(self.work_area_of(to));
        self.detach(window);
        let settings = self.settings;
        if let Some(ws) = self.workspaces.get_mut(&to) {
            ws.tiling.insert_at(window, point, toward, area, &settings);
        }
        let _previous = self.windows.insert(window, to);
        self.focus(window);
        if from == to {
            Vec::new()
        } else {
            vec![Change::MoveToWorkspace {
                window,
                workspace: to,
            }]
        }
    }

    fn switch_workspace(&mut self, target: WorkspaceTarget) -> Vec<Change> {
        if let Some(workspace) = self.resolve(target) {
            self.show(workspace);
        }
        Vec::new()
    }

    fn move_to_workspace(&mut self, target: WorkspaceTarget, follow: bool) -> Vec<Change> {
        let Some(window) = self.focused_window() else {
            return Vec::new();
        };
        let Some(to) = self.resolve(target) else {
            return Vec::new();
        };
        if self.workspace_of(window) == Some(to) {
            return Vec::new();
        }
        if let Some(monitor) = self.focused_monitor {
            self.ensure_workspace(to, monitor);
            // A window sent to a scratchpad goes to a workspace that is
            // shown over this one, not to one this monitor switches to.
            if Self::is_special(to)
                && follow
                && let Some(output) = self
                    .outputs
                    .iter_mut()
                    .find(|output| output.monitor.id == monitor)
            {
                output.special = Some(to);
            }
        }
        self.move_window_to(window, to);
        if follow {
            self.focus(window);
        }
        vec![Change::MoveToWorkspace {
            window,
            workspace: to,
        }]
    }

    fn toggle_floating(&mut self) -> Vec<Change> {
        let Some(window) = self.focused_window() else {
            return Vec::new();
        };
        let Some(workspace) = self.workspace_of(window) else {
            return Vec::new();
        };
        let floating = self.is_floating(window);
        let monitor_rect = self
            .workspace_monitor(workspace)
            .and_then(|monitor| self.output(monitor))
            .map(|output| output.monitor.rect)
            .unwrap_or_default();
        let area = Area::of(self.work_area_of(workspace));
        let beside = self.recent_tiled(workspace);
        if !floating && !self.floating_rects.contains_key(&window) {
            // A window floated for the first time is centred at half the
            // monitor's size.
            let width = monitor_rect.width / 2;
            let height = monitor_rect.height / 2;
            let rect = Rect::new(
                monitor_rect.width.saturating_sub(width) / 2,
                monitor_rect.height.saturating_sub(height) / 2,
                width,
                height,
            );
            let _previous = self.floating_rects.insert(window, rect);
        }
        let Some(ws) = self.workspaces.get_mut(&workspace) else {
            return Vec::new();
        };
        let mut changes = Vec::new();
        if ws.fullscreen.is_some_and(|(id, _)| id == window) {
            ws.fullscreen = None;
            changes.push(Change::Fullscreen { window, mode: None });
        }
        if floating {
            ws.floating.retain(|id| *id != window);
            ws.tiling.insert(window, beside, area, &self.settings);
        } else {
            ws.tiling.remove(window);
            ws.floating.push(window);
        }
        changes.push(Change::Floating {
            window,
            floating: !floating,
        });
        changes
    }

    fn toggle_fullscreen(&mut self, mode: FullscreenMode) -> Vec<Change> {
        let Some(window) = self.focused_window() else {
            return Vec::new();
        };
        let Some(ws) = self
            .workspace_of(window)
            .and_then(|workspace| self.workspaces.get_mut(&workspace))
        else {
            return Vec::new();
        };
        let old = ws.fullscreen;
        ws.fullscreen = if old == Some((window, mode)) {
            None
        } else {
            Some((window, mode))
        };
        let mut changes = Vec::new();
        // One fullscreen window to a workspace: another one stops being.
        if let Some((previous, _)) = old.filter(|(id, _)| *id != window) {
            changes.push(Change::Fullscreen {
                window: previous,
                mode: None,
            });
        }
        changes.push(Change::Fullscreen {
            window,
            mode: ws.fullscreen.map(|(_, mode)| mode),
        });
        changes
    }

    // -- Neighbours -----------------------------------------------------------

    /// The windows the direction searches look at: those on the workspaces
    /// the monitors show, less the ones a fullscreen window hides, in
    /// monitor order, tiled before floating.
    fn candidates(&self) -> Vec<Candidate> {
        let mut out = Vec::new();
        for output in &self.outputs {
            let Some(ws) = self.workspaces.get(&output.active) else {
                continue;
            };
            let monitor = &output.monitor;
            if let Some((window, _)) = ws.fullscreen {
                out.push(Candidate {
                    window,
                    workspace: output.active,
                    placed: monitor.rect,
                    ideal: monitor.rect,
                    floating: ws.floating.contains(&window),
                    fullscreen: true,
                });
                continue;
            }
            let work = geometry::work_area(monitor, &self.settings);
            let tiled = ws
                .tiling
                .slots(Area::of(work), &self.settings)
                .into_iter()
                .map(|(window, slot)| (window, slot.round(), false));
            let floating = ws.floating.iter().filter_map(|&window| {
                self.floating_rects
                    .get(&window)
                    .map(|rect| (window, rect.translate(monitor.rect.x, monitor.rect.y), true))
            });
            out.extend(
                tiled
                    .chain(floating)
                    .map(|(window, placed, floating)| Candidate {
                        window,
                        workspace: output.active,
                        placed,
                        ideal: ideal_box(placed, monitor.rect, work),
                        floating,
                        fullscreen: false,
                    }),
            );
        }
        out
    }

    /// Hyprland's `CCompositor::getWindowInDirection` from a window: the
    /// search from its box, by edges for a tiled window and by angles for a
    /// floating one.
    fn window_in_direction(&self, window: WindowId, direction: Direction) -> Option<WindowId> {
        let candidates = self.candidates();
        let from = candidates
            .iter()
            .find(|candidate| candidate.window == window)?;
        self.search(
            &candidates,
            from.ideal,
            from.workspace,
            direction,
            window,
            from.floating,
        )
    }

    /// What `movefocus` wraps around to when it finds neither a window nor
    /// a monitor: the search again, from a line one pixel outside the
    /// focused monitor's opposite edge. A window that already spans the
    /// monitor along the direction has nothing to wrap to.
    fn wrapped(&self, window: WindowId, direction: Direction) -> Option<WindowId> {
        let candidates = self.candidates();
        let from = candidates
            .iter()
            .find(|candidate| candidate.window == window)?;
        let output = self.output(self.workspace_monitor(from.workspace)?)?;
        let (monitor, placed) = (output.monitor.rect, from.placed);
        let spans = match direction {
            Direction::Left | Direction::Right => {
                sticks(placed.x, monitor.x) && sticks(placed.width, monitor.width)
            }
            Direction::Up | Direction::Down => {
                sticks(placed.y, monitor.y) && sticks(placed.height, monitor.height)
            }
        };
        if spans {
            return None;
        }
        let line = match direction {
            Direction::Left => Rect::new(monitor.right(), monitor.y, 1, monitor.height),
            Direction::Right => {
                Rect::new(monitor.x.saturating_sub(1), monitor.y, 1, monitor.height)
            }
            Direction::Up => Rect::new(monitor.x, monitor.bottom(), monitor.width, 1),
            Direction::Down => Rect::new(monitor.x, monitor.y.saturating_sub(1), monitor.width, 1),
        };
        self.search(
            &candidates,
            line,
            output.active,
            direction,
            window,
            from.floating,
        )
    }

    /// Hyprland's `getWindowInDirection` from a box on `workspace`, never
    /// finding `ignore`. From a workspace with a fullscreen window only
    /// other fullscreen windows are found.
    ///
    /// By edges: a tiled or fullscreen window whose box's opposite edge
    /// touches this one's and overlaps it along that edge, the most recently
    /// focused of several (`binds:focus_preferred_method` 0).
    ///
    /// By angles: among floating and fullscreen windows whose centre lies
    /// within a right angle of the direction, the nearest of those within
    /// 0.3 pi of it if there are any, else the one at the smallest angle.
    /// As in Hyprland, distances are compared in whole pixels.
    fn search(
        &self,
        candidates: &[Candidate],
        from: Rect,
        workspace: WorkspaceId,
        direction: Direction,
        ignore: WindowId,
        by_angle: bool,
    ) -> Option<WindowId> {
        let fullscreen = self.fullscreen(workspace);
        let eligible = candidates.iter().filter(|candidate| {
            candidate.window != ignore && (fullscreen.is_none() || candidate.fullscreen)
        });
        if !by_angle {
            let mut leader: Option<(usize, WindowId)> = None;
            for candidate in
                eligible.filter(|candidate| !candidate.floating || candidate.fullscreen)
            {
                let to = candidate.ideal;
                let touches = match direction {
                    Direction::Left => sticks(from.x, to.right()),
                    Direction::Right => sticks(from.right(), to.x),
                    Direction::Up => sticks(from.y, to.bottom()),
                    Direction::Down => sticks(from.bottom(), to.y),
                };
                let length = match direction {
                    Direction::Left | Direction::Right => {
                        overlap(from.y, from.bottom(), to.y, to.bottom())
                    }
                    Direction::Up | Direction::Down => {
                        overlap(from.x, from.right(), to.x, to.right())
                    }
                };
                if !touches || length <= 0 {
                    continue;
                }
                // A window never focused is not in the history, and
                // Hyprland's index of -1 for it never leads.
                let Some(recency) = self.history.iter().position(|id| *id == candidate.window)
                else {
                    continue;
                };
                if leader.is_none_or(|(best, _)| recency > best) {
                    leader = Some((recency, candidate.window));
                }
            }
            return leader.map(|(_, window)| window);
        }
        let threshold = 0.3 * PI;
        let (from_x, from_y) = geometry::center(from);
        let (dx, dy) = match direction {
            Direction::Left => (-1.0, 0.0),
            Direction::Right => (1.0, 0.0),
            Direction::Up => (0.0, -1.0),
            Direction::Down => (0.0, 1.0),
        };
        let mut leader: Option<(f64, WindowId)> = None;
        let mut best_angle = 2.0 * PI;
        for candidate in eligible.filter(|candidate| candidate.floating || candidate.fullscreen) {
            let (x, y) = geometry::center(candidate.placed);
            let (vx, vy) = (x - from_x, y - from_y);
            let distance = vx.hypot(vy);
            let angle = ((vx * dx + vy * dy) / distance).clamp(-1.0, 1.0).acos();
            if angle > FRAC_PI_2 {
                continue;
            }
            let nearer = leader.is_some_and(|(nearest, _)| distance < nearest);
            if (best_angle < threshold && nearer && angle < threshold)
                || (angle < best_angle && best_angle > threshold)
                || leader.is_none()
            {
                leader = Some((distance.trunc(), candidate.window));
                best_angle = angle;
            }
        }
        leader
            .map(|(_, window)| window)
            .or_else(|| fullscreen.map(|(window, _)| window))
    }

    /// The monitor whose edge touches the focused monitor's in `direction`,
    /// the one sharing the longest stretch of it if several do. One that
    /// meets it only at a corner counts, as in Hyprland's
    /// `getMonitorInDirection`.
    fn monitor_towards(&self, direction: Direction) -> Option<MonitorId> {
        let from = self.output(self.focused_monitor?)?.monitor.rect;
        let mut best: Option<(i64, MonitorId)> = None;
        for output in &self.outputs {
            let to = output.monitor.rect;
            if Some(output.monitor.id) == self.focused_monitor {
                continue;
            }
            let (touches, length) = match direction {
                Direction::Left => (
                    sticks(from.x, to.right()),
                    overlap(from.y, from.bottom(), to.y, to.bottom()),
                ),
                Direction::Right => (
                    sticks(from.right(), to.x),
                    overlap(from.y, from.bottom(), to.y, to.bottom()),
                ),
                Direction::Up => (
                    sticks(from.y, to.bottom()),
                    overlap(from.x, from.right(), to.x, to.right()),
                ),
                Direction::Down => (
                    sticks(from.bottom(), to.y),
                    overlap(from.x, from.right(), to.x, to.right()),
                ),
            };
            if touches && best.is_none_or(|(longest, _)| length > longest) {
                best = Some((length, output.monitor.id));
            }
        }
        best.map(|(_, monitor)| monitor)
    }

    /// The monitor a point is on, or else the nearest one: Hyprland's
    /// `getMonitorFromVector`.
    fn monitor_at(&self, (x, y): (f64, f64)) -> Option<MonitorId> {
        let mut nearest: Option<(f64, MonitorId)> = None;
        for output in &self.outputs {
            let rect = Area::of(output.monitor.rect);
            if x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h {
                return Some(output.monitor.id);
            }
            let dx = (rect.x - x).max(x - (rect.x + rect.w)).max(0.0);
            let dy = (rect.y - y).max(y - (rect.y + rect.h)).max(0.0);
            let distance = dx * dx + dy * dy;
            if nearest.is_none_or(|(best, _)| distance < best) {
                nearest = Some((distance, output.monitor.id));
            }
        }
        nearest.map(|(_, monitor)| monitor)
    }

    // -- Internals ------------------------------------------------------------

    /// Make a change, then settle the dwindle trees, drop workspaces nobody
    /// needs and work out what changed.
    fn run(&mut self, change: impl FnOnce(&mut Self) -> Vec<Change>) -> Vec<Change> {
        let before = self.snapshot();
        let mut changes = change(self);
        self.settle();
        self.prune();
        let after = self.snapshot();
        if after.monitor != before.monitor
            && let Some(monitor) = after.monitor
        {
            changes.push(Change::FocusMonitor(monitor));
        }
        for layout in &after.layout {
            let old = before
                .layout
                .iter()
                .find(|old| old.monitor == layout.monitor);
            if old.map(|old| old.workspace) != Some(layout.workspace) {
                changes.push(Change::Workspace {
                    monitor: layout.monitor,
                    workspace: layout.workspace,
                });
            }
            if !old.is_some_and(|old| same_geometry(&old.windows, &layout.windows)) {
                changes.push(Change::Layout(layout.monitor));
            }
        }
        if after.focus != before.focus {
            changes.push(Change::Focus(after.focus));
        }
        changes
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            focus: self.focused_window(),
            monitor: self.focused_monitor,
            layout: self.layout(),
        }
    }

    fn output(&self, monitor: MonitorId) -> Option<&Output> {
        self.outputs
            .iter()
            .find(|output| output.monitor.id == monitor)
    }

    /// The most recently focused window on a workspace.
    fn focused_on(&self, workspace: WorkspaceId) -> Option<WindowId> {
        self.history
            .iter()
            .rev()
            .find(|id| self.windows.get(id) == Some(&workspace))
            .copied()
    }

    /// The most recently focused tiled window on a workspace: where a new
    /// tiled window opens, as `dwindle:use_active_for_splits` has it.
    fn recent_tiled(&self, workspace: WorkspaceId) -> Option<WindowId> {
        let ws = self.workspaces.get(&workspace)?;
        self.history
            .iter()
            .rev()
            .find(|id| ws.tiling.contains(**id))
            .copied()
    }

    /// The origin of the monitor a workspace belongs to, `(0, 0)` if it has
    /// none.
    fn origin(&self, workspace: WorkspaceId) -> (i64, i64) {
        self.workspace_monitor(workspace)
            .and_then(|monitor| self.output(monitor))
            .map_or((0, 0), |output| {
                (output.monitor.rect.x, output.monitor.rect.y)
            })
    }

    /// A workspace's work area: its monitor less the reserved strips and
    /// `gaps_out`.
    fn work_area_of(&self, workspace: WorkspaceId) -> Rect {
        self.workspace_monitor(workspace)
            .map(|monitor| work_area(&self.outputs, monitor, &self.settings))
            .unwrap_or_default()
    }

    fn first_free_workspace(&self) -> WorkspaceId {
        let mut id = 1_i64;
        for existing in self.workspaces.keys() {
            if existing.0 == id {
                id = id.saturating_add(1);
            } else if existing.0 > id {
                break;
            }
        }
        WorkspaceId(id)
    }

    fn ensure_workspace(&mut self, workspace: WorkspaceId, monitor: MonitorId) {
        let layout = self.settings.layout;
        let _ws = self
            .workspaces
            .entry(workspace)
            .or_insert_with(|| Workspace {
                monitor,
                tiling: Tiling::new(layout),
                floating: Vec::new(),
                fullscreen: None,
            });
    }

    /// Put a window that is on no workspace onto one, tiled beside that
    /// workspace's most recently focused tiled window, or floating at the
    /// rectangle it has.
    fn attach(&mut self, window: WindowId, workspace: WorkspaceId, floating: bool) {
        let area = Area::of(self.work_area_of(workspace));
        let beside = self.recent_tiled(workspace);
        let Some(ws) = self.workspaces.get_mut(&workspace) else {
            return;
        };
        if floating {
            ws.floating.push(window);
        } else {
            ws.tiling.insert(window, beside, area, &self.settings);
        }
        let _previous = self.windows.insert(window, workspace);
    }

    /// Take a window off its workspace; it stops being fullscreen.
    fn detach(&mut self, window: WindowId) {
        let Some(workspace) = self.windows.remove(&window) else {
            return;
        };
        if let Some(ws) = self.workspaces.get_mut(&workspace) {
            ws.tiling.remove(window);
            ws.floating.retain(|id| *id != window);
            if ws.fullscreen.is_some_and(|(id, _)| id == window) {
                ws.fullscreen = None;
            }
        }
    }

    /// Move a window to another existing workspace, floating or tiled as it
    /// was.
    fn move_window_to(&mut self, window: WindowId, workspace: WorkspaceId) {
        let floating = self.is_floating(window);
        self.detach(window);
        self.attach(window, workspace, floating);
    }

    /// Focus a window: its monitor gets focus and shows its workspace, it
    /// becomes the most recent in the history, and it is raised if it
    /// floats.
    fn focus(&mut self, window: WindowId) {
        let Some(workspace) = self.workspace_of(window) else {
            return;
        };
        if let Some(monitor) = self.workspace_monitor(workspace)
            && let Some(output) = self
                .outputs
                .iter_mut()
                .find(|output| output.monitor.id == monitor)
        {
            // Focusing a window on a special workspace shows that workspace
            // over the monitor's own; it does not switch to it, or hiding
            // the scratchpad again would leave the monitor showing it.
            if Self::is_special(workspace) {
                output.special = Some(workspace);
            } else {
                output.active = workspace;
            }
            self.focused_monitor = Some(monitor);
        }
        self.history.retain(|id| *id != window);
        self.history.push(window);
        if let Some(ws) = self.workspaces.get_mut(&workspace)
            && ws.floating.contains(&window)
        {
            ws.floating.retain(|id| *id != window);
            ws.floating.push(window);
        }
    }

    /// Show a workspace on the monitor it belongs to, creating it on the
    /// focused monitor if it does not exist, and focus that monitor.
    fn show(&mut self, workspace: WorkspaceId) {
        let Some(focused) = self.focused_monitor else {
            return;
        };
        // A special workspace is never shown *instead of* the monitor's own:
        // `workspace special:name` puts the scratchpad over what is there,
        // as `togglespecialworkspace` does.
        if Self::is_special(workspace) {
            self.ensure_workspace(workspace, focused);
            if let Some(output) = self
                .outputs
                .iter_mut()
                .find(|output| output.monitor.id == focused)
            {
                output.special = Some(workspace);
            }
            return;
        }
        self.ensure_workspace(workspace, focused);
        let monitor = match self.workspace_monitor(workspace) {
            Some(monitor) if self.output(monitor).is_some() => monitor,
            _ => {
                if let Some(ws) = self.workspaces.get_mut(&workspace) {
                    ws.monitor = focused;
                }
                focused
            }
        };
        if let Some(output) = self
            .outputs
            .iter_mut()
            .find(|output| output.monitor.id == monitor)
        {
            output.active = workspace;
        }
        self.focused_monitor = Some(monitor);
    }

    /// The workspace a target names, from the one the focused monitor
    /// shows.
    fn resolve(&self, target: WorkspaceTarget) -> Option<WorkspaceId> {
        let current = self.current_workspace()?;
        match target {
            WorkspaceTarget::Special(name) => Some(self.special_id(&name)),
            WorkspaceTarget::Id(id) => Some(id),
            WorkspaceTarget::Relative(offset) => {
                Some(WorkspaceId(current.0.saturating_add(offset).max(1)))
            }
            WorkspaceTarget::Open(offset) => {
                let ids: Vec<WorkspaceId> = self.workspaces.keys().copied().collect();
                let count = i64::try_from(ids.len()).ok()?;
                let index = i64::try_from(ids.iter().position(|id| *id == current)?).ok()?;
                let next = index.checked_add(offset)?.checked_rem_euclid(count)?;
                ids.get(usize::try_from(next).ok()?).copied()
            }
        }
    }

    /// The id of the special workspace called `name`, making one if there is
    /// none.
    ///
    /// Hyprland's special workspaces have negative ids: `special:special` is
    /// `SPECIAL_WORKSPACE_START`, −99, and every other counts up from there
    /// towards −2 (`State::workspaceState()->newSpecialID()`). The name is
    /// `special:` and the name, which is what `hyprctl` prints and what a
    /// `workspace` rule matches.
    fn special_id(&self, name: &str) -> WorkspaceId {
        let full = format!("special:{name}");
        if let Some((id, _)) = self.names.iter().find(|(_, known)| **known == full) {
            return *id;
        }
        if name == "special" {
            return WorkspaceId(SPECIAL_START);
        }
        // The first free id from −99 upwards, as `newSpecialID` takes the
        // highest in use and adds one.
        let taken = |id: i64| {
            self.workspaces.contains_key(&WorkspaceId(id))
                || self.names.contains_key(&WorkspaceId(id))
        };
        let mut id = SPECIAL_START;
        while taken(id) && id < -2 {
            id = id.saturating_add(1);
        }
        WorkspaceId(id)
    }

    /// `togglespecialworkspace`: show the special workspace over the focused
    /// monitor, or hide it if it is the one already showing.
    ///
    /// The workspace is made if it does not exist, as Hyprland makes one; an
    /// empty scratchpad is a scratchpad you can put something in.
    fn toggle_special(&mut self, name: &str) -> Vec<Change> {
        let Some(monitor) = self
            .focused_monitor
            .or_else(|| self.outputs.first().map(|output| output.monitor.id))
        else {
            return Vec::new();
        };
        let id = self.special_id(name);
        let showing = self
            .outputs
            .iter()
            .find(|output| output.monitor.id == monitor)
            .and_then(|output| output.special);
        if showing == Some(id) {
            if let Some(output) = self
                .outputs
                .iter_mut()
                .find(|output| output.monitor.id == monitor)
            {
                output.special = None;
            }
            // The focus goes back to the monitor's own workspace.
            if let Some(window) = self.recent_on_monitor(monitor) {
                self.focus(window);
            }
            return Vec::new();
        }
        self.ensure_workspace(id, monitor);
        let _ = self.names.insert(id, format!("special:{name}"));
        if let Some(output) = self
            .outputs
            .iter_mut()
            .find(|output| output.monitor.id == monitor)
        {
            output.special = Some(id);
        }
        self.focused_monitor = Some(monitor);
        // A special workspace with something on it takes the focus, as
        // Hyprland's does; an empty one leaves it where it was.
        if let Some(window) = self.recent_tiled(id).or_else(|| {
            self.workspaces
                .get(&id)
                .and_then(|ws| ws.floating.last().copied())
        }) {
            self.focus(window);
        }
        Vec::new()
    }

    /// The most recently focused window on the monitor's own workspace.
    fn recent_on_monitor(&self, monitor: MonitorId) -> Option<WindowId> {
        let active = self
            .outputs
            .iter()
            .find(|output| output.monitor.id == monitor)?
            .active;
        self.history
            .iter()
            .rev()
            .find(|window| self.windows.get(window) == Some(&active))
            .copied()
    }

    /// The name a workspace has, which for a numbered one is its number.
    #[must_use]
    pub fn workspace_name(&self, workspace: WorkspaceId) -> String {
        self.names
            .get(&workspace)
            .cloned()
            .unwrap_or_else(|| workspace.0.to_string())
    }

    /// The special workspace a monitor is showing, if any.
    #[must_use]
    pub fn special_on(&self, monitor: MonitorId) -> Option<WorkspaceId> {
        self.outputs
            .iter()
            .find(|output| output.monitor.id == monitor)?
            .special
    }

    /// Whether `workspace` is a special one, by Hyprland's own range.
    #[must_use]
    pub const fn is_special(workspace: WorkspaceId) -> bool {
        workspace.0 >= SPECIAL_START && workspace.0 <= -2
    }

    /// Record each dwindle split's direction for its current box.
    fn settle(&mut self) {
        let outputs = &self.outputs;
        for ws in self.workspaces.values_mut() {
            let area = Area::of(work_area(outputs, ws.monitor, &self.settings));
            ws.tiling.settle(area, &self.settings);
        }
    }

    /// Remove workspaces that are empty and not shown.
    fn prune(&mut self) {
        let outputs = &self.outputs;
        self.workspaces.retain(|id, ws| {
            !ws.is_empty()
                || outputs
                    .iter()
                    .any(|output| output.active == *id || output.special == Some(*id))
        });
    }

    /// The visible windows of what `output` shows.
    fn placements(&self, output: &Output, focus: Option<WindowId>) -> Vec<Placed> {
        let Some(ws) = self.workspaces.get(&output.active) else {
            return Vec::new();
        };
        let area = geometry::work_area(&output.monitor, &self.settings);
        let border = self.settings.border_size;
        let place =
            |window: WindowId, rect: Rect, border: i64, floating: bool, fullscreen: bool| Placed {
                window,
                rect,
                border,
                focused: focus == Some(window),
                floating,
                fullscreen,
            };
        if let Some((window, mode)) = ws.fullscreen {
            let floating = ws.floating.contains(&window);
            return vec![match mode {
                FullscreenMode::Fullscreen => place(window, output.monitor.rect, 0, floating, true),
                FullscreenMode::Maximized => place(
                    window,
                    geometry::client(area, area, &self.settings),
                    border,
                    floating,
                    true,
                ),
            }];
        }
        let mut windows: Vec<Placed> = ws
            .tiling
            .slots(Area::of(area), &self.settings)
            .into_iter()
            .map(|(window, slot)| {
                let rect = geometry::client(slot.round(), area, &self.settings);
                place(window, rect, border, false, false)
            })
            .collect();
        let (x, y) = (output.monitor.rect.x, output.monitor.rect.y);
        windows.extend(ws.floating.iter().filter_map(|&window| {
            self.floating_rects
                .get(&window)
                .map(|rect| place(window, rect.translate(x, y), border, true, false))
        }));
        windows
    }
}

/// The work area of `monitor` among `outputs`, empty if it is not there.
fn work_area(outputs: &[Output], monitor: MonitorId, settings: &Settings) -> Rect {
    outputs
        .iter()
        .find(|output| output.monitor.id == monitor)
        .map(|output| geometry::work_area(&output.monitor, settings))
        .unwrap_or_default()
}

/// A window's box as Hyprland's direction searches see it,
/// `getWindowIdealBoundingBoxIgnoreReserved`: where the layout placed it,
/// grown out to the monitor's edge on each side where it meets the edge of
/// the work area, so that windows on two monitors side by side touch across
/// the gaps and reserved strips between them.
fn ideal_box(placed: Rect, monitor: Rect, work: Rect) -> Rect {
    let mut ideal = placed;
    if placed.y == work.y {
        ideal.y = monitor.y;
        ideal.height = ideal
            .height
            .saturating_add(work.y.saturating_sub(monitor.y));
    }
    if placed.x == work.x {
        ideal.x = monitor.x;
        ideal.width = ideal.width.saturating_add(work.x.saturating_sub(monitor.x));
    }
    if placed.right() == work.right() {
        ideal.width = ideal
            .width
            .saturating_add(monitor.right().saturating_sub(work.right()));
    }
    if placed.bottom() == work.bottom() {
        ideal.height = ideal
            .height
            .saturating_add(monitor.bottom().saturating_sub(work.bottom()));
    }
    ideal
}

/// The point one pixel past the middle of a box's edge in `direction`,
/// which Hyprland's `focalPointForDir` aims a moved window at.
fn focal_point(ideal: Rect, direction: Direction) -> (f64, f64) {
    let (x, y) = (ideal.x as f64, ideal.y as f64);
    let (width, height) = (ideal.width as f64, ideal.height as f64);
    match direction {
        Direction::Up => (x + width / 2.0, y - 1.0),
        Direction::Down => (x + width / 2.0, y + height + 1.0),
        Direction::Left => (x - 1.0, y + height / 2.0),
        Direction::Right => (x + width + 1.0, y + height / 2.0),
    }
}

/// Whether two lists of placements put the same windows in the same places,
/// ignoring focus, which [`Change::Focus`] reports.
fn same_geometry(a: &[Placed], b: &[Placed]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.window == b.window
                && a.rect == b.rect
                && a.border == b.border
                && a.floating == b.floating
                && a.fullscreen == b.fullscreen
        })
}
