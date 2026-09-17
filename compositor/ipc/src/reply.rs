//! Answering a request.

use core::fmt::Write;

use crate::json::Json;
use crate::request::{Flags, Format, Request};
use crate::state::{Bind, Devices, Layer, Monitor, Plugin, Snapshot, Window, Workspace};

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
        // `submapRequest`: the name, or `default` for the global map, and in
        // JSON a bare string rather than an object.
        "submap" => {
            let name = if snapshot.submap.is_empty() {
                "default"
            } else {
                &snapshot.submap
            };
            text(if flags.format() == Format::Json {
                format!("\"{name}\"\n")
            } else {
                format!("{name}\n")
            })
        }
        "splash" => text("a compositor, on an operating system, in Rust\n".to_owned()),
        "binds" => text(binds(flags, &snapshot.binds)),
        "devices" => text(devices(flags, &snapshot.devices)),
        "layers" => text(layers(flags, snapshot)),
        "cursorpos" => text(cursor_position(flags, snapshot.cursor)),
        "locked" => text(if flags.format() == Format::Json {
            let mut out = Json::new(pretty(flags));
            out.object();
            out.boolean("locked", snapshot.locked);
            out.end('}');
            finish(out, flags)
        } else {
            format!("{}\n", snapshot.locked)
        }),
        // Neither has anything to list, and both are answered rather than
        // refused: a bar asking for them should get an empty list, not
        // `unknown request`. `workspacerules` has no `workspacerule` keyword
        // to fill it yet, and `globalshortcuts` needs a protocol this
        // compositor does not offer.
        "workspacerules" | "globalshortcuts" => text(if flags.format() == Format::Json {
            let mut out = Json::new(pretty(flags));
            out.array();
            out.end(']');
            finish(out, flags)
        } else {
            String::new()
        }),
        "dispatch" => {
            let (name, argument) = split(&request.argument);
            Reply::Dispatch { name, argument }
        }
        "keyword" => {
            let (name, value) = split(&request.argument);
            Reply::Keyword { name, value }
        }
        "reload" => Reply::Reload,
        // `hyprctl plugin list`, and nothing else: Hyprland's `load` and
        // `unload` take a shared object, and this compositor's plugins are
        // programs it starts.
        "plugin" => text(match request.argument.trim() {
            "list" => plugins(flags, &snapshot.plugins),
            _ => "unknown opt\n".to_owned(),
        }),
        // Hyprland answers an unknown command with a line rather than by
        // closing the connection, so a program asking for something newer
        // keeps working for everything else it asks.
        other => text(format!("unknown request {other}\n")),
    }
}

/// `hyprctl binds`, in Hyprland's own shape.
///
/// The letters after `bind` are the flags the line was written with, in
/// `bindsRequest`'s own order, and the fields under them are its.
fn binds(flags: Flags, binds: &[Bind]) -> String {
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.array();
        for bind in binds {
            out.object();
            out.boolean("locked", bind.locked);
            out.boolean("mouse", bind.mouse);
            out.boolean("release", bind.release);
            out.boolean("repeat", bind.repeat);
            out.boolean("longPress", bind.long_press);
            out.boolean("non_consuming", bind.non_consuming);
            out.boolean("has_description", bind.has_description);
            out.number("modmask", i64::from(bind.modmask));
            out.string("submap", &bind.submap);
            out.boolean("submap_universal", bind.submap_universal);
            out.string("key", &bind.key);
            out.number("keycode", i64::from(bind.keycode));
            out.boolean("catch_all", bind.catch_all);
            out.string("description", &bind.description);
            out.string("dispatcher", &bind.dispatcher);
            out.string("arg", &bind.arg);
            out.end('}');
        }
        out.end(']');
        return finish(out, flags);
    }
    let mut text = String::new();
    for bind in binds {
        let mut letters = String::from("bind");
        for (on, letter) in [
            (bind.locked, 'l'),
            (bind.mouse, 'm'),
            (bind.release, 'r'),
            (bind.repeat, 'e'),
            (bind.non_consuming, 'n'),
            (bind.has_description, 'd'),
        ] {
            if on {
                letters.push(letter);
            }
        }
        let _ = writeln!(
            text,
            "{letters}\n\tmodmask: {}\n\tsubmap: {}\n\tkey: {}\n\tkeycode: {}\n\tcatchall: \
             {}\n\tdescription: {}\n\tdispatcher: {}\n\targ: {}\n",
            bind.modmask,
            bind.submap,
            bind.key,
            bind.keycode,
            bind.catch_all,
            bind.description,
            bind.dispatcher,
            bind.arg,
        );
    }
    text
}

