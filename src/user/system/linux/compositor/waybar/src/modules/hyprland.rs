//! `hyprland/window`: the focused window's title (`modules/hyprland/
//! window.cpp`), over Hyprland's IPC, which hyprix speaks.
//!
//! Hyprland's sockets are `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`
//! `.socket.sock` (a request per connection: `j/monitors` answers JSON) and
//! `.socket2.sock` (a line per event: `activewindow>>class,title`), with
//! `/tmp/hypr` when `$XDG_RUNTIME_DIR/hypr` is not there. hyprix makes both
//! and hands its children the signature (`src/user/system/linux/compositor/hyprix/src/control.rs`).
//!
//! The module listens for `activewindow`, `closewindow`, `movewindow`,
//! `changefloatingmode` and `fullscreen`, and on each asks three things,
//! as waybar does: `j/monitors` for the focused monitor's special or else
//! active workspace (the named output's with `separate-outputs`),
//! `j/workspaces` for that workspace's window count and last window, and
//! `j/clients` for that window. The title is `lastwindowtitle`, markup
//! escaped; `format` gets `{title}`, `{initialTitle}`, `{class}` and
//! `{initialClass}`. It sets `empty` on `window#waybar` while the workspace
//! has no windows -- which the user's style collapses the chip on --, and
//! `solo`, `floating`, `swallowing`, `fullscreen` and the solo window's
//! class name.
//!
//! `rewrite` is not carried out: waybar's rules are C++ `std::regex`
//! replacements with `$1` groups, and the tree's regex crate matches only.

use super::{Common, Host, Module};
use crate::fmt::{Args, format};
use crate::json::{self, Value};
use crate::view::{ModuleView, Shape};

/// The events the module updates on.
pub const EVENTS: [&str; 5] = [
    "activewindow",
    "closewindow",
    "movewindow",
    "changefloatingmode",
    "fullscreen",
];

/// The directory Hyprland's sockets are in, `getSocketFolder`: under
/// `$XDG_RUNTIME_DIR/hypr` if that exists, else `/tmp/hypr`.
#[must_use]
pub fn socket_folder(
    runtime: Option<&str>,
    signature: &str,
    exists: &dyn Fn(&str) -> bool,
) -> String {
    match runtime {
        Some(dir) if !dir.is_empty() && exists(&format!("{dir}/hypr")) => {
            format!("{dir}/hypr/{signature}")
        }
        _ => format!("/tmp/hypr/{signature}"),
    }
}

/// `sanitize_string`: `& < > " '` as entities.
#[must_use]
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The `hyprland/window` module.
#[derive(Debug)]
pub struct Window {
    common: Common,
    output: String,
    classes: Vec<(String, bool)>,
    solo_class: String,
    view: ModuleView,
}

/// What `queryActiveWorkspace` found.
#[derive(Debug, Default)]
struct Found {
    windows: i64,
    title: String,
    class: String,
    initial_class: String,
    initial_title: String,
    solo: bool,
    all_floating: bool,
    swallowing: bool,
    fullscreen: bool,
}

impl Window {
    /// Make it, for the bar on `output`.
    #[must_use]
    pub fn new(name: &str, config: &Value, output: &str) -> Self {
        let common = Common::new(name, config, "{title}", 0);
        let mut view = common.view("window", Shape::IconLabel);
        view.ellipsize = true;
        Self {
            common,
            output: output.to_owned(),
            classes: Vec::new(),
            solo_class: String::new(),
            view,
        }
    }

    fn ask(host: &mut dyn Host, request: &str) -> Value {
        host.hyprland(&format!("j/{request}"))
            .and_then(|text| json::parse(&text).ok())
            .unwrap_or(Value::Null)
    }

