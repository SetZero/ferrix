//! Declarations: property names, shorthands, and what each value parses
//! into.
//!
//! GTK3 knows a fixed set of properties (`gtkcssstylepropertyimpl.c` and
//! `gtkcssshorthandpropertyimpl.c`). A name outside it is the parse error
//! `'name' is not a valid property name` and the declaration is dropped; a
//! name inside it whose value does not parse is dropped with the value's
//! error. Shorthands expand into their longhands at parse time, and a
//! longhand the shorthand does not mention is reset to its initial value,
//! so `background: alpha(@ba-paper, 0.55)` also clears `background-image`.
//!
//! Every GTK3 property is accepted here. The ones waybar's drawing does not
//! carry out are kept as [`Prop::Unsupported`] with their name, which the
//! probe lists and the bar ignores.

use super::token::Token;
use super::value::{
    self, Allow, Area, BorderStyle, Color, Cursor, Fail, FontSize, FontWeight, Image, Length,
    Repeat, Shadow, Size,
};

/// A box side, in CSS order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Top.
    Top = 0,
    /// Right.
    Right = 1,
    /// Bottom.
    Bottom = 2,
    /// Left.
    Left = 3,
}

/// The four sides in CSS order.
pub const SIDES: [Side; 4] = [Side::Top, Side::Right, Side::Bottom, Side::Left];

/// A box corner, in CSS order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    /// Top left.
    TopLeft = 0,
    /// Top right.
    TopRight = 1,
    /// Bottom right.
    BottomRight = 2,
    /// Bottom left.
    BottomLeft = 3,
}

/// The four corners in CSS order.
pub const CORNERS: [Corner; 4] = [
    Corner::TopLeft,
    Corner::TopRight,
    Corner::BottomRight,
    Corner::BottomLeft,
];

/// `font-style`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontStyle {
    /// `normal`.
    Normal,
    /// `italic`.
    Italic,
    /// `oblique`.
    Oblique,
}

/// One longhand's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Prop {
    /// `color`.
    Color(Color),
    /// `opacity`.
    Opacity(f32),
    /// `font-family`.
    FontFamily(Vec<String>),
    /// `font-size`.
    FontSize(FontSize),
    /// `font-weight`.
    FontWeight(FontWeight),
    /// `font-style`.
    FontStyle(FontStyle),
    /// `text-shadow`.
    TextShadow(Vec<Shadow>),
    /// `box-shadow`.
    BoxShadow(Vec<Shadow>),
    /// `margin-<side>`.
    Margin(Side, Length),
    /// `padding-<side>`.
    Padding(Side, Length),
    /// `border-<side>-width`.
    BorderWidth(Side, Length),
    /// `border-<side>-style`.
    BorderStyle(Side, BorderStyle),
    /// `border-<side>-color`.
    BorderColor(Side, Color),
    /// `border-<corner>-radius`: horizontal and vertical.
    Radius(Corner, Length, Length),
    /// `background-color`.
    BackgroundColor(Color),
    /// `background-image`.
    BackgroundImage(Vec<Image>),
    /// `background-size`.
    BackgroundSize(Vec<Size>),
    /// `background-position`.
    BackgroundPosition(Vec<(Length, Length)>),
    /// `background-repeat`.
    BackgroundRepeat(Vec<(Repeat, Repeat)>),
    /// `background-clip`.
    BackgroundClip(Vec<Area>),
    /// `background-origin`.
    BackgroundOrigin(Vec<Area>),
    /// `min-width`.
    MinWidth(Length),
    /// `min-height`.
    MinHeight(Length),
    /// `transition-duration`, in seconds per listed property.
    TransitionDuration(Vec<f32>),
    /// A GTK3 property this waybar parses nothing of and does not draw, by
    /// name.
    Unsupported(String),
}

/// Which longhand a [`Prop`] sets, for `initial`, `inherit` and the
/// cascade.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Id {
    /// `color`.
    Color,
    /// `opacity`.
    Opacity,
    /// `font-family`.
    FontFamily,
    /// `font-size`.
    FontSize,
    /// `font-weight`.
    FontWeight,
    /// `font-style`.
    FontStyle,
    /// `text-shadow`.
    TextShadow,
    /// `box-shadow`.
    BoxShadow,
    /// `margin-<side>`.
    Margin(u8),
    /// `padding-<side>`.
    Padding(u8),
    /// `border-<side>-width`.
    BorderWidth(u8),
    /// `border-<side>-style`.
    BorderStyle(u8),
    /// `border-<side>-color`.
    BorderColor(u8),
    /// `border-<corner>-radius`.
    Radius(u8),
    /// `background-color`.
    BackgroundColor,
    /// `background-image`.
    BackgroundImage,
    /// `background-size`.
    BackgroundSize,
    /// `background-position`.
    BackgroundPosition,
    /// `background-repeat`.
    BackgroundRepeat,
    /// `background-clip`.
    BackgroundClip,
    /// `background-origin`.
    BackgroundOrigin,
    /// `min-width`.
    MinWidth,
    /// `min-height`.
    MinHeight,
    /// `transition-duration`.
    TransitionDuration,
    /// Anything else.
    Unsupported(String),
}