/// `hyprctl devices`, in Hyprland's own shape.
///
/// The five groups it prints, in its order. What this compositor has no
/// notion of -- a pointer's acceleration, a tablet's physical size -- is
/// left out of the readable form and given its zero in the JSON, because a
/// bar reading a field that is always missing is worse than one reading a
/// field that is always zero.
fn devices(flags: Flags, devices: &Devices) -> String {
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.object();
        out.array_field("mice");
        for mouse in &devices.mice {
            out.object();
            out.string("address", &format!("0x{:x}", mouse.address));
            out.string("name", &mouse.name);
            out.end('}');
        }
        out.end(']');
        out.array_field("keyboards");
        for keyboard in &devices.keyboards {
            out.object();
            out.string("address", &format!("0x{:x}", keyboard.device.address));
            out.string("name", &keyboard.device.name);
            out.string("rules", &keyboard.rules);
            out.string("model", &keyboard.model);
            out.string("layout", &keyboard.layout);
            out.string("variant", &keyboard.variant);
            out.string("options", &keyboard.options);
            out.number(
                "active_layout_index",
                i64::from(keyboard.active_layout_index),
            );
            out.string("active_keymap", &keyboard.active_keymap);
            out.boolean("capsLock", keyboard.caps_lock);
            out.boolean("numLock", keyboard.num_lock);
            out.boolean("main", keyboard.main);
            out.end('}');
        }
        out.end(']');
        for (name, group) in [
            ("tablets", &devices.tablets),
            ("touch", &devices.touch),
            ("switches", &devices.switches),
        ] {
            out.array_field(name);
            for device in group {
                out.object();
                out.string("address", &format!("0x{:x}", device.address));
                out.string("name", &device.name);
                out.end('}');
            }
            out.end(']');
        }
        out.end('}');
        return finish(out, flags);
    }
    let mut text = String::from("mice:\n");
    for mouse in &devices.mice {
        let _ = writeln!(text, "\tMouse at {:x}:\n\t\t{}", mouse.address, mouse.name);
    }
    let _ = write!(text, "\n\nKeyboards:\n");
    for keyboard in &devices.keyboards {
        let _ = writeln!(
            text,
            "\tKeyboard at {:x}:\n\t\t{}\n\t\t\trules: r \"{}\", m \"{}\", l \"{}\", v \"{}\", o \
             \"{}\"\n\t\t\tactive layout index: {}\n\t\t\tactive keymap: \
             {}\n\t\t\tcapsLock: {}\n\t\t\tnumLock: {}\n\t\t\tmain: {}",
            keyboard.device.address,
            keyboard.device.name,
            keyboard.rules,
            keyboard.model,
            keyboard.layout,
            keyboard.variant,
            keyboard.options,
            keyboard.active_layout_index,
            keyboard.active_keymap,
            yes_no(keyboard.caps_lock),
            yes_no(keyboard.num_lock),
            yes_no(keyboard.main),
        );
    }
    for (heading, group, what) in [
        ("Tablets", &devices.tablets, "Tablet"),
        ("Touch", &devices.touch, "Touch Device"),
        ("Switches", &devices.switches, "Switch Device"),
    ] {
        let _ = write!(text, "\n\n{heading}:\n");
        for device in group {
            let _ = writeln!(
                text,
                "\t{what} at {:x}:\n\t\t{}",
                device.address, device.name
            );
        }
    }
    text
}

/// `hyprctl layers`: every layer surface, by monitor and then by level.
fn layers(flags: Flags, snapshot: &Snapshot) -> String {
    /// `zwlr_layer_shell_v1`'s four, in its order.
    const LEVELS: [&str; 4] = ["background", "bottom", "top", "overlay"];
    let monitors: Vec<&str> = snapshot
        .monitors
        .iter()
        .map(|monitor| monitor.name.as_str())
        .collect();
    fn on<'a>(snapshot: &'a Snapshot, monitor: &'a str, level: u32) -> Vec<&'a Layer> {
        snapshot
            .layers
            .iter()
            .filter(|layer| layer.monitor == monitor && layer.level == level)
            .collect()
    }
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.object();
        for monitor in &monitors {
            out.object_field(monitor);
            out.object_field("levels");
            for level in 0..4u32 {
                out.array_field(&level.to_string());
                for layer in on(snapshot, monitor, level) {
                    out.object();
                    out.string("address", &format!("0x{:x}", layer.address));
                    out.number("x", i64::from(layer.at.0));
                    out.number("y", i64::from(layer.at.1));
                    out.number("w", i64::from(layer.size.0));
                    out.number("h", i64::from(layer.size.1));
                    out.string("namespace", &layer.namespace);
                    out.number("pid", i64::from(layer.pid));
                    out.end('}');
                }
                out.end(']');
            }
            out.end('}');
            out.end('}');
        }
        out.end('}');
        return finish(out, flags);
    }
    let mut text = String::new();
    for monitor in &monitors {
        let _ = writeln!(text, "Monitor {monitor}:");
        for (level, name) in LEVELS.iter().enumerate() {
            let level = u32::try_from(level).unwrap_or(0);
            let _ = writeln!(text, "\tLayer level {level} ({name}):");
            for layer in on(snapshot, monitor, level) {
                let _ = writeln!(
                    text,
                    "\t\tLayer {:x}: xywh: {} {} {} {}, namespace: {}, pid: {}",
                    layer.address,
                    layer.at.0,
                    layer.at.1,
                    layer.size.0,
                    layer.size.1,
                    layer.namespace,
                    layer.pid,
                );
            }
        }
        let _ = write!(text, "\n\n");
    }
    text
}

