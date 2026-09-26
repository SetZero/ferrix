//! The modules: what each shows, where it gets it, and what a click does.
//!
//! Each module is a state machine driven by the bar's loop. It asks the
//! loop, through [`Host`], for what it needs -- a child process, a timer, a
//! file's text, a Hyprland request -- and is told what came of it; after
//! every event it has a [`ModuleView`] the bar draws. So a module is tested
//! with a fake host and no compositor.
//!
//! What every module has in common is waybar's `AModule` and `ALabel`
//! ([`Common`]): `format`, `tooltip`, `tooltip-format`, `max-length`,
//! `on-click` and the other buttons, `on-scroll-*`, `format-icons` and
//! `states`.

pub mod clock;
pub mod custom;
pub mod hyprland;
pub mod network;
pub mod pulseaudio;
pub mod system;
pub mod tray;

use std::time::Duration;

use crate::diag::Diagnostics;
use crate::json::Value;
use crate::view::{ModuleView, Shape};

/// A child the host started, by its key.
pub type ChildKey = u64;
/// A timer the host set, by its key.
pub type TimerKey = u64;

/// What a module can ask of the bar's loop.
pub trait Host {
    /// Start `/bin/sh -c command` with `WAYBAR_OUTPUT_NAME` set; its
    /// output is handed back a line at a time if `lines`, else whole at
    /// exit. `None` if it could not be started.
    fn run(&mut self, command: &str, lines: bool) -> Option<ChildKey>;
    /// Signal a child's process group with `SIGTERM`.
    fn kill(&mut self, child: ChildKey);
    /// Start `/bin/sh -c command` and forget it: a click's command.
    fn spawn(&mut self, command: &str);
    /// A timer due once after `after`.
    fn timer(&mut self, after: Duration) -> TimerKey;
    /// Forget a timer.
    fn cancel(&mut self, timer: TimerKey);
    /// A file's text, `None` if it cannot be read.
    fn read(&mut self, path: &str) -> Option<String>;
    /// Seconds since some fixed point, for rates.
    fn now(&self) -> f64;
    /// The seconds since the epoch and the local offset from UTC in
    /// seconds, for the clock.
    fn wall(&self) -> (i64, i64);
    /// A request to Hyprland's `.socket.sock`, and its answer.
    fn hyprland(&mut self, request: &str) -> Option<String>;
    /// The environment variable `name`.
    fn var(&self, name: &str) -> Option<String>;
    /// Where to say things.
    fn diag(&mut self) -> &mut Diagnostics;
    /// The `ifreq` ioctls' answers for an interface.
    fn interface(&mut self, name: &str) -> Option<network::Interface>;
    /// `SIGRTMIN` as this program's C library has it (glibc 34, musl 35),
    /// which is what `signal: N` counts from and what `pkill -RTMIN+N`
    /// from the same system means.
    fn sigrtmin(&self) -> i32;
}

/// A mouse button as GDK numbers them: 1 left, 2 middle, 3 right, 8 back,
/// 9 forward.
pub type Button = u32;

/// How a button event came, as GDK's event types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// `GDK_BUTTON_PRESS`.
    Single,
    /// `GDK_2BUTTON_PRESS`, after a second press within the double-click
    /// time.
    Double,
    /// `GDK_3BUTTON_PRESS`.
    Triple,
    /// `GDK_BUTTON_RELEASE`.
    Release,
}

/// A scroll's direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    /// Up.
    Up,
    /// Down.
    Down,
    /// Left.
    Left,
    /// Right.
    Right,
}

/// The evdev button codes the compositor sends, as GDK's numbers.
#[must_use]
pub fn gdk_button(evdev: u32) -> Option<Button> {
    match evdev {
        0x110 => Some(1),
        0x111 => Some(3),
        0x112 => Some(2),
        0x113 => Some(8),
        0x114 => Some(9),
        _ => None,
    }
}

