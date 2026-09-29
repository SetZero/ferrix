//! Property values, parsed as GTK3 parses them.
//!
//! GTK3 turns a declaration's value into a typed value when the stylesheet
//! is loaded, and a value it cannot parse is a parse error that drops the
//! declaration. The parsers here are that step, one per value type:
//! colours with GTK's own functions (`alpha()`, `shade()`, `mix()`,
//! `lighter()`, `darker()` and `@name` references to `@define-color`),
//! lengths with `calc()`, images (`url()`, `linear-gradient()`), and the
//! background, border, shadow and font value grammars.
//!
//! A colour that names a `@define-color` stays symbolic until the style is
//! computed, as in GTK, where a later definition of the same name wins for
//! every use of it.

use super::token::Token;

/// A colour as GDK has it: four channels in `0..=1`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rgba {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha: 0 is transparent.
    pub a: f32,
}

impl Rgba {
    /// Transparent black, `rgba(0,0,0,0)`.
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// Opaque, from 8-bit channels.
    #[must_use]
    pub fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        }
    }

    /// Each channel as a byte, rounded, in `r, g, b, a` order.
    #[must_use]
    pub fn bytes(&self) -> [u8; 4] {
        let byte = |v: f32| {
            #[expect(clippy::cast_possible_truncation, reason = "clamped to a byte first")]
            #[expect(clippy::cast_sign_loss, reason = "clamped to a byte first")]
            let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            b
        };
        [byte(self.r), byte(self.g), byte(self.b), byte(self.a)]
    }
}

/// A colour value before `@name` references are resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum Color {
    /// A literal.
    Rgba(Rgba),
    /// `currentColor`: the element's `color`.
    Current,
    /// `@name`: a `@define-color`.
    Named(String),
    /// `alpha(color, factor)`: alpha multiplied.
    Alpha(Box<Color>, f32),
    /// `shade(color, factor)`: lightness and saturation multiplied.
    Shade(Box<Color>, f32),
    /// `mix(a, b, factor)`: `a` towards `b`.
    Mix(Box<Color>, Box<Color>, f32),
}

/// A length: a sum of pixels, `em`s and a percentage of whatever the
/// property measures against, which is all `calc()` needs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Length {
    /// Absolute pixels (`pt`, `mm` and the rest converted at 96 dpi).
    pub px: f32,
    /// Multiples of the element's font size (`ex` is half an `em` here).
    pub em: f32,
    /// A percentage of the reference size.
    pub percent: f32,
}

impl Length {
    /// Zero.
    pub const ZERO: Length = Length {
        px: 0.0,
        em: 0.0,
        percent: 0.0,
    };

    /// Pixels.
    #[must_use]
    pub fn px(px: f32) -> Self {
        Self { px, ..Self::ZERO }
    }

    /// A percentage.
    #[must_use]
    pub fn percent(percent: f32) -> Self {
        Self {
            percent,
            ..Self::ZERO
        }
    }

    /// The length in pixels, for an element whose font is `font_px` and a
    /// reference size of `reference`.
    #[must_use]
    pub fn resolve(&self, font_px: f32, reference: f32) -> f32 {
        self.px + self.em * font_px + self.percent / 100.0 * reference
    }

    fn scaled(self, by: f32) -> Self {
        Self {
            px: self.px * by,
            em: self.em * by,
            percent: self.percent * by,
        }
    }
}

/// A `linear-gradient()`.
#[derive(Clone, Debug, PartialEq)]
pub struct Linear {
    /// The direction, in degrees clockwise from "to top"; `to bottom` is
    /// 180, the default.
    pub angle: Angle,
    /// The stops, each a colour and where along the line it is.
    pub stops: Vec<(Color, Option<Length>)>,
    /// Whether it is `repeating-linear-gradient()`.
    pub repeating: bool,
}

/// A gradient's direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Angle {
    /// An angle in degrees.
    Degrees(f32),
    /// `to` a side or corner: horizontal -1 (left), 0 or 1 (right), and
    /// vertical -1 (top), 0 or 1 (bottom). Corners depend on the box.
    To(i8, i8),
}

/// A `background-image` layer, or `border-image-source`.
#[derive(Clone, Debug, PartialEq)]
pub enum Image {
    /// `none`.
    None,
    /// `url()`, as written; resolved against the stylesheet's directory.
    Url(String),
    /// `linear-gradient()`.
    Linear(Linear),
    /// An image GTK3 knows and this does not draw: `radial-gradient()`,
    /// `-gtk-icontheme()`, `-gtk-gradient()`, `cross-fade()`, `image()`,
    /// `-gtk-scaled()`, `-gtk-recolor()`. It is kept by name so the probe
    /// can report it, and paints nothing.
    Other(String),
}

/// One layer's `background-size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    /// `cover`.
    Cover,
    /// `contain`.
    Contain,
    /// Width and height, each `None` for `auto`.
    Explicit(Option<Length>, Option<Length>),
}

/// One layer's `background-repeat`, per axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    /// `repeat`.
    Repeat,
    /// `no-repeat`.
    NoRepeat,
    /// `space`.
    Space,
    /// `round`.
    Round,
}

/// Which box a background is clipped to or positioned in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    /// `border-box`.
    Border,
    /// `padding-box`.
    Padding,
    /// `content-box`.
    Content,
}

/// A border's style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    /// `none`: the width computes to zero.
    None,
    /// `hidden`: the width computes to zero.
    Hidden,
    /// `solid`.
    Solid,
    /// `inset`.
    Inset,
    /// `outset`.
    Outset,
    /// `dotted`.
    Dotted,
    /// `dashed`.
    Dashed,
    /// `double`.
    Double,
    /// `groove`.
    Groove,
    /// `ridge`.
    Ridge,
}

