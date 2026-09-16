//! The value syntaxes of Hyprland's options: integers (which are also its
//! booleans and its colours), floats, CSS-shaped gap lists and gradients.
//!
//! Each follows hyprlang's `configStringToInt` and Hyprland's gradient and
//! gap handlers, including where they are looser than they look: a boolean
//! is recognised by its prefix, so `yesterday` is true.

use core::fmt;

/// A colour the way Hyprland stores one: `0xAARRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color(pub u32);

impl Color {
    /// The alpha channel.
    #[must_use]
    pub const fn alpha(self) -> u8 {
        (self.0 >> 24) as u8
    }

    /// The red channel.
    #[must_use]
    pub const fn red(self) -> u8 {
        (self.0 >> 16) as u8
    }

    /// The green channel.
    #[must_use]
    pub const fn green(self) -> u8 {
        (self.0 >> 8) as u8
    }

    /// The blue channel.
    #[must_use]
    pub const fn blue(self) -> u8 {
        self.0 as u8
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:08x}", self.0)
    }
}

/// A border's gradient: up to ten colours and an angle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gradient {
    /// The colours in order, at least one.
    pub colors: Vec<Color>,
    /// The angle in whole degrees, as written.
    pub angle_degrees: i32,
}

impl Gradient {
    /// A gradient of one colour at angle zero.
    #[must_use]
    pub fn solid(color: Color) -> Self {
        Self {
            colors: vec![color],
            angle_degrees: 0,
        }
    }
}

impl fmt::Display for Gradient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for color in &self.colors {
            write!(f, "{color} ")?;
        }
        write!(f, "{}deg", self.angle_degrees)
    }
}

/// The most colours a gradient holds.
pub const MAX_GRADIENT_COLORS: usize = 10;

/// Gaps on the four sides, in CSS order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gaps {
    /// Above.
    pub top: i64,
    /// To the right.
    pub right: i64,
    /// Below.
    pub bottom: i64,
    /// To the left.
    pub left: i64,
}

impl Gaps {
    /// The same gap on every side.
    #[must_use]
    pub const fn all(gap: i64) -> Self {
        Self {
            top: gap,
            right: gap,
            bottom: gap,
            left: gap,
        }
    }
}

impl fmt::Display for Gaps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} {}",
            self.top, self.right, self.bottom, self.left
        )
    }
}

/// Whether `text` is an optional minus sign and then digits, with one dot
/// among them when `float` allows it: hyprlang's `isNumber`.
fn is_number(text: &str, float: bool) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() {
        return false;
    }
    let mut dots = 0;
    for c in digits.chars() {
        match c {
            '0'..='9' => {}
            '.' if float && dots == 0 => dots += 1,
            _ => return false,
        }
    }
    digits != "."
}

/// Parse an integer option's value: a decimal number, `0x` hex, `rgba(…)`,
/// `rgb(…)`, or a boolean word.
pub fn parse_int(text: &str) -> Result<i64, String> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x") {
        return i64::from_str_radix(hex, 16)
            .map_err(|_| format!("cannot parse \"{text}\" as a hex number"));
    }
    if let Some(inner) = text
        .strip_prefix("rgba(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return parse_rgba(inner.trim()).map(|color| i64::from(color.0));
    }
    if let Some(inner) = text
        .strip_prefix("rgb(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return parse_rgb(inner.trim()).map(|color| i64::from(color.0));
    }
    if ["true", "on", "yes"]
        .iter()
        .any(|word| text.starts_with(word))
    {
        return Ok(1);
    }
    if ["false", "off", "no"]
        .iter()
        .any(|word| text.starts_with(word))
    {
        return Ok(0);
    }
    if !is_number(text, false) {
        return Err(format!("cannot parse \"{text}\" as an int."));
    }
    text.parse::<i64>()
        .map_err(|_| format!("cannot parse \"{text}\" as an int."))
}

/// Parse a float option's value.
pub fn parse_float(text: &str) -> Result<f64, String> {
    let text = text.trim();
    if !is_number(text, true) {
        return Err(format!("cannot parse \"{text}\" as a float."));
    }
    text.parse::<f64>()
        .map_err(|_| format!("cannot parse \"{text}\" as a float."))
}

