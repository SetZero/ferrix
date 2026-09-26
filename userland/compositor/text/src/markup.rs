//! Pango markup: the subset waybar's tooltips and hyprlock's labels use.
//!
//! [`parse`] turns `<span foreground="#cccccc">Password</span>` into
//! [`Span`]s, each a piece of text with the attributes in force over it.
//! What Pango would refuse is refused here too, with Pango's wording, so a
//! program can fall back as upstream does (waybar and hyprlock both show
//! the text unmarked when `pango_parse_markup` fails).
//!
//! Tags: `<span>` (and its synonym `<markup>` at the root), `<b>`, `<big>`,
//! `<i>`, `<s>`, `<sub>`, `<sup>`, `<small>`, `<tt>`, `<u>`. `<span>`
//! attributes: `font`/`font_desc`, `font_family`/`face`, `size`/
//! `font_size`, `style`/`font_style`, `weight`/`font_weight`,
//! `foreground`/`fgcolor`/`color`, `background`/`bgcolor`, `alpha`/
//! `fgalpha`, `bgalpha`, `underline`, `underline_color`, `strikethrough`,
//! `rise`, `letter_spacing`, `line_height`. Others parse and are reported
//! in [`Parsed::ignored`]. Entities: `&amp; &lt; &gt; &quot; &apos;` and
//! `&#N;` / `&#xN;`.

use crate::{Style, Weight};

/// A colour, not premultiplied.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Rgba {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha, 255 opaque.
    pub a: u8,
}

impl Rgba {
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);

    /// An opaque colour.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// A colour as Pango's `pango_color_parse` reads one: `#rgb`,
    /// `#rrggbb`, `#rrrgggbbb`, `#rrrrggggbbbb`, `#rrggbbaa` (1.46), or one
    /// of the X11 colour names (`red`, `white`, `SteelBlue`, case ignored).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        parse_color(text).map(|(color, _)| color)
    }

    /// As tiny-skia's colour.
    #[must_use]
    pub fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }
}

/// How big a span is, as `size=` said it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SpanSize {
    /// An absolute size: `size="12pt"`, `size="12288"` (1024ths of a
    /// point), or `size="15px"`-shaped where the caller gave pixels.
    Size(crate::Size),
    /// A factor of the size around it: `larger`, `smaller`, `<big>`,
    /// `<small>` (1.2 each way, Pango's), `x-large` and the other CSS
    /// keywords (relative to the *base* size, as Pango does), or `150%`.
    Scale(f32),
}

/// `line_height=`: Pango 1.50's rule is that a value below 1024 is a
/// factor of the font's height and anything else is in 1024ths of a point.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LineHeight {
    /// A factor of the line's logical height: `'2.0'`.
    Factor(f32),
    /// An absolute height.
    Size(crate::Size),
}

/// `underline=`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Underline {
    /// None.
    #[default]
    None,
    /// One line (`single`, `true`, `<u>`).
    Single,
    /// Two.
    Double,
    /// One, thin and low (`low`).
    Low,
    /// A wavy line, which is drawn as `single` here.
    Error,
}

/// The attributes in force over a piece of text. `None` is "as around it",
/// down to the program's own defaults.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SpanStyle {
    /// A whole font description (`font=`), applied before the single fields.
    /// Its families replace the ones around it only if it has any, and its
    /// size only if it is more than nothing: [`parse`] gives a
    /// `font="Sans Bold"` with no size a size of zero points.
    pub font: Option<crate::FontDescription>,
    /// `font_family=`, a comma-separated list.
    pub family: Option<String>,
    /// `size=`, `<big>`, `<small>`.
    pub size: Option<SpanSize>,
    /// `style=`, `<i>`.
    pub style: Option<Style>,
    /// `weight=`, `<b>`.
    pub weight: Option<Weight>,
    /// `foreground=`.
    pub foreground: Option<Rgba>,
    /// `background=`: the span's logical rectangle is filled with it.
    pub background: Option<Rgba>,
    /// `underline=`, `<u>`.
    pub underline: Option<Underline>,
    /// `underline_color=`.
    pub underline_color: Option<Rgba>,
    /// `strikethrough=`, `<s>`.
    pub strikethrough: Option<bool>,
    /// `rise=` in pixels (converted from 1024ths of a point), `<sup>`,
    /// `<sub>`.
    pub rise: Option<f32>,
    /// `letter_spacing=` in pixels.
    pub letter_spacing: Option<f32>,
    /// `line_height=`.
    pub line_height: Option<LineHeight>,
    /// `<tt>`: the family `monospace`.
    pub monospace: bool,
}

/// A piece of text and the attributes over it.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Span {
    /// The text, entities resolved.
    pub text: String,
    /// The attributes, already combined from every enclosing tag.
    pub style: SpanStyle,
}

