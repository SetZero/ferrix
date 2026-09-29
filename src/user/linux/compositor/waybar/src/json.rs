//! JSON with comments, read the way waybar's jsoncpp reads it.
//!
//! waybar parses its configuration, every `return-type: json` line a custom
//! script prints and every Hyprland IPC answer with jsoncpp's
//! `CharReaderBuilder` at its default settings (`util/json.hpp`). Those
//! settings are what make `config.jsonc` legal: `//` and `/* */` comments
//! are allowed, a trailing comma before `]` or `}` is allowed, text after
//! the root value is ignored, and a key written twice keeps its last value.
//! Before parsing, waybar replaces every `\x` in the text with `\u00`,
//! because scripts print `\x1b`-style escapes that JSON does not have; that
//! is done here too, byte for byte, including its blindness to whether the
//! backslash was itself escaped.
//!
//! The value model is jsoncpp's where waybar's behaviour depends on it:
//! numbers remember whether they were written as integers, and
//! [`Value::is_uint`], [`Value::is_int`] and [`Value::as_string`] answer as
//! `isUInt`, `isInt` and `asString` do -- `"height": 40` is an unsigned
//! integer, `"interval": 0.5` is not, and `asString` of a number is its
//! decimal text. Objects keep the order keys were written in, which jsoncpp
//! does not (it sorts them); nothing waybar does depends on the order.

