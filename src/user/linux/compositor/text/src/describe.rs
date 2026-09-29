//! What a program asks for: a family list, a weight, a style and a size.

/// A weight on the OpenType scale: 100 thin to 900 black, 400 regular.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Weight(pub u16);

impl Weight {
    /// 100.
    pub const THIN: Self = Self(100);
    /// 200.
    pub const EXTRA_LIGHT: Self = Self(200);
    /// 300.
    pub const LIGHT: Self = Self(300);
    /// 350, Pango's "Semi-Light" and "Book".
    pub const SEMI_LIGHT: Self = Self(350);
    /// 400.
    pub const NORMAL: Self = Self(400);
    /// 500.
    pub const MEDIUM: Self = Self(500);
    /// 600.
    pub const SEMI_BOLD: Self = Self(600);
    /// 700.
    pub const BOLD: Self = Self(700);
    /// 800.
    pub const EXTRA_BOLD: Self = Self(800);
    /// 900.
    pub const BLACK: Self = Self(900);

    /// A weight's name as Pango, CSS and fontconfig write it, case ignored:
    /// `"Light"`, `"bold"`, `"ultrabold"`, `"semibold"`, `"heavy"`, or a
    /// number (`"700"`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let lower = name.trim().to_ascii_lowercase();
        if let Ok(number) = lower.parse::<u16>() {
            return (1..=1000).contains(&number).then_some(Self(number));
        }
        Some(match lower.replace(['-', ' '], "").as_str() {
            "thin" | "hairline" => Self::THIN,
            "ultralight" | "extralight" => Self::EXTRA_LIGHT,
            "light" => Self::LIGHT,
            "semilight" | "demilight" | "book" => Self::SEMI_LIGHT,
            "normal" | "regular" | "roman" => Self::NORMAL,
            "medium" => Self::MEDIUM,
            "semibold" | "demibold" => Self::SEMI_BOLD,
            "bold" => Self::BOLD,
            "ultrabold" | "extrabold" => Self::EXTRA_BOLD,
            "heavy" | "black" | "ultraheavy" | "extrablack" | "ultrablack" => Self::BLACK,
            _ => return None,
        })
    }
}

impl Default for Weight {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Upright, italic or oblique.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Style {
    /// Upright.
    #[default]
    Normal,
    /// Italic.
    Italic,
    /// Slanted upright.
    Oblique,
}

/// A size, in the unit the configuration wrote it in.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Size {
    /// Points, which are pixels × 72 / [`crate::DPI`].
    Points(f32),
    /// Pixels.
    Pixels(f32),
}

impl Size {
    /// In pixels at `dpi` (Pango's and fcft's is [`crate::DPI`]).
    #[must_use]
    pub fn pixels(self, dpi: f32) -> f32 {
        match self {
            Self::Points(points) => points * dpi / 72.0,
            Self::Pixels(pixels) => pixels,
        }
    }
}

impl Default for Size {
    /// Pango's default, 10 points... is not what either program uses; this
    /// is 12 points, fcft's and fuzzel's default.
    fn default() -> Self {
        Self::Points(12.0)
    }
}

/// A font as a program asks for one.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct FontDescription {
    /// Families in order of preference. A generic name -- `sans-serif`,
    /// `sans`, `serif`, `monospace`, `mono`, `system-ui` -- stands for
    /// whatever face this machine gives it.
    pub families: Vec<String>,
    /// The weight.
    pub weight: Weight,
    /// The style.
    pub style: Style,
    /// The size.
    pub size: Size,
}

impl FontDescription {
    /// Pango's string form, as hyprlock's `font_family` (and a
    /// `<span font=…>`) writes it: comma-separated families, then style,
    /// weight and stretch words, then a size in points (or `Npx`), each
    /// optional. `"Ubuntu Light"` is the family Ubuntu at weight Light;
    /// `"Sans Bold Italic 12"`; `"GFS Didot, Serif 16"`.
    ///
    /// A word Pango does not know as a style or weight stays part of the
    /// family name, which is what makes `"GFS Didot"` one family.
    #[must_use]
    pub fn pango(text: &str) -> Self {
        pango_parts(text).0
    }