/// The config key a button event runs, as `AModule::eventMap_` names them.
#[must_use]
pub fn event_name(button: Button, press: Press) -> Option<&'static str> {
    Some(match (button, press) {
        (1, Press::Single) => "on-click",
        (1, Press::Release) => "on-click-release",
        (1, Press::Double) => "on-double-click",
        (1, Press::Triple) => "on-triple-click",
        (2, Press::Single) => "on-click-middle",
        (2, Press::Release) => "on-click-middle-release",
        (2, Press::Double) => "on-double-click-middle",
        (2, Press::Triple) => "on-triple-click-middle",
        (3, Press::Single) => "on-click-right",
        (3, Press::Release) => "on-click-right-release",
        (3, Press::Double) => "on-double-click-right",
        (3, Press::Triple) => "on-triple-click-right",
        (8, Press::Single) => "on-click-backward",
        (8, Press::Release) => "on-click-backward-release",
        (8, Press::Double) => "on-double-click-backward",
        (8, Press::Triple) => "on-triple-click-backward",
        (9, Press::Single) => "on-click-forward",
        (9, Press::Release) => "on-click-forward-release",
        (9, Press::Double) => "on-double-click-forward",
        (9, Press::Triple) => "on-triple-click-forward",
        _ => return None,
    })
}

/// The icon of the first `{"icon", "max"}` threshold `percentage` is at or
/// under, or the last one.
fn threshold_icon(list: &[Value], percentage: u16) -> String {
    let valid = list
        .iter()
        .filter(|t| t.get("icon").is_string() && t.get("max").is_uint());
    let mut last = String::new();
    for threshold in valid {
        last = threshold.get("icon").as_string();
        if i64::from(percentage) <= threshold.get("max").as_i64().unwrap_or(0) {
            break;
        }
    }
    last
}

/// What `AModule` and `ALabel` read for every module.
#[derive(Clone, Debug, PartialEq)]
pub struct Common {
    /// The module's config block.
    pub config: Value,
    /// Its name in the module list: `custom/ws-1`, `cpu#two`.
    pub name: String,
    /// The part after `#`, a class on the named node.
    pub id: String,
    /// `format`, or the module's default.
    pub format: String,
    /// The default format, for `format-alt` to toggle back to.
    pub default_format: String,
    /// Whether `format-alt` is showing.
    pub alt: bool,
    /// `tooltip`, true unless false.
    pub tooltip: bool,
    /// `interval` in seconds; `None` for `"once"`, `Some(0.0)` for none.
    pub interval: Option<f64>,
}

impl Common {
    /// Read the common options of `config`, for module `name` whose default
    /// format is `format` and default interval `interval` seconds.
    #[must_use]
    pub fn new(name: &str, config: &Value, format: &str, interval: u32) -> Self {
        let id = name
            .split_once('#')
            .map(|(_, id)| id.to_owned())
            .unwrap_or_default();
        let format = config.get("format").as_str().unwrap_or(format).to_owned();
        let interval = match config.get("interval") {
            value if value.is("once") => None,
            value if value.is_numeric() => {
                let seconds = value.as_f64().unwrap_or(0.0);
                Some(if seconds > 0.0 {
                    (seconds * 1000.0).max(1.0).floor() / 1000.0
                } else if interval == 0 {
                    0.0
                } else {
                    f64::from(interval)
                })
            }
            _ => Some(f64::from(interval)),
        };
        Self {
            config: config.clone(),
            name: name.to_owned(),
            id,
            default_format: format.clone(),
            format,
            alt: false,
            tooltip: config.get("tooltip").as_bool() || !config.get("tooltip").is_bool(),
            interval,
        }
    }

