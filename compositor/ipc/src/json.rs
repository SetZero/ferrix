//! Writing JSON, which is what every bar reads.
//!
//! A JSON writer rather than a crate: the shapes here are fixed and shallow,
//! the compositor's dependency policy is to take nothing it can write, and
//! escaping a string is the only part with a rule worth getting right --
//! a window title is a person's to choose and goes straight into an answer a
//! bar parses.

use core::fmt::Write;

/// A JSON document being built.
#[derive(Clone, Debug, Default)]
pub struct Json {
    text: String,
    /// How deep, for the indentation Hyprland's own output has.
    depth: usize,
    /// Whether something has already been written at this depth.
    fresh: Vec<bool>,
    /// Whether to indent at all; `hyprctl -j -r` asks for none.
    pretty: bool,
}

impl Json {
    /// An empty document.
    #[must_use]
    pub fn new(pretty: bool) -> Self {
        Self {
            text: String::new(),
            depth: 0,
            fresh: vec![true],
            pretty,
        }
    }

    /// What has been written.
    #[must_use]
    pub fn finish(self) -> String {
        self.text
    }

    /// Start an array.
    pub fn array(&mut self) {
        self.comma();
        self.text.push('[');
        self.open();
    }

    /// Start an object.
    pub fn object(&mut self) {
        self.comma();
        self.text.push('{');
        self.open();
    }

    /// Start an object that is a field of the object being written.
    pub fn object_field(&mut self, name: &str) {
        self.field(name);
        self.text.push('{');
        self.open();
    }

    /// Start an array that is a field of the object being written.
    pub fn array_field(&mut self, name: &str) {
        self.field(name);
        self.text.push('[');
        self.open();
    }

    /// End the array or object being written.
    pub fn end(&mut self, bracket: char) {
        self.depth = self.depth.saturating_sub(1);
        let _ = self.fresh.pop();
        if !self.fresh.last().copied().unwrap_or(true) || self.pretty {
            self.newline();
        }
        self.text.push(bracket);
    }

    /// A string field.
    pub fn string(&mut self, name: &str, value: &str) {
        self.field(name);
        self.quoted(value);
    }

    /// A number field.
    pub fn number(&mut self, name: &str, value: i64) {
        self.field(name);
        let _ = write!(self.text, "{value}");
    }

    /// A boolean field.
    pub fn boolean(&mut self, name: &str, value: bool) {
        self.field(name);
        self.text.push_str(if value { "true" } else { "false" });
    }

    /// A field holding an array of two numbers, as `at` and `size` are.
    pub fn pair(&mut self, name: &str, first: i64, second: i64) {
        self.field(name);
        let _ = write!(self.text, "[{first}, {second}]");
    }

    /// A refresh rate, which Hyprland prints with five decimal places.
    pub fn field_refresh(&mut self, value: f64) {
        self.field("refreshRate");
        let _ = write!(self.text, "{value:.5}");
    }

    /// A scale, which Hyprland prints with two.
    pub fn field_scale(&mut self, value: f64) {
        self.field("scale");
        let _ = write!(self.text, "{value:.2}");
    }

    /// A field holding an empty array, which several of Hyprland's window
    /// fields always are here.
    pub fn empty_array(&mut self, name: &str) {
        self.field(name);
        self.text.push_str("[]");
    }

    /// A string in an array.
    pub fn item(&mut self, value: &str) {
        self.comma();
        self.quoted(value);
    }

    /// Write a string with JSON's escapes.
    ///
    /// A window title is whatever a program chose to call itself, so it may
    /// hold a quote, a backslash or a control character, and a bar parsing
    /// the answer must not see any of them raw. The rules are RFC 8259's:
    /// quote, backslash and everything below 0x20.
    fn quoted(&mut self, value: &str) {
        self.text.push('"');
        for character in value.chars() {
            match character {
                '"' => self.text.push_str("\\\""),
                '\\' => self.text.push_str("\\\\"),
                '\n' => self.text.push_str("\\n"),
                '\r' => self.text.push_str("\\r"),
                '\t' => self.text.push_str("\\t"),
                '\u{8}' => self.text.push_str("\\b"),
                '\u{c}' => self.text.push_str("\\f"),
                other if (other as u32) < 0x20 => {
                    let _ = write!(self.text, "\\u{:04x}", other as u32);
                }
                other => self.text.push(other),
            }
        }
        self.text.push('"');
    }

    fn field(&mut self, name: &str) {
        self.comma();
        self.quoted(name);
        self.text.push(':');
        if self.pretty {
            self.text.push(' ');
        }
    }

    fn comma(&mut self) {
        match self.fresh.last_mut() {
            Some(fresh) if *fresh => *fresh = false,
            Some(_) => self.text.push(','),
            None => {}
        }
        if self.depth > 0 {
            self.newline();
        }
    }

    fn open(&mut self) {
        self.depth += 1;
        self.fresh.push(true);
    }

    fn newline(&mut self) {
        if !self.pretty {
            return;
        }
        self.text.push('\n');
        for _ in 0..self.depth {
            self.text.push_str("    ");
        }
    }
}