/// `hyprctl cursorpos`: where the pointer is.
fn cursor_position(flags: Flags, at: (i32, i32)) -> String {
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.object();
        out.number("x", i64::from(at.0));
        out.number("y", i64::from(at.1));
        out.end('}');
        return finish(out, flags);
    }
    format!("{}, {}\n", at.0, at.1)
}

/// `yes` or `no`, which is what the readable form prints for a flag.
const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// `hyprctl plugin list`, in Hyprland's own shape.
fn plugins(flags: Flags, plugins: &[Plugin]) -> String {
    if flags.format() == Format::Json {
        let mut out = Json::new(pretty(flags));
        out.array();
        for plugin in plugins {
            out.object();
            out.string("name", &plugin.name);
            out.string("author", &plugin.author);
            out.string("handle", &format!("{:x}", plugin.handle));
            out.string("version", &plugin.version);
            out.string("description", &plugin.description);
            out.end('}');
        }
        out.end(']');
        return finish(out, flags);
    }
    if plugins.is_empty() {
        return "no plugins loaded\n".to_owned();
    }
    let mut text = String::new();
    for plugin in plugins {
        let _ = writeln!(
            text,
            "\nPlugin {} by {}:\n\tHandle: {:x}\n\tVersion: {}\n\tDescription: {}",
            plugin.name, plugin.author, plugin.handle, plugin.version, plugin.description
        );
        // What Hyprland has no line for, because its plugins add their
        // dispatchers to the compositor's own table and this one's keep
        // them: the dispatchers a `dispatch` is handed to this plugin for.
        if !plugin.dispatchers.is_empty() {
            let _ = writeln!(text, "\tDispatchers: {}", plugin.dispatchers.join(", "));
        }
    }
    text
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
            // A monitor showing no scratchpad has id 0 and an empty name,
            // which is what Hyprland writes for one.
            out.object_field("specialWorkspace");
            out.number(
                "id",
                i64::from(monitor.special_workspace.as_ref().map_or(0, |(id, _)| *id)),
            );
            out.string(
                "name",
                monitor
                    .special_workspace
                    .as_ref()
                    .map_or("", |(_, name)| name.as_str()),
            );
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
            "Monitor {} (ID {}):\n\t{}x{}@{:.5} at {}x{}\n\tactive workspace: {} ({})\n\tspecial workspace: {} ({})\n\tscale: {:.2}\n\tfocused: {}\n",
            monitor.name,
            monitor.id,
            monitor.width,
            monitor.height,
            monitor.refresh,
            monitor.at.0,
            monitor.at.1,
            monitor.active_workspace,
            monitor.active_workspace_name,
            monitor.special_workspace.as_ref().map_or(0, |(id, _)| *id),
            monitor
                .special_workspace
                .as_ref()
                .map_or("", |(_, name)| name.as_str()),
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
    out.boolean("hidden", window.hidden);
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
    if window.grouped.is_empty() {
        out.empty_array("grouped");
    } else {
        out.array_field("grouped");
        for member in &window.grouped {
            out.item(&format!("0x{member:x}"));
        }
        out.end(']');
    }
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
    let _ = writeln!(text, "\thidden: {}", i32::from(window.hidden));
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
    // Hyprland prints a lone `0` for a window in no group, and the members'
    // addresses without `0x` for one in a group.
    let _ = writeln!(text, "\tgrouped: {}", grouped(&window.grouped));
    let _ = writeln!(text, "\tfocusHistoryID: {}\n", window.focus_history);
    text
}

/// The readable form's `grouped:` value: `0` for no group.
fn grouped(members: &[u64]) -> String {
    if members.is_empty() {
        return "0".to_owned();
    }
    members
        .iter()
        .map(|member| format!("{member:x}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Add the trailing newline unless the request asked for none.
fn finish(out: Json, flags: Flags) -> String {
    let mut text = out.finish();
    if !flags.no_newline {
        text.push('\n');
    }
    text
}
