//! `custom/<name>`: a script's output as a label (`modules/custom.cpp`).
//!
//! How the script runs is picked once, from the config:
//!
//! * **waiting**: `signal` set and neither `interval` nor
//!   `restart-interval`: `exec` runs at start, and again for each
//!   `SIGRTMIN+signal` or click;
//! * **delayed**: an `interval` (seconds, or `"once"`, which is forever):
//!   `exec` runs at start and then every interval; a signal or a click runs
//!   it early;
//! * **continuous**: `exec` with neither: it runs once and every line it
//!   prints is the new output, and when it exits it is restarted after
//!   `restart-interval`, if there is one.
//!
//! `exec-if` runs first, each time, and a non-zero exit is an empty output.
//!
//! The module is hidden while it has an `exec` (or `exec-if`) and its last
//! output was empty or its exit non-zero -- which is how the user's
//! `custom/ws-N` modules hide a desktop that does not exist, and why every
//! one of them is hidden on Ferrix, where `hypr-workspaces` is not.
//! Without `exec`, the module shows its `format` with an empty `{text}`: the
//! launcher's and logout's `" "`.
//!
//! `return-type: json` reads the first line as an object: `text`, `alt`,
//! `tooltip`, `class` (a string or an array, set on the module's box) and
//! `percentage`, which picks `{icon}` from `format-icons`.

use std::time::Duration;

use super::{ChildKey, Common, Host, Module, TimerKey, escape};
use crate::fmt::{Args, format};
use crate::json::{self, Value};
use crate::view::{ModuleView, Shape};

/// How the script is run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Worker {
    /// Run at start, on a signal and on a click.
    Waiting,
    /// Run at start and then every interval (`None`: never again).
    Delayed(Option<f64>),
    /// Run once; each line is the output.
    Continuous,
    /// No `exec`: nothing runs.
    None,
}

/// What the script last gave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Output {
    exit_code: i32,
    out: String,
}

/// A custom module.
#[derive(Debug)]
pub struct Custom {
    common: Common,
    /// The name after `custom/`.
    name: String,
    worker: Worker,
    output: Output,
    text: String,
    alt: String,
    tooltip: String,
    class: Vec<String>,
    percentage: u16,
    /// The child running `exec-if` or `exec`, and which.
    running: Option<(ChildKey, bool)>,
    /// A run was asked for while one was going.
    again: bool,
    timer: Option<TimerKey>,
    view: ModuleView,
    /// The output was parsed, and the view is waybar's; `false` before
    /// the first output arrives.
    updated: bool,
}

impl Custom {
    /// Make `custom/<name>` from its config block.
    pub fn new(full: &str, name: &str, config: &Value, _output: &str, host: &mut dyn Host) -> Self {
        let common = Common::new(full, config, "{}", 0);
        if config.is_null() {
            host.diag().warn(format!(
                "There is no configuration for 'custom/{name}', element will be hidden"
            ));
        }
        let has_signal = !config.get("signal").is_empty();
        let worker = if has_signal
            && config.get("interval").is_empty()
            && config.get("restart-interval").is_empty()
        {
            Worker::Waiting
        } else if common.interval.is_none_or(|seconds| seconds > 0.0) {
            Worker::Delayed(common.interval)
        } else if config.get("exec").is_string() {
            Worker::Continuous
        } else {
            Worker::None
        };
        let mut view = common.view(&format!("custom-{name}"), Shape::IconLabel);
        view.label_classes = vec!["flat".to_owned(), "text-button".to_owned()];
        Self {
            common,
            name: name.to_owned(),
            worker,
            output: Output::default(),
            text: String::new(),
            alt: String::new(),
            tooltip: String::new(),
            class: Vec::new(),
            percentage: 0,
            running: None,
            again: false,
            timer: None,
            view,
            updated: false,
        }
    }

    /// Which way the script is run.
    #[must_use]
    pub fn worker(&self) -> Worker {
        self.worker
    }

    fn has_exec(&self) -> bool {
        self.common.config.get("exec").is_string() || self.common.config.get("exec-if").is_string()
    }

