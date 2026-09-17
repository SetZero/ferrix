//! The `hyprctl` socket: one request a connection, answered and closed.
//!
//! Hyprland puts two sockets in an instance directory under
//! `$XDG_RUNTIME_DIR/hypr/`, and `hyprctl` finds them there. This is the
//! first of them: a connection sends one line, is answered, and is closed.
//! What a request means and what an answer says are `compositor/ipc`'s; this
//! is the socket under them.

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
    /// # Errors
    ///
    /// Whatever the bind said.
    pub fn bind(runtime: &Path, instance: &str) -> std::io::Result<Self> {
        let directory = runtime.join("hypr").join(instance);
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
    screen: (u32, u32),
) -> Snapshot {
    let mut snapshot = Snapshot {
        monitors: vec![Monitor {
            id: 0,
            name: "HEADLESS-1".to_owned(),
            width: i32::try_from(screen.0).unwrap_or(0),
            height: i32::try_from(screen.1).unwrap_or(0),
            refresh: 60.0,
            at: (0, 0),
            active_workspace: 1,
            active_workspace_name: "1".to_owned(),
            scale: 1.0,
            focused: true,
        }],
        ..Snapshot::default()
    };

    let focused = state.focused_window();
    // Hyprland's focus history id: 0 is the focused window, and the rest
    // count back from it. Only the focused one is known here, so the others
    // are -1, which Hyprland uses for a window that is not in the history.
    for output in state.layout() {
        let mut windows = 0;
        let mut has_fullscreen = false;
        for placed in &output.windows {
            windows += 1;
            has_fullscreen |= placed.fullscreen;
            let Some(source) = sources.get(&placed.window) else {
                continue;
            };
            let Some(slot) = clients.get(source.client) else {
                continue;
            };
            let named = toplevel_of(slot.client(), source.surface);
            snapshot.windows.push(Window {
                address: placed.window.0,
                mapped: slot
                    .client()
                    .surface(source.surface)
                    .is_some_and(compositor_server::Surface::is_mapped),
                visible: true,
                at: (
                    i32::try_from(placed.rect.x).unwrap_or(0),
                    i32::try_from(placed.rect.y).unwrap_or(0),
                ),
                size: (
                    i32::try_from(placed.rect.width).unwrap_or(0),
                    i32::try_from(placed.rect.height).unwrap_or(0),
                ),
                workspace: i32::try_from(output.workspace.0).unwrap_or(0),
                workspace_name: output.workspace.0.to_string(),
                floating: placed.floating,
                fullscreen: placed.fullscreen,
                monitor: 0,
                class: named.as_ref().map(|top| top.1.clone()).unwrap_or_default(),
                title: named.map(|top| top.0).unwrap_or_default(),
                pid: 0,
                focus_history: if Some(placed.window) == focused {
                    0
                } else {
                    -1
                },
            });
        }
        snapshot.workspaces.push(Workspace {
            id: i32::try_from(output.workspace.0).unwrap_or(0),
            name: output.workspace.0.to_string(),
            monitor: "HEADLESS-1".to_owned(),
            windows,
            has_fullscreen,
        });
        snapshot.active_workspace = i32::try_from(output.workspace.0).unwrap_or(0);
        if let Some(monitor) = snapshot.monitors.first_mut() {
            monitor.active_workspace = snapshot.active_workspace;
            monitor.active_workspace_name = snapshot.active_workspace.to_string();
        }
    }
    snapshot.active_window = focused.map(|window| window.0);
    snapshot
}

/// The title and app id of the window on `surface`, if it has them.
fn toplevel_of(client: &Client, surface: ObjectId) -> Option<(String, String)> {
    client
        .toplevels()
        .find(|(_, top)| top.surface == surface)
        .map(|(_, top)| (top.title.clone(), top.app_id.clone()))
}