/// Parse a colour: anything [`parse_int`] takes, kept to 32 bits.
pub fn parse_color(text: &str) -> Result<Color, String> {
    let value = parse_int(text)?;
    u32::try_from(value)
        .map(Color)
        .map_err(|_| format!("\"{}\" is not a colour", text.trim()))
}

/// Eight hex digits, or no digits at all is refused.
fn parse_hex(text: &str, digits: usize) -> Option<u32> {
    if text.len() != digits || !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(text, 16).ok()
}

/// One channel of a comma-separated colour, 0 to 255.
fn channel(text: &str) -> Result<u32, String> {
    let value = text
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("\"{}\" is not a colour channel", text.trim()))?;
    if value > 255 {
        return Err(format!("colour channel {value} is above 255"));
    }
    Ok(value)
}

/// The inside of `rgba(…)`: `RRGGBBAA`, or `r, g, b, a` with alpha from 0 to 1.
fn parse_rgba(inner: &str) -> Result<Color, String> {
    let fields: Vec<&str> = inner.split(',').collect();
    if let [red, green, blue, alpha] = fields.as_slice() {
        let alpha = parse_float(alpha)?;
        if !(0.0..=1.0).contains(&alpha) {
            return Err(format!("alpha {alpha} is outside 0 to 1"));
        }
        // In range by the check above, so the cast neither wraps nor saturates.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "alpha is in 0..=1, so the product is in 0..=255"
        )]
        let alpha = (alpha * 255.0).round() as u32;
        return Ok(Color(
            (alpha << 24) | (channel(red)? << 16) | (channel(green)? << 8) | channel(blue)?,
        ));
    }
    let rgba = parse_hex(inner, 8).ok_or_else(|| {
        "rgba() expects length of 8 characters (4 bytes) or 4 comma separated values".to_owned()
    })?;
    Ok(Color(rgba.rotate_right(8)))
}

/// The inside of `rgb(…)`: `RRGGBB`, or `r, g, b`. Opaque either way.
fn parse_rgb(inner: &str) -> Result<Color, String> {
    let fields: Vec<&str> = inner.split(',').collect();
    if let [red, green, blue] = fields.as_slice() {
        return Ok(Color(
            0xff00_0000 | (channel(red)? << 16) | (channel(green)? << 8) | channel(blue)?,
        ));
    }
    let rgb = parse_hex(inner, 6).ok_or_else(|| {
        "rgb() expects length of 6 characters (3 bytes) or 3 comma separated values".to_owned()
    })?;
    Ok(Color(0xff00_0000 | rgb))
}

/// Parse a gradient: colours separated by spaces, then optionally an angle
/// such as `45deg`, after which anything more is ignored as Hyprland ignores
/// it.
pub fn parse_gradient(text: &str) -> Result<Gradient, String> {
    let mut gradient = Gradient {
        colors: Vec::new(),
        angle_degrees: 0,
    };
    for word in text.split_whitespace() {
        if let Some((degrees, _)) = word.split_once("deg") {
            gradient.angle_degrees = degrees
                .parse::<i32>()
                .map_err(|_| format!("error parsing gradient angle \"{word}\""))?;
            break;
        }
        if gradient.colors.len() >= MAX_GRADIENT_COLORS {
            return Err("Too many colors in a gradient".to_owned());
        }
        gradient.colors.push(
            parse_color(word).map_err(|error| format!("error parsing gradient {text}: {error}"))?,
        );
    }
    if gradient.colors.is_empty() {
        return Err("Colors in gradient must be at least 1".to_owned());
    }
    Ok(gradient)
}

/// Parse gaps: one to four integers separated by spaces, spread over the
/// sides as CSS spreads `margin`.
pub fn parse_gaps(text: &str) -> Result<Gaps, String> {
    let values = text
        .split_whitespace()
        .map(parse_int)
        .collect::<Result<Vec<_>, _>>()?;
    match *values.as_slice() {
        [all] => Ok(Gaps::all(all)),
        [vertical, horizontal] => Ok(Gaps {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }),
        [top, horizontal, bottom] => Ok(Gaps {
            top,
            right: horizontal,
            bottom,
            left: horizontal,
        }),
        [top, right, bottom, left] => Ok(Gaps {
            top,
            right,
            bottom,
            left,
        }),
        _ => Err(format!(
            "gaps take one to four values, not \"{}\"",
            text.trim()
        )),
    }
}
