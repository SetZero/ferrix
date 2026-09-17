//! Answering a request.

use core::fmt::Write;

use crate::json::Json;
use crate::request::{Flags, Format, Request};
use crate::state::{Monitor, Snapshot, Window, Workspace};

/// What the compositor calls itself in `hyprctl version`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Version {
    /// The compositor's name.
    pub name: &'static str,
    /// Its version.
    pub version: &'static str,
    /// The commit it was built from, or an empty string.
    pub commit: &'static str,
}

impl Default for Version {
    fn default() -> Self {
        Self {
            name: "hyprix",
            version: env!("CARGO_PKG_VERSION"),
            commit: "",
        }
    }
}

/// What a request asked the compositor to do that this crate cannot do
/// itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reply {
    /// The answer, written.
    Text(String),
    /// `dispatch <name> <argument>`: the compositor runs it and says what
    /// happened.
    Dispatch {
        /// The dispatcher's name.
        name: String,
        /// Its argument.
        argument: String,
    },
    /// `keyword <name> <value>`: the configuration is changed while it runs.
    Keyword {
        /// The option's name, `category:key`.
        name: String,
        /// Its new value, as written.
        value: String,
    },
    /// `reload`: read the configuration file again.
    Reload,
}

/// Answer `request` from `snapshot`.
///
/// The commands that only read state are answered here; the ones that change
/// it come back for the compositor to run, because this crate holds no
/// compositor.
#[must_use]
pub fn answer(request: &Request, snapshot: &Snapshot, version: Version) -> Reply {
    let flags = request.flags;
    let text = |value: String| Reply::Text(value);
    match request.command.as_str() {
        "version" => text(self_version(flags, version)),
        "monitors" => text(monitors(flags, &snapshot.monitors)),
        "workspaces" => text(workspaces(flags, &snapshot.workspaces)),
        "clients" => text(clients(flags, snapshot)),
        "activewindow" => text(active_window(flags, snapshot)),
        "activeworkspace" => text(active_workspace(flags, snapshot)),
        "splash" => text("a compositor, on an operating system, in Rust\n".to_owned()),
        "dispatch" => {
            let (name, argument) = split(&request.argument);
            Reply::Dispatch { name, argument }
        }
        "keyword" => {
            let (name, value) = split(&request.argument);
            Reply::Keyword { name, value }
        }
        "reload" => Reply::Reload,
        // Hyprland answers an unknown command with a line rather than by
        // closing the connection, so a program asking for something newer
        // keeps working for everything else it asks.
        other => text(format!("unknown request {other}\n")),
    }
}

/// A command's argument split into its first word and the rest.
fn split(argument: &str) -> (String, String) {
    match argument.split_once(char::is_whitespace) {
        Some((first, rest)) => (first.to_owned(), rest.trim().to_owned()),
        None => (argument.to_owned(), String::new()),
    }
}

/// Whether to pretty-print: `r` asks for compact JSON.
const fn pretty(flags: Flags) -> bool {
    !flags.raw
}

fn self_version(flags: Flags, version: Version) -> String {
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.object();
        out.string("branch", "");
        out.string("commit", version.commit);
        out.boolean("dirty", false);
        out.string("commit_message", "");
        out.string("commit_date", "");
        out.string("tag", version.version);
        out.number("commits", 0);
        out.string("buildAquamarine", "");
        out.string("buildHyprlang", "");
        out.string("buildHyprutils", "");
        out.string("buildHyprcursor", "");
        out.string("buildHyprgraphics", "");
        out.empty_array("flags");
        out.end('}');
        return finish(out, flags);
    }
    format!(
        "{} {}\n\nno flags were set\n",
        version.name, version.version
    )
}

fn monitors(flags: Flags, monitors: &[Monitor]) -> String {
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.array();
        for monitor in monitors {
            out.object();
            out.number("id", i64::from(monitor.id));
            out.string("name", &monitor.name);
            out.string("description", &monitor.name);
            out.string("make", "Ferrix");
            out.string("model", "hyprix");
            out.string("serial", "");
            out.number("width", i64::from(monitor.width));
            out.number("height", i64::from(monitor.height));
            out.field_refresh(monitor.refresh);
            out.number("x", i64::from(monitor.at.0));
            out.number("y", i64::from(monitor.at.1));
            // Hyprland has one `activeWorkspace`, an object; writing a
            // number of the same name beside it makes two fields with one
            // key, and which a reader takes is its own business.
            out.object_field("activeWorkspace");
            out.number("id", i64::from(monitor.active_workspace));
            out.string("name", &monitor.active_workspace_name);
            out.end('}');
            out.field_scale(monitor.scale);
            out.boolean("focused", monitor.focused);
            out.boolean("dpmsStatus", true);
            out.end('}');
        }
        out.end(']');
        return finish(out, flags);
    }
    let mut text = String::new();
    for monitor in monitors {
        let _ = writeln!(
            text,
            "Monitor {} (ID {}):\n\t{}x{}@{:.5} at {}x{}\n\tactive workspace: {} ({})\n\tscale: {:.2}\n\tfocused: {}\n",
            monitor.name,
            monitor.id,
            monitor.width,
            monitor.height,
            monitor.refresh,
            monitor.at.0,
            monitor.at.1,
            monitor.active_workspace,
            monitor.active_workspace_name,
            monitor.scale,
            if monitor.focused { "yes" } else { "no" },
        );
    }
    text
}