    /// A view with the module's widget name, the `#id` class and `module`,
    /// and `max-length`.
    #[must_use]
    pub fn view(&self, widget: &str, shape: Shape) -> ModuleView {
        let mut view = ModuleView::new(widget, shape);
        if !self.id.is_empty() {
            view.classes.push(self.id.clone());
        }
        if self.config.get("max-length").is_uint() {
            view.max_chars = self
                .config
                .get("max-length")
                .as_i64()
                .and_then(|n| u32::try_from(n).ok());
            view.ellipsize = true;
        }
        view
    }

    /// `resolveTooltipFormat`: `tooltip-format-<state>`, `tooltip-format`,
    /// or `default`.
    #[must_use]
    pub fn tooltip_format(&self, default: &str, state: &str) -> String {
        if !state.is_empty()
            && let Some(format) = self.config.get(&format!("tooltip-format-{state}")).as_str()
        {
            return format.to_owned();
        }
        self.config
            .get("tooltip-format")
            .as_str()
            .unwrap_or(default)
            .to_owned()
    }

    /// `ALabel::getIcon(percentage, alts)`: the icon `format-icons` gives a
    /// percentage, from the first of `alts` it has a list for.
    #[must_use]
    pub fn icon(&self, percentage: u16, alts: &[&str], max: u16) -> String {
        let mut icons = self.config.get("format-icons");
        if icons.is_object() {
            let key = alts
                .iter()
                .find(|alt| {
                    !alt.is_empty() && {
                        let entry = icons.get(alt);
                        entry.is_string() || entry.is_array()
                    }
                })
                .copied()
                .unwrap_or("default");
            icons = icons.get(key);
        }
        if let Value::Array(list) = icons {
            if list.first().is_some_and(Value::is_object) {
                return threshold_icon(list, percentage);
            }
            if !list.is_empty() {
                let size = u32::try_from(list.len()).unwrap_or(1);
                let max = if max == 0 { 100 } else { u32::from(max) };
                let divisor = (max / size).max(1);
                let index = (u32::from(percentage) / divisor).min(size - 1);
                return list
                    .get(usize::try_from(index).unwrap_or(0))
                    .map(Value::as_string)
                    .unwrap_or_default();
            }
            return String::new();
        }
        icons.as_str().map(str::to_owned).unwrap_or_default()
    }