    /// Start a run of `exec-if` then `exec`, or queue one if one is going.
    fn run(&mut self, host: &mut dyn Host) {
        if self.running.is_some() {
            self.again = true;
            return;
        }
        if let Some(condition) = self.common.config.get("exec-if").as_str() {
            self.running = host.run(condition, false).map(|child| (child, true));
            return;
        }
        if let Some(exec) = self.common.config.get("exec").as_str() {
            self.running = host
                .run(exec, self.worker == Worker::Continuous)
                .map(|child| (child, false));
        } else {
            self.update(host);
        }
    }

    fn schedule(&mut self, host: &mut dyn Host) {
        if let Worker::Delayed(Some(seconds)) = self.worker {
            if let Some(timer) = self.timer.take() {
                host.cancel(timer);
            }
            self.timer = Some(host.timer(Duration::from_secs_f64(seconds.max(0.001))));
        }
    }

    fn parse_raw(&mut self) {
        let escape_on = self.common.config.get("escape").as_bool();
        self.class.clear();
        for (index, line) in self.output.out.lines().enumerate() {
            match index {
                0 => {
                    self.text = if escape_on {
                        escape(line)
                    } else {
                        line.to_owned()
                    };
                    self.tooltip = line.to_owned();
                }
                1 => {
                    self.tooltip = if escape_on {
                        escape(line)
                    } else {
                        line.to_owned()
                    }
                }
                2 => self.class.push(line.to_owned()),
                _ => break,
            }
        }
        if self.output.out.is_empty() {
            self.text.clear();
            self.tooltip.clear();
        }
    }

    fn parse_json(&mut self, host: &mut dyn Host) -> bool {
        self.class.clear();
        let Some(line) = self.output.out.lines().next() else {
            return true;
        };
        let parsed = match json::parse(line) {
            Ok(parsed) => parsed,
            Err(error) => {
                host.diag()
                    .error(format!("{}: Error parsing JSON: {error}", self.common.name));
                return false;
            }
        };
        let escape_on = self.common.config.get("escape").as_bool();
        let field = |key: &str| {
            let value = parsed.get(key).as_string();
            if escape_on { escape(&value) } else { value }
        };
        self.text = field("text");
        self.alt = field("alt");
        self.tooltip = field("tooltip");
        match parsed.get("class") {
            Value::String(class) => self.class.push(class.clone()),
            Value::Array(classes) => self.class.extend(classes.iter().map(Value::as_string)),
            _ => {}
        }
        let percentage = parsed.get("percentage");
        self.percentage = if percentage.is_numeric() {
            percentage
                .as_f64()
                .map(|p| p.round().clamp(0.0, f64::from(u16::MAX)))
                .map_or(0, |p| {
                    #[expect(clippy::cast_possible_truncation, reason = "clamped to u16")]
                    #[expect(clippy::cast_sign_loss, reason = "clamped to u16")]
                    let p = p as u16;
                    p
                })
        } else {
            0
        };
        true
    }

    /// `Custom::update`.
    fn update(&mut self, host: &mut dyn Host) {
        self.updated = true;
        if self.has_exec() && (self.output.out.is_empty() || self.output.exit_code != 0) {
            self.view.visible = false;
            return;
        }
        if self.common.config.get("return-type").is("json") {
            if !self.parse_json(host) {
                return;
            }
        } else {
            self.parse_raw();
        }
        let icon = self.common.icon(self.percentage, &[&self.alt], 0);
        let args = Args::new()
            .named("text", self.text.as_str())
            .named("alt", self.alt.as_str())
            .named("icon", icon.as_str())
            .named("percentage", u32::from(self.percentage));
        match format(&self.common.format, &args) {
            Ok(label) => {
                let hide_empty =
                    self.common.config.get("hide-empty-text").as_bool() && self.text.is_empty();
                if hide_empty || label.is_empty() {
                    self.view.visible = false;
                    return;
                }
                self.view.markup = label.clone();
                self.view.tooltip = self
                    .common
                    .tooltip
                    .then(|| self.tooltip_markup(&label, &icon, host));
                self.view.classes = vec!["module".to_owned()];
                if !self.common.id.is_empty() {
                    self.view.classes.push(self.common.id.clone());
                }
                self.view.classes.extend(self.class.iter().cloned());
                self.view.visible = true;
                self.view.label_visible = true;
            }
            Err(error) => {
                host.diag().warn(format!("{}: {error}", self.name));
                self.view.visible = true;
                self.view.label_visible = true;
                self.view.markup = self.text.clone();
                if self.common.tooltip {
                    self.view.tooltip = Some(if self.tooltip.is_empty() {
                        self.text.clone()
                    } else {
                        self.tooltip.clone()
                    });
                }
            }
        }
    }