fn workspaces(flags: Flags, workspaces: &[Workspace]) -> String {
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.array();
        for workspace in workspaces {
            out.object();
            write_workspace(&mut out, workspace);
            out.end('}');
        }
        out.end(']');
        return finish(out, flags);
    }
    let mut text = String::new();
    for workspace in workspaces {
        let _ = writeln!(
            text,
            "workspace ID {} ({}) on monitor {}:\n\twindows: {}\n\thasfullscreen: {}\n",
            workspace.id,
            workspace.name,
            workspace.monitor,
            workspace.windows,
            i32::from(workspace.has_fullscreen),
        );
    }
    text
}

fn write_workspace(out: &mut Json, workspace: &Workspace) {
    out.number("id", i64::from(workspace.id));
    out.string("name", &workspace.name);
    out.string("monitor", &workspace.monitor);
    out.number("monitorID", 0);
    out.number("windows", i64::from(workspace.windows));
    out.boolean("hasfullscreen", workspace.has_fullscreen);
    out.string("lastwindow", "0x0");
    out.string("lastwindowtitle", "");
    out.boolean("ispersistent", false);
}

fn clients(flags: Flags, snapshot: &Snapshot) -> String {
    let shown: Vec<&Window> = snapshot
        .windows
        .iter()
        .filter(|window| flags.all || window.mapped)
        .collect();
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.array();
        for window in shown {
            out.object();
            write_window(&mut out, window);
            out.end('}');
        }
        out.end(']');
        return finish(out, flags);
    }
    let mut text = String::new();
    for window in shown {
        let _ = write!(text, "{}", readable_window(window));
    }
    text
}

fn active_window(flags: Flags, snapshot: &Snapshot) -> String {
    let Some(window) = snapshot.active() else {
        // Hyprland answers `{}` in JSON and nothing at all otherwise, and
        // every bar is written for that.
        return match flags.format() {
            Format::Json => "{}\n".to_owned(),
            Format::Readable => "Invalid\n".to_owned(),
        };
    };
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.object();
        write_window(&mut out, window);
        out.end('}');
        return finish(out, flags);
    }
    readable_window(window)
}

fn active_workspace(flags: Flags, snapshot: &Snapshot) -> String {
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.id == snapshot.active_workspace)
        .cloned()
        .unwrap_or_else(|| Workspace {
            id: snapshot.active_workspace,
            name: snapshot.active_workspace.to_string(),
            ..Workspace::default()
        });
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.object();
        write_workspace(&mut out, &workspace);
        out.end('}');
        return finish(out, flags);
    }
    format!(
        "workspace ID {} ({}) on monitor {}:\n\twindows: {}\n\thasfullscreen: {}\n\n",
        workspace.id,
        workspace.name,
        workspace.monitor,
        workspace.windows,
        i32::from(workspace.has_fullscreen),
    )
}

/// The fields `hyprctl clients` gives a window, in Hyprland's own order.
///
/// The ones this compositor has no answer for are written at the value
/// Hyprland gives a window that has none, rather than left out: a bar reads
/// them by name, and a missing name is a crash in somebody else's program.
fn write_window(out: &mut Json, window: &Window) {
    out.string("address", &format!("0x{:x}", window.address));
    out.boolean("mapped", window.mapped);
    out.boolean("hidden", false);
    out.boolean("visible", window.visible);
    out.boolean("acceptsInput", true);
    out.pair("at", i64::from(window.at.0), i64::from(window.at.1));
    out.pair("size", i64::from(window.size.0), i64::from(window.size.1));
    out.object_field("workspace");
    out.number("id", i64::from(window.workspace));
    out.string("name", &window.workspace_name);
    out.end('}');
    out.boolean("floating", window.floating);
    out.number("monitor", i64::from(window.monitor));
    out.string("class", &window.class);
    out.string("title", &window.title);
    out.string("initialClass", &window.class);
    out.string("initialTitle", &window.title);
    out.number("pid", i64::from(window.pid));
    out.boolean("xwayland", false);
    out.boolean("pinned", false);
    out.boolean("fullscreen", window.fullscreen);
    out.number("fullscreenClient", 0);
    out.empty_array("grouped");
    out.empty_array("tags");
    out.string("swallowing", "0x0");
    out.number("focusHistoryID", i64::from(window.focus_history));
    out.boolean("inhibitingIdle", false);
}

/// The readable form, which `hyprctl clients` prints without `-j`.
fn readable_window(window: &Window) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "Window {:x} -> {}:", window.address, window.title);
    let _ = writeln!(text, "\tmapped: {}", window.mapped);
    let _ = writeln!(text, "\thidden: 0");
    let _ = writeln!(text, "\tat: {},{}", window.at.0, window.at.1);
    let _ = writeln!(text, "\tsize: {},{}", window.size.0, window.size.1);
    let _ = writeln!(
        text,
        "\tworkspace: {} ({})",
        window.workspace, window.workspace_name
    );
    let _ = writeln!(text, "\tfloating: {}", i32::from(window.floating));
    let _ = writeln!(text, "\tmonitor: {}", window.monitor);
    let _ = writeln!(text, "\tclass: {}", window.class);
    let _ = writeln!(text, "\ttitle: {}", window.title);
    let _ = writeln!(text, "\tpid: {}", window.pid);
    let _ = writeln!(text, "\txwayland: 0");
    let _ = writeln!(text, "\tfullscreen: {}", i32::from(window.fullscreen));
    let _ = writeln!(text, "\tfocusHistoryID: {}\n", window.focus_history);
    text
}

/// Add the trailing newline unless the request asked for none.
fn finish(out: Json, flags: Flags) -> String {
    let mut text = out.finish();
    if !flags.no_newline {
        text.push('\n');
    }
    text
}