impl Prop {
    /// The longhand this sets.
    #[must_use]
    pub fn id(&self) -> Id {
        match self {
            Prop::Color(_) => Id::Color,
            Prop::Opacity(_) => Id::Opacity,
            Prop::FontFamily(_) => Id::FontFamily,
            Prop::FontSize(_) => Id::FontSize,
            Prop::FontWeight(_) => Id::FontWeight,
            Prop::FontStyle(_) => Id::FontStyle,
            Prop::TextShadow(_) => Id::TextShadow,
            Prop::BoxShadow(_) => Id::BoxShadow,
            Prop::Margin(side, _) => Id::Margin(*side as u8),
            Prop::Padding(side, _) => Id::Padding(*side as u8),
            Prop::BorderWidth(side, _) => Id::BorderWidth(*side as u8),
            Prop::BorderStyle(side, _) => Id::BorderStyle(*side as u8),
            Prop::BorderColor(side, _) => Id::BorderColor(*side as u8),
            Prop::Radius(corner, ..) => Id::Radius(*corner as u8),
            Prop::BackgroundColor(_) => Id::BackgroundColor,
            Prop::BackgroundImage(_) => Id::BackgroundImage,
            Prop::BackgroundSize(_) => Id::BackgroundSize,
            Prop::BackgroundPosition(_) => Id::BackgroundPosition,
            Prop::BackgroundRepeat(_) => Id::BackgroundRepeat,
            Prop::BackgroundClip(_) => Id::BackgroundClip,
            Prop::BackgroundOrigin(_) => Id::BackgroundOrigin,
            Prop::MinWidth(_) => Id::MinWidth,
            Prop::MinHeight(_) => Id::MinHeight,
            Prop::TransitionDuration(_) => Id::TransitionDuration,
            Prop::Unsupported(name) => Id::Unsupported(name.clone()),
        }
    }
}

/// What a declaration sets one longhand to.
#[derive(Clone, Debug, PartialEq)]
pub enum Declared {
    /// A value.
    Value(Prop),
    /// `initial`, or a longhand a shorthand left out.
    Initial(Id),
    /// `inherit`.
    Inherit(Id),
    /// `unset`: `inherit` for an inherited property, else `initial`.
    Unset(Id),
}

/// GTK3's longhands that this waybar does not carry out, and its
/// shorthands for them, all accepted as valid names.
const UNSUPPORTED: [&str; 58] = [
    "-gtk-dpi",
    "-gtk-icon-theme",
    "-gtk-icon-palette",
    "font-variant",
    "font-stretch",
    "letter-spacing",
    "text-decoration-line",
    "text-decoration-color",
    "text-decoration-style",
    "text-decoration",
    "font-kerning",
    "font-variant-ligatures",
    "font-variant-position",
    "font-variant-caps",
    "font-variant-numeric",
    "font-variant-alternates",
    "font-variant-east-asian",
    "font-feature-settings",
    "font-variation-settings",
    "outline-style",
    "outline-width",
    "outline-offset",
    "outline-color",
    "outline",
    "-gtk-outline-radius",
    "-gtk-outline-top-left-radius",
    "-gtk-outline-top-right-radius",
    "-gtk-outline-bottom-right-radius",
    "-gtk-outline-bottom-left-radius",
    "background-blend-mode",
    "border-image-source",
    "border-image-repeat",
    "border-image-slice",
    "border-image-width",
    "border-image",
    "-gtk-icon-source",
    "-gtk-icon-shadow",
    "-gtk-icon-style",
    "-gtk-icon-transform",
    "-gtk-icon-effect",
    "-gtk-icon-filter",
    "transition-property",
    "transition-timing-function",
    "transition-delay",
    "animation-name",
    "animation-duration",
    "animation-timing-function",
    "animation-iteration-count",
    "animation-direction",
    "animation-play-state",
    "animation-delay",
    "animation-fill-mode",
    "animation",
    "caret-color",
    "-gtk-secondary-caret-color",
    "gtk-key-bindings",
    "-gtk-key-bindings",
    "engine",
];