    /// The tooltip: `tooltip-format`, or the label when the text is the
    /// tooltip, or the tooltip.
    fn tooltip_markup(&self, label: &str, icon: &str, host: &mut dyn Host) -> String {
        let Some(tooltip_format) = self.common.config.get("tooltip-format").as_str() else {
            return if self.text == self.tooltip {
                label.to_owned()
            } else {
                self.tooltip.clone()
            };
        };
        let args = Args::new()
            .named("text", self.text.as_str())
            .named("tooltip", self.tooltip.as_str())
            .named("alt", self.alt.as_str())
            .named("icon", icon)
            .named("percentage", u32::from(self.percentage));
        format(tooltip_format, &args).unwrap_or_else(|error| {
            host.diag().warn(format!("{}: {error}", self.name));
            self.tooltip.clone()
        })
    }

    /// `handleEvent`: a click or scroll runs the script again unless
    /// `exec-on-event` is false.
    fn event(&mut self, host: &mut dyn Host) {
        let on_event = self.common.config.get("exec-on-event");
        if (!on_event.is_bool() || on_event.as_bool())
            && matches!(self.worker, Worker::Waiting | Worker::Delayed(_))
        {
            self.run(host);
        }
    }
}

impl Module for Custom {
    fn start(&mut self, host: &mut dyn Host) {
        match self.worker {
            Worker::None => self.update(host),
            _ if !self.has_exec() => self.update(host),
            _ => self.run(host),
        }
    }

    fn view(&self) -> &ModuleView {
        &self.view
    }

    fn common(&mut self) -> &mut Common {
        &mut self.common
    }

    fn child_line(&mut self, host: &mut dyn Host, child: ChildKey, line: &str) -> bool {
        if self.running.map(|(key, _)| key) != Some(child) || self.worker != Worker::Continuous {
            return false;
        }
        self.output = Output {
            exit_code: 0,
            out: line.to_owned(),
        };
        self.update(host);
        true
    }

    fn child_exit(
        &mut self,
        host: &mut dyn Host,
        child: ChildKey,
        status: Option<i32>,
        output: &str,
    ) -> bool {
        let Some((key, condition)) = self.running else {
            return false;
        };
        if key != child {
            return false;
        }
        self.running = None;
        // A signal's death is an exit status of -1 to waybar's WEXITSTATUS
        // of the raw status: non-zero.
        let code = status.unwrap_or(-1);
        if condition {
            if code != 0 {
                self.output = Output {
                    exit_code: code,
                    out: String::new(),
                };
                self.update(host);
                self.finish(host);
                return true;
            }
            if let Some(exec) = self.common.config.get("exec").as_str() {
                self.running = host.run(exec, false).map(|child| (child, false));
            } else {
                self.update(host);
                self.finish(host);
            }
            return true;
        }
        if self.worker == Worker::Continuous {
            if code != 0 {
                self.output = Output {
                    exit_code: code,
                    out: String::new(),
                };
                self.update(host);
                host.diag().error(format!(
                    "{} stopped unexpectedly, is it endless?",
                    self.name
                ));
            }
            let restart = self.common.config.get("restart-interval");
            if restart.is_numeric() && restart.as_f64().unwrap_or(0.0) > 0.0 {
                let seconds = restart.as_f64().unwrap_or(1.0);
                self.timer = Some(host.timer(Duration::from_secs_f64(seconds.max(0.001))));
            }
            return true;
        }
        // waybar's `read` takes the last newline off.
        let out = output.strip_suffix('\n').unwrap_or(output);
        self.output = Output {
            exit_code: code,
            out: out.to_owned(),
        };
        self.update(host);
        self.finish(host);
        true
    }