/// What [`parse`] made.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Parsed {
    /// The pieces, in order. Adjacent pieces with the same style are
    /// joined.
    pub spans: Vec<Span>,
    /// Attributes that were read and have no effect here, by name, once
    /// each: a program may report them.
    pub ignored: Vec<String>,
}

impl Parsed {
    /// The text with the markup taken away.
    #[must_use]
    pub fn plain(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// Parse `markup`.
///
/// # Errors
///
/// What `pango_parse_markup` would refuse, in its words: an unknown tag
/// (`Unknown tag 'blink' on line 1 char 7`), an unknown attribute, a bad
/// value, a tag closed that was not open, an unknown entity.
pub fn parse(markup: &str) -> Result<Parsed, String> {
    Parser::new(markup).run()
}

/// `text` as one unmarked span: what upstream shows when markup fails.
#[must_use]
pub fn plain(text: &str) -> Parsed {
    Parsed {
        spans: vec![Span {
            text: text.to_owned(),
            style: SpanStyle::default(),
        }],
        ignored: Vec::new(),
    }
}

/// `text` with `& < > " '` written as entities, as `g_markup_escape_text`
/// does, for putting a window title into markup.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// `pango_color_parse_with_alpha`: the colour, and whether the text gave
/// an alpha of its own (`#rgba`, `#rrggbbaa`, `#rrrrggggbbbbaaaa`).
fn parse_color(text: &str) -> Option<(Rgba, bool)> {
    let Some(hex) = text.strip_prefix('#') else {
        // A name, case ignored and spaces skipped, as Pango's table
        // lookup compares them.
        let key: String = text
            .chars()
            .filter(|character| *character != ' ')
            .map(|character| character.to_ascii_lowercase())
            .collect();
        let at = crate::colors::NAMES
            .binary_search_by(|(name, _)| (*name).cmp(key.as_str()))
            .ok()?;
        let &(_, [r, g, b]) = crate::colors::NAMES.get(at)?;
        return Some((Rgba::rgb(r, g, b), false));
    };
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let (digits, alpha) = match hex.len() {
        3 | 6 | 9 | 12 => (hex.len() / 3, false),
        4 | 8 | 16 => (hex.len() / 4, true),
        _ => return None,
    };
    // Each channel is widened to 16 bits by repeating its digits, as
    // Pango does (`#f00` is 0xffff red), and its high byte kept.
    let channel = |index: usize| -> Option<u8> {
        let part = hex.get(index * digits..(index + 1) * digits)?;
        let mut value = u32::from_str_radix(part, 16).ok()?;
        let mut bits = u32::try_from(digits * 4).ok()?;
        value <<= 16 - bits;
        while bits < 16 {
            value |= value >> bits;
            bits *= 2;
        }
        u8::try_from(value >> 8).ok()
    };
    let color = Rgba {
        r: channel(0)?,
        g: channel(1)?,
        b: channel(2)?,
        a: if alpha { channel(3)? } else { 255 },
    };
    Some((color, alpha))
}

/// Points in 1024ths (Pango units) to pixels at [`crate::DPI`].
fn units_to_pixels(units: f64) -> f32 {
    points_to_pixels(units / 1024.0)
}

/// Points to pixels at [`crate::DPI`].
fn points_to_pixels(points: f64) -> f32 {
    (points * f64::from(crate::DPI) / 72.0) as f32
}

/// Where a tag's size comes from, before `<big>` and `<small>`: Pango's
/// `OpenTag` `base_font_size` / `base_scale_factor` and `scale_level`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum SizeBase {
    /// The layout's own size.
    #[default]
    Layout,
    /// A factor of the layout's size (`x-large`, `150%`).
    Scale(f32),
    /// An absolute size (`12pt`, `font="Sans 12"`).
    Absolute(crate::Size),
}

/// A tag's size: a base and how many steps of 1.2 above or below it.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct SizeState {
    /// The base.
    base: SizeBase,
    /// `<big>` and `larger` add one, `<small>` and `smaller` take one.
    level: i32,
}

impl SizeState {
    /// What the span's `size` is.
    fn span_size(self) -> Option<SpanSize> {
        let factor = 1.2_f32.powi(self.level);
        match self.base {
            SizeBase::Layout if self.level == 0 => None,
            SizeBase::Layout => Some(SpanSize::Scale(factor)),
            SizeBase::Scale(scale) => Some(SpanSize::Scale(scale * factor)),
            SizeBase::Absolute(crate::Size::Points(points)) => {
                Some(SpanSize::Size(crate::Size::Points(points * factor)))
            }
            SizeBase::Absolute(crate::Size::Pixels(pixels)) => {
                Some(SpanSize::Size(crate::Size::Pixels(pixels * factor)))
            }
        }
    }
}