    /// `ALabel::getState`: the name of the `states` threshold `value` is
    /// past (at or above, or at or below when `lesser`), and the classes to
    /// set and clear on the label for it.
    #[must_use]
    pub fn state(&self, value: u8, lesser: bool) -> (String, Vec<String>) {
        let mut states: Vec<(String, i64)> = self
            .config
            .get("states")
            .members()
            .iter()
            .filter(|(_, v)| v.is_uint())
            .map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0)))
            .collect();
        if lesser {
            states.sort_by_key(|(_, v)| *v);
        } else {
            states.sort_by_key(|(_, v)| core::cmp::Reverse(*v));
        }
        let value = i64::from(value);
        let found = states
            .iter()
            .find(|(_, threshold)| {
                if lesser {
                    value <= *threshold
                } else {
                    value >= *threshold
                }
            })
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        let all = states.into_iter().map(|(name, _)| name).collect();
        (found, all)
    }

    /// The command a button event runs, if the module has one.
    #[must_use]
    pub fn click_command(&self, button: Button, press: Press) -> Option<String> {
        let name = event_name(button, press)?;
        self.config.get(name).as_str().map(str::to_owned)
    }

    /// `format-alt` toggled by `format-alt-click` (button 1 unless it says
    /// `click-right`, `click-middle`, `click-backward` or `click-forward`).
    pub fn toggle_alt(&mut self, button: Button) -> bool {
        let Some(alt) = self.config.get("format-alt").as_str().map(str::to_owned) else {
            return false;
        };
        let wanted = match self.config.get("format-alt-click") {
            value if value.is("click-right") => 3,
            value if value.is("click-middle") => 2,
            value if value.is("click-backward") => 8,
            value if value.is("click-forward") => 9,
            value if value.is_uint() => u32::try_from(value.as_i64().unwrap_or(1)).unwrap_or(1),
            _ => 1,
        };
        if button != wanted {
            return false;
        }
        self.alt = !self.alt;
        self.format = if self.alt {
            alt
        } else {
            self.default_format.clone()
        };
        true
    }

    /// Run the command a button event names, as `AModule::handleUserEvent`
    /// does: `{x}` and `{y}` are the click's place in percent of the
    /// module, when the command has them.
    pub fn run_click(&mut self, host: &mut dyn Host, button: Button, press: Press, at: (f32, f32)) {
        if press != Press::Release {
            let _ = self.toggle_alt(button);
        }
        let Some(command) = self.click_command(button, press) else {
            return;
        };
        let command = if command.contains("{x}") || command.contains("{y}") {
            let args = crate::fmt::Args::new()
                .named("x", (at.0 * 100.0).round() as i64)
                .named("y", (at.1 * 100.0).round() as i64);
            crate::fmt::format(&command, &args).unwrap_or_else(|error| {
                host.diag().warn(format!(
                    "Failed to format command '{command}': {error}. Running it unformatted."
                ));
                command.clone()
            })
        } else {
            command
        };
        host.spawn(&command);
    }

    /// Run the command for a scroll, as `AModule::handleScroll` does.
    pub fn run_scroll(&self, host: &mut dyn Host, scroll: Scroll) {
        let name = match scroll {
            Scroll::Up => "on-scroll-up",
            Scroll::Down => "on-scroll-down",
            Scroll::Left => "on-scroll-left",
            Scroll::Right => "on-scroll-right",
        };
        if let Some(command) = self.config.get(name).as_str() {
            host.spawn(command);
        }
    }

    /// Whether any button or scroll does something: the pointer becomes a
    /// hand over the module (`hasUserEvents_`).
    #[must_use]
    pub fn clickable(&self) -> bool {
        self.config.members().iter().any(|(key, value)| {
            key.starts_with("on-")
                && !key.ends_with("-release")
                && !key.starts_with("on-scroll")
                && key != "on-update"
                && (value.is_string() || value.is_bool())
        }) || self.config.has("format-alt")
    }
}

/// What a module is, to the bar.
pub trait Module {
    /// Start: the first update, and whatever it runs.
    fn start(&mut self, host: &mut dyn Host);
    /// What it shows now.
    fn view(&self) -> &ModuleView;
    /// Classes it puts on `window#waybar` (set, clear); the window module's
    /// `empty`, `solo` and the rest.
    fn window_classes(&self) -> Vec<(String, bool)> {
        Vec::new()
    }
    /// The common options.
    fn common(&mut self) -> &mut Common;
    /// A child's line. Answers whether the view changed.
    fn child_line(&mut self, _host: &mut dyn Host, _child: ChildKey, _line: &str) -> bool {
        false
    }
    /// A child's exit.
    fn child_exit(
        &mut self,
        _host: &mut dyn Host,
        _child: ChildKey,
        _status: Option<i32>,
        _output: &str,
    ) -> bool {
        false
    }
    /// A timer.
    fn timer(&mut self, _host: &mut dyn Host, _timer: TimerKey) -> bool {
        false
    }
    /// A signal the bar was sent.
    fn signal(&mut self, _host: &mut dyn Host, _signal: i32) -> bool {
        false
    }
    /// A line of Hyprland's event socket: `name>>data`.
    fn hyprland_event(&mut self, _host: &mut dyn Host, _name: &str, _data: &str) -> bool {
        false
    }
    /// A button. `at` is where, as fractions of the module's size.
    fn click(&mut self, host: &mut dyn Host, button: Button, press: Press, at: (f32, f32)) -> bool {
        let common = self.common();
        let before = common.format.clone();
        common.run_click(host, button, press, at);
        let changed = common.format != before;
        if changed {
            self.start(host);
        }
        changed
    }
    /// A scroll.
    fn scroll(&mut self, host: &mut dyn Host, scroll: Scroll) -> bool {
        self.common().run_scroll(host, scroll);
        false
    }
    /// Whether the pointer should be a hand over it.
    fn clickable(&mut self) -> bool {
        self.common().clickable()
    }
}

