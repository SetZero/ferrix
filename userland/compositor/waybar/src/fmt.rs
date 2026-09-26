//! `format` strings, as waybar hands them to libfmt.
//!
//! Every `format` and `tooltip-format` goes through `fmt::format(runtime(…),
//! fmt::arg("name", value)…)`: `{name}` is a named argument, `{}` takes the
//! arguments in order -- named ones count, so `{}` in a custom module is
//! `text`, its first -- `{name:spec}` formats with libfmt's standard spec
//! (`[[fill]align][sign][#][0][width][.precision][type]`), and `{{`/`}}` are
//! literal braces. A string that breaks those rules is a `format_error`,
//! which each module catches and logs; [`Error`] carries libfmt's message.
//!
//! The network module's bandwidths are waybar's own `pow_format` type, whose
//! spec is waybar's (`util/format.hpp`): [`Arg::Pow`] formats as it does.

use core::fmt::Write;

/// One argument's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    /// A string, as `std::string`.
    Str(String),
    /// A signed integer.
    Int(i64),
    /// A `float`, which libfmt writes in its shortest round-trip form as a
    /// `float` -- memory's `{used}` is one.
    F32(f32),
    /// A `double`.
    F64(f64),
    /// waybar's `pow_format`: a value, a unit, and how to scale it.
    Pow(Pow),
}

/// waybar's `pow_format`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pow {
    /// The value, in the unit's base.
    pub value: i64,
    /// The unit written after the prefix, such as `b/s`.
    pub unit: String,
    /// Powers of 1024 rather than 1000.
    pub binary: bool,
    /// No decimal when the value divides evenly.
    pub skip_decimal: bool,
    /// Below this power, no decimal.
    pub min_pow_for_decimal: u32,
}

impl Pow {
    /// A value and a unit, decimal powers, a decimal always.
    #[must_use]
    pub fn new(value: i64, unit: &str) -> Self {
        Self {
            value,
            unit: unit.to_owned(),
            binary: false,
            skip_decimal: false,
            min_pow_for_decimal: 0,
        }
    }
}

impl From<&str> for Arg {
    fn from(text: &str) -> Self {
        Arg::Str(text.to_owned())
    }
}

impl From<String> for Arg {
    fn from(text: String) -> Self {
        Arg::Str(text)
    }
}

impl From<i64> for Arg {
    fn from(value: i64) -> Self {
        Arg::Int(value)
    }
}

impl From<i32> for Arg {
    fn from(value: i32) -> Self {
        Arg::Int(i64::from(value))
    }
}

impl From<u32> for Arg {
    fn from(value: u32) -> Self {
        Arg::Int(i64::from(value))
    }
}