    fn timer(&mut self, host: &mut dyn Host, timer: TimerKey) -> bool {
        if self.timer != Some(timer) {
            return false;
        }
        self.timer = None;
        if self.worker == Worker::Continuous {
            if let Some(exec) = self.common.config.get("exec").as_str() {
                self.running = host.run(exec, true).map(|child| (child, false));
                if self.running.is_none() {
                    self.output = Output {
                        exit_code: 1,
                        out: String::new(),
                    };
                    self.update(host);
                    host.diag()
                        .error(format!("Unable to restart {}", self.name));
                }
            }
            return true;
        }
        self.run(host);
        false
    }

    fn signal(&mut self, host: &mut dyn Host, signal: i32) -> bool {
        let wanted = self.common.config.get("signal");
        if wanted.is_int()
            && wanted.as_i64().map(|n| i64::from(host.sigrtmin()) + n) == Some(i64::from(signal))
        {
            if matches!(self.worker, Worker::Waiting | Worker::Delayed(_)) {
                self.run(host);
            }
            return true;
        }
        false
    }

    fn click(
        &mut self,
        host: &mut dyn Host,
        button: super::Button,
        press: super::Press,
        at: (f32, f32),
    ) -> bool {
        let before = self.common.format.clone();
        self.common.run_click(host, button, press, at);
        if self.common.format != before {
            self.update(host);
        }
        if press != super::Press::Release {
            self.event(host);
        }
        true
    }

    fn scroll(&mut self, host: &mut dyn Host, scroll: super::Scroll) -> bool {
        self.common.run_scroll(host, scroll);
        self.event(host);
        false
    }
}

impl Custom {
    /// After a run: another if one was asked for meanwhile, else the next
    /// interval.
    fn finish(&mut self, host: &mut dyn Host) {
        if self.again {
            self.again = false;
            self.run(host);
        } else {
            self.schedule(host);
        }
    }