    fn query(&self, host: &mut dyn Host) -> Found {
        let mut found = Found::default();
        let monitors = Self::ask(host, "monitors");
        let separate = self.common.config.get("separate-outputs").as_bool();
        let monitor = monitors.items().iter().find(|monitor| {
            if separate {
                monitor.get("name").is(&self.output)
            } else {
                monitor.get("focused").as_bool()
            }
        });
        let Some(monitor) = monitor else {
            if monitors.is_array() {
                host.diag().warn(format!(
                    "Monitor not found: {}",
                    if separate { self.output.as_str() } else { "" }
                ));
            }
            return found;
        };
        let special = monitor
            .get("specialWorkspace")
            .get("id")
            .as_i64()
            .unwrap_or(0);
        let id = if special != 0 {
            special
        } else {
            monitor
                .get("activeWorkspace")
                .get("id")
                .as_i64()
                .unwrap_or(0)
        };
        let workspaces = Self::ask(host, "workspaces");
        let Some(workspace) = workspaces
            .items()
            .iter()
            .find(|w| w.get("id").as_i64() == Some(id))
        else {
            host.diag().warn(format!("No workspace with id {id}"));
            return found;
        };
        found.windows = workspace.get("windows").as_i64().unwrap_or(0);
        found.title = workspace.get("lastwindowtitle").as_string();
        let last = workspace.get("lastwindow").as_string();
        if found.windows <= 0 {
            return found;
        }
        let clients = Self::ask(host, "clients");
        let Some(active) = clients.items().iter().find(|c| c.get("address").is(&last)) else {
            return found;
        };
        found.class = active.get("class").as_string();
        found.initial_class = active.get("initialClass").as_string();
        found.initial_title = active.get("initialTitle").as_string();
        let on_workspace: Vec<&Value> = clients
            .items()
            .iter()
            .filter(|c| {
                c.get("workspace").get("id").as_i64() == Some(id) && c.get("mapped").as_bool()
            })
            .collect();
        found.swallowing = on_workspace.iter().any(|c| {
            let swallowing = c.get("swallowing");
            !swallowing.is_null() && !swallowing.is("0x0")
        });
        let visible: Vec<&&Value> = on_workspace
            .iter()
            .filter(|c| !c.get("hidden").as_bool())
            .collect();
        found.solo = visible
            .iter()
            .filter(|c| !c.get("floating").as_bool())
            .count()
            == 1;
        found.all_floating = visible.iter().all(|c| c.get("floating").as_bool());
        found.fullscreen = active.get("fullscreen").as_bool()
            || active.get("fullscreen").as_i64().is_some_and(|n| n != 0);
        if found.fullscreen {
            found.solo = true;
        }
        found
    }

    fn update(&mut self, host: &mut dyn Host) {
        if self.common.config.has("rewrite") {
            host.diag().once(
                "window-rewrite",
                "hyprland/window: \"rewrite\" is not carried out: its rules are regex replacements, and this waybar only matches".to_owned(),
            );
        }
        let found = self.query(host);
        let title = sanitize(&found.title);
        let shown_title = if title.is_empty() {
            self.common
                .config
                .get("fallback")
                .as_str()
                .map_or_else(String::new, str::to_owned)
        } else {
            title.clone()
        };
        let mut label = String::new();
        if self.common.format.is_empty() {
            self.view.label_visible = false;
        } else {
            self.view.label_visible = true;
            let args = Args::new()
                .named("title", shown_title.as_str())
                .named("initialTitle", found.initial_title.as_str())
                .named("class", found.class.as_str())
                .named("initialClass", found.initial_class.as_str());
            match format(&self.common.format, &args) {
                Ok(text) => label = text,
                Err(error) => host.diag().error(format!("hyprland/window: {error}")),
            }
            self.view.markup.clone_from(&label);
        }
        if self.common.tooltip {
            if let Some(tooltip) = self.common.config.get("tooltip-format").as_str() {
                let args = Args::new()
                    .named("title", title.as_str())
                    .named("initialTitle", found.initial_title.as_str())
                    .named("class", found.class.as_str())
                    .named("initialClass", found.initial_class.as_str());
                self.view.tooltip = format(tooltip, &args).ok();
            } else if !label.is_empty() {
                self.view.tooltip = Some(label);
            }
        }
        let mut classes = vec![
            ("empty".to_owned(), found.windows == 0),
            ("solo".to_owned(), found.solo),
            (
                "floating".to_owned(),
                found.all_floating && found.windows > 0,
            ),
            ("swallowing".to_owned(), found.swallowing),
            ("fullscreen".to_owned(), found.fullscreen),
        ];
        let solo_class = if found.solo {
            found.class.clone()
        } else {
            String::new()
        };
        if !self.solo_class.is_empty() && self.solo_class != solo_class {
            classes.push((self.solo_class.clone(), false));
        }
        if !solo_class.is_empty() {
            classes.push((solo_class.clone(), true));
        }
        self.solo_class = solo_class;
        self.classes = classes;
    }
}