/// Whether `id` is inherited when nothing sets it.
#[must_use]
pub fn inherited(id: &Id) -> bool {
    matches!(
        id,
        Id::Color | Id::FontFamily | Id::FontSize | Id::FontWeight | Id::FontStyle | Id::TextShadow
    )
}

fn side_name(name: &str) -> Option<(Side, &str)> {
    for (side, word) in [
        (Side::Top, "top"),
        (Side::Right, "right"),
        (Side::Bottom, "bottom"),
        (Side::Left, "left"),
    ] {
        if let Some(rest) = name.strip_prefix(word) {
            return Some((side, rest));
        }
    }
    None
}

/// One to four values for four sides, CSS's way: top, right, bottom, left,
/// the missing ones copied from the opposite side.
fn four<T: Clone>(values: Vec<T>) -> Result<[T; 4], Fail> {
    match values.as_slice() {
        [a] => Ok([a.clone(), a.clone(), a.clone(), a.clone()]),
        [a, b] => Ok([a.clone(), b.clone(), a.clone(), b.clone()]),
        [a, b, c] => Ok([a.clone(), b.clone(), c.clone(), b.clone()]),
        [a, b, c, d] => Ok([a.clone(), b.clone(), c.clone(), d.clone()]),
        _ => Err("Expected 1 to 4 values".to_owned()),
    }
}

fn lengths(cursor: &mut Cursor<'_>, allow: Allow) -> Result<Vec<Length>, Fail> {
    let mut out = Vec::new();
    while !cursor.done() {
        out.push(value::length(cursor, allow)?);
    }
    Ok(out)
}

fn done<T>(cursor: &Cursor<'_>, value: T) -> Result<T, Fail> {
    if cursor.done() {
        Ok(value)
    } else {
        Err("Junk at end of value".to_owned())
    }
}

