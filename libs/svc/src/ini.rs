//! systemd's INI subset, as `src/shared/conf-parser.c` reads it.
//!
//! A file is `[Section]` headers and `Key=value` assignments, one per line.
//! Lines whose first non-blank character is `#` or `;` are comments, even in
//! the middle of a continued line. A line ending in an odd number of
//! backslashes continues on the next, the last backslash becoming a space.
//! Key and value lose the blanks around them; the value is otherwise kept as
//! written, because what quoting means differs from key to key (an
//! `ExecStart=` unquotes, a `Description=` does not).
//!
//! Nothing is decided here about what a key means. Every assignment is kept
//! in the order it was written, with the file and line it came from, so that
//! the kind reading a section can apply systemd's rules: a later scalar
//! assignment wins, a list key adds, and an empty assignment clears. A
//! drop-in is the same syntax, merged after its unit's file (§4.1).
//!
//! Only one thing refuses a file: a section header without its closing
//! bracket, which systemd refuses too, since every line after it would land
//! in a section nobody meant. Everything else is a [`Warning`](crate::Warning).

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::Warnings;

/// The longest logical line, continuations included: systemd's
/// `LONG_LINE_MAX`.
pub const LINE_MAX: usize = 1024 * 1024;

/// One `Key=value`, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The key, without the blanks around it.
    pub key: String,
    /// The value, without the blanks around it; empty for `Key=`.
    pub value: String,
    /// The file it was written in.
    pub file: Arc<str>,
    /// The line it starts on, counting from 1.
    pub line: u32,
}

/// Every assignment under one section name, from every file merged so far,
/// in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The name between the brackets, as written.
    pub name: String,
    /// The assignments, in the order they were read.
    pub assignments: Vec<Assignment>,
}

impl Section {
    /// A section with nothing in it.
    pub fn empty(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            assignments: Vec::new(),
        }
    }

    /// The assignments to `key`, in order.
    pub fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Assignment> + 'a {
        self.assignments
            .iter()
            .filter(move |assignment| assignment.key == key)
    }
}

/// A parsed file, or several merged: its sections, each once, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    /// The sections, by the name between their brackets.
    pub sections: BTreeMap<String, Section>,
}

impl Document {
    /// The section called `name`, if any file had one.
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.get(name)
    }

    /// The section called `name`, made if it does not exist.
    fn section_mut(&mut self, name: &str) -> &mut Section {
        self.sections
            .entry(name.to_owned())
            .or_insert_with(|| Section::empty(name))
    }

    /// Add `later`'s assignments after this document's own, section by
    /// section: how a drop-in applies to its unit.
    pub fn merge(&mut self, later: Document) {
        for (name, section) in later.sections {
            self.section_mut(&name)
                .assignments
                .extend(section.assignments);
        }
    }

    /// Every assignment, in every section, for the specifier pass to rewrite.
    pub fn assignments_mut(&mut self) -> impl Iterator<Item = &mut Assignment> {
        self.sections
            .values_mut()
            .flat_map(|section| section.assignments.iter_mut())
    }
}

/// A file that does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    /// The file.
    pub file: Arc<str>,
    /// The line, counting from 1.
    pub line: u32,
    /// What is wrong.
    pub message: String,
}

/// The blanks systemd strips: its `WHITESPACE`.
fn blank(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Whether `line` ends in a backslash that is not itself escaped.
fn continues(line: &str) -> bool {
    let mut escaped = false;
    for c in line.chars() {
        escaped = !escaped && c == '\\';
    }
    escaped
}

/// Parse one file.
///
/// # Errors
///
/// A section header without its closing bracket, or a logical line longer
/// than [`LINE_MAX`]. Everything else is a warning, and the line it is about
/// is skipped.
pub fn parse(
    file: &Arc<str>,
    bytes: &[u8],
    warnings: &mut Warnings,
) -> Result<Document, SyntaxError> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut reader = Reader {
        file,
        warnings,
        document: Document::default(),
        section: None,
    };
    let mut continuation: Option<(String, u32)> = None;
    let mut number: u32 = 0;
    for raw in bytes.split(|&byte| byte == b'\n') {
        number = number.saturating_add(1);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let Ok(text) = core::str::from_utf8(raw) else {
            reader.warn(number, "Line is not valid UTF-8, ignoring.".to_owned());
            continue;
        };
        if text.trim_start_matches(blank).starts_with(['#', ';']) {
            continue;
        }
        let (mut line, start) = match continuation.take() {
            Some((mut joined, start)) => {
                joined.push_str(text);
                (joined, start)
            }
            None => (text.to_owned(), number),
        };
        if line.len() > LINE_MAX {
            return Err(reader.error(start, "Line too long".to_owned()));
        }
        if continues(&line) {
            let _ = line.pop();
            line.push(' ');
            continuation = Some((line, start));
            continue;
        }
        reader.line(&line, start)?;
    }
    if let Some((line, start)) = continuation {
        reader.line(&line, start)?;
    }
    Ok(reader.document)
}

/// The state of one [`parse`].
struct Reader<'a> {
    file: &'a Arc<str>,
    warnings: &'a mut Warnings,
    document: Document,
    /// The section being filled, by name; `None` before the first header.
    section: Option<String>,
}

impl Reader<'_> {
    fn warn(&mut self, line: u32, message: String) {
        self.warnings.warn(self.file, line, message);
    }

    fn error(&self, line: u32, message: String) -> SyntaxError {
        SyntaxError {
            file: Arc::clone(self.file),
            line,
            message,
        }
    }

    /// One logical line, continuations joined.
    fn line(&mut self, line: &str, number: u32) -> Result<(), SyntaxError> {
        let line = line.trim_matches(blank);
        if line.is_empty() {
            return Ok(());
        }
        if let Some(header) = line.strip_prefix('[') {
            let Some(name) = header.strip_suffix(']') else {
                return Err(self.error(number, format!("Invalid section header '{line}'")));
            };
            let _ = self.document.section_mut(name);
            self.section = Some(name.to_owned());
            return Ok(());
        }
        let Some(section) = self.section.as_deref() else {
            self.warn(
                number,
                "Assignment outside of section. Ignoring.".to_owned(),
            );
            return Ok(());
        };
        let Some((key, value)) = line.split_once('=') else {
            self.warn(number, "Missing '=', ignoring line.".to_owned());
            return Ok(());
        };
        let key = key.trim_matches(blank);
        if key.is_empty() {
            self.warn(
                number,
                "Missing key name before '=', ignoring line.".to_owned(),
            );
            return Ok(());
        }
        let assignment = Assignment {
            key: key.to_owned(),
            value: value.trim_matches(blank).to_owned(),
            file: Arc::clone(self.file),
            line: number,
        };
        let section = section.to_owned();
        self.document
            .section_mut(&section)
            .assignments
            .push(assignment);
        Ok(())
    }
}