/// One shadow of `box-shadow` or `text-shadow`.
#[derive(Clone, Debug, PartialEq)]
pub struct Shadow {
    /// `inset`.
    pub inset: bool,
    /// Offset right.
    pub x: Length,
    /// Offset down.
    pub y: Length,
    /// Blur radius.
    pub blur: Length,
    /// Spread.
    pub spread: Length,
    /// Colour; `currentColor` when not given.
    pub color: Color,
}

/// A `font-size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontSize {
    /// A length; a percentage or `em` of the parent's size.
    Length(Length),
    /// A keyword's factor of `medium`: `xx-small` 3/5 up to `xx-large` 2.
    Absolute(f32),
    /// `smaller` or `larger`: a factor of the parent's size.
    Relative(f32),
}

/// A `font-weight`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontWeight {
    /// 100 to 1000; `normal` is 400 and `bold` 700.
    Number(u16),
    /// `bolder`.
    Bolder,
    /// `lighter`.
    Lighter,
}

/// A value that could not be parsed: GTK's message for it.
pub type Fail = String;

/// A cursor over one declaration's tokens.
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    tokens: &'a [Token],
    at: usize,
}

impl<'a> Cursor<'a> {
    /// A cursor at the start of `tokens`.
    #[must_use]
    pub fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, at: 0 }
    }

    /// Skip white space.
    pub fn skip(&mut self) {
        while self.tokens.get(self.at) == Some(&Token::Space) {
            self.at += 1;
        }
    }

    /// The next token that is not white space, not taken.
    #[must_use]
    pub fn peek(&self) -> Option<&'a Token> {
        let mut at = self.at;
        while self.tokens.get(at) == Some(&Token::Space) {
            at += 1;
        }
        self.tokens.get(at)
    }

    /// Take the next token that is not white space.
    pub fn next_token(&mut self) -> Option<&'a Token> {
        self.skip();
        let token = self.tokens.get(self.at)?;
        self.at += 1;
        Some(token)
    }

    /// Whether only white space is left.
    #[must_use]
    pub fn done(&self) -> bool {
        self.peek().is_none()
    }

    /// Take `token` if it is next.
    pub fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            let _ = self.next_token();
            true
        } else {
            false
        }
    }

    /// Take an identifier equal to `word`, ignoring case, if it is next.
    pub fn eat_ident(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(Token::Ident(name)) if name.eq_ignore_ascii_case(word)) {
            let _ = self.next_token();
            true
        } else {
            false
        }
    }

    /// The tokens inside a function whose name was just taken, up to its
    /// matching `)`, which is taken too.
    fn arguments(&mut self) -> Result<Cursor<'a>, Fail> {
        let start = self.at;
        let mut depth = 0usize;
        while let Some(token) = self.tokens.get(self.at) {
            self.at += 1;
            match token {
                Token::Function(_) | Token::OpenParen => depth += 1,
                Token::CloseParen if depth == 0 => {
                    return Ok(Cursor::new(
                        self.tokens.get(start..self.at - 1).unwrap_or_default(),
                    ));
                }
                Token::CloseParen => depth -= 1,
                _ => {}
            }
        }
        Err("Expected ')'".to_owned())
    }

    /// Split the rest at top-level commas.
    #[must_use]
    pub fn split_commas(&self) -> Vec<Cursor<'a>> {
        let rest = self.tokens.get(self.at..).unwrap_or_default();
        let mut out = Vec::new();
        let mut start = 0usize;
        let mut depth = 0usize;
        for (at, token) in rest.iter().enumerate() {
            match token {
                Token::Function(_) | Token::OpenParen => depth += 1,
                Token::CloseParen => depth = depth.saturating_sub(1),
                Token::Comma if depth == 0 => {
                    out.push(Cursor::new(rest.get(start..at).unwrap_or_default()));
                    start = at + 1;
                }
                _ => {}
            }
        }
        out.push(Cursor::new(rest.get(start..).unwrap_or_default()));
        out
    }

    /// The tokens taken since `start`, a copy of this cursor made earlier.
    #[must_use]
    pub fn since(&self, start: &Cursor<'a>) -> &'a [Token] {
        self.tokens.get(start.at..self.at).unwrap_or_default()
    }

    /// Everything left, taken.
    pub fn rest(&mut self) -> &'a [Token] {
        let rest = self.tokens.get(self.at..).unwrap_or_default();
        self.at = self.tokens.len();
        rest
    }
}

/// A plain number.
///
/// # Errors
///
/// When the next token is not one.
pub fn number(cursor: &mut Cursor<'_>) -> Result<f32, Fail> {
    match cursor.next_token() {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "CSS numbers are floats in GTK"
        )]
        Some(Token::Number(value)) => Ok(*value as f32),
        _ => Err("Expected a number".to_owned()),
    }
}

/// A number or a percentage as a fraction (`50%` is 0.5), as `alpha()`,
/// `shade()` and `mix()` take their factors.
fn factor(cursor: &mut Cursor<'_>) -> Result<f32, Fail> {
    match cursor.next_token() {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "CSS numbers are floats in GTK"
        )]
        Some(Token::Number(value)) => Ok(*value as f32),
        #[expect(
            clippy::cast_possible_truncation,
            reason = "CSS numbers are floats in GTK"
        )]
        Some(Token::Percentage(value)) => Ok(*value as f32 / 100.0),
        _ => Err("Expected a number".to_owned()),
    }
}