    /// fontconfig's pattern form, as fuzzel's `font=` writes it:
    /// `family[,family…][-size][:name=value…]`, with `size=` (points),
    /// `pixelsize=`, `weight=`, `slant=` and `style=` understood and the
    /// rest ignored. `"GFS Didot:size=16"`.
    #[must_use]
    pub fn fontconfig(text: &str) -> Self {
        fontconfig_pattern(text)
    }

    /// A description of `families` at `size`, normal weight and style.
    #[must_use]
    pub fn new(families: &[&str], size: Size) -> Self {
        Self {
            families: families.iter().map(|family| (*family).to_owned()).collect(),
            weight: Weight::NORMAL,
            style: Style::Normal,
            size,
        }
    }
}

/// What one of Pango's style words sets.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Word {
    /// A weight.
    Weight(Weight),
    /// A style.
    Style(Style),
    /// A word Pango knows and this crate has no use for: `Normal`, a
    /// variant, a stretch, a gravity.
    Other,
}

/// Pango's style, variant, weight, stretch and gravity words
/// (`pango-fonts.c`'s field maps), each written lower case without its
/// hyphens: Pango's `field_matches` lets a hyphen in the table go
/// unwritten, so `semibold` and `semi-bold` are both `Semi-Bold`. The
/// weights are Pango's own, where `Book` is 380 and `Ultra-Heavy` 1000.
const WORDS: &[(&str, Word)] = &[
    ("normal", Word::Other),
    ("roman", Word::Style(Style::Normal)),
    ("oblique", Word::Style(Style::Oblique)),
    ("italic", Word::Style(Style::Italic)),
    ("smallcaps", Word::Other),
    ("allsmallcaps", Word::Other),
    ("petitecaps", Word::Other),
    ("allpetitecaps", Word::Other),
    ("unicase", Word::Other),
    ("titlecaps", Word::Other),
    ("thin", Word::Weight(Weight::THIN)),
    ("ultralight", Word::Weight(Weight::EXTRA_LIGHT)),
    ("extralight", Word::Weight(Weight::EXTRA_LIGHT)),
    ("light", Word::Weight(Weight::LIGHT)),
    ("semilight", Word::Weight(Weight::SEMI_LIGHT)),
    ("demilight", Word::Weight(Weight::SEMI_LIGHT)),
    ("book", Word::Weight(Weight(380))),
    ("regular", Word::Weight(Weight::NORMAL)),
    ("medium", Word::Weight(Weight::MEDIUM)),
    ("semibold", Word::Weight(Weight::SEMI_BOLD)),
    ("demibold", Word::Weight(Weight::SEMI_BOLD)),
    ("bold", Word::Weight(Weight::BOLD)),
    ("ultrabold", Word::Weight(Weight::EXTRA_BOLD)),
    ("extrabold", Word::Weight(Weight::EXTRA_BOLD)),
    ("heavy", Word::Weight(Weight::BLACK)),
    ("black", Word::Weight(Weight::BLACK)),
    ("ultraheavy", Word::Weight(Weight(1000))),
    ("extraheavy", Word::Weight(Weight(1000))),
    ("ultrablack", Word::Weight(Weight(1000))),
    ("extrablack", Word::Weight(Weight(1000))),
    ("ultracondensed", Word::Other),
    ("extracondensed", Word::Other),
    ("condensed", Word::Other),
    ("semicondensed", Word::Other),
    ("semiexpanded", Word::Other),
    ("expanded", Word::Other),
    ("extraexpanded", Word::Other),
    ("ultraexpanded", Word::Other),
    ("notrotated", Word::Other),
    ("south", Word::Other),
    ("upsidedown", Word::Other),
    ("north", Word::Other),
    ("rotatedleft", Word::Other),
    ("east", Word::Other),
    ("rotatedright", Word::Other),
    ("west", Word::Other),
];

/// One of Pango's words, or its `weight=N` form, as what it sets.
fn style_word(word: &str) -> Option<Word> {
    let lower = word.to_ascii_lowercase();
    if let Some(number) = lower.strip_prefix("weight=") {
        return number
            .parse::<u16>()
            .ok()
            .filter(|weight| (1..=1000).contains(weight))
            .map(|weight| Word::Weight(Weight(weight)));
    }
    let bare: String = lower
        .chars()
        .filter(|&character| character != '-')
        .collect();
    WORDS
        .iter()
        .find(|(name, _)| *name == bare)
        .map(|&(_, what)| what)
}