fn list<T>(
    cursor: &Cursor<'_>,
    mut one: impl FnMut(&mut Cursor<'_>) -> Result<T, Fail>,
) -> Result<Vec<T>, Fail> {
    let mut out = Vec::new();
    for mut part in cursor.split_commas() {
        let value = one(&mut part)?;
        out.push(done(&part, value)?);
    }
    Ok(out)
}

/// Parse the declaration `name: tokens` into what it sets.
///
/// # Errors
///
/// GTK3's message: an unknown property name, or a value that does not
/// parse.
pub fn parse(name: &str, tokens: &[Token]) -> Result<Vec<Declared>, Fail> {
    let lower = name.to_ascii_lowercase();
    let mut cursor = Cursor::new(tokens);
    if cursor.done() {
        return Err("Expected a value".to_owned());
    }
    // `initial`, `inherit` and `unset` stand alone, for every longhand of
    // a shorthand.
    let solid: Vec<&Token> = tokens.iter().filter(|t| **t != Token::Space).collect();
    if let [Token::Ident(word)] = solid.as_slice() {
        let keyword = word.to_ascii_lowercase();
        if matches!(keyword.as_str(), "initial" | "inherit" | "unset") {
            let ids = longhands(&lower)?;
            return Ok(ids
                .into_iter()
                .map(|id| match keyword.as_str() {
                    "initial" => Declared::Initial(id),
                    "inherit" => Declared::Inherit(id),
                    _ => Declared::Unset(id),
                })
                .collect());
        }
    }
    let one = |prop: Prop| Ok(vec![Declared::Value(prop)]);
    match lower.as_str() {
        "color" => {
            let color = value::color(&mut cursor)?;
            one(Prop::Color(done(&cursor, color)?))
        }
        "background-color" => {
            let color = value::color(&mut cursor)?;
            one(Prop::BackgroundColor(done(&cursor, color)?))
        }
        "opacity" => {
            let opacity = value::number(&mut cursor)?;
            one(Prop::Opacity(done(&cursor, opacity.clamp(0.0, 1.0))?))
        }
        "font-family" => one(Prop::FontFamily(value::font_family(&cursor)?)),
        "font-size" => one(Prop::FontSize(value::font_size(&mut cursor)?)),
        "font-weight" => one(Prop::FontWeight(value::font_weight(&mut cursor)?)),
        "font-style" => {
            let style = if cursor.eat_ident("normal") {
                FontStyle::Normal
            } else if cursor.eat_ident("italic") {
                FontStyle::Italic
            } else if cursor.eat_ident("oblique") {
                FontStyle::Oblique
            } else {
                return Err("Expected a font style".to_owned());
            };
            one(Prop::FontStyle(done(&cursor, style)?))
        }
        "text-shadow" | "box-shadow" => {
            let shadows = if cursor.eat_ident("none") {
                done(&cursor, Vec::new())?
            } else {
                let inset = lower == "box-shadow";
                list(&cursor, |part| value::shadow(part, inset))?
            };
            one(if lower == "box-shadow" {
                Prop::BoxShadow(shadows)
            } else {
                Prop::TextShadow(shadows)
            })
        }
        "margin" | "padding" | "border-width" => {
            let allow = if lower == "margin" {
                Allow::ANY
            } else {
                Allow::POSITIVE
            };
            let values = four(lengths(&mut cursor, allow)?)?;
            Ok(SIDES
                .iter()
                .zip(values)
                .map(|(&side, value)| {
                    Declared::Value(match lower.as_str() {
                        "margin" => Prop::Margin(side, value),
                        "padding" => Prop::Padding(side, value),
                        _ => Prop::BorderWidth(side, value),
                    })
                })
                .collect())
        }
        "border-style" => {
            let mut styles = Vec::new();
            while let Some(style) = value::border_style(&mut cursor) {
                styles.push(style);
            }
            let styles = done(&cursor, four(styles)?)?;
            Ok(SIDES
                .iter()
                .zip(styles)
                .map(|(&side, style)| Declared::Value(Prop::BorderStyle(side, style)))
                .collect())
        }
        "border-color" => {
            let mut colors = Vec::new();
            while !cursor.done() {
                colors.push(value::color(&mut cursor)?);
            }
            let colors = four(colors)?;
            Ok(SIDES
                .iter()
                .zip(colors)
                .map(|(&side, color)| Declared::Value(Prop::BorderColor(side, color)))
                .collect())
        }
        "border-radius" => {
            let all = cursor.rest();
            let (horizontal, vertical) = match all.iter().position(|t| *t == Token::Delim('/')) {
                Some(at) => (
                    all.get(..at).unwrap_or_default(),
                    all.get(at + 1..).unwrap_or_default(),
                ),
                None => (all, all),
            };
            let radius = Allow {
                percent: true,
                negative: false,
            };
            let h = four(lengths(&mut Cursor::new(horizontal), radius)?)?;
            let v = four(lengths(&mut Cursor::new(vertical), radius)?)?;
            Ok(CORNERS
                .iter()
                .zip(h.into_iter().zip(v))
                .map(|(&corner, (h, v))| Declared::Value(Prop::Radius(corner, h, v)))
                .collect())
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let mut width = None;
            let mut style = None;
            let mut color = None;
            while !cursor.done() {
                if width.is_none() && value::starts_length(&cursor) {
                    width = Some(value::length(&mut cursor, Allow::POSITIVE)?);
                } else if style.is_none()
                    && let Some(found) = value::border_style(&mut cursor)
                {
                    style = Some(found);
                } else if color.is_none() && value::starts_color(&cursor) {
                    color = Some(value::color(&mut cursor)?);
                } else {
                    return Err("Junk at end of value".to_owned());
                }
            }
            let sides: Vec<Side> = match side_name(lower.trim_start_matches("border-")) {
                Some((side, "")) if lower != "border" => vec![side],
                _ => SIDES.to_vec(),
            };
            let mut out = Vec::new();
            for side in sides {
                out.push(match width {
                    Some(width) => Declared::Value(Prop::BorderWidth(side, width)),
                    None => Declared::Initial(Id::BorderWidth(side as u8)),
                });
                out.push(match style {
                    Some(style) => Declared::Value(Prop::BorderStyle(side, style)),
                    None => Declared::Initial(Id::BorderStyle(side as u8)),
                });
                out.push(match &color {
                    Some(color) => Declared::Value(Prop::BorderColor(side, color.clone())),
                    None => Declared::Initial(Id::BorderColor(side as u8)),
                });
            }
            if lower == "border" {
                // `border` resets `border-image` too, which is not drawn.
                out.push(Declared::Initial(Id::Unsupported(
                    "border-image".to_owned(),
                )));
            }
            Ok(out)
        }
        "background" => background(&cursor),
        "background-image" => one(Prop::BackgroundImage(list(&cursor, value::image)?)),
        "background-size" => one(Prop::BackgroundSize(list(&cursor, value::size)?)),
        "background-position" => one(Prop::BackgroundPosition(list(&cursor, value::position)?)),
        "background-repeat" => one(Prop::BackgroundRepeat(list(&cursor, value::repeat)?)),
        "background-clip" => one(Prop::BackgroundClip(list(&cursor, value::area)?)),
        "background-origin" => one(Prop::BackgroundOrigin(list(&cursor, value::area)?)),
        "min-width" | "min-height" => {
            let length = value::length(&mut cursor, Allow::POSITIVE)?;
            let length = done(&cursor, length)?;
            one(if lower == "min-width" {
                Prop::MinWidth(length)
            } else {
                Prop::MinHeight(length)
            })
        }
        "transition-duration" => one(Prop::TransitionDuration(list(&cursor, value::time)?)),
        "transition" => {
            // `transition: <property> <duration> [<timing>] [<delay>]`, as
            // a list. Only the duration is kept: a change is drawn at its
            // end, without the frames in between.
            let durations = list(&cursor, |part| {
                let mut duration = 0.0;
                while let Some(token) = part.next_token() {
                    if let Token::Dimension(_, unit) = token
                        && (unit.eq_ignore_ascii_case("s") || unit.eq_ignore_ascii_case("ms"))
                    {
                        let mut again = Cursor::new(core::slice::from_ref(token));
                        duration = value::time(&mut again)?;
                        break;
                    }
                }
                let _ = part.rest();
                Ok(duration)
            })?;
            one(Prop::TransitionDuration(durations))
        }
        _ => {
            if let Some(rest) = lower.strip_prefix("margin-")
                && let Some((side, "")) = side_name(rest)
            {
                let length = value::length(&mut cursor, Allow::ANY)?;
                return one(Prop::Margin(side, done(&cursor, length)?));
            }
            if let Some(rest) = lower.strip_prefix("padding-")
                && let Some((side, "")) = side_name(rest)
            {
                let length = value::length(&mut cursor, Allow::POSITIVE)?;
                return one(Prop::Padding(side, done(&cursor, length)?));
            }
            if let Some(rest) = lower.strip_prefix("border-")
                && let Some(prop) = border_longhand(rest, &mut cursor)
            {
                return one(prop?);
            }
            if lower == "font" {
                return font(&cursor);
            }
            if UNSUPPORTED.contains(&lower.as_str()) || style_property(name) {
                let _ = cursor.rest();
                return one(Prop::Unsupported(lower));
            }
            Err(format!("'{name}' is not a valid property name"))
        }
    }
}

/// `border-<side>-width|style|color` and `border-<corner>-radius`, from the
/// name after `border-`; `None` for any other name.
fn border_longhand(rest: &str, cursor: &mut Cursor<'_>) -> Option<Result<Prop, Fail>> {
    let parsed = |cursor: &mut Cursor<'_>| -> Option<Result<Prop, Fail>> {
        if let Some((side, tail)) = side_name(rest) {
            match tail {
                "-width" => {
                    return Some(
                        value::length(cursor, Allow::POSITIVE).map(|l| Prop::BorderWidth(side, l)),
                    );
                }
                "-style" => {
                    return Some(
                        value::border_style(cursor)
                            .map(|s| Prop::BorderStyle(side, s))
                            .ok_or_else(|| "Expected a border style".to_owned()),
                    );
                }
                "-color" => return Some(value::color(cursor).map(|c| Prop::BorderColor(side, c))),
                _ => {}
            }
        }
        let corner = match rest {
            "top-left-radius" => Corner::TopLeft,
            "top-right-radius" => Corner::TopRight,
            "bottom-right-radius" => Corner::BottomRight,
            "bottom-left-radius" => Corner::BottomLeft,
            _ => return None,
        };
        let radius = Allow {
            percent: true,
            negative: false,
        };
        Some(value::length(cursor, radius).and_then(|h| {
            let v = if cursor.done() {
                h
            } else {
                value::length(cursor, radius)?
            };
            Ok(Prop::Radius(corner, h, v))
        }))
    };
    let prop = parsed(cursor)?;
    Some(prop.and_then(|prop| done(cursor, prop)))
}

