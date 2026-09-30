//! The TOML a manifest and a record are written in: a subset, read and
//! written here.
//!
//! Tables (`[name]`), arrays of tables (`[[name]]`), and three kinds of
//! value: a basic string, `true` or `false`, and a one-line array of
//! strings. Comments from `#` outside a string to the end of the line. That
//! is every form `app.toml` and a record use, and anything else is refused
//! with its line, rather than read as something it is not: a manifest the
//! real TOML would read differently must not pass here.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// A value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `"text"`.
    String(String),
    /// `true` or `false`.
    Bool(bool),
    /// `["a", "b"]`.
    Array(Vec<String>),
}

/// A table: `[name]`, or one element of `[[name]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Its dotted name; empty for the keys before any header.
    pub name: String,
    /// Whether it is an element of an array of tables.
    pub array: bool,
    /// Its keys and values, in order.
    pub entries: Vec<(String, Value)>,
    /// The line its header is on, from one; zero for the top.
    pub line: usize,
}

impl Table {
    /// The value of `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find_map(|(name, value)| (name == key).then_some(value))
    }
}

/// A document: its tables, in order, the top one first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Every table.
    pub tables: Vec<Table>,
}

impl Document {
    /// The one `[name]` table.
    #[must_use]
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables
            .iter()
            .find(|table| table.name == name && !table.array)
    }

    /// Every element of `[[name]]`, in order.
    pub fn array<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Table> {
        self.tables
            .iter()
            .filter(move |table| table.name == name && table.array)
    }
}

/// Why a document did not read, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The line, from one.
    pub line: usize,
    /// What was wrong with it.
    pub what: &'static str,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.what)
    }
}

/// Read `text`.
///
/// # Errors
///
/// The first line that is not in the subset, or a key or `[name]` given
/// twice.
pub fn parse(text: &str) -> Result<Document, Error> {
    let mut tables = Vec::from([Table {
        name: String::new(),
        array: false,
        entries: Vec::new(),
        line: 0,
    }]);
    for (at, raw) in text.lines().enumerate() {
        let line = at + 1;
        let fail = |what| Error { line, what };
        let content = strip_comment(raw).map_err(fail)?.trim();
        if content.is_empty() {
            continue;
        }
        if let Some(header) = content.strip_prefix('[') {
            let table = header_table(header, line).map_err(fail)?;
            if !table.array && tables.iter().any(|seen| seen.name == table.name) {
                return Err(fail("the table is given twice"));
            }
            tables.push(table);
            continue;
        }
        let (key, value) = content.split_once('=').ok_or(fail("not `key = value`"))?;
        let key = key.trim();
        if !is_bare_key(key) {
            return Err(fail("a key is letters, digits, `-` and `_`"));
        }
        let value = parse_value(value.trim()).map_err(fail)?;
        let table = tables.last_mut().ok_or(fail("no table"))?;
        if table.get(key).is_some() {
            return Err(fail("the key is given twice"));
        }
        table.entries.push((String::from(key), value));
    }
    Ok(Document { tables })
}

/// The table a header (after its first `[`) opens.
fn header_table(header: &str, line: usize) -> Result<Table, &'static str> {
    let (array, name) = match header.strip_prefix('[') {
        Some(inner) => (true, inner.strip_suffix("]]")),
        None => (false, header.strip_suffix(']')),
    };
    let name = name.ok_or("a header is not closed")?.trim();
    if name.is_empty() || !name.split('.').all(is_bare_key) {
        return Err("a table's name is keys joined by `.`");
    }
    Ok(Table {
        name: String::from(name),
        array,
        entries: Vec::new(),
        line,
    })
}

/// `line` without its comment.
fn strip_comment(line: &str) -> Result<&str, &'static str> {
    let mut in_string = false;
    let mut escaped = false;
    for (at, character) in line.char_indices() {
        match character {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '#' if !in_string => return Ok(line.get(..at).unwrap_or_default()),
            _ => {}
        }
    }
    if in_string {
        Err("a string is not closed")
    } else {
        Ok(line)
    }
}

/// Whether `key` is a bare key.
fn is_bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// One value.
fn parse_value(text: &str) -> Result<Value, &'static str> {
    match text {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    if let Some(inner) = text.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or("an array is one line, closed")?;
        return parse_array(inner).map(Value::Array);
    }
    let (string, rest) = parse_string(text)?;
    if rest.trim().is_empty() {
        Ok(Value::String(string))
    } else {
        Err("text after a value")
    }
}

/// The strings between an array's brackets, a trailing comma allowed.
fn parse_array(mut inner: &str) -> Result<Vec<String>, &'static str> {
    let mut items = Vec::new();
    loop {
        inner = inner.trim_start();
        if inner.is_empty() {
            return Ok(items);
        }
        let (item, rest) = parse_string(inner)?;
        items.push(item);
        let rest = rest.trim_start();
        match rest.strip_prefix(',') {
            Some(after) => inner = after,
            None if rest.is_empty() => return Ok(items),
            None => return Err("an array's strings are separated by commas"),
        }
    }
}

/// A basic string at the start of `text`, and what follows it.
fn parse_string(text: &str) -> Result<(String, &str), &'static str> {
    let body = text
        .strip_prefix('"')
        .ok_or("a value is a string, `true`, `false` or an array of strings")?;
    let mut out = String::new();
    let mut characters = body.char_indices();
    while let Some((at, character)) = characters.next() {
        match character {
            '"' => return Ok((out, body.get(at + 1..).unwrap_or_default())),
            '\\' => out.push(match characters.next() {
                Some((_, '"')) => '"',
                Some((_, '\\')) => '\\',
                Some((_, 'n')) => '\n',
                Some((_, 't')) => '\t',
                _ => return Err("an escape is one of \\\" \\\\ \\n \\t"),
            }),
            control if control.is_control() => return Err("a control character in a string"),
            other => out.push(other),
        }
    }
    Err("a string is not closed")
}

/// `text` as a basic string, quoted and escaped so that [`parse`] reads it
/// back.
///
/// # Errors
///
/// What `out` answers.
pub fn write_string(out: &mut impl fmt::Write, text: &str) -> fmt::Result {
    out.write_char('"')?;
    for character in text.chars() {
        match character {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\t' => out.write_str("\\t")?,
            other => out.write_char(other)?,
        }
    }
    out.write_char('"')
}

/// `items` as a one-line array of strings.
///
/// # Errors
///
/// What `out` answers.
pub fn write_array(out: &mut impl fmt::Write, items: &[String]) -> fmt::Result {
    out.write_char('[')?;
    for (at, item) in items.iter().enumerate() {
        if at > 0 {
            out.write_str(", ")?;
        }
        write_string(out, item)?;
    }
    out.write_char(']')
}