/// Pango's `parse_size`: a number, then `px` for pixels; points otherwise.
fn pango_size(word: &str) -> Option<Size> {
    let (number, pixels) = match word.strip_suffix("px") {
        Some(number) => (number, true),
        None => (word, false),
    };
    let value = number.parse::<f64>().ok()?;
    if !value.is_finite() || !(0.0..=1_000_000.0).contains(&value) {
        return None;
    }
    let value = value as f32;
    Some(if pixels {
        Size::Pixels(value)
    } else {
        Size::Points(value)
    })
}

/// The last word of `text` after trailing white space, split at white space
/// or (with `comma`) a comma: `(what is before it, the word)`.
fn last_word(text: &str, comma: bool) -> (&str, &str) {
    let trimmed = text.trim_end();
    match trimmed
        .rsplit_once(|character: char| character.is_whitespace() || (comma && character == ','))
    {
        Some((before, word)) => (before, word),
        None => ("", trimmed),
    }
}

/// [`FontDescription::pango`], and whether the string gave a size: a
/// `<span font=…>` with none leaves the size around it alone, which the
/// description on its own cannot say.
pub(crate) fn pango_parts(text: &str) -> (FontDescription, bool) {
    let mut description = FontDescription::default();
    let mut rest = text;
    let mut sized = false;

    // Variations (`@wght=200`) come last; they are read and dropped.
    let (before, word) = last_word(rest, false);
    if word.starts_with('@') {
        rest = before;
    }
    // Then the size, a word of its own at the end.
    let (before, word) = last_word(rest, false);
    if !word.is_empty()
        && let Some(size) = pango_size(word)
    {
        description.size = size;
        sized = true;
        rest = before;
    }
    // Then style words, right to left, for as long as they are words.
    loop {
        let (before, word) = last_word(rest, true);
        if word.is_empty() {
            break;
        }
        match style_word(word) {
            Some(Word::Weight(weight)) => description.weight = weight,
            Some(Word::Style(style)) => description.style = style,
            Some(Word::Other) => {}
            None => break,
        }
        rest = before;
    }
    description.families = rest
        .split(',')
        .map(str::trim)
        .filter(|family| !family.is_empty())
        .map(str::to_owned)
        .collect();
    (description, sized)
}

/// `text` cut at every `separator` a backslash does not escape, each piece
/// with its escapes removed.
fn split_escaped(text: &str, separator: char) -> Vec<String> {
    let mut pieces = vec![String::new()];
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            if let (Some(next), Some(last)) = (characters.next(), pieces.last_mut()) {
                last.push(next);
            }
        } else if character == separator {
            pieces.push(String::new());
        } else if let Some(last) = pieces.last_mut() {
            last.push(character);
        }
    }
    pieces
}

/// Where the first `separator` a backslash does not escape is, as a byte
/// offset.
fn find_unescaped(text: &str, separators: &[char]) -> Option<usize> {
    let mut escaped = false;
    for (at, character) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if separators.contains(&character) {
            return Some(at);
        }
    }
    None
}

/// fontconfig's `FcWeightToOpenType`: its own 0-215 weight scale onto
/// OpenType's, linearly between the named weights.
fn fontconfig_weight(value: f64) -> Weight {
    const MAP: [(f64, f64); 13] = [
        (0.0, 0.0),
        (0.0, 100.0),
        (40.0, 200.0),
        (50.0, 300.0),
        (55.0, 350.0),
        (75.0, 380.0),
        (80.0, 400.0),
        (100.0, 500.0),
        (180.0, 600.0),
        (200.0, 700.0),
        (205.0, 800.0),
        (210.0, 900.0),
        (215.0, 1000.0),
    ];
    let value = value.clamp(0.0, 215.0);
    let mut previous = (0.0, 100.0);
    let mut open_type = 1000.0;
    for &(fc, ot) in MAP.iter().skip(1) {
        if value <= fc {
            open_type = if value >= fc || fc <= previous.0 {
                ot
            } else {
                previous.1 + (value - previous.0) * (ot - previous.1) / (fc - previous.0)
            };
            break;
        }
        previous = (fc, ot);
    }
    Weight(open_type.round() as u16)
}