/// Take a `background-position`'s tokens: keywords and lengths.
fn take_position<'a>(part: &mut Cursor<'a>) -> Result<&'a [Token], Fail> {
    let start = part.clone();
    loop {
        if ident_in(part, POSITIONS) {
            let _ = part.next_token();
        } else if value::starts_length(part) {
            let _ = value::length(part, Allow::PERCENT)?;
        } else {
            return Ok(part.since(&start));
        }
    }
}

/// Take a `background-size`'s tokens after the `/`.
fn take_size<'a>(part: &mut Cursor<'a>) -> Result<&'a [Token], Fail> {
    let start = part.clone();
    loop {
        if ident_in(part, &["auto", "cover", "contain"]) {
            let _ = part.next_token();
        } else if value::starts_length(part) {
            let _ = value::length(part, Allow::PERCENT)?;
        } else {
            return Ok(part.since(&start));
        }
    }
}

/// A widget style property, `-GtkWidget-focus-line-width`: GTK3 accepts
/// them from any stylesheet, and waybar's widgets read none that matter.
fn style_property(name: &str) -> bool {
    let mut parts = name.split('-');
    parts.next() == Some("")
        && parts
            .next()
            .is_some_and(|class| class.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        && parts.next().is_some()
}

/// The longhands a property name sets, for a lone `initial`/`inherit`.
fn longhands(name: &str) -> Result<Vec<Id>, Fail> {
    let sides = |make: fn(u8) -> Id| (0..4).map(make).collect::<Vec<_>>();
    Ok(match name {
        "color" => vec![Id::Color],
        "opacity" => vec![Id::Opacity],
        "font-family" => vec![Id::FontFamily],
        "font-size" => vec![Id::FontSize],
        "font-weight" => vec![Id::FontWeight],
        "font-style" => vec![Id::FontStyle],
        "font" => vec![Id::FontFamily, Id::FontSize, Id::FontWeight, Id::FontStyle],
        "text-shadow" => vec![Id::TextShadow],
        "box-shadow" => vec![Id::BoxShadow],
        "margin" => sides(Id::Margin),
        "padding" => sides(Id::Padding),
        "border-width" => sides(Id::BorderWidth),
        "border-style" => sides(Id::BorderStyle),
        "border-color" => sides(Id::BorderColor),
        "border-radius" => sides(Id::Radius),
        "border" => {
            let mut all = sides(Id::BorderWidth);
            all.extend(sides(Id::BorderStyle));
            all.extend(sides(Id::BorderColor));
            all
        }
        "background" => vec![
            Id::BackgroundColor,
            Id::BackgroundImage,
            Id::BackgroundSize,
            Id::BackgroundPosition,
            Id::BackgroundRepeat,
            Id::BackgroundClip,
            Id::BackgroundOrigin,
        ],
        "background-color" => vec![Id::BackgroundColor],
        "background-image" => vec![Id::BackgroundImage],
        "background-size" => vec![Id::BackgroundSize],
        "background-position" => vec![Id::BackgroundPosition],
        "background-repeat" => vec![Id::BackgroundRepeat],
        "background-clip" => vec![Id::BackgroundClip],
        "background-origin" => vec![Id::BackgroundOrigin],
        "min-width" => vec![Id::MinWidth],
        "min-height" => vec![Id::MinHeight],
        "transition" | "transition-duration" => vec![Id::TransitionDuration],
        other => {
            if let Some(rest) = other.strip_prefix("margin-")
                && let Some((side, "")) = side_name(rest)
            {
                return Ok(vec![Id::Margin(side as u8)]);
            }
            if let Some(rest) = other.strip_prefix("padding-")
                && let Some((side, "")) = side_name(rest)
            {
                return Ok(vec![Id::Padding(side as u8)]);
            }
            if let Some(rest) = other.strip_prefix("border-")
                && let Some((side, tail)) = side_name(rest)
            {
                let side = side as u8;
                return Ok(match tail {
                    "-width" => vec![Id::BorderWidth(side)],
                    "-style" => vec![Id::BorderStyle(side)],
                    "-color" => vec![Id::BorderColor(side)],
                    "" => vec![
                        Id::BorderWidth(side),
                        Id::BorderStyle(side),
                        Id::BorderColor(side),
                    ],
                    _ => {
                        let corner = match rest {
                            "top-left-radius" => 0,
                            "top-right-radius" => 1,
                            "bottom-right-radius" => 2,
                            "bottom-left-radius" => 3,
                            _ => return Err(format!("'{other}' is not a valid property name")),
                        };
                        vec![Id::Radius(corner)]
                    }
                });
            }
            if UNSUPPORTED.contains(&other) {
                return Ok(vec![Id::Unsupported(other.to_owned())]);
            }
            return Err(format!("'{other}' is not a valid property name"));
        }
    })
}

/// `background`: layers separated by commas, each any of an image, a
/// position with an optional `/ size`, a repeat, and one or two boxes; a
/// colour in the last layer only. Every longhand not given is reset.
fn background(cursor: &Cursor<'_>) -> Result<Vec<Declared>, Fail> {
    let parts = cursor.split_commas();
    let count = parts.len();
    let mut images = Vec::new();
    let mut sizes = Vec::new();
    let mut positions = Vec::new();
    let mut repeats = Vec::new();
    let mut clips = Vec::new();
    let mut origins = Vec::new();
    let mut color = None;
    for (index, mut part) in parts.into_iter().enumerate() {
        let mut image = None;
        let mut place = None;
        let mut size = None;
        let mut repeat = None;
        let mut boxes = Vec::new();
        while !part.done() {
            if image.is_none() && starts_image(&part) {
                image = Some(value::image(&mut part)?);
                continue;
            }
            if index + 1 == count && color.is_none() && value::starts_color(&part) {
                color = Some(value::color(&mut part)?);
                continue;
            }
            if repeat.is_none() && ident_in(&part, REPEATS) {
                let start = part.clone();
                let _ = part.next_token();
                if ident_in(&part, REPEATS.get(..4).unwrap_or_default()) {
                    let _ = part.next_token();
                }
                repeat = Some(value::repeat(&mut Cursor::new(part.since(&start)))?);
                continue;
            }
            if boxes.len() < 2 && ident_in(&part, &["border-box", "padding-box", "content-box"]) {
                let start = part.clone();
                let _ = part.next_token();
                boxes.push(value::area(&mut Cursor::new(part.since(&start)))?);
                continue;
            }
            if place.is_none() && (value::starts_length(&part) || ident_in(&part, POSITIONS)) {
                place = Some(value::position(&mut Cursor::new(take_position(
                    &mut part,
                )?))?);
                if part.eat(&Token::Delim('/')) {
                    size = Some(value::size(&mut Cursor::new(take_size(&mut part)?))?);
                }
                continue;
            }
            return Err("Junk at end of value".to_owned());
        }
        images.push(image.unwrap_or(Image::None));
        sizes.push(size.unwrap_or(Size::Explicit(None, None)));
        positions.push(place.unwrap_or((Length::percent(0.0), Length::percent(0.0))));
        repeats.push(repeat.unwrap_or((Repeat::Repeat, Repeat::Repeat)));
        let origin = boxes.first().copied().unwrap_or(Area::Padding);
        let clip = boxes
            .get(1)
            .copied()
            .or(boxes.first().copied())
            .unwrap_or(Area::Border);
        origins.push(origin);
        clips.push(clip);
    }
    Ok(vec![
        match color {
            Some(color) => Declared::Value(Prop::BackgroundColor(color)),
            None => Declared::Initial(Id::BackgroundColor),
        },
        Declared::Value(Prop::BackgroundImage(images)),
        Declared::Value(Prop::BackgroundSize(sizes)),
        Declared::Value(Prop::BackgroundPosition(positions)),
        Declared::Value(Prop::BackgroundRepeat(repeats)),
        Declared::Value(Prop::BackgroundClip(clips)),
        Declared::Value(Prop::BackgroundOrigin(origins)),
    ])
}

/// The repeat keywords, the two-axis ones first.
const REPEATS: &[&str] = &[
    "repeat",
    "no-repeat",
    "space",
    "round",
    "repeat-x",
    "repeat-y",
];

/// The position keywords.
const POSITIONS: &[&str] = &["left", "right", "top", "bottom", "center"];

/// Whether the next token is an identifier in `words`.
fn ident_in(cursor: &Cursor<'_>, words: &[&str]) -> bool {
    matches!(cursor.peek(), Some(Token::Ident(word)) if words.iter().any(|w| word.eq_ignore_ascii_case(w)))
}

/// Whether the next token begins an image.
fn starts_image(cursor: &Cursor<'_>) -> bool {
    match cursor.peek() {
        Some(Token::Url(_)) => true,
        Some(Token::Ident(word)) => word.eq_ignore_ascii_case("none"),
        Some(Token::Function(name)) => {
            let lower = name.to_ascii_lowercase();
            lower == "url"
                || lower.contains("gradient")
                || lower.starts_with("-gtk-")
                || matches!(lower.as_str(), "cross-fade" | "image")
        }
        _ => false,
    }
}

/// `font`: `[style] [weight] size family…`, the Pango-description-like
/// form GTK3 takes.
fn font(cursor: &Cursor<'_>) -> Result<Vec<Declared>, Fail> {
    let mut cursor = cursor.clone();
    let mut style = None;
    let mut weight = None;
    loop {
        if style.is_none() && cursor.eat_ident("italic") {
            style = Some(FontStyle::Italic);
        } else if style.is_none() && cursor.eat_ident("oblique") {
            style = Some(FontStyle::Oblique);
        } else if weight.is_none() && cursor.eat_ident("bold") {
            weight = Some(FontWeight::Number(700));
        } else if cursor.eat_ident("normal") {
        } else {
            break;
        }
    }
    let size = value::font_size(&mut Cursor::new(
        &cursor
            .next_token()
            .cloned()
            .map(|t| vec![t])
            .unwrap_or_default(),
    ))?;
    let family = value::font_family(&cursor)?;
    Ok(vec![
        Declared::Value(Prop::FontStyle(style.unwrap_or(FontStyle::Normal))),
        Declared::Value(Prop::FontWeight(weight.unwrap_or(FontWeight::Number(400)))),
        Declared::Value(Prop::FontSize(size)),
        Declared::Value(Prop::FontFamily(family)),
    ])
}

#[cfg(test)]
mod tests {
    use super::super::token::{Token, tokenize};
    use super::super::value::{Color, Image, Length, Rgba};
    use super::{Declared, Id, Prop, Side, parse};

    fn declare(name: &str, value: &str) -> Result<Vec<Declared>, String> {
        let tokens: Vec<Token> = tokenize(value).into_iter().map(|(t, _)| t).collect();
        parse(name, &tokens)
    }

    #[test]
    fn margin_takes_two_values_and_negatives() {
        let set = declare("margin", "4px -2px").unwrap_or_default();
        assert_eq!(set.len(), 4);
        assert_eq!(
            set.get(1),
            Some(&Declared::Value(Prop::Margin(
                Side::Right,
                Length::px(-2.0)
            )))
        );
        assert!(declare("padding", "0 -1px").is_err());
    }

    #[test]
    fn the_background_shorthand_resets_the_image() {
        let set = declare("background", "alpha(@ba-paper, 0.55)").unwrap_or_default();
        assert!(matches!(
            set.first(),
            Some(Declared::Value(Prop::BackgroundColor(Color::Alpha(..))))
        ));
        assert_eq!(
            set.get(1),
            Some(&Declared::Value(Prop::BackgroundImage(vec![Image::None])))
        );
    }

    #[test]
    fn border_sets_all_sides() {
        let set = declare("border", "2px solid @ba-blue").unwrap_or_default();
        assert_eq!(set.len(), 13);
        let set = declare("border-radius", "0").unwrap_or_default();
        assert_eq!(set.len(), 4);
    }

    #[test]
    fn unknown_names_and_bad_values_are_errors() {
        assert_eq!(
            declare("colour", "red"),
            Err("'colour' is not a valid property name".to_owned())
        );
        assert!(declare("color", "12px").is_err());
        assert!(matches!(
            declare("-gtk-icon-shadow", "none").as_deref(),
            Ok([Declared::Value(Prop::Unsupported(_))])
        ));
        assert!(declare("-GtkWidget-focus-line-width", "0").is_ok());
    }

    #[test]
    fn keywords_apply_to_every_longhand() {
        let set = declare("padding", "inherit").unwrap_or_default();
        assert_eq!(set.len(), 4);
        assert_eq!(set.first(), Some(&Declared::Inherit(Id::Padding(0))));
        let set = declare("color", "#fff").unwrap_or_default();
        assert_eq!(
            set,
            vec![Declared::Value(Prop::Color(Color::Rgba(Rgba::rgb8(
                255, 255, 255
            ))))]
        );
    }

    #[test]
    fn layered_backgrounds_as_the_users_chips_write_them() {
        let set = declare(
            "background-image",
            "url(\"icons/cap-l-soft.svg\"), url(\"icons/cap-r-soft.svg\"), linear-gradient(#16273c, #16273c)",
        )
        .unwrap_or_default();
        assert!(
            matches!(set.first(), Some(Declared::Value(Prop::BackgroundImage(l))) if l.len() == 3)
        );
        let set = declare(
            "background-size",
            "12px 100%, 12px 100%, calc(100% - 24px) 100%",
        )
        .unwrap_or_default();
        assert!(
            matches!(set.first(), Some(Declared::Value(Prop::BackgroundSize(l))) if l.len() == 3)
        );
        let set = declare("background-position", "0% 50%, 100% 50%, 12px 50%").unwrap_or_default();
        assert!(
            matches!(set.first(), Some(Declared::Value(Prop::BackgroundPosition(l))) if l.len() == 3)
        );
        assert!(declare("box-shadow", "inset 0 -3px 0 0 @ba-gold").is_ok());
    }
}