/// Pixels in one `unit`, at 96 dpi as GTK assumes, or `None` for an `em`
/// kind of unit or one GTK does not know.
fn absolute(unit: &str) -> Option<f32> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "px" => 1.0,
        "pt" => 96.0 / 72.0,
        "pc" => 16.0,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 96.0 / 25.4,
        _ => return None,
    })
}

/// Which lengths a property takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allow {
    /// A percentage.
    pub percent: bool,
    /// A negative value.
    pub negative: bool,
}

impl Allow {
    /// Lengths only, not below zero: padding, border widths, min sizes.
    pub const POSITIVE: Allow = Allow {
        percent: false,
        negative: false,
    };
    /// Lengths of either sign: margins, shadow offsets.
    pub const ANY: Allow = Allow {
        percent: false,
        negative: true,
    };
    /// Lengths and percentages: backgrounds, gradient stops.
    pub const PERCENT: Allow = Allow {
        percent: true,
        negative: true,
    };
}

/// A length, a percentage where allowed, or a `calc()`. A bare number is
/// taken as pixels, as GTK3 does with a deprecation warning for anything
/// but `0`.
///
/// # Errors
///
/// When it is not a length, is negative where that is refused, or is a
/// percentage where that is refused.
pub fn length(cursor: &mut Cursor<'_>, allow: Allow) -> Result<Length, Fail> {
    let value = match cursor.next_token() {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "CSS numbers are floats in GTK"
        )]
        Some(Token::Number(value)) => Length::px(*value as f32),
        Some(Token::Percentage(value)) if allow.percent => {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "CSS numbers are floats in GTK"
            )]
            let value = *value as f32;
            Length::percent(value)
        }
        Some(Token::Percentage(_)) => return Err("Percentages are not allowed here".to_owned()),
        Some(Token::Dimension(value, unit)) => {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "CSS numbers are floats in GTK"
            )]
            let value = *value as f32;
            if let Some(scale) = absolute(unit) {
                Length::px(value * scale)
            } else if unit.eq_ignore_ascii_case("em") || unit.eq_ignore_ascii_case("rem") {
                Length {
                    em: value,
                    ..Length::ZERO
                }
            } else if unit.eq_ignore_ascii_case("ex") {
                Length {
                    em: value / 2.0,
                    ..Length::ZERO
                }
            } else {
                return Err(format!("'{unit}' is not a valid unit"));
            }
        }
        Some(Token::Function(name)) if name.eq_ignore_ascii_case("calc") => {
            let mut inner = cursor.arguments()?;
            let value = calc_sum(&mut inner, allow)?;
            if !inner.done() {
                return Err("Expected ')' after calc() statement".to_owned());
            }
            return Ok(value);
        }
        _ => return Err("Expected a length".to_owned()),
    };
    if !allow.negative && (value.px < 0.0 || value.em < 0.0 || value.percent < 0.0) {
        return Err("Negative values are not allowed".to_owned());
    }
    Ok(value)
}

/// `calc()`'s sum: products joined by `+` and `-`.
fn calc_sum(cursor: &mut Cursor<'_>, allow: Allow) -> Result<Length, Fail> {
    let inner = Allow {
        negative: true,
        ..allow
    };
    let mut total = calc_product(cursor, inner)?;
    loop {
        match cursor.peek() {
            Some(Token::Delim('+')) => {
                let _ = cursor.next_token();
                let term = calc_product(cursor, inner)?;
                total = Length {
                    px: total.px + term.px,
                    em: total.em + term.em,
                    percent: total.percent + term.percent,
                };
            }
            Some(Token::Delim('-')) => {
                let _ = cursor.next_token();
                let term = calc_product(cursor, inner)?;
                total = Length {
                    px: total.px - term.px,
                    em: total.em - term.em,
                    percent: total.percent - term.percent,
                };
            }
            // `100% -24px` tokenizes the sign into the number; CSS requires
            // the space, and so does GTK.
            _ => return Ok(total),
        }
    }
}

fn calc_product(cursor: &mut Cursor<'_>, allow: Allow) -> Result<Length, Fail> {
    let mut value = calc_term(cursor, allow)?;
    loop {
        match cursor.peek() {
            Some(Token::Delim('*')) => {
                let _ = cursor.next_token();
                value = value.scaled(number(cursor)?);
            }
            Some(Token::Delim('/')) => {
                let _ = cursor.next_token();
                let by = number(cursor)?;
                if by == 0.0 {
                    return Err("Division by zero".to_owned());
                }
                value = value.scaled(1.0 / by);
            }
            _ => return Ok(value),
        }
    }
}

fn calc_term(cursor: &mut Cursor<'_>, allow: Allow) -> Result<Length, Fail> {
    match cursor.peek() {
        Some(Token::OpenParen) => {
            let _ = cursor.next_token();
            let mut inner = cursor.arguments()?;
            calc_sum(&mut inner, allow)
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "CSS numbers are floats in GTK"
        )]
        Some(Token::Number(value)) => {
            // A number followed by `*` scales what follows.
            let value = *value as f32;
            let mut ahead = cursor.clone();
            let _ = ahead.next_token();
            if ahead.peek() == Some(&Token::Delim('*')) {
                let _ = cursor.next_token();
                let _ = cursor.next_token();
                return Ok(calc_term(cursor, allow)?.scaled(value));
            }
            length(cursor, allow)
        }
        _ => length(cursor, allow),
    }
}