/// fontconfig's weight constants, on its own scale.
fn fontconfig_weight_name(name: &str) -> Option<f64> {
    Some(match name {
        "thin" => 0.0,
        "extralight" | "ultralight" => 40.0,
        "light" => 50.0,
        "demilight" | "semilight" => 55.0,
        "book" => 75.0,
        "regular" | "normal" => 80.0,
        "medium" => 100.0,
        "demibold" | "semibold" => 180.0,
        "bold" => 200.0,
        "extrabold" | "ultrabold" => 205.0,
        "black" | "heavy" => 210.0,
        "extrablack" | "ultrablack" => 215.0,
        _ => return None,
    })
}

/// fontconfig's slant constants and numbers.
fn fontconfig_slant(value: &str) -> Option<Style> {
    Some(match value {
        "roman" | "0" => Style::Normal,
        "italic" | "100" => Style::Italic,
        "oblique" | "110" => Style::Oblique,
        _ => return None,
    })
}

/// A fontconfig size value: a number, or the first of a `[from to]` range.
fn fontconfig_number(value: &str) -> Option<f32> {
    let value = value.trim().trim_start_matches('[');
    let first = value.split_whitespace().next().unwrap_or(value);
    let first = first.trim_end_matches(']');
    let number = first.parse::<f64>().ok()?;
    (number.is_finite() && number > 0.0 && number < 1_000_000.0).then_some(number as f32)
}

/// [`FontDescription::fontconfig`]: `FcNameParse`'s grammar, with the
/// elements a font chooser reads.
fn fontconfig_pattern(text: &str) -> FontDescription {
    let mut description = FontDescription::default();
    let (head, properties) = match find_unescaped(text, &[':']) {
        Some(at) => text.split_at(at),
        None => (text, ""),
    };
    let (families, sizes) = match find_unescaped(head, &['-']) {
        Some(at) => head.split_at(at),
        None => (head, ""),
    };
    description.families = split_escaped(families, ',')
        .into_iter()
        .map(|family| family.trim().to_owned())
        .filter(|family| !family.is_empty())
        .collect();
    if let Some(size) = sizes
        .strip_prefix('-')
        .and_then(|sizes| sizes.split(',').find_map(fontconfig_number))
    {
        description.size = Size::Points(size);
    }
    for element in split_escaped(properties.strip_prefix(':').unwrap_or(properties), ':') {
        let element = element.trim();
        let Some((key, value)) = element.split_once('=') else {
            // A constant on its own: `:bold`, `:italic`.
            let name = element.to_ascii_lowercase();
            if let Some(weight) = fontconfig_weight_name(&name) {
                description.weight = fontconfig_weight(weight);
            } else if let Some(style) = fontconfig_slant(&name) {
                description.style = style;
            }
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        // A value may be a list; the first is the one asked for.
        let value = value.split(',').next().unwrap_or(value).trim();
        let lower = value.to_ascii_lowercase();
        match key.as_str() {
            "family" => {
                if !value.is_empty() {
                    description.families.push(value.to_owned());
                }
            }
            "size" => {
                if let Some(size) = fontconfig_number(value) {
                    description.size = Size::Points(size);
                }
            }
            "pixelsize" => {
                if let Some(size) = fontconfig_number(value) {
                    description.size = Size::Pixels(size);
                }
            }
            "weight" => {
                let weight = fontconfig_weight_name(&lower).or_else(|| {
                    lower
                        .parse::<f64>()
                        .ok()
                        .filter(|weight| weight.is_finite())
                });
                if let Some(weight) = weight {
                    description.weight = fontconfig_weight(weight);
                }
            }
            "slant" => {
                if let Some(style) = fontconfig_slant(&lower) {
                    description.style = style;
                }
            }
            "style" => {
                for word in value.split_whitespace() {
                    match style_word(word) {
                        Some(Word::Weight(weight)) => description.weight = weight,
                        Some(Word::Style(style)) => description.style = style,
                        Some(Word::Other) | None => {}
                    }
                }
            }
            _ => {}
        }
    }
    description
}