use core::fmt;

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`, and what indexing a missing key gives.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number written without a fraction or exponent that fits in `i64`.
    Int(i64),
    /// A non-negative integer too large for `i64`.
    UInt(u64),
    /// Any other number.
    Real(f64),
    /// A string, escapes decoded.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object, in the order its keys were first written.
    Object(Vec<(String, Value)>),
}

/// The `null` indexing hands back for a key or element that is not there.
static NULL: Value = Value::Null;

impl Value {
    /// The member `key` of an object, or `null` -- jsoncpp's `operator[]` on
    /// a const value, which never fails.
    #[must_use]
    pub fn get(&self, key: &str) -> &Value {
        match self {
            Value::Object(members) => members
                .iter()
                .find(|(name, _)| name == key)
                .map_or(&NULL, |(_, value)| value),
            _ => &NULL,
        }
    }

    /// Element `index` of an array, or `null`.
    #[must_use]
    pub fn at(&self, index: usize) -> &Value {
        match self {
            Value::Array(items) => items.get(index).unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    /// Whether an object has `key`, as `isMember`.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        matches!(self, Value::Object(members) if members.iter().any(|(name, _)| name == key))
    }

    /// Set `key` in an object, replacing its value if it has one. Does
    /// nothing to anything that is not an object.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Value::Object(members) = self {
            if let Some(slot) = members.iter_mut().find(|(name, _)| name == key) {
                slot.1 = value;
            } else {
                members.push((key.to_owned(), value));
            }
        }
    }

    /// The member `key` of an object, mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Value::Object(members) => members
                .iter_mut()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The members of an object, empty for anything else.
    #[must_use]
    pub fn members(&self) -> &[(String, Value)] {
        match self {
            Value::Object(members) => members,
            _ => &[],
        }
    }

    /// The elements of an array, empty for anything else.
    #[must_use]
    pub fn items(&self) -> &[Value] {
        match self {
            Value::Array(items) => items,
            _ => &[],
        }
    }

    /// `isNull`.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// `isString`.
    #[must_use]
    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// `isBool`.
    #[must_use]
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Bool(_))
    }

    /// `isObject`.
    #[must_use]
    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    /// `isArray`.
    #[must_use]
    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    /// `isNumeric`: any number, integer or not. jsoncpp counts no bool.
    #[must_use]
    pub fn is_numeric(&self) -> bool {
        matches!(self, Value::Int(_) | Value::UInt(_) | Value::Real(_))
    }

    /// `isUInt`: a number that is a whole value in `0..=u32::MAX`, however
    /// it was written.
    #[must_use]
    pub fn is_uint(&self) -> bool {
        match *self {
            Value::Int(value) => u32::try_from(value).is_ok(),
            Value::UInt(value) => u32::try_from(value).is_ok(),
            Value::Real(value) => {
                value >= 0.0 && value <= f64::from(u32::MAX) && value.fract() == 0.0
            }
            _ => false,
        }
    }

    /// `isInt`: a number that is a whole value in the range of `i32`.
    #[must_use]
    pub fn is_int(&self) -> bool {
        match *self {
            Value::Int(value) => i32::try_from(value).is_ok(),
            Value::UInt(value) => i32::try_from(value).is_ok(),
            Value::Real(value) => {
                value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX) && value.fract() == 0.0
            }
            _ => false,
        }
    }

    /// The number as `f64`, as `asDouble`; `None` for anything that is not
    /// a number or a bool.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            #[expect(clippy::cast_precision_loss, reason = "asDouble is lossy the same way")]
            Value::Int(value) => Some(value as f64),
            #[expect(clippy::cast_precision_loss, reason = "asDouble is lossy the same way")]
            Value::UInt(value) => Some(value as f64),
            Value::Real(value) => Some(value),
            Value::Bool(value) => Some(if value { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// The number as `i64` if it is a whole one (`asInt64` of an integral
    /// value).
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Value::Int(value) => Some(value),
            Value::UInt(value) => i64::try_from(value).ok(),
            Value::Real(value) if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 =>
            {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "checked whole and in range"
                )]
                Some(value as i64)
            }
            Value::Bool(value) => Some(i64::from(value)),
            _ => None,
        }
    }

    /// `asBool`: `null` and zero are false, a bool is itself, a nonzero
    /// number is true. jsoncpp throws for a string, array or object; that is
    /// `false` here.
    #[must_use]
    pub fn as_bool(&self) -> bool {
        match *self {
            Value::Bool(value) => value,
            Value::Int(value) => value != 0,
            Value::UInt(value) => value != 0,
            Value::Real(value) => value != 0.0,
            _ => false,
        }
    }

    /// The string if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    /// `asString`: a string is itself, `null` is empty, a bool is `true` or
    /// `false`, a number is its decimal text. jsoncpp throws for an array or
    /// an object; that is empty here.
    #[must_use]
    pub fn as_string(&self) -> String {
        match self {
            Value::String(text) => text.clone(),
            Value::Bool(value) => if *value { "true" } else { "false" }.to_owned(),
            Value::Int(value) => value.to_string(),
            Value::UInt(value) => value.to_string(),
            Value::Real(value) => real_text(*value),
            _ => String::new(),
        }
    }

    /// Whether this is the string `text`, as jsoncpp's `value == "text"`.
    #[must_use]
    pub fn is(&self, text: &str) -> bool {
        self.as_str() == Some(text)
    }

    /// `empty()`: `null`, or an empty array or object.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Value::Null => true,
            Value::Array(items) => items.is_empty(),
            Value::Object(members) => members.is_empty(),
            _ => false,
        }
    }
}

/// A real as jsoncpp's `valueToString` writes it: 17 significant digits,
/// shortest form, and a `.0` kept on a whole value.
fn real_text(value: f64) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            "null".to_owned()
        } else if value > 0.0 {
            "1e+9999".to_owned()
        } else {
            "-1e+9999".to_owned()
        };
    }
    // Rust's shortest round-trip form is what %.17g trimmed gives for every
    // value a person writes in a config.
    let text = format!("{value}");
    if text.contains(['.', 'e', 'E']) {
        text
    } else {
        format!("{text}.0")
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("null"),
            Value::Bool(value) => write!(f, "{value}"),
            Value::Int(value) => write!(f, "{value}"),
            Value::UInt(value) => write!(f, "{value}"),
            Value::Real(value) => f.write_str(&real_text(*value)),
            Value::String(text) => write_string(f, text),
            Value::Array(items) => {
                f.write_str("[")?;
                for (at, item) in items.iter().enumerate() {
                    if at > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Value::Object(members) => {
                f.write_str("{")?;
                for (at, (name, value)) in members.iter().enumerate() {
                    if at > 0 {
                        f.write_str(",")?;
                    }
                    write_string(f, name)?;
                    write!(f, ":{value}")?;
                }
                f.write_str("}")
            }
        }
    }
}

/// A string as JSON writes it.
fn write_string(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in text.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            c if u32::from(c) < 0x20 => write!(f, "\\u{:04x}", u32::from(c))?,
            c => write!(f, "{c}")?,
        }
    }
    f.write_str("\"")
}

/// Why a text is not JSON, with where, as jsoncpp's
/// `getFormattedErrorMessages` puts it: `* Line 3, Column 5` and then the
/// reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// 1-based line.
    pub line: usize,
    /// 1-based column, in characters.
    pub column: usize,
    /// What was wrong, in jsoncpp's words where it has them.
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "* Line {}, Column {}\n  {}\n",
            self.line, self.column, self.message
        )
    }
}

/// Parse `text` as waybar does: `\x` rewritten to `\u00` first, then
/// jsoncpp's defaults.
///
/// # Errors
///
/// When the text holds no value, or the root value is malformed.
pub fn parse(text: &str) -> Result<Value, Error> {
    let rewritten;
    let text = if text.contains("\\x") {
        rewritten = text.replace("\\x", "\\u00");
        rewritten.as_str()
    } else {
        text
    };
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        at: 0,
    };
    // A byte-order mark is skipped, as `skipBom` does.
    if parser.bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        parser.at = 3;
    }
    parser.value(0)
}

/// How deep arrays and objects may nest: jsoncpp's `stackLimit`.
const STACK_LIMIT: usize = 1000;

/// The reader: the text, and how far into it.
struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn error_at(&self, at: usize, message: &str) -> Error {
        let before = self.text.get(..at).unwrap_or(self.text);
        let line = before.matches('\n').count() + 1;
        let column = before
            .rsplit('\n')
            .next()
            .map_or(0, |last| last.chars().count())
            + 1;
        Error {
            line,
            column,
            message: message.to_owned(),
        }
    }

    fn error(&self, message: &str) -> Error {
        self.error_at(self.at, message)
    }

    /// Skip white space and comments.
    fn skip(&mut self) -> Result<(), Error> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.at += 1,
                Some(b'/') => match self.bytes.get(self.at + 1) {
                    Some(b'/') => self.line_comment(),
                    Some(b'*') => self.block_comment()?,
                    _ => return Ok(()),
                },
                _ => return Ok(()),
            }
        }
    }

    /// Skip a `//` comment and its newline.
    fn line_comment(&mut self) {
        while let Some(byte) = self.peek() {
            self.at += 1;
            if byte == b'\n' {
                break;
            }
        }
    }

    /// Skip a `/* */` comment; one never closed is an error.
    fn block_comment(&mut self) -> Result<(), Error> {
        let start = self.at;
        self.at += 2;
        loop {
            match self.peek() {
                None => {
                    return Err(
                        self.error_at(start, "Syntax error: value, object or array expected.")
                    );
                }
                Some(b'*') if self.bytes.get(self.at + 1) == Some(&b'/') => {
                    self.at += 2;
                    return Ok(());
                }
                Some(_) => self.at += 1,
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > STACK_LIMIT {
            return Err(self.error("Exceeded stackLimit in readValue()."));
        }
        self.skip()?;
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::String),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            _ => Err(self.error("Syntax error: value, object or array expected.")),
        }
    }

    fn word(&mut self, word: &str, value: Value) -> Result<Value, Error> {
        if self
            .bytes
            .get(self.at..)
            .is_some_and(|rest| rest.starts_with(word.as_bytes()))
        {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.error("Syntax error: value, object or array expected."))
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, Error> {
        self.at += 1;
        let mut members: Vec<(String, Value)> = Vec::new();
        loop {
            self.skip()?;
            match self.peek() {
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Value::Object(members));
                }
                Some(b'"') => {}
                _ => return Err(self.error("Missing '}' or object member name")),
            }
            let name = self.string()?;
            self.skip()?;
            if self.peek() != Some(b':') {
                return Err(self.error("Missing ':' after object member name"));
            }
            self.at += 1;
            let value = self.value(depth + 1)?;
            if let Some(slot) = members.iter_mut().find(|(key, _)| *key == name) {
                slot.1 = value;
            } else {
                members.push((name, value));
            }
            self.skip()?;
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Value::Object(members));
                }
                _ => return Err(self.error("Missing ',' or '}' in object declaration")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, Error> {
        self.at += 1;
        let mut items = Vec::new();
        loop {
            self.skip()?;
            if self.peek() == Some(b']') {
                self.at += 1;
                return Ok(Value::Array(items));
            }
            items.push(self.value(depth + 1)?);
            self.skip()?;
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.error("Missing ',' or ']' in array declaration")),
            }
        }
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        let mut real = false;
        while let Some(byte) = self.peek() {
            match byte {
                b'0'..=b'9' => {}
                b'.' | b'e' | b'E' => real = true,
                b'+' | b'-' if real => {}
                _ => break,
            }
            self.at += 1;
        }
        let text = self.text.get(start..self.at).unwrap_or_default();
        if !real {
            if let Ok(value) = text.parse::<i64>() {
                return Ok(Value::Int(value));
            }
            if let Ok(value) = text.parse::<u64>() {
                return Ok(Value::UInt(value));
            }
        }
        text.parse::<f64>()
            .map(Value::Real)
            .map_err(|_| self.error_at(start, &format!("'{text}' is not a number.")))
    }

    fn string(&mut self) -> Result<String, Error> {
        let start = self.at;
        self.at += 1;
        let mut out = String::new();
        loop {
            let rest = self.text.get(self.at..).unwrap_or_default();
            let Some(c) = rest.chars().next() else {
                return Err(self.error_at(start, "Missing '\"' at end of string"));
            };
            self.at += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let Some(escape) = self.peek() else {
                        return Err(self.error_at(start, "Empty escape sequence in string"));
                    };
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'/' => out.push('/'),
                        b'\\' => out.push('\\'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode()?),
                        _ => {
                            return Err(self.error_at(self.at - 2, "Bad escape sequence in string"));
                        }
                    }
                }
                c => out.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let digits = self.text.get(self.at..self.at + 4).ok_or_else(|| {
            self.error("Bad unicode escape sequence in string: four digits expected.")
        })?;
        let value = u32::from_str_radix(digits, 16).map_err(|_| {
            self.error("Bad unicode escape sequence in string: hexadecimal digit expected.")
        })?;
        self.at += 4;
        Ok(value)
    }

    fn unicode(&mut self) -> Result<char, Error> {
        let first = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&first) {
            if self.bytes.get(self.at..self.at + 2) != Some(b"\\u") {
                return Err(self.error(
                    "expecting another \\u token to begin the second half of a unicode surrogate pair",
                ));
            }
            self.at += 2;
            let second = self.hex4()?;
            if !(0xDC00..0xE000).contains(&second) {
                return Err(self.error(
                    "expecting another \\u token to begin the second half of a unicode surrogate pair",
                ));
            }
            0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
        } else {
            first
        };
        // jsoncpp writes a lone low surrogate out as its three bytes, which is
        // not UTF-8; here it becomes U+FFFD, as Glib's make_valid would make
        // of it before Pango sees it.
        Ok(char::from_u32(code).unwrap_or('\u{FFFD}'))
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, parse};

    #[test]
    fn comments_and_trailing_commas_are_jsonc() {
        let value = parse("{\n  // a line comment\n  \"a\": 1, /* a block */ \"b\": [1, 2,],\n}\n");
        let value = value.unwrap_or(Value::Null);
        assert_eq!(value.get("a"), &Value::Int(1));
        assert_eq!(value.get("b").items().len(), 2);
    }

    #[test]
    fn numbers_answer_as_jsoncpp_does() {
        let value = parse("[40, -3, 0.5, 40.0, 1e3, 18446744073709551615]").unwrap_or(Value::Null);
        assert!(value.at(0).is_uint() && value.at(0).is_int());
        assert!(!value.at(1).is_uint() && value.at(1).is_int());
        assert!(!value.at(2).is_uint() && value.at(2).is_numeric());
        assert!(value.at(3).is_uint(), "a whole real is a uint to jsoncpp");
        assert!(value.at(4).is_uint());
        assert_eq!(value.at(5), &Value::UInt(u64::MAX));
        assert_eq!(value.at(0).as_string(), "40");
        assert_eq!(value.at(2).as_string(), "0.5");
        assert_eq!(value.at(3).as_string(), "40.0");
    }

    #[test]
    fn a_duplicate_key_keeps_its_last_value() {
        let value = parse("{\"a\": 1, \"a\": 2}").unwrap_or(Value::Null);
        assert_eq!(value.get("a"), &Value::Int(2));
        assert_eq!(value.members().len(), 1);
    }

    #[test]
    fn backslash_x_becomes_a_unicode_escape_first() {
        let value = parse("{\"text\": \"\\x41\\u00e9\"}").unwrap_or(Value::Null);
        assert_eq!(value.get("text").as_str(), Some("Aé"));
    }

    #[test]
    fn surrogate_pairs_decode() {
        let value = parse("\"\\ud83d\\ude00\"").unwrap_or(Value::Null);
        assert_eq!(value.as_str(), Some("\u{1F600}"));
    }

    #[test]
    fn text_after_the_root_is_ignored() {
        assert_eq!(parse("{} trailing"), Ok(Value::Object(Vec::new())));
    }

    #[test]
    fn errors_say_where() {
        let error = parse("{\n  \"a\" 1\n}").err();
        let error = error.map(|error| (error.line, error.column, error.message));
        assert_eq!(
            error,
            Some((2, 7, "Missing ':' after object member name".to_owned()))
        );
        assert!(parse("").is_err());
        assert!(parse("{\"a\": }").is_err());
        assert!(parse("/* never closed").is_err());
    }

    #[test]
    fn a_missing_member_is_null_and_comparisons_are_by_string() {
        let value = parse("{\"interval\": \"once\"}").unwrap_or(Value::Null);
        assert!(value.get("interval").is("once"));
        assert!(value.get("nothing").is_null());
        assert!(value.get("nothing").get("deeper").is_null());
        assert_eq!(value.get("nothing").as_string(), "");
    }

    #[test]
    fn display_round_trips() {
        let text = "{\"a\":[1,2.5,\"x\\\"y\",null,true],\"b\":{}}";
        let value = parse(text).unwrap_or(Value::Null);
        assert_eq!(value.to_string(), text);
    }
}