/// X11's colour names that a stylesheet is likely to use, as Pango's
/// `pango_color_parse` (which GDK uses) knows them: X11 values, so `green`
/// is `#00ff00` and `gray` is `#bebebe`, not CSS's.
const NAMES: [(&str, [u8; 3]); 24] = [
    ("black", [0, 0, 0]),
    ("white", [255, 255, 255]),
    ("red", [255, 0, 0]),
    ("green", [0, 255, 0]),
    ("blue", [0, 0, 255]),
    ("yellow", [255, 255, 0]),
    ("cyan", [0, 255, 255]),
    ("magenta", [255, 0, 255]),
    ("gray", [190, 190, 190]),
    ("grey", [190, 190, 190]),
    ("orange", [255, 165, 0]),
    ("purple", [160, 32, 240]),
    ("pink", [255, 192, 203]),
    ("brown", [165, 42, 42]),
    ("navy", [0, 0, 128]),
    ("maroon", [176, 48, 96]),
    ("silver", [192, 192, 192]),
    ("gold", [255, 215, 0]),
    ("violet", [238, 130, 238]),
    ("orchid", [218, 112, 214]),
    ("salmon", [250, 128, 114]),
    ("tomato", [255, 99, 71]),
    ("teal", [0, 128, 128]),
    ("lime", [0, 255, 0]),
];

/// `#rgb`, `#rrggbb`, `#rrrgggbbb` or `#rrrrggggbbbb`, as `gdk_rgba_parse`
/// takes them. (waybar rewrites `#rrggbbaa` to `rgba()` before GTK sees
/// the file; [`super::parse`] does the same.)
fn hex(text: &str) -> Option<Rgba> {
    if !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let per = match text.len() {
        3 => 1,
        6 => 2,
        9 => 3,
        12 => 4,
        _ => return None,
    };
    let channel = |index: usize| -> Option<f32> {
        let digits = text.get(index * per..(index + 1) * per)?;
        let value = u32::from_str_radix(digits, 16).ok()?;
        let max = (1u32 << (4 * per)) - 1;
        #[expect(clippy::cast_precision_loss, reason = "at most 16 bits")]
        let channel = value as f32 / max as f32;
        Some(channel)
    };
    Some(Rgba {
        r: channel(0)?,
        g: channel(1)?,
        b: channel(2)?,
        a: 1.0,
    })
}

/// A colour.
///
/// # Errors
///
/// When it is not one GTK3 parses.
pub fn color(cursor: &mut Cursor<'_>) -> Result<Color, Fail> {
    match cursor.next_token() {
        Some(Token::Hash(text, _)) => hex(text)
            .map(Color::Rgba)
            .ok_or_else(|| format!("'#{text}' is not a valid color name")),
        Some(Token::AtKeyword(name)) => Ok(Color::Named(name.clone())),
        Some(Token::Ident(name)) => {
            let lower = name.to_ascii_lowercase();
            if lower == "transparent" {
                return Ok(Color::Rgba(Rgba::TRANSPARENT));
            }
            if lower == "currentcolor" {
                return Ok(Color::Current);
            }
            NAMES
                .iter()
                .find(|(known, _)| *known == lower)
                .map(|(_, [r, g, b])| Color::Rgba(Rgba::rgb8(*r, *g, *b)))
                .ok_or_else(|| format!("'{name}' is not a valid color name"))
        }
        Some(Token::Function(name)) => {
            let name = name.to_ascii_lowercase();
            let inner = cursor.arguments()?;
            let parts = inner.split_commas();
            color_function(&name, parts)
        }
        _ => Err("Expected a valid color".to_owned()),
    }
}

fn finished<T>(cursor: &Cursor<'_>, value: T) -> Result<T, Fail> {
    if cursor.done() {
        Ok(value)
    } else {
        Err("Junk at end of value".to_owned())
    }
}

fn color_function(name: &str, mut parts: Vec<Cursor<'_>>) -> Result<Color, Fail> {
    let count = parts.len();
    let mut take = |index: usize| -> Option<Cursor<'_>> { parts.get_mut(index).map(|c| c.clone()) };
    match (name, count) {
        ("rgb" | "rgba", 3 | 4) => {
            let mut channels = [0f32; 4];
            channels[3] = 1.0;
            for (index, slot) in channels.iter_mut().enumerate().take(count) {
                let mut part = take(index).ok_or("Expected a number")?;
                let value = match part.next_token() {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "CSS numbers are floats in GTK"
                    )]
                    Some(Token::Number(value)) if index < 3 => *value as f32 / 255.0,
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "CSS numbers are floats in GTK"
                    )]
                    Some(Token::Number(value)) => *value as f32,
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "CSS numbers are floats in GTK"
                    )]
                    Some(Token::Percentage(value)) => *value as f32 / 100.0,
                    _ => return Err("Expected a number".to_owned()),
                };
                *slot = finished(&part, value.clamp(0.0, 1.0))?;
            }
            let [r, g, b, a] = channels;
            Ok(Color::Rgba(Rgba { r, g, b, a }))
        }
        ("alpha" | "shade", 2) => {
            let mut first = take(0).ok_or("Expected a valid color")?;
            let base = color(&mut first)?;
            let base = finished(&first, base)?;
            let mut second = take(1).ok_or("Expected a number")?;
            let by = factor(&mut second)?;
            let by = finished(&second, by)?;
            Ok(if name == "alpha" {
                Color::Alpha(Box::new(base), by)
            } else {
                Color::Shade(Box::new(base), by)
            })
        }
        ("lighter" | "darker", 1) => {
            let mut first = take(0).ok_or("Expected a valid color")?;
            let base = color(&mut first)?;
            let base = finished(&first, base)?;
            Ok(Color::Shade(
                Box::new(base),
                if name == "lighter" { 1.3 } else { 0.7 },
            ))
        }
        ("mix", 3) => {
            let mut first = take(0).ok_or("Expected a valid color")?;
            let a = color(&mut first)?;
            let a = finished(&first, a)?;
            let mut second = take(1).ok_or("Expected a valid color")?;
            let b = color(&mut second)?;
            let b = finished(&second, b)?;
            let mut third = take(2).ok_or("Expected a number")?;
            let by = factor(&mut third)?;
            let by = finished(&third, by)?;
            Ok(Color::Mix(Box::new(a), Box::new(b), by))
        }
        _ => Err(format!("'{name}' is not a valid color function")),
    }
}