/// An open tag and what is in force inside it.
#[derive(Clone, Debug, Default)]
struct Open {
    /// Its name.
    name: String,
    /// The attributes in force.
    style: SpanStyle,
    /// The size, of which `style.size` is the result.
    size: SizeState,
    /// The foreground as given, before `alpha`.
    foreground: Option<Rgba>,
    /// `alpha=`.
    foreground_alpha: Option<u8>,
    /// The background as given, before `bgalpha`.
    background: Option<Rgba>,
    /// `bgalpha=`.
    background_alpha: Option<u8>,
}

/// The `<span>` attributes, by what they set: each synonym is one of these.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Attribute {
    Font,
    Family,
    Size,
    Style,
    Weight,
    Foreground,
    Background,
    Alpha,
    BackgroundAlpha,
    Underline,
    UnderlineColor,
    Strikethrough,
    Rise,
    LetterSpacing,
    LineHeight,
    /// Read, checked for being given twice, and without effect here.
    Ignored(&'static str),
}

/// A `<span>` attribute's name onto what it sets, `None` for a name Pango
/// does not know.
fn span_attribute(name: &str) -> Option<Attribute> {
    Some(match name {
        "font" | "font_desc" => Attribute::Font,
        "font_family" | "face" => Attribute::Family,
        "size" | "font_size" => Attribute::Size,
        "style" | "font_style" => Attribute::Style,
        "weight" | "font_weight" => Attribute::Weight,
        "foreground" | "fgcolor" | "color" => Attribute::Foreground,
        "background" | "bgcolor" => Attribute::Background,
        "alpha" | "fgalpha" => Attribute::Alpha,
        "bgalpha" => Attribute::BackgroundAlpha,
        "underline" => Attribute::Underline,
        "underline_color" => Attribute::UnderlineColor,
        "strikethrough" => Attribute::Strikethrough,
        "rise" => Attribute::Rise,
        "letter_spacing" => Attribute::LetterSpacing,
        "line_height" => Attribute::LineHeight,
        "variant" | "font_variant" => Attribute::Ignored("variant"),
        "stretch" | "font_stretch" => Attribute::Ignored("stretch"),
        "font_features" => Attribute::Ignored("font_features"),
        "strikethrough_color" => Attribute::Ignored("strikethrough_color"),
        "overline" => Attribute::Ignored("overline"),
        "overline_color" => Attribute::Ignored("overline_color"),
        "fallback" => Attribute::Ignored("fallback"),
        "lang" => Attribute::Ignored("lang"),
        "gravity" => Attribute::Ignored("gravity"),
        "gravity_hint" => Attribute::Ignored("gravity_hint"),
        "show" => Attribute::Ignored("show"),
        "insert_hyphens" => Attribute::Ignored("insert_hyphens"),
        "allow_breaks" => Attribute::Ignored("allow_breaks"),
        "baseline_shift" => Attribute::Ignored("baseline_shift"),
        "font_scale" => Attribute::Ignored("font_scale"),
        "text_transform" => Attribute::Ignored("text_transform"),
        "segment" => Attribute::Ignored("segment"),
        _ => return None,
    })
}

/// A number in Pango units (1024ths of a point), or `Npt`, as pixels.
fn length(value: &str) -> Option<f32> {
    if let Some(points) = value.strip_suffix("pt") {
        let points = points.trim().parse::<f64>().ok()?;
        return points.is_finite().then(|| points_to_pixels(points));
    }
    let units = value.trim().parse::<i64>().ok()?;
    #[expect(clippy::cast_precision_loss, reason = "a length in Pango units")]
    Some(units_to_pixels(units as f64))
}

/// `span_parse_boolean`.
fn boolean(value: &str) -> Option<bool> {
    match value {
        "true" | "yes" | "t" | "y" | "1" => Some(true),
        "false" | "no" | "f" | "n" | "0" => Some(false),
        _ => None,
    }
}

/// `alpha=`: `50%`, or 1 to 65536 in 65536ths.
fn alpha(value: &str) -> Option<u8> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent.parse::<f64>().ok()?;
        if !(0.0..=100.0).contains(&percent) {
            return None;
        }
        return u8::try_from((percent * 255.0 / 100.0).round() as u32).ok();
    }
    let number = value.parse::<u32>().ok()?;
    if !(1..=65536).contains(&number) {
        return None;
    }
    u8::try_from((u64::from(number) * 255 / 65535).min(255)).ok()
}

/// A CSS size keyword onto its factor of the base size: 1.2 to the power
/// of its distance from `medium`.
fn size_keyword(value: &str) -> Option<f32> {
    let level = match value {
        "xx-small" => -3,
        "x-small" => -2,
        "small" => -1,
        "medium" => 0,
        "large" => 1,
        "x-large" => 2,
        "xx-large" => 3,
        _ => return None,
    };
    Some(1.2_f32.powi(level))
}