    /// Whether the first output has been handled.
    #[must_use]
    pub fn updated(&self) -> bool {
        self.updated
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::Fake;
    use super::super::{Module, Press};
    use super::{Custom, Worker};
    use crate::json::parse;

    fn custom(config: &str) -> (Custom, Fake) {
        let mut host = Fake::default();
        let config = parse(config).unwrap_or_default_value();
        let module = Custom::new("custom/x", "x", &config, "DP-1", &mut host);
        (module, host)
    }

    trait OrNull {
        fn unwrap_or_default_value(self) -> crate::json::Value;
    }

    impl OrNull for Result<crate::json::Value, crate::json::Error> {
        fn unwrap_or_default_value(self) -> crate::json::Value {
            self.unwrap_or(crate::json::Value::Null)
        }
    }

    #[test]
    fn the_workers_are_picked_as_waybar_picks_them() {
        let (module, _) = custom(r#"{"exec": "x", "interval": "once", "signal": 5}"#);
        assert_eq!(module.worker(), Worker::Delayed(None));
        let (module, _) = custom(r#"{"exec": "x", "signal": 5}"#);
        assert_eq!(module.worker(), Worker::Waiting);
        let (module, _) = custom(r#"{"exec": "x"}"#);
        assert_eq!(module.worker(), Worker::Continuous);
        let (module, _) = custom(r#"{"exec": "x", "interval": 5}"#);
        assert_eq!(module.worker(), Worker::Delayed(Some(5.0)));
        let (module, _) = custom(r#"{"format": " "}"#);
        assert_eq!(module.worker(), Worker::None);
    }

    #[test]
    fn a_module_without_exec_shows_its_format() {
        let (mut module, mut host) = custom(
            r#"{"format": " ", "tooltip-format": "<span>Apps</span>", "on-click": "fuzzel"}"#,
        );
        module.start(&mut host);
        assert!(module.view().visible);
        assert_eq!(module.view().markup, " ");
        assert_eq!(module.view().tooltip.as_deref(), Some("<span>Apps</span>"));
        assert!(host.runs.is_empty());
        let _ = module.click(&mut host, 1, Press::Single, (0.5, 0.5));
        assert_eq!(host.spawned, vec!["fuzzel".to_owned()]);
    }

    #[test]
    fn a_missing_script_hides_the_module() {
        // What happens on Ferrix to the user's desktop chips: sh answers
        // 127 for a command that is not there.
        let (mut module, mut host) = custom(
            r#"{"exec": "/home/u/.local/bin/hypr-workspaces state 1", "return-type": "json", "interval": "once", "signal": 5}"#,
        );
        module.start(&mut host);
        let (child, command, lines) = host.runs.first().cloned().unwrap_or_default();
        assert_eq!(command, "/home/u/.local/bin/hypr-workspaces state 1");
        assert!(!lines);
        let _ = module.child_exit(&mut host, child, Some(127), "");
        assert!(!module.view().visible);
        assert!(host.timers.is_empty(), "once is never again");
        // SIGRTMIN+5 runs it again.
        assert!(module.signal(&mut host, 39));
        assert_eq!(host.runs.len(), 2);
    }

    #[test]
    fn json_output_sets_text_tooltip_and_classes() {
        let (mut module, mut host) =
            custom(r#"{"exec": "s", "return-type": "json", "interval": "once"}"#);
        module.start(&mut host);
        let (child, _, _) = host.runs.first().cloned().unwrap_or_default();
        let _ = module.child_exit(
            &mut host,
            child,
            Some(0),
            "{\"text\": \"3\", \"tooltip\": \"Desktop 3\", \"class\": [\"ws\", \"active\"]}\n",
        );
        let view = module.view();
        assert!(view.visible);
        assert_eq!(view.markup, "3");
        assert_eq!(view.tooltip.as_deref(), Some("Desktop 3"));
        assert_eq!(view.classes, vec!["module", "ws", "active"]);
        assert_eq!(view.label_classes, vec!["flat", "text-button"]);
    }

    #[test]
    fn a_continuous_script_updates_per_line_and_says_when_it_stops() {
        let (mut module, mut host) =
            custom(r#"{"exec": "ba-calendar daemon", "return-type": "json"}"#);
        module.start(&mut host);
        let (child, _, lines) = host.runs.first().cloned().unwrap_or_default();
        assert!(lines);
        assert!(module.child_line(&mut host, child, "{\"text\": \"12:00\"}"));
        assert_eq!(module.view().markup, "12:00");
        let _ = module.child_exit(&mut host, child, Some(127), "");
        assert!(!module.view().visible);
        assert!(host.diag.has("x stopped unexpectedly, is it endless?"));
    }

    #[test]
    fn an_interval_reruns_and_a_click_reruns_early() {
        let (mut module, mut host) = custom(r#"{"exec": "date", "interval": 5}"#);
        module.start(&mut host);
        let (child, _, _) = host.runs.first().cloned().unwrap_or_default();
        let _ = module.child_exit(&mut host, child, Some(0), "12:00\n");
        assert_eq!(module.view().markup, "12:00");
        let (&timer, &after) = host
            .timers
            .iter()
            .next()
            .unwrap_or((&0, &std::time::Duration::ZERO));
        assert_eq!(after.as_secs(), 5);
        let _ = module.timer(&mut host, timer);
        assert_eq!(host.runs.len(), 2);
    }

    #[test]
    fn a_bad_format_falls_back_to_the_text() {
        let (mut module, mut host) =
            custom(r#"{"exec": "s", "interval": "once", "format": "{nope}"}"#);
        module.start(&mut host);
        let (child, _, _) = host.runs.first().cloned().unwrap_or_default();
        let _ = module.child_exit(&mut host, child, Some(0), "hello");
        assert_eq!(module.view().markup, "hello");
        assert!(host.diag.has("x: argument not found"));
    }
}