/// GTK's `_gtk_rgba_shade`: into HLS, lightness and saturation multiplied
/// and clamped, back into RGB.
#[must_use]
pub fn shade(color: Rgba, factor: f32) -> Rgba {
    let (h, l, s) = rgb_to_hls(color.r, color.g, color.b);
    let l = (l * factor).clamp(0.0, 1.0);
    let s = (s * factor).clamp(0.0, 1.0);
    let (r, g, b) = hls_to_rgb(h, l, s);
    Rgba {
        r,
        g,
        b,
        a: color.a,
    }
}

fn rgb_to_hls(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return (0.0, l, 0.0);
    }
    let delta = max - min;
    let s = if l <= 0.5 {
        delta / (max + min)
    } else {
        delta / (2.0 - max - min)
    };
    let mut h = if (r - max).abs() < f32::EPSILON {
        (g - b) / delta
    } else if (g - max).abs() < f32::EPSILON {
        2.0 + (b - r) / delta
    } else {
        4.0 + (r - g) / delta
    };
    h *= 60.0;
    if h < 0.0 {
        h += 360.0;
    }
    (h, l, s)
}

fn hls_to_rgb(h: f32, l: f32, s: f32) -> (f32, f32, f32) {
    if s == 0.0 {
        return (l, l, l);
    }
    let m2 = if l <= 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let m1 = 2.0 * l - m2;
    let channel = |hue: f32| {
        let mut hue = hue;
        while hue > 360.0 {
            hue -= 360.0;
        }
        while hue < 0.0 {
            hue += 360.0;
        }
        if hue < 60.0 {
            m1 + (m2 - m1) * hue / 60.0
        } else if hue < 180.0 {
            m2
        } else if hue < 240.0 {
            m1 + (m2 - m1) * (240.0 - hue) / 60.0
        } else {
            m1
        }
    };
    (channel(h + 120.0), channel(h), channel(h - 120.0))
}

/// An image, or `none`.
///
/// # Errors
///
/// When it is neither an image GTK3 knows nor `none`.
pub fn image(cursor: &mut Cursor<'_>) -> Result<Image, Fail> {
    match cursor.next_token() {
        Some(Token::Ident(word)) if word.eq_ignore_ascii_case("none") => Ok(Image::None),
        Some(Token::Url(path)) => Ok(Image::Url(path.clone())),
        Some(Token::Function(name)) => {
            let lower = name.to_ascii_lowercase();
            let inner = cursor.arguments()?;
            match lower.as_str() {
                "url" => {
                    let mut inner = inner;
                    match inner.next_token() {
                        Some(Token::String(path)) => Ok(Image::Url(path.clone())),
                        _ => Err("Expected a string".to_owned()),
                    }
                }
                "linear-gradient" | "repeating-linear-gradient" => {
                    linear(&inner, lower.starts_with("repeating"))
                }
                "radial-gradient"
                | "repeating-radial-gradient"
                | "-gtk-icontheme"
                | "-gtk-gradient"
                | "cross-fade"
                | "image"
                | "-gtk-scaled"
                | "-gtk-recolor"
                | "-gtk-win32-theme-part" => Ok(Image::Other(lower)),
                _ => Err(format!("'{name}' is not a valid image")),
            }
        }
        _ => Err("Expected a valid image".to_owned()),
    }
}

fn linear(inner: &Cursor<'_>, repeating: bool) -> Result<Image, Fail> {
    let mut parts = inner.split_commas().into_iter().peekable();
    let mut angle = Angle::To(0, 1);
    if let Some(first) = parts.peek() {
        let mut probe = first.clone();
        match probe.peek() {
            Some(Token::Dimension(value, unit)) => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "CSS numbers are floats in GTK"
                )]
                let value = *value as f32;
                let degrees = match unit.to_ascii_lowercase().as_str() {
                    "deg" => value,
                    "rad" => value.to_degrees(),
                    "grad" => value * 0.9,
                    "turn" => value * 360.0,
                    _ => return Err(format!("'{unit}' is not a valid unit")),
                };
                let _ = probe.next_token();
                angle = finished(&probe, Angle::Degrees(degrees))?;
                let _ = parts.next();
            }
            Some(Token::Ident(word)) if word.eq_ignore_ascii_case("to") => {
                let _ = probe.next_token();
                let (mut x, mut y) = (0i8, 0i8);
                while let Some(Token::Ident(side)) = probe.peek() {
                    match side.to_ascii_lowercase().as_str() {
                        "left" => x = -1,
                        "right" => x = 1,
                        "top" => y = -1,
                        "bottom" => y = 1,
                        _ => return Err(format!("'{side}' is not a valid side")),
                    }
                    let _ = probe.next_token();
                }
                if x == 0 && y == 0 {
                    return Err("Expected side that gradient should go to".to_owned());
                }
                angle = finished(&probe, Angle::To(x, y))?;
                let _ = parts.next();
            }
            _ => {}
        }
    }
    let mut stops = Vec::new();
    for mut part in parts {
        let stop_color = color(&mut part)?;
        let at = if part.done() {
            None
        } else {
            Some(length(&mut part, Allow::PERCENT)?)
        };
        stops.push(finished(&part, (stop_color, at))?);
    }
    if stops.len() < 2 {
        return Err("Expected at least two color stops".to_owned());
    }
    Ok(Image::Linear(Linear {
        angle,
        stops,
        repeating,
    }))
}

