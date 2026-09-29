//! Diagnostics: what waybar logs, at spdlog's levels and in its words.
//!
//! waybar logs through spdlog, whose lines are `[time] [level] message`.
//! The lines here are `[level] message` -- the same level names
//! (`info`, `warning`, `error`) and, wherever upstream has a message for the
//! case, upstream's message -- so a person who greps waybar's log for a
//! warning finds this one's the same way. A line the upstream program has no
//! words for (something Ferrix cannot do) says so plainly.

use std::collections::BTreeSet;

/// How loud a line is: spdlog's levels, least first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// `trace`.
    Trace,
    /// `debug`.
    Debug,
    /// `info`, which is the default level printed.
    Info,
    /// `warning`.
    Warning,
    /// `error`.
    Error,
    /// `critical`.
    Critical,
    /// `off`: nothing is printed.
    Off,
}

impl Level {
    /// spdlog's name for the level, as `-l` takes it and a line shows it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
            Level::Critical => "critical",
            Level::Off => "off",
        }
    }

    /// The level `-l` names. spdlog's `from_str` takes `warn` and `err` too,
    /// and anything else is `off`.
    #[must_use]
    pub fn parse(text: &str) -> Level {
        match text {
            "trace" => Level::Trace,
            "debug" => Level::Debug,
            "info" => Level::Info,
            "warning" | "warn" => Level::Warning,
            "error" | "err" => Level::Error,
            "critical" => Level::Critical,
            _ => Level::Off,
        }
    }
}

/// The lines said so far, and the ones already said once.
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    /// Every line, in order.
    pub lines: Vec<(Level, String)>,
    /// Keys of the lines [`Diagnostics::once`] has said.
    said: BTreeSet<String>,
}

impl Diagnostics {
    /// A line at `level`.
    pub fn say(&mut self, level: Level, message: String) {
        self.lines.push((level, message));
    }

    /// A `debug` line.
    pub fn debug(&mut self, message: String) {
        self.say(Level::Debug, message);
    }

    /// An `info` line.
    pub fn info(&mut self, message: String) {
        self.say(Level::Info, message);
    }

    /// A `warning` line.
    pub fn warn(&mut self, message: String) {
        self.say(Level::Warning, message);
    }

    /// An `error` line.
    pub fn error(&mut self, message: String) {
        self.say(Level::Error, message);
    }

    /// A `warning` line said only the first time `key` is seen, for what
    /// would otherwise repeat on every update.
    pub fn once(&mut self, key: &str, message: String) {
        if self.said.insert(key.to_owned()) {
            self.warn(message);
        }
    }

    /// Take the lines at or above `level`, formatted, leaving none behind.
    pub fn drain(&mut self, level: Level) -> Vec<String> {
        core::mem::take(&mut self.lines)
            .into_iter()
            .filter(|(at, _)| *at >= level && level != Level::Off)
            .map(|(at, message)| format!("[{}] {message}", at.name()))
            .collect()
    }

    /// Whether any line holds `text`, for tests.
    #[must_use]
    pub fn has(&self, text: &str) -> bool {
        self.lines.iter().any(|(_, line)| line.contains(text))
    }
}