/// A line and a column, both from one.
#[derive(Clone, Copy, Debug)]
struct Position {
    line: usize,
    column: usize,
}

/// The markup parser: `GMarkup`'s syntax, Pango's tags.
struct Parser {
    /// The markup.
    chars: Vec<char>,
    /// Where the parser is.
    at: usize,
    /// The open tags, root first.
    stack: Vec<Open>,
    /// Whether the root `<markup>` is Pango's, not the text's.
    implicit: bool,
    /// Whether the root element has been closed.
    root_closed: bool,
    /// The text read since the last tag.
    text: String,
    /// The pieces so far.
    spans: Vec<Span>,
    /// Attributes read to no effect.
    ignored: Vec<String>,
}

impl Parser {
    fn new(markup: &str) -> Self {
        let implicit = !markup.starts_with("<markup>");
        let stack = if implicit {
            vec![Open {
                name: "markup".to_owned(),
                ..Open::default()
            }]
        } else {
            Vec::new()
        };
        Self {
            chars: markup.chars().collect(),
            at: 0,
            stack,
            implicit,
            root_closed: false,
            text: String::new(),
            spans: Vec::new(),
            ignored: Vec::new(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        text.chars()
            .enumerate()
            .all(|(offset, character)| self.peek_at(offset) == Some(character))
    }

    /// Where character `index` is.
    fn position(&self, index: usize) -> Position {
        let mut position = Position { line: 1, column: 1 };
        for &character in self.chars.iter().take(index) {
            if character == '\n' {
                position.line += 1;
                position.column = 1;
            } else {
                position.column += 1;
            }
        }
        position
    }

    /// `GMarkup`'s error, with its prefix, at character `index`.
    fn syntax(&self, index: usize, message: &str) -> String {
        let position = self.position(index);
        format!(
            "Error on line {} char {}: {message}",
            position.line, position.column
        )
    }

    fn run(mut self) -> Result<Parsed, String> {
        while let Some(character) = self.peek() {
            match character {
                '<' => {
                    self.flush();
                    self.tag()?;
                }
                '&' => {
                    let character = self.entity()?;
                    self.push_text(character)?;
                }
                other => {
                    self.at += 1;
                    self.push_text(other)?;
                }
            }
        }
        self.flush();
        let end = self.chars.len();
        if self.implicit {
            if self.root_closed {
                return Err(self.syntax(
                    end,
                    "Element \u{201c}markup\u{201d} was closed, no element is currently open",
                ));
            }
        } else if !self.root_closed && self.stack.is_empty() {
            return Err(self.syntax(end, "Document was empty or contained only whitespace"));
        }
        let still_open = if self.implicit { 1 } else { 0 };
        if self.stack.len() > still_open
            && let Some(last) = self.stack.last()
        {
            return Err(self.syntax(
                end,
                &format!(
                    "Document ended unexpectedly with elements still open \u{2014} \
                     \u{201c}{}\u{201d} was the last element opened",
                    last.name
                ),
            ));
        }
        Ok(Parsed {
            spans: self.spans,
            ignored: self.ignored,
        })
    }

    /// A character of text, which only an element may hold.
    fn push_text(&mut self, character: char) -> Result<(), String> {
        if self.stack.is_empty() {
            if character.is_whitespace() {
                return Ok(());
            }
            let message = if self.root_closed {
                "Extra content at the end of the document"
            } else {
                "Document must begin with an element (e.g. <book>)"
            };
            return Err(self.syntax(self.at.saturating_sub(1), message));
        }
        self.text.push(character);
        Ok(())
    }

    /// The text read so far as a span in the innermost tag's style, joined
    /// to the one before when their styles are the same.
    fn flush(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        let style = self
            .stack
            .last()
            .map(|open| open.style.clone())
            .unwrap_or_default();
        if let Some(last) = self.spans.last_mut()
            && last.style == style
        {
            last.text.push_str(&text);
            return;
        }
        self.spans.push(Span { text, style });
    }

    /// `&…;` at the parser, as the character it stands for.
    fn entity(&mut self) -> Result<char, String> {
        let start = self.at;
        self.at += 1;
        let Some(first) = self.peek() else {
            return Err(self.syntax(
                start,
                "Document ended unexpectedly just after an open angle bracket \u{201c}&\u{201d}",
            ));
        };
        if !(first.is_alphabetic() || first == '#' || first == '_' || first == ':') {
            return Err(self.syntax(
                self.at,
                &format!(
                    "Character \u{201c}{first}\u{201d} is not valid at the start of an entity \
                     name; the & character begins an entity; if this ampersand isn't supposed \
                     to be an entity, escape it as &amp;"
                ),
            ));
        }
        let mut name = String::new();
        loop {
            match self.peek() {
                Some(';') => break,
                Some(character)
                    if character.is_alphanumeric()
                        || matches!(character, '#' | '_' | ':' | '-' | '.') =>
                {
                    name.push(character);
                    self.at += 1;
                }
                _ => {
                    return Err(self.syntax(
                        self.at,
                        "Entity did not end with a semicolon; most likely you used an \
                         ampersand character without intending to start an entity \u{2014} \
                         escape ampersand as &amp;",
                    ));
                }
            }
        }
        self.at += 1;
        let character = match name.as_str() {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let Some(number) = name.strip_prefix('#') else {
                    return Err(self.syntax(
                        start,
                        &format!("Entity name \u{201c}{name}\u{201d} is not known"),
                    ));
                };
                let value = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => number.parse::<u32>(),
                };
                let Ok(value) = value else {
                    return Err(self.syntax(
                        start,
                        &format!(
                            "Failed to parse \u{201c}{number}\u{201d}, which should have been a \
                             digit inside a character reference (&#234; for example) \u{2014} \
                             perhaps the digit is too large"
                        ),
                    ));
                };
                match char::from_u32(value).filter(|character| *character != '\0') {
                    Some(character) => character,
                    None => {
                        return Err(self.syntax(
                            start,
                            &format!(
                                "Character reference \u{201c}{number}\u{201d} does not encode a \
                                 permitted character"
                            ),
                        ));
                    }
                }
            }
        };
        Ok(character)
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += 1;
        }
    }

    fn name(&mut self) -> String {
        let mut name = String::new();
        while let Some(character) = self.peek() {
            if character.is_alphanumeric() || matches!(character, '_' | ':' | '-' | '.') {
                name.push(character);
                self.at += 1;
            } else {
                break;
            }
        }
        name
    }

    fn ended(&self, inside: &str) -> String {
        self.syntax(
            self.chars.len(),
            &format!("Document ended unexpectedly inside {inside}"),
        )
    }

    /// Skip to just past `end`.
    fn skip_past(&mut self, end: &str, inside: &str) -> Result<(), String> {
        while !self.starts_with(end) {
            if self.peek().is_none() {
                return Err(self.ended(inside));
            }
            self.at += 1;
        }
        self.at += end.chars().count();
        Ok(())
    }

    /// A tag, a comment, a CDATA section or a processing instruction.
    fn tag(&mut self) -> Result<(), String> {
        if self.starts_with("<!--") {
            return self.skip_past("-->", "a comment");
        }
        if self.starts_with("<![CDATA[") {
            self.at += "<![CDATA[".len();
            while !self.starts_with("]]>") {
                let Some(character) = self.peek() else {
                    return Err(self.ended("a CDATA section"));
                };
                self.at += 1;
                self.push_text(character)?;
            }
            self.at += 3;
            return Ok(());
        }
        if self.starts_with("<?") {
            return self.skip_past("?>", "a processing instruction");
        }
        if self.starts_with("<!") {
            return self.skip_past(">", "a declaration");
        }
        self.at += 1;
        let closing = self.peek() == Some('/');
        if closing {
            self.at += 1;
        }
        match self.peek() {
            Some(character)
                if character.is_alphabetic() || character == '_' || character == ':' => {}
            Some(character) => {
                return Err(self.syntax(
                    self.at,
                    &format!(
                        "\u{201c}{character}\u{201d} is not a valid character following a \
                         \u{201c}<\u{201d} character; it may not begin an element name"
                    ),
                ));
            }
            None => return Err(self.ended("an element name")),
        }
        let name = self.name();
        if closing {
            self.skip_whitespace();
            match self.peek() {
                Some('>') => {}
                Some(character) => {
                    return Err(self.syntax(
                        self.at,
                        &format!(
                            "\u{201c}{character}\u{201d} is not a valid character following the \
                             close element name \u{201c}{name}\u{201d}; the allowed character is \
                             \u{201c}>\u{201d}"
                        ),
                    ));
                }
                None => {
                    return Err(
                        self.ended(&format!("the close tag for element \u{201c}{name}\u{201d}"))
                    );
                }
            }
            let end = self.at;
            self.at += 1;
            return self.close(&name, end);
        }
        let mut attributes: Vec<(String, String)> = Vec::new();
        let self_closing = loop {
            self.skip_whitespace();
            match self.peek() {
                Some('>') => break false,
                Some('/') if self.peek_at(1) == Some('>') => {
                    self.at += 1;
                    break true;
                }
                Some(character) if character.is_alphabetic() || character == '_' => {
                    let attribute = self.name();
                    self.skip_whitespace();
                    if self.peek() != Some('=') {
                        let found = self.peek().map(String::from).unwrap_or_default();
                        return Err(self.syntax(
                            self.at,
                            &format!(
                                "Odd character \u{201c}{found}\u{201d}, expected a \u{201c}=\u{201d} \
                                 after attribute name \u{201c}{attribute}\u{201d} of element \
                                 \u{201c}{name}\u{201d}"
                            ),
                        ));
                    }
                    self.at += 1;
                    self.skip_whitespace();
                    let value = self.value(&attribute, &name)?;
                    attributes.push((attribute, value));
                }
                Some(character) => {
                    return Err(self.syntax(
                        self.at,
                        &format!(
                            "Odd character \u{201c}{character}\u{201d}, expected a \u{201c}>\u{201d} \
                             or \u{201c}/\u{201d} character to end the start tag of element \
                             \u{201c}{name}\u{201d}, or optionally an attribute"
                        ),
                    ));
                }
                None => {
                    return Err(self.ended(&format!(
                        "the opening tag of element \u{201c}{name}\u{201d}"
                    )));
                }
            }
        };
        let end = self.at;
        self.at += 1;
        self.open(&name, &attributes, end)?;
        if self_closing {
            self.close(&name, end)?;
        }
        Ok(())
    }

    /// A quoted attribute value, entities resolved.
    fn value(&mut self, attribute: &str, element: &str) -> Result<String, String> {
        let quote = match self.peek() {
            Some(quote @ ('"' | '\'')) => quote,
            other => {
                let found = other.map(String::from).unwrap_or_default();
                return Err(self.syntax(
                    self.at,
                    &format!(
                        "Odd character \u{201c}{found}\u{201d}, expected an open quote mark after \
                         the equals sign when giving value for attribute \u{201c}{attribute}\u{201d} \
                         of element \u{201c}{element}\u{201d}"
                    ),
                ));
            }
        };
        self.at += 1;
        let mut value = String::new();
        loop {
            match self.peek() {
                Some(character) if character == quote => {
                    self.at += 1;
                    return Ok(value);
                }
                Some('&') => value.push(self.entity()?),
                Some(character) => {
                    value.push(character);
                    self.at += 1;
                }
                None => {
                    return Err(self.ended(&format!(
                        "the value of attribute \u{201c}{attribute}\u{201d}"
                    )));
                }
            }
        }
    }

    fn close(&mut self, name: &str, end: usize) -> Result<(), String> {
        let Some(open) = self.stack.last() else {
            return Err(self.syntax(
                end,
                &format!("Element \u{201c}{name}\u{201d} was closed, no element is currently open"),
            ));
        };
        if open.name != name {
            return Err(self.syntax(
                end,
                &format!(
                    "Element \u{201c}{name}\u{201d} was closed, but the currently open element \
                     is \u{201c}{}\u{201d}",
                    open.name
                ),
            ));
        }
        let _ = self.stack.pop();
        if self.stack.is_empty() {
            self.root_closed = true;
        }
        Ok(())
    }

    /// A start tag, as Pango's tag handlers take it.
    fn open(
        &mut self,
        name: &str,
        attributes: &[(String, String)],
        end: usize,
    ) -> Result<(), String> {
        let position = self.position(end);
        if self.stack.is_empty() && self.root_closed {
            return Err(self.syntax(end, "Extra content at the end of the document"));
        }
        let mut open = self.stack.last().cloned().unwrap_or_default();
        open.name = name.to_owned();
        let no_attributes = |tag: &str| -> Result<(), String> {
            match attributes.first() {
                Some((attribute, _)) => Err(format!(
                    "Attribute '{attribute}' is not allowed on the <{tag}> tag on line {} char {}",
                    position.line, position.column
                )),
                None => Ok(()),
            }
        };
        match name {
            "markup" => no_attributes(name)?,
            "b" => {
                no_attributes(name)?;
                open.style.weight = Some(Weight::BOLD);
            }
            "big" => {
                no_attributes(name)?;
                open.size.level += 1;
            }
            "i" => {
                no_attributes(name)?;
                open.style.style = Some(Style::Italic);
            }
            "s" => {
                no_attributes(name)?;
                open.style.strikethrough = Some(true);
            }
            "sub" | "sup" => {
                no_attributes(name)?;
                open.size.level -= 1;
                let rise = units_to_pixels(5000.0);
                open.style.rise = Some(if name == "sub" { -rise } else { rise });
            }
            "small" => {
                no_attributes(name)?;
                open.size.level -= 1;
            }
            "tt" => {
                no_attributes(name)?;
                open.style.monospace = true;
                open.style.family = None;
            }
            "u" => {
                no_attributes(name)?;
                open.style.underline = Some(Underline::Single);
            }
            "span" => self.span(&mut open, attributes, position)?,
            _ => {
                return Err(format!(
                    "Unknown tag '{name}' on line {} char {}",
                    position.line, position.column
                ));
            }
        }
        open.style.size = open.size.span_size();
        open.style.foreground = open.foreground.map(|color| match open.foreground_alpha {
            Some(a) => Rgba { a, ..color },
            None => color,
        });
        open.style.background = open.background.map(|color| match open.background_alpha {
            Some(a) => Rgba { a, ..color },
            None => color,
        });
        self.stack.push(open);
        Ok(())
    }

    /// `<span>`'s attributes, in the order Pango's `span_parse_func` applies
    /// them whatever order they are written in.
    fn span(
        &mut self,
        open: &mut Open,
        attributes: &[(String, String)],
        position: Position,
    ) -> Result<(), String> {
        let line = position.line;
        let mut given: Vec<(Attribute, &str, &str)> = Vec::new();
        for (name, value) in attributes {
            let Some(attribute) = span_attribute(name) else {
                return Err(format!(
                    "Attribute '{name}' is invalid on <span> tag, line {line}"
                ));
            };
            if given.iter().any(|(seen, _, _)| *seen == attribute) {
                return Err(format!(
                    "Attribute '{name}' occurs twice on <span> tag on line {line} char {}",
                    position.column
                ));
            }
            given.push((attribute, name.as_str(), value.as_str()));
        }
        given.sort_by_key(|(attribute, _, _)| *attribute);
        let color = |name: &str, value: &str| {
            parse_color(value).ok_or_else(|| {
                format!(
                    "Value of '{name}' attribute on <span> tag on line {line} could not be \
                     parsed; should be a color specification, not '{value}'"
                )
            })
        };
        let integer = |name: &str, value: &str| {
            length(value).ok_or_else(|| {
                format!(
                    "Value of '{name}' attribute on <span> tag on line {line} could not be \
                     parsed; should be an integer, not '{value}'"
                )
            })
        };
        // An alpha with no colour to apply to, here or around it.
        let mut alphas: Vec<(&str, bool)> = Vec::new();
        for (attribute, name, value) in given {
            match attribute {
                Attribute::Font => {
                    let (mut font, sized) = crate::describe::pango_parts(value);
                    if !font.families.is_empty() {
                        open.style.family = None;
                        open.style.monospace = false;
                    }
                    // A description's style fields are always set.
                    open.style.weight = None;
                    open.style.style = None;
                    if sized {
                        open.size = SizeState {
                            base: SizeBase::Absolute(font.size),
                            level: 0,
                        };
                    } else {
                        // No size: the one around it stands (a size of
                        // nothing is what Pango's unset size is).
                        font.size = crate::Size::Points(0.0);
                    }
                    open.style.font = Some(font);
                }
                Attribute::Family => {
                    open.style.family = Some(value.to_owned());
                    open.style.monospace = false;
                }
                Attribute::Size => open.size = parse_size(open.size, value, line)?,
                Attribute::Style => {
                    open.style.style = Some(match value.to_ascii_lowercase().as_str() {
                        "normal" => Style::Normal,
                        "oblique" => Style::Oblique,
                        "italic" => Style::Italic,
                        _ => {
                            return Err(format!(
                                "'{value}' is not a valid value for the '{name}' attribute on \
                                 <span> tag, line {line}; valid values are \
                                 'normal', 'oblique', 'italic'"
                            ));
                        }
                    });
                }
                Attribute::Weight => {
                    let weight = Weight::from_name(value).ok_or_else(|| {
                        format!(
                            "'{value}' is not a valid value for the 'weight' attribute on <span> \
                             tag, line {line}; valid values are for example 'light', \
                             'ultrabold' or a number"
                        )
                    })?;
                    open.style.weight = Some(weight);
                }
                Attribute::Foreground => {
                    let (rgba, has_alpha) = color(name, value)?;
                    open.foreground = Some(rgba);
                    if has_alpha {
                        open.foreground_alpha = Some(rgba.a);
                    }
                }
                Attribute::Background => {
                    let (rgba, has_alpha) = color(name, value)?;
                    open.background = Some(rgba);
                    if has_alpha {
                        open.background_alpha = Some(rgba.a);
                    }
                }
                Attribute::Alpha => {
                    open.foreground_alpha = Some(parse_alpha(name, value, line)?);
                    alphas.push((name, true));
                }
                Attribute::BackgroundAlpha => {
                    open.background_alpha = Some(parse_alpha(name, value, line)?);
                    alphas.push((name, false));
                }
                Attribute::Underline => {
                    open.style.underline = Some(match value.to_ascii_lowercase().as_str() {
                        "none" | "false" | "0" => Underline::None,
                        "single" | "single-line" | "true" | "1" => Underline::Single,
                        "double" | "double-line" | "2" => Underline::Double,
                        "low" | "3" => Underline::Low,
                        "error" | "error-line" | "4" => Underline::Error,
                        _ => {
                            return Err(format!(
                                "'{value}' is not a valid value for the 'underline' attribute \
                                 on <span> tag, line {line}; valid values are \
                                 none/single/double/low/error/single-line/double-line/error-line"
                            ));
                        }
                    });
                }
                Attribute::UnderlineColor => {
                    open.style.underline_color = Some(color(name, value)?.0);
                }
                Attribute::Strikethrough => {
                    let struck = boolean(value).ok_or_else(|| {
                        format!(
                            "Value of '{name}' attribute on <span> tag line {line} could not be \
                             parsed; should be 'true' or 'false', not '{value}'"
                        )
                    })?;
                    open.style.strikethrough = Some(struck);
                }
                Attribute::Rise => open.style.rise = Some(integer(name, value)?),
                Attribute::LetterSpacing => {
                    open.style.letter_spacing = Some(integer(name, value)?);
                }
                Attribute::LineHeight => {
                    open.style.line_height = Some(parse_line_height(value).ok_or_else(|| {
                        format!(
                            "Value of '{name}' attribute on <span> tag on line {line} could not \
                             be parsed; should be a number, not '{value}'"
                        )
                    })?);
                }
                Attribute::Ignored(_) => self.ignore(name),
            }
        }
        for (name, foreground) in alphas {
            let colour = if foreground {
                open.foreground
            } else {
                open.background
            };
            if colour.is_none() {
                self.ignore(name);
            }
        }
        Ok(())
    }

    fn ignore(&mut self, name: &str) {
        if !self.ignored.iter().any(|seen| seen == name) {
            self.ignored.push(name.to_owned());
        }
    }
}