/// One layer's `background-size`.
///
/// # Errors
///
/// When it is not `cover`, `contain`, or one or two lengths or `auto`s.
pub fn size(cursor: &mut Cursor<'_>) -> Result<Size, Fail> {
    if cursor.eat_ident("cover") {
        return finished(cursor, Size::Cover);
    }
    if cursor.eat_ident("contain") {
        return finished(cursor, Size::Contain);
    }
    let one = |cursor: &mut Cursor<'_>| -> Result<Option<Length>, Fail> {
        if cursor.eat_ident("auto") {
            Ok(None)
        } else {
            length(
                cursor,
                Allow {
                    percent: true,
                    negative: false,
                },
            )
            .map(Some)
        }
    };
    let width = one(cursor)?;
    let height = if cursor.done() { None } else { one(cursor)? };
    finished(cursor, Size::Explicit(width, height))
}

/// One layer's `background-position`: two offsets, each a percentage of
/// the room left and a length.
///
/// # Errors
///
/// When the keywords and lengths do not make a position.
pub fn position(cursor: &mut Cursor<'_>) -> Result<(Length, Length), Fail> {
    // Keywords first, in either order; then lengths. The four-value form
    // (`right 10px bottom 5px`) is taken too.
    let mut values: Vec<(Option<char>, Length)> = Vec::new();
    while !cursor.done() {
        if let Some(Token::Ident(word)) = cursor.peek() {
            let (axis, value) = match word.to_ascii_lowercase().as_str() {
                "left" => (Some('x'), Length::percent(0.0)),
                "right" => (Some('x'), Length::percent(100.0)),
                "top" => (Some('y'), Length::percent(0.0)),
                "bottom" => (Some('y'), Length::percent(100.0)),
                "center" => (None, Length::percent(50.0)),
                _ => return Err(format!("'{word}' is not a valid position")),
            };
            let _ = cursor.next_token();
            // An offset after a side keyword counts from that side.
            let offset = match cursor.peek() {
                Some(Token::Dimension(..) | Token::Percentage(_) | Token::Number(_))
                    if axis.is_some() =>
                {
                    Some(length(cursor, Allow::PERCENT)?)
                }
                _ => None,
            };
            let value = match offset {
                Some(offset) if value.percent > 0.0 => Length {
                    px: -offset.px,
                    em: -offset.em,
                    percent: 100.0 - offset.percent,
                },
                Some(offset) => offset,
                None => value,
            };
            values.push((axis, value));
        } else {
            values.push((Some('?'), length(cursor, Allow::PERCENT)?));
        }
        if values.len() > 2 {
            return Err("Too many values for background-position".to_owned());
        }
    }
    match values.as_slice() {
        [] => Err("Expected a position".to_owned()),
        [(axis, value)] => Ok(match axis {
            Some('y') => (Length::percent(50.0), *value),
            _ => (*value, Length::percent(50.0)),
        }),
        [(first_axis, first), (second_axis, second)] => {
            if *first_axis == Some('y') || *second_axis == Some('x') {
                if *first_axis == Some('x') || *second_axis == Some('y') {
                    return Err("Invalid combination of values".to_owned());
                }
                Ok((*second, *first))
            } else {
                Ok((*first, *second))
            }
        }
        _ => Err("Too many values for background-position".to_owned()),
    }
}

/// One layer's `background-repeat`.
///
/// # Errors
///
/// When it is not a repeat keyword or two.
pub fn repeat(cursor: &mut Cursor<'_>) -> Result<(Repeat, Repeat), Fail> {
    let word = |cursor: &mut Cursor<'_>| -> Result<Option<Repeat>, Fail> {
        let Some(Token::Ident(word)) = cursor.peek() else {
            return Ok(None);
        };
        let value = match word.to_ascii_lowercase().as_str() {
            "repeat" => Repeat::Repeat,
            "no-repeat" => Repeat::NoRepeat,
            "space" => Repeat::Space,
            "round" => Repeat::Round,
            _ => return Ok(None),
        };
        let _ = cursor.next_token();
        Ok(Some(value))
    };
    if cursor.eat_ident("repeat-x") {
        return finished(cursor, (Repeat::Repeat, Repeat::NoRepeat));
    }
    if cursor.eat_ident("repeat-y") {
        return finished(cursor, (Repeat::NoRepeat, Repeat::Repeat));
    }
    let Some(x) = word(cursor)? else {
        return Err("Not a valid value".to_owned());
    };
    let y = word(cursor)?.unwrap_or(x);
    finished(cursor, (x, y))
}

/// A box keyword for `background-clip` and `background-origin`.
///
/// # Errors
///
/// When it is not one.
pub fn area(cursor: &mut Cursor<'_>) -> Result<Area, Fail> {
    let value = match cursor.next_token() {
        Some(Token::Ident(word)) => match word.to_ascii_lowercase().as_str() {
            "border-box" => Area::Border,
            "padding-box" => Area::Padding,
            "content-box" => Area::Content,
            _ => return Err("Expected a box".to_owned()),
        },
        _ => return Err("Expected a box".to_owned()),
    };
    finished(cursor, value)
}