/// Make the module `name` from its config block, as waybar's factory
/// does, or say why not.
///
/// # Errors
///
/// The factory's own message: `Unknown module: name`, or one the module
/// gave.
pub fn make(
    name: &str,
    config: &Value,
    output: &str,
    host: &mut dyn Host,
) -> Result<Box<dyn Module>, String> {
    let reference = name.split_once('#').map_or(name, |(r, _)| r);
    let module: Box<dyn Module> = if let Some(custom) = reference.strip_prefix("custom/") {
        Box::new(custom::Custom::new(name, custom, config, output, host))
    } else {
        match reference {
            "cpu" => Box::new(system::Cpu::new(name, config)),
            "memory" => Box::new(system::Memory::new(name, config)),
            "network" => Box::new(network::Network::new(name, config, host)),
            "clock" => Box::new(clock::Clock::new(name, config, host)),
            "hyprland/window" => Box::new(hyprland::Window::new(name, config, output)),
            "pulseaudio" => Box::new(pulseaudio::Pulseaudio::new(name, config, host)),
            "tray" => Box::new(tray::Tray::new(name, config, host)),
            _ => return Err(format!("Unknown module: {name}")),
        }
    };
    Ok(module)
}

/// `Glib::Markup::escape_text`.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
pub mod fake {
    //! A host for tests: it records what it was asked and answers from maps.

    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::{ChildKey, Host, TimerKey};
    use crate::diag::Diagnostics;

    /// What a test's modules asked for.
    #[derive(Debug, Default)]
    pub struct Fake {
        /// Children started: key, command, lines.
        pub runs: Vec<(ChildKey, String, bool)>,
        /// Children killed.
        pub killed: Vec<ChildKey>,
        /// Commands spawned.
        pub spawned: Vec<String>,
        /// Timers set, by key, with their delay.
        pub timers: BTreeMap<TimerKey, Duration>,
        /// Files.
        pub files: BTreeMap<String, String>,
        /// Hyprland's answers, by request.
        pub hyprland: BTreeMap<String, String>,
        /// Environment.
        pub vars: BTreeMap<String, String>,
        /// The clock.
        pub now: f64,
        /// The wall clock and offset.
        pub wall: (i64, i64),
        /// Interfaces.
        pub interfaces: BTreeMap<String, crate::modules::network::Interface>,
        /// What was said.
        pub diag: Diagnostics,
        next: u64,
    }

    impl Host for Fake {
        fn run(&mut self, command: &str, lines: bool) -> Option<ChildKey> {
            self.next += 1;
            self.runs.push((self.next, command.to_owned(), lines));
            Some(self.next)
        }
        fn kill(&mut self, child: ChildKey) {
            self.killed.push(child);
        }
        fn spawn(&mut self, command: &str) {
            self.spawned.push(command.to_owned());
        }
        fn timer(&mut self, after: Duration) -> TimerKey {
            self.next += 1;
            let _ = self.timers.insert(self.next, after);
            self.next
        }
        fn cancel(&mut self, timer: TimerKey) {
            let _ = self.timers.remove(&timer);
        }
        fn read(&mut self, path: &str) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn now(&self) -> f64 {
            self.now
        }
        fn wall(&self) -> (i64, i64) {
            self.wall
        }
        fn hyprland(&mut self, request: &str) -> Option<String> {
            self.hyprland.get(request).cloned()
        }
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }
        fn diag(&mut self) -> &mut Diagnostics {
            &mut self.diag
        }
        fn sigrtmin(&self) -> i32 {
            34
        }
        fn interface(&mut self, name: &str) -> Option<crate::modules::network::Interface> {
            self.interfaces.get(name).copied()
        }
    }
}
