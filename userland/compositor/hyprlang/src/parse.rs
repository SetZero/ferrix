//! Reading a file against a [`Schema`].

use std::path::{Path, PathBuf};

use crate::Schema;

/// An option set, where it was set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    /// Its name: the full name for an ordinary option, the name relative to
    /// the category for one in an [`Instance`].
    pub name: String,
    /// Its value, variables expanded, `##` made `#`, comments and the
    /// spaces around it gone, escapes of `{` `}` removed.
    pub value: String,
    /// The file it was in.
    pub file: PathBuf,
    /// Its line, counting from 1 (the first line of a continued one).
    pub line: usize,
}

/// One instance of a special category: one `background { }` block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// The category, `"background"`.
    pub category: String,
    /// The key's value for a keyed category; `None` for an anonymous one.
    pub key: Option<String>,
    /// What it set, in order. A name set twice is here twice; [`Instance::get`]
    /// answers the last.
    pub values: Vec<Setting>,
    /// Where it began.
    pub file: PathBuf,
    /// The line it began on.
    pub line: usize,
}

impl Instance {
    /// The last value set for `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|setting| setting.name == name)
            .map(|setting| setting.value.as_str())
    }

    /// The setting itself, for its line number.
    #[must_use]
    pub fn setting(&self, name: &str) -> Option<&Setting> {
        self.values
            .iter()
            .rev()
            .find(|setting| setting.name == name)
    }
}

/// A keyword line: `bezier = linear, 1, 1, 0, 0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyword {
    /// The name as written, `"bezier"`.
    pub name: String,
    /// The value.
    pub value: String,
    /// The categories it was inside, outermost first: `["animations"]`.
    pub categories: Vec<String>,
    /// The file it was in.
    pub file: PathBuf,
    /// Its line.
    pub line: usize,
}

/// A line hyprlang would have complained about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The file.
    pub file: PathBuf,
    /// The line, counting from 1; `0` for the file as a whole (`Unclosed
    /// category at EOF`).
    pub line: usize,
    /// hyprlang's words: `config option <general:foo> does not exist.`,
    /// `Invalid config line`, `Stray category close`.
    pub message: String,
}

impl core::fmt::Display for Diagnostic {
    /// `Config error in file /path at line 12: …`, hyprlang's own form, or
    /// `Config error at line 12: …` for text with no file.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let unnamed = self.file.as_os_str().is_empty();
        if self.line == 0 {
            return if unnamed {
                write!(formatter, "Config error: {}", self.message)
            } else {
                write!(
                    formatter,
                    "Config error in file {}: {}",
                    self.file.display(),
                    self.message
                )
            };
        }
        if unnamed {
            write!(
                formatter,
                "Config error at line {}: {}",
                self.line, self.message
            )
        } else {
            write!(
                formatter,
                "Config error in file {} at line {}: {}",
                self.file.display(),
                self.line,
                self.message
            )
        }
    }
}

/// Everything a file said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    /// Ordinary options, in the order set. A name set twice is here twice;
    /// [`Document::get`] answers the last, as hyprlang keeps the last.
    pub options: Vec<Setting>,
    /// Special category instances, in the order their blocks began.
    pub instances: Vec<Instance>,
    /// Keyword lines, in order.
    pub keywords: Vec<Keyword>,
    /// Variables as they stood at the end, `$` dropped.
    pub variables: Vec<(String, String)>,
    /// What was wrong, in order. Never a reason to stop: every other line
    /// is still read.
    pub diagnostics: Vec<Diagnostic>,
}

impl Document {
    /// The last value set for the ordinary option `name`
    /// (`"general:lock_cmd"`).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|setting| setting.name == name)
            .map(|setting| setting.value.as_str())
    }

    /// The instances of one special category, in file order.
    pub fn instances_of<'a>(
        &'a self,
        category: &'a str,
    ) -> impl Iterator<Item = &'a Instance> + 'a {
        self.instances
            .iter()
            .filter(move |instance| instance.category == category)
    }

    /// The keyword lines of one name, in file order.
    pub fn keywords_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Keyword> + 'a {
        self.keywords
            .iter()
            .filter(move |keyword| keyword.name == name)
    }
}

/// Parse `text` as the file `file` (which names diagnostics and anchors
/// relative `source` paths; empty for text that is not a file).
#[must_use]
pub fn parse(schema: &Schema, text: &str, file: &Path) -> Document {
    crate::reader::read(schema, text, file)
}

/// Read and parse the file at `path`.
///
/// # Errors
///
/// The file cannot be read; hyprlock and hypridle both refuse to start
/// then (`Config file is missing`), which is the caller's to say.
pub fn parse_file(schema: &Schema, path: &Path) -> std::io::Result<Document> {
    let text = std::fs::read_to_string(path)?;
    Ok(parse(schema, &text, path))
}