/// A border style keyword, if the next token is one.
pub fn border_style(cursor: &mut Cursor<'_>) -> Option<BorderStyle> {
    let Some(Token::Ident(word)) = cursor.peek() else {
        return None;
    };
    let value = match word.to_ascii_lowercase().as_str() {
        "none" => BorderStyle::None,
        "hidden" => BorderStyle::Hidden,
        "solid" => BorderStyle::Solid,
        "inset" => BorderStyle::Inset,
        "outset" => BorderStyle::Outset,
        "dotted" => BorderStyle::Dotted,
        "dashed" => BorderStyle::Dashed,
        "double" => BorderStyle::Double,
        "groove" => BorderStyle::Groove,
        "ridge" => BorderStyle::Ridge,
        _ => return None,
    };
    let _ = cursor.next_token();
    Some(value)
}

/// Whether the next token begins a colour.
#[must_use]
pub fn starts_color(cursor: &Cursor<'_>) -> bool {
    match cursor.peek() {
        Some(Token::Hash(..) | Token::AtKeyword(_)) => true,
        Some(Token::Function(name)) => matches!(
            name.to_ascii_lowercase().as_str(),
            "rgb" | "rgba" | "alpha" | "shade" | "mix" | "lighter" | "darker"
        ),
        Some(Token::Ident(name)) => {
            let lower = name.to_ascii_lowercase();
            lower == "transparent"
                || lower == "currentcolor"
                || NAMES.iter().any(|(known, _)| *known == lower)
        }
        _ => false,
    }
}

/// Whether the next token begins a length.
#[must_use]
pub fn starts_length(cursor: &Cursor<'_>) -> bool {
    match cursor.peek() {
        Some(Token::Number(_) | Token::Dimension(..) | Token::Percentage(_)) => true,
        Some(Token::Function(name)) => name.eq_ignore_ascii_case("calc"),
        _ => false,
    }
}

/// One shadow: `[inset] x y [blur [spread]] [color]`, in any order of the
/// three groups.
///
/// # Errors
///
/// When it is not one.
pub fn shadow(cursor: &mut Cursor<'_>, allow_inset: bool) -> Result<Shadow, Fail> {
    let mut inset = false;
    let mut lengths = Vec::new();
    let mut shade_color = None;
    while !cursor.done() {
        if allow_inset && cursor.eat_ident("inset") {
            inset = true;
        } else if starts_length(cursor) && lengths.is_empty() {
            while starts_length(cursor) && lengths.len() < 4 {
                lengths.push(length(cursor, Allow::ANY)?);
            }
        } else if shade_color.is_none() && starts_color(cursor) {
            shade_color = Some(color(cursor)?);
        } else {
            return Err("Junk at end of value".to_owned());
        }
    }
    let at = |index: usize| lengths.get(index).copied().unwrap_or(Length::ZERO);
    if lengths.len() < 2 {
        return Err("Expected a length".to_owned());
    }
    let blur = at(2);
    if blur.px < 0.0 {
        return Err("Negative values are not allowed".to_owned());
    }
    Ok(Shadow {
        inset,
        x: at(0),
        y: at(1),
        blur,
        spread: at(3),
        color: shade_color.unwrap_or(Color::Current),
    })
}

/// A `font-size`.
///
/// # Errors
///
/// When it is none of GTK3's.
pub fn font_size(cursor: &mut Cursor<'_>) -> Result<FontSize, Fail> {
    if let Some(Token::Ident(word)) = cursor.peek() {
        let factor = match word.to_ascii_lowercase().as_str() {
            "xx-small" => Some(FontSize::Absolute(3.0 / 5.0)),
            "x-small" => Some(FontSize::Absolute(3.0 / 4.0)),
            "small" => Some(FontSize::Absolute(8.0 / 9.0)),
            "medium" => Some(FontSize::Absolute(1.0)),
            "large" => Some(FontSize::Absolute(6.0 / 5.0)),
            "x-large" => Some(FontSize::Absolute(3.0 / 2.0)),
            "xx-large" => Some(FontSize::Absolute(2.0)),
            "smaller" => Some(FontSize::Relative(1.0 / 1.2)),
            "larger" => Some(FontSize::Relative(1.2)),
            _ => None,
        };
        if let Some(value) = factor {
            let _ = cursor.next_token();
            return finished(cursor, value);
        }
    }
    let value = length(
        cursor,
        Allow {
            percent: true,
            negative: false,
        },
    )?;
    finished(cursor, FontSize::Length(value))
}

/// A `font-weight`.
///
/// # Errors
///
/// When it is none of GTK3's.
pub fn font_weight(cursor: &mut Cursor<'_>) -> Result<FontWeight, Fail> {
    let value = match cursor.next_token() {
        Some(Token::Ident(word)) => match word.to_ascii_lowercase().as_str() {
            "normal" => FontWeight::Number(400),
            "bold" => FontWeight::Number(700),
            "bolder" => FontWeight::Bolder,
            "lighter" => FontWeight::Lighter,
            _ => return Err("Expected a valid font weight".to_owned()),
        },
        Some(Token::Number(value)) if (1.0..=1000.0).contains(value) => {
            #[expect(clippy::cast_possible_truncation, reason = "checked in 1..=1000")]
            #[expect(clippy::cast_sign_loss, reason = "checked in 1..=1000")]
            let weight = *value as u16;
            FontWeight::Number(weight)
        }
        _ => return Err("Expected a valid font weight".to_owned()),
    };
    finished(cursor, value)
}

