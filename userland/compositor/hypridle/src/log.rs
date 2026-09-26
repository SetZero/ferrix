//! Upstream's `Debug::log`: a `[LEVEL] ` prefix and the line, on the
//! standard output, with `-q` saying nothing and `-v` adding `TRACE`.

use std::io::Write as _;

/// How much a line matters, with upstream's names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Only with `-v`.
    Trace,
    /// Upstream's `INFO`.
    Info,
    /// Upstream's `LOG`, which is most of what it says.
    Log,
    /// Something that works differently than asked.
    Warn,
    /// Something that did not work.
    Err,
    /// Something that ends the program.
    Crit,
    /// No prefix at all.
    None,
}

impl Level {
    /// The prefix upstream prints, without its brackets.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::Trace => "TRACE",
            Self::Info => "INFO",
            Self::Log => "LOG",
            Self::Warn => "WARN",
            Self::Err => "ERR",
            Self::Crit => "CRITICAL",
            Self::None => return None,
        })
    }
}

/// Where lines go, and which are said.
#[derive(Clone, Copy, Debug, Default)]
pub struct Log {
    /// `-q`: nothing is said.
    pub quiet: bool,
    /// `-v`: `TRACE` is said too.
    pub verbose: bool,
}

impl Log {
    /// The line as upstream formats it, or `None` when it is not said.
    #[must_use]
    pub fn format(&self, level: Level, line: &str) -> Option<String> {
        if self.quiet || (level == Level::Trace && !self.verbose) {
            return None;
        }
        Some(match level.name() {
            Some(name) => format!("[{name}] {line}"),
            None => line.to_owned(),
        })
    }

    /// Say it.
    pub fn say(&self, level: Level, line: &str) {
        if let Some(text) = self.format(level, line) {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{text}");
            let _ = out.flush();
        }
    }
}
