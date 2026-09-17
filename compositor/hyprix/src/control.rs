//! The two `hyprctl` sockets: the one that answers, and the one that tells.
//!
//! Hyprland puts both in an instance directory under
//! `$XDG_RUNTIME_DIR/hypr/`, and `hyprctl` and every bar find them there.
//!
//! * **`.socket.sock`**, [`Control`]: a connection sends one line, is
//!   answered, and is closed.
//! * **`.socket2.sock`**, [`Events`]: a connection is kept, and every state
//!   change is a line written to it. It is never read from.
//!
//! What a request means, what an answer says and what an event's line is are
//! `compositor/ipc`'s; this is the sockets under them.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use compositor_ipc::{Monitor, Request, Snapshot, Window, Workspace};
use compositor_layout::{State, WindowId};
use compositor_server::Client;
use compositor_wire::ObjectId;

use crate::frame::Source;

/// The instance directory and the socket in it.
#[derive(Debug)]
pub struct Control {
    listener: UnixListener,
    path: PathBuf,
    directory: PathBuf,
}

impl Control {
    /// Bind the request socket for `instance`.
    ///
    /// Hyprland's own layout: `$XDG_RUNTIME_DIR/hypr/<instance>/.socket.sock`,
    /// with the instance in `HYPRLAND_INSTANCE_SIGNATURE`. A program looks
    /// there and nowhere else, so a compositor that puts it somewhere else is
    /// one `hyprctl` cannot find.
    ///
    /// An `instance` with a `/` in it is the directory itself, as a
    /// `--display` with one is the socket itself. Nothing real passes one:
    /// `HYPRLAND_INSTANCE_SIGNATURE` is a name. It is how a test gives two
    /// compositors in one process two instance directories without setting
    /// an environment variable that the whole process shares.
    ///
    /// # Errors
    ///
    /// Whatever the bind said.
    pub fn bind(runtime: &Path, instance: &str) -> std::io::Result<Self> {
        let directory = if instance.contains('/') {
            PathBuf::from(instance)
        } else {
            runtime.join("hypr").join(instance)
        };
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(compositor_ipc::REQUEST_SOCKET);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path,
            directory,
        })
    }

    /// Where the socket is, for `HYPRLAND_INSTANCE_SIGNATURE` and for a test.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The instance directory, which the event socket goes in beside this
    /// one.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Take one waiting connection, if there is one.
    ///
    /// A request is one line and an answer is one write, so a connection is
    /// served and closed in one go rather than kept: `hyprctl` opens one per
    /// request and so does every bar.
    pub fn accept(&self) -> Option<UnixStream> {
        match self.listener.accept() {
            Ok((stream, _)) => Some(stream),
            Err(_) => None,
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        // A socket file left behind is a `hyprctl` that connects to nothing.
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

/// Read one request from `stream`, answer it, and close.
///
/// Gives back the requests that change something, which the caller runs: this
/// function holds no compositor.
pub fn serve(
    stream: &mut UnixStream,
    snapshot: &Snapshot,
) -> std::io::Result<Vec<compositor_ipc::Reply>> {
    let mut line = String::new();
    let mut buffer = [0u8; 4096];
    // One read: `hyprctl` writes its line and waits, and a request longer
    // than a buffer is one nothing sends.
    let read = stream.read(&mut buffer)?;
    line.push_str(&String::from_utf8_lossy(buffer.get(..read).unwrap_or(&[])));

    let mut todo = Vec::new();
    let mut answer = String::new();
    for request in Request::parse_batch(&line) {
        match compositor_ipc::answer(&request, snapshot, compositor_ipc::Version::default()) {
            compositor_ipc::Reply::Text(text) => answer.push_str(&text),
            other => {
                // Hyprland answers `ok` for a request that did something and
                // leaves the doing to the compositor; so does this.
                answer.push_str("ok\n");
                todo.push(other);
            }
        }
    }
    stream.write_all(answer.as_bytes())?;
    stream.flush()?;
    Ok(todo)
}

/// Describe the compositor for an answer.
///
/// A snapshot rather than a borrow: an answer is written whole, and a request
/// that arrived halfway through a frame should describe the frame before it
/// rather than half of the next.
pub fn snapshot(
    state: &State,
    clients: &[crate::state::Slot],
    sources: &BTreeMap<WindowId, Source>,
) -> Snapshot {
    let mut snapshot = Snapshot::default();
    // One monitor a screen, in the order they were added, which is the order
    // `hyprctl monitors` prints and the order `focusmonitor +1` walks.
    for monitor in state.monitors() {
        let active = state.active_workspace(monitor.id);
        snapshot.monitors.push(Monitor {
            id: monitor_id(monitor.id),
            name: monitor.name.clone(),
            width: i32::try_from(monitor.rect.width).unwrap_or(0),
            height: i32::try_from(monitor.rect.height).unwrap_or(0),
            refresh: 60.0,
            at: (
                i32::try_from(monitor.rect.x).unwrap_or(0),
                i32::try_from(monitor.rect.y).unwrap_or(0),
            ),
            active_workspace: active.map_or(0, |id| i32::try_from(id.0).unwrap_or(0)),
            active_workspace_name: active
                .map(|id| state.workspace_name(id))
                .unwrap_or_default(),
            special_workspace: state
                .special_on(monitor.id)
                .map(|id| (i32::try_from(id.0).unwrap_or(0), state.workspace_name(id))),
            scale: 1.0,
            focused: Some(monitor.id) == state.focused_monitor(),
        });
    }

    let focused = state.focused_window();
    // Hyprland's focus history id: 0 is the focused window, and the rest
    // count back from it. Only the focused one is known here, so the others
    // are -1, which Hyprland uses for a window that is not in the history.
    for output in state.layout() {
        let mut windows = 0;
        let mut has_fullscreen = false;
        for placed in &output.windows {
            has_fullscreen |= placed.fullscreen;
            // A group's slot draws one member; Hyprland lists the rest as
            // hidden windows with the same box, so a bar can draw the tabs.
            let grouped: Vec<u64> = state
                .group(placed.window)
                .map(|group| group.members.iter().map(|member| member.0).collect())
                .unwrap_or_default();
            let shown = placed.window;
            let members = if grouped.is_empty() {
                vec![shown]
            } else {
                grouped.iter().copied().map(WindowId).collect()
            };
            for member in members {
                windows += 1;
                let described = Described {
                    window: member,
                    placed,
                    workspace: output.workspace,
                    hidden: member != shown,
                    grouped: &grouped,
                };
                if let Some(window) = describe(state, clients, sources, focused, &described) {
                    snapshot.windows.push(window);
                }
            }
        }
        snapshot.workspaces.push(Workspace {
            id: i32::try_from(output.workspace.0).unwrap_or(0),
            name: state.workspace_name(output.workspace),
            monitor: state
                .monitors()
                .find(|monitor| monitor.id == output.monitor)
                .map(|monitor| monitor.name.clone())
                .unwrap_or_default(),
            windows,
            has_fullscreen,
        });
        // The active workspace is the focused monitor's, not the last
        // monitor's: `hyprctl activeworkspace` answers about where the
        // person is.
        if Some(output.monitor) == state.focused_monitor() {
            snapshot.active_workspace = i32::try_from(output.workspace.0).unwrap_or(0);
        }
    }
    snapshot.active_window = focused.map(|window| window.0);
    snapshot
}

/// The id `hyprctl` gives a monitor, which Hyprland counts from zero where
/// this tree's `MonitorId` counts from one.
fn monitor_id(monitor: compositor_layout::MonitorId) -> i32 {
    i32::try_from(monitor.0.saturating_sub(1)).unwrap_or(0)
}

/// The title and app id of the window on `surface`, if it has them.
/// One window for `hyprctl clients`, and where it is.
struct Described<'a> {
    /// The window being described, which for a group's hidden member is not
    /// the window whose placement it borrows.
    window: WindowId,
    /// The placement of the slot it is in.
    placed: &'a compositor_layout::Placed,
    /// The workspace that slot is on.
    workspace: compositor_layout::WorkspaceId,
    /// Whether a group draws another member in its place.
    hidden: bool,
    /// Its group's members, empty if it is in none.
    grouped: &'a [u64],
}

/// What `hyprctl clients` says about one window, or nothing if no client of
/// this compositor owns it.
fn describe(
    state: &State,
    clients: &[crate::state::Slot],
    sources: &BTreeMap<WindowId, Source>,
    focused: Option<WindowId>,
    it: &Described,
) -> Option<Window> {
    let source = sources.get(&it.window)?;
    let slot = clients.get(source.client)?;
    let named = toplevel_of(slot.client(), source.surface);
    let placed = it.placed;
    Some(Window {
        address: it.window.0,
        mapped: slot
            .client()
            .surface(source.surface)
            .is_some_and(compositor_server::Surface::is_mapped),
        hidden: it.hidden,
        visible: !it.hidden,
        at: (
            i32::try_from(placed.rect.x).unwrap_or(0),
            i32::try_from(placed.rect.y).unwrap_or(0),
        ),
        size: (
            i32::try_from(placed.rect.width).unwrap_or(0),
            i32::try_from(placed.rect.height).unwrap_or(0),
        ),
        workspace: i32::try_from(it.workspace.0).unwrap_or(0),
        workspace_name: state.workspace_name(it.workspace),
        floating: placed.floating,
        fullscreen: placed.fullscreen,
        monitor: 0,
        class: named.as_ref().map(|top| top.1.clone()).unwrap_or_default(),
        title: named.map(|top| top.0).unwrap_or_default(),
        pid: 0,
        focus_history: if Some(it.window) == focused { 0 } else { -1 },
        grouped: it.grouped.to_vec(),
    })
}

fn toplevel_of(client: &Client, surface: ObjectId) -> Option<(String, String)> {
    client
        .toplevels()
        .find(|(_, top)| top.surface == surface)
        .map(|(_, top)| (top.title.clone(), top.app_id.clone()))
}

/// The event socket: connections that are written to and never read.
///
/// A bar connects once and stays connected for the session, so the
/// connections are kept and each is written to as things happen. A client
/// that goes away is dropped on the write that fails, which is how Hyprland
/// notices too (`CEventManager::flushClient`).
#[derive(Debug)]
pub struct Events {
    listener: UnixListener,
    subscribers: Vec<UnixStream>,
    watcher: compositor_ipc::Watcher,
    /// How many lines have been written, for the compositor's own report.
    written: u64,
}

impl Events {
    /// Bind the event socket beside the request one.
    ///
    /// # Errors
    ///
    /// Whatever the bind said.
    pub fn bind(directory: &Path) -> std::io::Result<Self> {
        let path = directory.join(compositor_ipc::EVENT_SOCKET);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            subscribers: Vec::new(),
            watcher: compositor_ipc::Watcher::new(),
            written: 0,
        })
    }

    /// How many subscribers there are and how many lines they have been
    /// sent, for the compositor's own report.
    #[must_use]
    pub fn counts(&self) -> (usize, u64) {
        (self.subscribers.len(), self.written)
    }

    /// Take any new subscribers, work out what changed, and write it.
    ///
    /// A subscriber that has just connected is told what already exists,
    /// because its [`compositor_ipc::Watcher`] would otherwise start from
    /// the state it connected in and tell it nothing until something moved.
    /// Hyprland leaves a fresh client to ask `hyprctl` for that; telling it
    /// is strictly more useful and no reader can be surprised by an event
    /// for a window it does not know about yet.
    pub fn publish(&mut self, snapshot: &Snapshot) {
        let mut fresh = Vec::new();
        while let Ok((stream, _)) = self.listener.accept() {
            // Written to and never read: a blocking write to a bar that has
            // stopped reading would stop the compositor.
            if stream.set_nonblocking(true).is_ok() {
                fresh.push(stream);
            }
        }
        // A first subscriber is told the whole state; one joining an
        // existing one gets the changes from here on, as Hyprland's would.
        if !fresh.is_empty() && self.subscribers.is_empty() {
            self.watcher = compositor_ipc::Watcher::new();
        }
        let events = self.watcher.changed(snapshot);
        self.subscribers.append(&mut fresh);
        if events.is_empty() || self.subscribers.is_empty() {
            return;
        }
        let lines: Vec<String> = events
            .iter()
            .flat_map(compositor_ipc::Event::lines)
            .collect();
        self.subscribers.retain_mut(|stream| {
            for line in &lines {
                if stream.write_all(line.as_bytes()).is_err() {
                    return false;
                }
            }
            true
        });
        self.written = self
            .written
            .saturating_add(lines.len().try_into().unwrap_or(u64::MAX));
    }
}