impl Module for Window {
    fn start(&mut self, host: &mut dyn Host) {
        self.update(host);
    }

    fn view(&self) -> &ModuleView {
        &self.view
    }

    fn common(&mut self) -> &mut Common {
        &mut self.common
    }

    fn window_classes(&self) -> Vec<(String, bool)> {
        self.classes.clone()
    }

    fn hyprland_event(&mut self, host: &mut dyn Host, name: &str, _data: &str) -> bool {
        if EVENTS.contains(&name) {
            self.update(host);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::super::Module;
    use super::super::fake::Fake;
    use super::{Window, socket_folder};
    use crate::json::parse;

    fn host(windows: u32) -> Fake {
        let mut host = Fake::default();
        let _ = host.hyprland.insert(
            "j/monitors".into(),
            r#"[{"name": "Virtual-1", "focused": true, "activeWorkspace": {"id": 1}, "specialWorkspace": {"id": 0}}]"#.into(),
        );
        let _ = host.hyprland.insert(
            "j/workspaces".into(),
            format!(r#"[{{"id": 1, "windows": {windows}, "lastwindow": "0x1", "lastwindowtitle": "~ & zsh <1>"}}]"#),
        );
        let _ = host.hyprland.insert(
            "j/clients".into(),
            r#"[{"address": "0x1", "mapped": true, "hidden": false, "floating": false, "workspace": {"id": 1}, "class": "term", "fullscreen": 0}]"#.into(),
        );
        host
    }

    #[test]
    fn the_title_is_the_focused_workspaces_last_window() {
        let mut host = host(1);
        let config = parse(r#"{"format": "{title}", "separate-outputs": false, "max-length": 44, "tooltip-format": "  {title}  "}"#)
            .unwrap_or(crate::json::Value::Null);
        let mut window = Window::new("hyprland/window", &config, "Virtual-1");
        window.start(&mut host);
        assert_eq!(window.view().markup, "~ &amp; zsh &lt;1&gt;");
        assert_eq!(
            window.view().tooltip.as_deref(),
            Some("  ~ &amp; zsh &lt;1&gt;  ")
        );
        assert_eq!(window.view().max_chars, Some(44));
        let classes = window.window_classes();
        assert!(classes.contains(&("empty".to_owned(), false)));
        assert!(classes.contains(&("solo".to_owned(), true)));
        assert!(classes.contains(&("term".to_owned(), true)));
    }

    #[test]
    fn no_windows_is_empty_on_the_bar() {
        let mut host = host(0);
        let config = parse("{}").unwrap_or(crate::json::Value::Null);
        let mut window = Window::new("hyprland/window", &config, "Virtual-1");
        window.start(&mut host);
        assert!(
            window
                .window_classes()
                .contains(&("empty".to_owned(), true))
        );
        assert!(window.hyprland_event(&mut host, "activewindow", "term,zsh"));
        assert!(!window.hyprland_event(&mut host, "workspace", "2"));
    }

    #[test]
    fn the_sockets_fall_back_to_tmp() {
        assert_eq!(
            socket_folder(Some("/run/user/0"), "abc", &|_| true),
            "/run/user/0/hypr/abc"
        );
        assert_eq!(
            socket_folder(Some("/run/user/0"), "abc", &|_| false),
            "/tmp/hypr/abc"
        );
        assert_eq!(socket_folder(None, "abc", &|_| true), "/tmp/hypr/abc");
    }
}