/// A `font-family` list.
///
/// # Errors
///
/// When an entry is neither a string nor identifiers.
pub fn font_family(cursor: &Cursor<'_>) -> Result<Vec<String>, Fail> {
    let mut out = Vec::new();
    for mut part in cursor.split_commas() {
        let mut words = Vec::new();
        while let Some(token) = part.next_token() {
            match token {
                Token::String(name) => words.push(name.clone()),
                Token::Ident(name) => words.push(name.clone()),
                _ => return Err("Expected a font family name".to_owned()),
            }
        }
        if words.is_empty() {
            return Err("Expected a font family name".to_owned());
        }
        out.push(words.join(" "));
    }
    Ok(out)
}

/// A time for `transition-duration`: seconds.
///
/// # Errors
///
/// When it is not a time.
pub fn time(cursor: &mut Cursor<'_>) -> Result<f32, Fail> {
    match cursor.next_token() {
        Some(Token::Dimension(value, unit)) => {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "CSS numbers are floats in GTK"
            )]
            let value = *value as f32;
            match unit.to_ascii_lowercase().as_str() {
                "s" => Ok(value),
                "ms" => Ok(value / 1000.0),
                _ => Err(format!("'{unit}' is not a valid unit")),
            }
        }
        Some(Token::Number(value)) if *value == 0.0 => Ok(0.0),
        _ => Err("Expected a time".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::token::{Token, tokenize};
    use super::{
        Allow, Color, Cursor, Image, Length, Rgba, Size, color, image, length, position, shade,
        size,
    };

    fn tokens(text: &str) -> Vec<Token> {
        tokenize(text).into_iter().map(|(token, _)| token).collect()
    }

    #[test]
    fn hex_and_functions() {
        let all = tokens("#0e1b2c");
        assert_eq!(
            color(&mut Cursor::new(&all)),
            Ok(Color::Rgba(Rgba::rgb8(0x0e, 0x1b, 0x2c)))
        );
        let all = tokens("alpha(@ba-paper, 0.55)");
        assert_eq!(
            color(&mut Cursor::new(&all)),
            Ok(Color::Alpha(
                Box::new(Color::Named("ba-paper".into())),
                0.55
            ))
        );
        let all = tokens("rgba(255, 0, 0, 0.5)");
        assert_eq!(
            color(&mut Cursor::new(&all)),
            Ok(Color::Rgba(Rgba {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 0.5
            }))
        );
        let all = tokens("#12345");
        assert!(color(&mut Cursor::new(&all)).is_err());
    }

    #[test]
    fn shade_keeps_hue() {
        let grey = Rgba::rgb8(128, 128, 128);
        let lighter = shade(grey, 1.5);
        assert!((lighter.r - lighter.g).abs() < 1e-6 && lighter.r > grey.r);
        let blue = Rgba::rgb8(0x38, 0xa3, 0xec);
        let same = shade(blue, 1.0);
        assert!((same.r - blue.r).abs() < 1e-4 && (same.b - blue.b).abs() < 1e-4);
    }

    #[test]
    fn calc_takes_percent_minus_pixels() {
        let all = tokens("calc(100% - 24px)");
        assert_eq!(
            length(&mut Cursor::new(&all), Allow::PERCENT),
            Ok(Length {
                px: -24.0,
                em: 0.0,
                percent: 100.0
            })
        );
        let all = tokens("calc(2 * 3px + 1em)");
        assert_eq!(
            length(&mut Cursor::new(&all), Allow::PERCENT),
            Ok(Length {
                px: 6.0,
                em: 1.0,
                percent: 0.0
            })
        );
    }

    #[test]
    fn padding_refuses_negatives_and_margins_take_them() {
        let all = tokens("-2px");
        assert!(length(&mut Cursor::new(&all), Allow::POSITIVE).is_err());
        assert_eq!(
            length(&mut Cursor::new(&all), Allow::ANY),
            Ok(Length::px(-2.0))
        );
        let all = tokens("12pt");
        assert_eq!(
            length(&mut Cursor::new(&all), Allow::ANY),
            Ok(Length::px(16.0))
        );
    }

    #[test]
    fn images_and_gradients() {
        let all = tokens("url(\"icons/cap-l-soft.svg\")");
        assert_eq!(
            image(&mut Cursor::new(&all)),
            Ok(Image::Url("icons/cap-l-soft.svg".into()))
        );
        let all = tokens("linear-gradient(#16273c, #16273c)");
        assert!(matches!(
            image(&mut Cursor::new(&all)),
            Ok(Image::Linear(_))
        ));
        let all = tokens("linear-gradient(#16273c)");
        assert!(image(&mut Cursor::new(&all)).is_err());
    }

    #[test]
    fn sizes_and_positions() {
        let all = tokens("12px 100%");
        assert_eq!(
            size(&mut Cursor::new(&all)),
            Ok(Size::Explicit(
                Some(Length::px(12.0)),
                Some(Length::percent(100.0))
            ))
        );
        let all = tokens("center");
        assert_eq!(
            position(&mut Cursor::new(&all)),
            Ok((Length::percent(50.0), Length::percent(50.0)))
        );
        let all = tokens("12px 50%");
        assert_eq!(
            position(&mut Cursor::new(&all)),
            Ok((Length::px(12.0), Length::percent(50.0)))
        );
        let all = tokens("bottom left");
        assert_eq!(
            position(&mut Cursor::new(&all)),
            Ok((Length::percent(0.0), Length::percent(100.0)))
        );
    }
}