impl From<u64> for Arg {
    fn from(value: u64) -> Self {
        Arg::Int(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

impl From<f64> for Arg {
    fn from(value: f64) -> Self {
        Arg::F64(value)
    }
}

impl From<f32> for Arg {
    fn from(value: f32) -> Self {
        Arg::F32(value)
    }
}

/// The arguments of one call, in order, each with its name if it has one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
    list: Vec<(Option<String>, Arg)>,
}

impl Args {
    /// No arguments.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a positional argument.
    #[must_use]
    pub fn positional(mut self, value: impl Into<Arg>) -> Self {
        self.list.push((None, value.into()));
        self
    }

    /// Add a named argument, `fmt::arg(name, value)`.
    #[must_use]
    pub fn named(mut self, name: &str, value: impl Into<Arg>) -> Self {
        self.list.push((Some(name.to_owned()), value.into()));
        self
    }

    /// Add a named argument in place.
    pub fn push(&mut self, name: &str, value: impl Into<Arg>) {
        self.list.push((Some(name.to_owned()), value.into()));
    }

    fn by_name(&self, name: &str) -> Option<&Arg> {
        self.list
            .iter()
            .find(|(named, _)| named.as_deref() == Some(name))
            .map(|(_, value)| value)
    }

    fn by_index(&self, index: usize) -> Option<&Arg> {
        self.list.get(index).map(|(_, value)| value)
    }
}

/// A `fmt::format_error`, in libfmt's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

fn error(message: &str) -> Error {
    Error(message.to_owned())
}

/// `fmt::format(fmt::runtime(text), args…)`.
///
/// # Errors
///
/// libfmt's: an argument that is not there, an unmatched brace, a spec the
/// argument's type does not take.
pub fn format(text: &str, args: &Args) -> Result<String, Error> {
    let mut out = String::new();
    let mut chars = text.char_indices().peekable();
    let mut next_index = 0usize;
    let mut manual = false;
    while let Some((_, c)) = chars.next() {
        match c {
            '{' => {
                if chars.peek().map(|&(_, c)| c) == Some('{') {
                    let _ = chars.next();
                    out.push('{');
                    continue;
                }
                let mut field = String::new();
                let mut depth = 0usize;
                let mut closed = false;
                for (_, c) in chars.by_ref() {
                    match c {
                        '{' => depth += 1,
                        '}' if depth == 0 => {
                            closed = true;
                            break;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                    field.push(c);
                }
                if !closed {
                    return Err(error("invalid format string"));
                }
                let (id, spec) = field.split_once(':').unwrap_or((field.as_str(), ""));
                let arg = if id.is_empty() {
                    if manual {
                        return Err(error(
                            "cannot switch from manual to automatic argument indexing",
                        ));
                    }
                    let arg = args.by_index(next_index);
                    next_index += 1;
                    arg.ok_or_else(|| error("argument not found"))?
                } else if let Ok(index) = id.parse::<usize>() {
                    if next_index > 0 {
                        return Err(error(
                            "cannot switch from automatic to manual argument indexing",
                        ));
                    }
                    manual = true;
                    args.by_index(index)
                        .ok_or_else(|| error("argument not found"))?
                } else if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && id.chars().next().is_some_and(|c| !c.is_ascii_digit())
                {
                    args.by_name(id)
                        .ok_or_else(|| error("argument not found"))?
                } else {
                    return Err(error("invalid format string"));
                };
                format_arg(&mut out, arg, spec)?;
            }
            '}' => {
                if chars.peek().map(|&(_, c)| c) == Some('}') {
                    let _ = chars.next();
                    out.push('}');
                } else {
                    return Err(error("unmatched '}' in format string"));
                }
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

/// A standard format spec, parsed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Spec {
    fill: Option<char>,
    align: Option<char>,
    sign: Option<char>,
    alternate: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
    kind: Option<char>,
}

fn parse_spec(text: &str) -> Result<Spec, Error> {
    let mut spec = Spec::default();
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0usize;
    let is_align = |c: Option<&char>| matches!(c, Some('<' | '>' | '^'));
    if is_align(chars.get(1)) && chars.first().is_some_and(|&c| c != '{' && c != '}') {
        spec.fill = chars.first().copied();
        spec.align = chars.get(1).copied();
        at = 2;
    } else if is_align(chars.first()) {
        spec.align = chars.first().copied();
        at = 1;
    }
    if let Some(&c @ ('+' | '-' | ' ')) = chars.get(at) {
        spec.sign = Some(c);
        at += 1;
    }
    if chars.get(at) == Some(&'#') {
        spec.alternate = true;
        at += 1;
    }
    if chars.get(at) == Some(&'0') {
        spec.zero = true;
        at += 1;
    }
    let start = at;
    while chars.get(at).is_some_and(char::is_ascii_digit) {
        at += 1;
    }
    let digits: String = chars.get(start..at).unwrap_or_default().iter().collect();
    if !digits.is_empty() {
        spec.width = digits.parse().map_err(|_| error("number is too big"))?;
    }
    if chars.get(at) == Some(&'.') {
        at += 1;
        let start = at;
        while chars.get(at).is_some_and(char::is_ascii_digit) {
            at += 1;
        }
        let digits: String = chars.get(start..at).unwrap_or_default().iter().collect();
        if digits.is_empty() {
            return Err(error("missing precision specifier"));
        }
        spec.precision = Some(digits.parse().map_err(|_| error("number is too big"))?);
    }
    if chars.get(at) == Some(&'L') {
        at += 1;
    }
    if let Some(&c) = chars.get(at) {
        spec.kind = Some(c);
        at += 1;
    }
    if at != chars.len() {
        return Err(error("invalid format specifier"));
    }
    Ok(spec)
}

fn format_arg(out: &mut String, arg: &Arg, spec_text: &str) -> Result<(), Error> {
    if let Arg::Pow(pow) = arg {
        out.push_str(&format_pow(pow, spec_text)?);
        return Ok(());
    }
    let spec = parse_spec(spec_text)?;
    let (body, numeric) = match arg {
        Arg::Str(text) => {
            if !matches!(spec.kind, None | Some('s')) {
                return Err(error("invalid format specifier"));
            }
            if spec.sign.is_some() || spec.zero || spec.alternate {
                return Err(error("invalid format specifier"));
            }
            let text = match spec.precision {
                Some(precision) => text.chars().take(precision).collect(),
                None => text.clone(),
            };
            (text, false)
        }
        Arg::Int(value) => {
            if spec.precision.is_some() {
                return Err(error("precision not allowed for this argument type"));
            }
            (int_text(*value, &spec)?, true)
        }
        Arg::F32(value) => (float_text(f64::from(*value), Some(*value), &spec)?, true),
        Arg::F64(value) => (float_text(*value, None, &spec)?, true),
        Arg::Pow(_) => (String::new(), false),
    };
    pad(out, &body, &spec, numeric);
    Ok(())
}

fn with_sign(negative: bool, digits: &str, spec: &Spec) -> String {
    let sign = if negative {
        "-"
    } else {
        match spec.sign {
            Some('+') => "+",
            Some(' ') => " ",
            _ => "",
        }
    };
    format!("{sign}{digits}")
}

fn int_text(value: i64, spec: &Spec) -> Result<String, Error> {
    let magnitude = value.unsigned_abs();
    let digits = match spec.kind {
        None | Some('d') => magnitude.to_string(),
        Some('x') => format!("{}{magnitude:x}", if spec.alternate { "0x" } else { "" }),
        Some('X') => format!("{}{magnitude:X}", if spec.alternate { "0X" } else { "" }),
        Some('o') => format!("{}{magnitude:o}", if spec.alternate { "0" } else { "" }),
        Some('b') => format!("{}{magnitude:b}", if spec.alternate { "0b" } else { "" }),
        Some('c') => {
            return u32::try_from(value)
                .ok()
                .and_then(char::from_u32)
                .map(String::from)
                .ok_or_else(|| error("invalid format specifier"));
        }
        Some('f' | 'F' | 'e' | 'E' | 'g' | 'G') => {
            #[expect(clippy::cast_precision_loss, reason = "libfmt converts the same way")]
            let real = value as f64;
            return float_text(real, None, spec);
        }
        _ => return Err(error("invalid format specifier")),
    };
    Ok(with_sign(value < 0, &digits, spec))
}

fn float_text(value: f64, single: Option<f32>, spec: &Spec) -> Result<String, Error> {
    let negative = value.is_sign_negative() && value != 0.0;
    let magnitude = value.abs();
    let digits = match (spec.kind, spec.precision) {
        (Some('f' | 'F'), precision) => format!("{magnitude:.*}", precision.unwrap_or(6)),
        (Some('e'), precision) => exponent_text(magnitude, precision.unwrap_or(6), 'e'),
        (Some('E'), precision) => exponent_text(magnitude, precision.unwrap_or(6), 'E'),
        (Some('%'), precision) => format!("{:.*}%", precision.unwrap_or(6), magnitude * 100.0),
        (None | Some('g' | 'G'), Some(precision)) => general_text(magnitude, precision.max(1)),
        (None, None) => match single {
            Some(single) => format!("{}", single.abs()),
            None => format!("{magnitude}"),
        },
        (Some('g' | 'G'), None) => general_text(magnitude, 6),
        _ => return Err(error("invalid format specifier")),
    };
    Ok(with_sign(negative, &digits, spec))
}

/// `%e` with `precision` digits after the point and a two-digit exponent.
fn exponent_text(value: f64, precision: usize, letter: char) -> String {
    let text = format!("{value:.precision$e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((text.as_str(), "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("{mantissa}{letter}{sign}{:02}", exponent.abs())
}

/// `%g` with `precision` significant digits, trailing zeros taken off.
fn general_text(value: f64, precision: usize) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    #[expect(clippy::cast_possible_truncation, reason = "a decimal exponent fits")]
    let exponent = value.log10().floor() as i64;
    let limit = i64::try_from(precision).unwrap_or(i64::MAX);
    if exponent < -4 || exponent >= limit {
        let text = exponent_text(value, precision.saturating_sub(1), 'e');
        let (mantissa, rest) = text.split_once('e').unwrap_or((text.as_str(), ""));
        let mantissa = trim_zeros(mantissa);
        format!("{mantissa}e{rest}")
    } else {
        let decimals = usize::try_from(limit - 1 - exponent).unwrap_or(0);
        trim_zeros(&format!("{value:.decimals$}"))
    }
}

fn trim_zeros(text: &str) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text.to_owned()
    }
}

/// Fill, align and zero-pad `body` to the spec's width. Numbers align right
/// by default and strings left, as in libfmt.
fn pad(out: &mut String, body: &str, spec: &Spec, numeric: bool) {
    let length = body.chars().count();
    if length >= spec.width {
        out.push_str(body);
        return;
    }
    let missing = spec.width - length;
    if spec.zero && numeric && spec.align.is_none() {
        let (sign, digits) = match body.chars().next() {
            Some(c @ ('-' | '+' | ' ')) => (Some(c), body.get(c.len_utf8()..).unwrap_or_default()),
            _ => (None, body),
        };
        if let Some(sign) = sign {
            out.push(sign);
        }
        out.extend(core::iter::repeat_n('0', missing));
        out.push_str(digits);
        return;
    }
    let fill = spec.fill.unwrap_or(' ');
    let align = spec.align.unwrap_or(if numeric { '>' } else { '<' });
    let (before, after) = match align {
        '>' => (missing, 0),
        '^' => (missing / 2, missing - missing / 2),
        _ => (0, missing),
    };
    out.extend(core::iter::repeat_n(fill, before));
    out.push_str(body);
    out.extend(core::iter::repeat_n(fill, after));
}

/// `formatter<pow_format>::format`, as `util/format.hpp` has it.
fn format_pow(pow: &Pow, spec: &str) -> Result<String, Error> {
    const UNITS: [&str; 6] = ["", "k", "M", "G", "T", "P"];
    let mut chars = spec.chars().peekable();
    let mut align = None;
    if let Some(&c @ ('>' | '<' | '=')) = chars.peek() {
        align = Some(c);
        let _ = chars.next();
    }
    let mut width = 0usize;
    let mut scale = None;
    let mut unit_pref = None;
    let mut base_pref = None;
    let mut force_int = false;
    while let Some(c) = chars.next() {
        match c {
            '#' | 'k' | 'M' | 'G' | 'T' | 'P' => scale = Some(c),
            'u' | 'U' => unit_pref = Some(c),
            'b' | 'B' => base_pref = Some(c),
            'i' => force_int = true,
            '0'..='9' => {
                let mut digits = String::from(c);
                while let Some(&d @ '0'..='9') = chars.peek() {
                    digits.push(d);
                    let _ = chars.next();
                }
                width = digits.parse().map_err(|_| error("number is too big"))?;
            }
            _ => return Err(error("invalid format specifier")),
        }
    }
    let binary = match base_pref {
        Some('B') => true,
        Some('b') => false,
        _ => pow.binary,
    };
    let base: i64 = if binary { 1024 } else { 1000 };
    let mut div: i64 = 1;
    #[expect(
        clippy::cast_precision_loss,
        reason = "the same double waybar computes"
    )]
    let mut fraction = pow.value as f64;
    #[expect(clippy::cast_precision_loss, reason = "a power of 1000 or 1024")]
    let base_f = base as f64;
    let power: usize = if let Some(scale) = scale {
        let power = match scale {
            'k' => 1,
            'M' => 2,
            'G' => 3,
            'T' => 4,
            'P' => 5,
            _ => 0,
        };
        for _ in 0..power {
            div = div.saturating_mul(base);
        }
        #[expect(clippy::cast_precision_loss, reason = "as waybar divides")]
        let divisor = div as f64;
        fraction /= divisor;
        power
    } else {
        let mut power = 0usize;
        while power + 1 < UNITS.len() && fraction / base_f >= 1.0 {
            fraction /= base_f;
            div = div.saturating_mul(base);
            power += 1;
        }
        power
    };
    let mut precision: usize =
        if force_int || (pow.skip_decimal && div != 0 && pow.value.checked_rem(div) == Some(0)) {
            0
        } else {
            1
        };
    if !force_int && scale.is_none() && power < pow.min_pow_for_decimal as usize {
        precision = 0;
    }
    let hide_unit = unit_pref == Some('u') || (unit_pref.is_none() && scale.is_some());
    let prefix = if scale.is_some() {
        String::new()
    } else {
        format!(
            "{}{}",
            UNITS.get(power).copied().unwrap_or_default(),
            if binary && power > 0 { "i" } else { "" }
        )
    };
    let unit = if hide_unit { "" } else { pow.unit.as_str() };
    let number_width = 3 + precision + usize::from(precision != 0) + usize::from(binary);
    let prefix_col = if scale.is_some() {
        0
    } else {
        1 + usize::from(binary)
    };
    let max_width = number_width + prefix_col + unit.chars().count();
    let fixed = scale.is_some() && width > 0;
    let mut number = format!("{fraction:.precision$}");
    if fixed && number.len() > width {
        number = "#".repeat(width);
    }
    let padding = if scale.is_none() && power == 0 {
        if binary { "  " } else { " " }
    } else {
        ""
    };
    let mut out = String::new();
    match align {
        Some('=') => {
            let column = if fixed { width } else { number_width };
            let _ = write!(out, "{number:<column$}{padding}{prefix}{unit}");
        }
        _ => {
            let body = if fixed {
                format!("{number:>width$}{prefix}{unit}")
            } else {
                format!("{number}{prefix}{unit}")
            };
            match align {
                Some('>') => {
                    let _ = write!(out, "{body:>max_width$}");
                }
                Some('<') => {
                    let _ = write!(out, "{body:<max_width$}");
                }
                _ => out.push_str(&body),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Arg, Args, Error, Pow, format};

    fn f(text: &str, args: &Args) -> String {
        format(text, args).unwrap_or_else(|Error(message)| format!("ERROR {message}"))
    }

    #[test]
    fn names_positions_and_braces() {
        let args = Args::new().named("text", "hi").named("percentage", 42);
        assert_eq!(
            f("{}", &args),
            "hi",
            "{{}} is the first argument, named or not"
        );
        assert_eq!(f("{text} {percentage}%", &args), "hi 42%");
        assert_eq!(f("{{}} {1}", &args), "{} 42");
        assert_eq!(f(" ", &args), " ");
        assert_eq!(f("{missing}", &args), "ERROR argument not found");
        assert_eq!(f("{", &args), "ERROR invalid format string");
        assert_eq!(f("}", &args), "ERROR unmatched '}' in format string");
    }

    #[test]
    fn floats_as_the_memory_module_prints_them() {
        let args = Args::new()
            .named("used", Arg::F32(3.2))
            .named("total", Arg::F32(15.54));
        assert_eq!(f("{used:0.1f}G / {total:0.1f}G", &args), "3.2G / 15.5G");
        assert_eq!(f("{used}", &args), "3.2");
        let args = Args::new().named("load", Arg::F64(0.0));
        assert_eq!(f("load {load}", &args), "load 0");
        let args = Args::new().named("load", Arg::F64(1.5));
        assert_eq!(f("{load}", &args), "1.5");
    }

    #[test]
    fn widths_fill_and_alignment() {
        let args = Args::new().named("n", 5).named("s", "ab");
        assert_eq!(f("[{n:3}]", &args), "[  5]");
        assert_eq!(f("[{n:03}]", &args), "[005]");
        assert_eq!(f("[{s:4}]", &args), "[ab  ]");
        assert_eq!(f("[{s:*^6}]", &args), "[**ab**]");
        assert_eq!(f("[{n:<3}]", &args), "[5  ]");
        assert_eq!(f("[{n:+}]", &args), "[+5]");
        assert_eq!(f("{s:d}", &args), "ERROR invalid format specifier");
    }

    #[test]
    fn pow_format_scales_as_waybar_does() {
        let args = Args::new()
            .named("zero", Arg::Pow(Pow::new(0, "b/s")))
            .named("kilo", Arg::Pow(Pow::new(12_345, "b/s")))
            .named("mega", Arg::Pow(Pow::new(3_400_000, "b/s")));
        assert_eq!(f("{zero}", &args), "0.0b/s");
        assert_eq!(f("{kilo}", &args), "12.3kb/s");
        assert_eq!(f("{mega}", &args), "3.4Mb/s");
        assert_eq!(f("{kilo:>}", &args), " 12.3kb/s");
        assert_eq!(f("{zero:>}", &args), "   0.0b/s");
        assert_eq!(f("{mega:=}", &args), "3.4  Mb/s");
    }
}