/// `alpha=` and `bgalpha=`, or Pango's refusal.
fn parse_alpha(name: &str, value: &str, line: usize) -> Result<u8, String> {
    alpha(value).ok_or_else(|| {
        format!(
            "Value of '{name}' attribute on <span> tag on line {line} could not be parsed; \
             should be an integer, or a string such as '50%', not '{value}'"
        )
    })
}

/// `size=`: Pango units, `Npt`, `Npx`, `N%`, `larger`, `smaller` or a CSS
/// keyword, onto the tag's size.
///
/// A number is 1024ths of a point, and may have a fraction here where Pango
/// takes only an integer.
fn parse_size(size: SizeState, value: &str, line: usize) -> Result<SizeState, String> {
    let absolute = |size: crate::Size| SizeState {
        base: SizeBase::Absolute(size),
        level: 0,
    };
    let number = |text: &str| {
        text.trim()
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite() && *number >= 0.0)
    };
    let parsed = if let Some(points) = value.strip_suffix("pt") {
        number(points).map(|points| absolute(crate::Size::Points(points as f32)))
    } else if let Some(pixels) = value.strip_suffix("px") {
        number(pixels).map(|pixels| absolute(crate::Size::Pixels(pixels as f32)))
    } else if let Some(percent) = value.strip_suffix('%') {
        number(percent).map(|percent| SizeState {
            base: SizeBase::Scale((percent / 100.0) as f32),
            level: 0,
        })
    } else if value.starts_with(|character: char| character.is_ascii_digit()) {
        number(value).map(|units| absolute(crate::Size::Points((units / 1024.0) as f32)))
    } else if value == "smaller" {
        Some(SizeState {
            level: size.level - 1,
            ..size
        })
    } else if value == "larger" {
        Some(SizeState {
            level: size.level + 1,
            ..size
        })
    } else {
        size_keyword(value).map(|factor| SizeState {
            base: SizeBase::Scale(factor),
            level: 0,
        })
    };
    parsed.ok_or_else(|| {
        format!(
            "Value of 'size' attribute on <span> tag on line {line} could not be parsed; should \
             be an integer, or a string such as 'small', not '{value}'"
        )
    })
}

/// `line_height=`: a factor, 1024ths of a point for a whole number above
/// 1024 (Pango 1.50's rule), or `Npt`.
fn parse_line_height(value: &str) -> Option<LineHeight> {
    if let Some(points) = value.strip_suffix("pt") {
        let points = points.trim().parse::<f32>().ok()?;
        return (points.is_finite() && points >= 0.0)
            .then_some(LineHeight::Size(crate::Size::Points(points)));
    }
    let number = value.trim().parse::<f64>().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    Some(if number > 1024.0 && !value.contains('.') {
        LineHeight::Size(crate::Size::Points((number / 1024.0) as f32))
    } else {
        LineHeight::Factor(number as f32)
    })
}
