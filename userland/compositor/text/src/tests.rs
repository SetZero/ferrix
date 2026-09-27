//! Host tests.
//!
//! The faces are the tree's own (`assets/fonts/liberation`, `assets/fonts/inter`), so
//! nothing here depends on what the machine has installed. Liberation Sans
//! is metric-compatible with Arial: at 2048 units to the em its `H` is
//! 1479 units wide, which a size of 2048 pixels makes 1479 pixels.

use std::path::{Path, PathBuf};

use crate::markup::{self, LineHeight, SpanSize, Underline};
use crate::{
    Align, Ellipsize, FaceId, FontDescription, Fonts, LayoutOptions, Rgba, Size, Span, Style,
    Weight, plain_spans,
};

fn tree_fonts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../assets/fonts")
}

fn fonts() -> Fonts {
    let mut fonts = Fonts::new();
    assert_eq!(fonts.add_dir(&tree_fonts()), 14);
    fonts
}

/// A `Fonts` holding one file of the tree's.
fn only(file: &str) -> Fonts {
    let mut fonts = Fonts::new();
    let bytes = std::fs::read(tree_fonts().join(file)).unwrap();
    assert_eq!(fonts.add_bytes(bytes, file), 1);
    fonts
}

fn full_name(fonts: &Fonts, face: Option<FaceId>) -> String {
    fonts.info(face.unwrap()).unwrap().full_name.clone()
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

// ---------------------------------------------------------------------------
// Descriptions

#[test]
fn pango_descriptions_read_words_from_the_end() {
    let light = FontDescription::pango("Ubuntu Light");
    assert_eq!(light.families, ["Ubuntu"]);
    assert_eq!(light.weight, Weight::LIGHT);
    assert_eq!(light.size, Size::default());

    let didot = FontDescription::pango("GFS Didot");
    assert_eq!(didot.families, ["GFS Didot"]);
    assert_eq!(didot.weight, Weight::NORMAL);

    let sans = FontDescription::pango("Sans Bold Italic 12");
    assert_eq!(sans.families, ["Sans"]);
    assert_eq!(sans.weight, Weight::BOLD);
    assert_eq!(sans.style, Style::Italic);
    assert_eq!(sans.size, Size::Points(12.0));

    let list = FontDescription::pango("GFS Didot, Serif 16");
    assert_eq!(list.families, ["GFS Didot", "Serif"]);
    assert_eq!(list.size, Size::Points(16.0));

    let big = FontDescription::pango("Ubuntu Light 96");
    assert_eq!(big.families, ["Ubuntu"]);
    assert_eq!(big.weight, Weight::LIGHT);
    assert_eq!(big.size, Size::Points(96.0));

    assert_eq!(FontDescription::pango("Mono 13px").size, Size::Pixels(13.0));
    assert_eq!(
        FontDescription::pango("Foo Semi-Bold").weight,
        Weight::SEMI_BOLD
    );
    assert_eq!(
        FontDescription::pango("Foo semibold").weight,
        Weight::SEMI_BOLD
    );
    assert_eq!(FontDescription::pango("Foo Book").weight, Weight(380));
    assert_eq!(
        FontDescription::pango("Foo Condensed Oblique").style,
        Style::Oblique
    );
    assert_eq!(FontDescription::pango("Foo weight=450").weight, Weight(450));

    let size_only = FontDescription::pango("12");
    assert!(size_only.families.is_empty());
    assert_eq!(size_only.size, Size::Points(12.0));

    // A word Pango does not know ends the style words: "Light" before it
    // is part of the family.
    let odd = FontDescription::pango("Foo Light Grotesk 10");
    assert_eq!(odd.families, ["Foo Light Grotesk"]);
    assert_eq!(odd.weight, Weight::NORMAL);
}

#[test]
fn fontconfig_patterns_read_families_sizes_and_properties() {
    let didot = FontDescription::fontconfig("GFS Didot:size=16");
    assert_eq!(didot.families, ["GFS Didot"]);
    assert_eq!(didot.size, Size::Points(16.0));

    let mono = FontDescription::fontconfig("DejaVu Sans Mono-10:weight=bold");
    assert_eq!(mono.families, ["DejaVu Sans Mono"]);
    assert_eq!(mono.size, Size::Points(10.0));
    assert_eq!(mono.weight, Weight::BOLD);

    // A number is on fontconfig's own scale, where 200 is bold.
    assert_eq!(
        FontDescription::fontconfig("Dina:weight=200").weight,
        Weight::BOLD
    );
    assert_eq!(
        FontDescription::fontconfig("Dina:weight=80").weight,
        Weight::NORMAL
    );
    assert_eq!(
        FontDescription::fontconfig("Dina:weight=45").weight,
        Weight(250)
    );

    let italic = FontDescription::fontconfig("Inter:pixelsize=14:slant=italic");
    assert_eq!(italic.size, Size::Pixels(14.0));
    assert_eq!(italic.style, Style::Italic);

    let styled = FontDescription::fontconfig("Foo:style=Bold Italic");
    assert_eq!(styled.weight, Weight::BOLD);
    assert_eq!(styled.style, Style::Italic);

    let list = FontDescription::fontconfig("a,b-12:bold:antialias=true");
    assert_eq!(list.families, ["a", "b"]);
    assert_eq!(list.size, Size::Points(12.0));
    assert_eq!(list.weight, Weight::BOLD);

    let escaped = FontDescription::fontconfig("Foo\\-Bar:size=9");
    assert_eq!(escaped.families, ["Foo-Bar"]);
    assert_eq!(FontDescription::fontconfig("Foo").size, Size::default());
}

// ---------------------------------------------------------------------------
// Colours

#[test]
fn colours_parse_as_pango_color_parse_does() {
    assert_eq!(Rgba::parse("#fff"), Some(Rgba::WHITE));
    assert_eq!(Rgba::parse("#800"), Some(Rgba::rgb(0x88, 0, 0)));
    assert_eq!(Rgba::parse("#123456"), Some(Rgba::rgb(0x12, 0x34, 0x56)));
    assert_eq!(Rgba::parse("#fff000000"), Some(Rgba::rgb(255, 0, 0)));
    assert_eq!(Rgba::parse("#ffff00008000"), Some(Rgba::rgb(255, 0, 0x80)));
    assert_eq!(
        Rgba::parse("#12345678"),
        Some(Rgba {
            r: 0x12,
            g: 0x34,
            b: 0x56,
            a: 0x78
        })
    );
    assert_eq!(Rgba::parse("#abc8").map(|color| color.a), Some(0x88));
    assert_eq!(Rgba::parse("SteelBlue"), Some(Rgba::rgb(70, 130, 180)));
    assert_eq!(Rgba::parse("steel blue"), Some(Rgba::rgb(70, 130, 180)));
    assert_eq!(Rgba::parse("RED"), Some(Rgba::rgb(255, 0, 0)));
    assert_eq!(
        Rgba::parse("light goldenrod yellow"),
        Some(Rgba::rgb(250, 250, 210))
    );
    assert_eq!(Rgba::parse("grey50"), Some(Rgba::rgb(127, 127, 127)));
    assert_eq!(Rgba::parse("nope"), None);
    assert_eq!(Rgba::parse("#12"), None);
    assert_eq!(Rgba::parse("#ggg"), None);
    assert_eq!(Rgba::parse(""), None);
    assert!(crate::colors::NAMES.len() > 600);
    assert!(
        crate::colors::NAMES
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0)
    );
}

// ---------------------------------------------------------------------------
// Markup

#[test]
fn markup_gives_spans_with_their_attributes() {
    let parsed = markup::parse("<span foreground=\"#cccccc\">Password</span>").unwrap();
    assert_eq!(parsed.spans.len(), 1);
    assert_eq!(parsed.spans[0].text, "Password");
    assert_eq!(
        parsed.spans[0].style.foreground,
        Some(Rgba::rgb(0xcc, 0xcc, 0xcc))
    );

    let parsed = markup::parse("<b>Bold</b> text").unwrap();
    assert_eq!(parsed.spans.len(), 2);
    assert_eq!(parsed.spans[0].style.weight, Some(Weight::BOLD));
    assert_eq!(parsed.spans[1].style.weight, None);
    assert_eq!(parsed.plain(), "Bold text");

    // Adjacent pieces in one style are joined.
    let parsed = markup::parse("<b>a</b><b>b</b>").unwrap();
    assert_eq!(parsed.spans.len(), 1);
    assert_eq!(parsed.spans[0].text, "ab");

    let parsed = markup::parse("<i><u><s><tt>x</tt></s></u></i>").unwrap();
    let style = &parsed.spans[0].style;
    assert_eq!(style.style, Some(Style::Italic));
    assert_eq!(style.underline, Some(Underline::Single));
    assert_eq!(style.strikethrough, Some(true));
    assert!(style.monospace);

    let parsed = markup::parse("<markup><b>x</b></markup>").unwrap();
    assert_eq!(parsed.plain(), "x");

    let parsed = markup::parse("plain, no tags").unwrap();
    assert_eq!(parsed.spans.len(), 1);
    assert_eq!(parsed.spans[0].style, crate::SpanStyle::default());

    assert_eq!(markup::parse("a<!-- note -->b").unwrap().plain(), "ab");
    assert_eq!(markup::parse("<b/>x").unwrap().plain(), "x");
    assert!(markup::parse("").unwrap().spans.is_empty());
}

#[test]
fn markup_resolves_entities() {
    let parsed = markup::parse("a &amp; b &lt;&gt; &#65;&#x42; &quot;&apos;").unwrap();
    assert_eq!(parsed.plain(), "a & b <> AB \"'");
    let parsed = markup::parse("<span font_family=\"A&amp;B\">x</span>").unwrap();
    assert_eq!(parsed.spans[0].style.family.as_deref(), Some("A&B"));
    assert_eq!(markup::escape("<a & 'b'>"), "&lt;a &amp; &#39;b&#39;&gt;");
    let round = markup::parse(&markup::escape("x < y & \"z\"")).unwrap();
    assert_eq!(round.plain(), "x < y & \"z\"");
}

#[test]
fn markup_refuses_what_pango_refuses_in_its_words() {
    assert_eq!(
        markup::parse("<blink>x</blink>").unwrap_err(),
        "Unknown tag 'blink' on line 1 char 7"
    );
    assert_eq!(
        markup::parse("<b foo=\"1\">x</b>").unwrap_err(),
        "Attribute 'foo' is not allowed on the <b> tag on line 1 char 11"
    );
    assert_eq!(
        markup::parse("a\n<span blah=\"1\">x</span>").unwrap_err(),
        "Attribute 'blah' is invalid on <span> tag, line 2"
    );
    assert_eq!(
        markup::parse("<span foreground=\"nope\">x</span>").unwrap_err(),
        "Value of 'foreground' attribute on <span> tag on line 1 could not be parsed; \
         should be a color specification, not 'nope'"
    );
    assert_eq!(
        markup::parse("<span weight=\"heavyish\">x</span>").unwrap_err(),
        "'heavyish' is not a valid value for the 'weight' attribute on <span> tag, line 1; \
         valid values are for example 'light', 'ultrabold' or a number"
    );
    assert_eq!(
        markup::parse("<span size=\"huge\">x</span>").unwrap_err(),
        "Value of 'size' attribute on <span> tag on line 1 could not be parsed; should be an \
         integer, or a string such as 'small', not 'huge'"
    );
    assert!(
        markup::parse("<span color=\"red\" foreground=\"blue\">x</span>")
            .unwrap_err()
            .contains("occurs twice")
    );
    assert!(
        markup::parse("<b>x</i>")
            .unwrap_err()
            .contains("was closed, but the currently open element is \u{201c}b\u{201d}")
    );
    assert!(markup::parse("<b>x").unwrap_err().contains("still open"));
    assert!(
        markup::parse("a &foo; b")
            .unwrap_err()
            .contains("Entity name \u{201c}foo\u{201d} is not known")
    );
    assert!(
        markup::parse("a & b")
            .unwrap_err()
            .starts_with("Error on line 1 char 4")
    );
    assert!(markup::parse("<span strikethrough=\"maybe\">x</span>").is_err());
    assert!(markup::parse("<span rise=\"up\">x</span>").is_err());
}

#[test]
fn markup_sizes_nest_as_pangos_do() {
    let size = |text: &str| markup::parse(text).unwrap().spans[0].style.size;
    assert_eq!(
        size("<span size=\"12pt\">x</span>"),
        Some(SpanSize::Size(Size::Points(12.0)))
    );
    assert_eq!(
        size("<span size=\"12288\">x</span>"),
        Some(SpanSize::Size(Size::Points(12.0)))
    );
    assert_eq!(
        size("<span font_size=\"15px\">x</span>"),
        Some(SpanSize::Size(Size::Pixels(15.0)))
    );
    assert_eq!(
        size("<span size=\"150%\">x</span>"),
        Some(SpanSize::Scale(1.5))
    );
    assert_eq!(size("<big>x</big>"), Some(SpanSize::Scale(1.2)));
    assert_eq!(size("<small>x</small>"), Some(SpanSize::Scale(1.0 / 1.2)));
    assert_eq!(
        size("<span size=\"larger\">x</span>"),
        Some(SpanSize::Scale(1.2))
    );
    let Some(SpanSize::Scale(twice)) = size("<big><big>x</big></big>") else {
        panic!("a scale");
    };
    assert!(close(twice, 1.44));
    let Some(SpanSize::Scale(keyword)) = size("<big><span size=\"x-large\">x</span></big>") else {
        panic!("a scale");
    };
    // A keyword is relative to the base size, not the size around it.
    assert!(close(keyword, 1.44));
    let Some(SpanSize::Size(Size::Points(bigger))) =
        size("<span size=\"20pt\"><big>x</big></span>")
    else {
        panic!("a size");
    };
    assert!(close(bigger, 24.0));
    let Some(SpanSize::Scale(small)) = size("<span size=\"xx-small\">x</span>") else {
        panic!("a scale");
    };
    assert!((small - 0.5787).abs() < 0.001);
}

#[test]
fn markup_reads_every_span_attribute() {
    let style = |text: &str| markup::parse(text).unwrap().spans[0].style.clone();

    assert_eq!(
        style("<span weight=\"300\">x</span>").weight,
        Some(Weight::LIGHT)
    );
    assert_eq!(
        style("<span font_weight=\"ultrabold\">x</span>").weight,
        Some(Weight::EXTRA_BOLD)
    );
    assert_eq!(
        style("<span style=\"oblique\">x</span>").style,
        Some(Style::Oblique)
    );
    assert_eq!(
        style("<span background=\"red\">x</span>").background,
        Some(Rgba::rgb(255, 0, 0))
    );
    assert_eq!(
        style("<span foreground=\"#ff0000\" alpha=\"50%\">x</span>").foreground,
        Some(Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 128
        })
    );
    assert_eq!(
        style("<span fgcolor=\"#ff000080\">x</span>").foreground,
        Some(Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 0x80
        })
    );
    assert_eq!(
        style("<span underline=\"double\">x</span>").underline,
        Some(Underline::Double)
    );
    assert_eq!(
        style("<span underline=\"true\">x</span>").underline,
        Some(Underline::Single)
    );
    assert_eq!(
        style("<span underline=\"error\">x</span>").underline,
        Some(Underline::Error)
    );
    assert_eq!(
        style("<span underline_color=\"blue\">x</span>").underline_color,
        Some(Rgba::rgb(0, 0, 255))
    );
    assert_eq!(
        style("<span strikethrough=\"true\">x</span>").strikethrough,
        Some(true)
    );
    // 5120 1024ths of a point is five points: 6⅔ pixels at 96 dpi.
    assert!(close(
        style("<span rise=\"5120\">x</span>").rise.unwrap(),
        20.0 / 3.0
    ));
    assert!(close(
        style("<span rise=\"-3pt\">x</span>").rise.unwrap(),
        -4.0
    ));
    assert!(close(
        style("<span letter_spacing=\"1024\">x</span>")
            .letter_spacing
            .unwrap(),
        4.0 / 3.0
    ));
    assert_eq!(
        style("<span line_height=\"2.0\">x</span>").line_height,
        Some(LineHeight::Factor(2.0))
    );
    assert_eq!(
        style("<span line_height=\"20480\">x</span>").line_height,
        Some(LineHeight::Size(Size::Points(20.0)))
    );
    assert_eq!(
        style("<span line_height=\"12pt\">x</span>").line_height,
        Some(LineHeight::Size(Size::Points(12.0)))
    );
    assert_eq!(
        style("<span font_family=\"GFS Didot\">x</span>")
            .family
            .as_deref(),
        Some("GFS Didot")
    );
    assert!(style("<sup>x</sup>").rise.unwrap() > 0.0);
    assert!(style("<sub>x</sub>").rise.unwrap() < 0.0);

    let font = style("<b><span font=\"Sans Italic 14\">x</span></b>");
    let description = font.font.unwrap();
    assert_eq!(description.families, ["Sans"]);
    assert_eq!(description.style, Style::Italic);
    // The description's weight (normal) replaces the <b> around it.
    assert_eq!(font.weight, None);
    assert_eq!(font.size, Some(SpanSize::Size(Size::Points(14.0))));

    let parsed =
        markup::parse("<span font_features=\"tnum\" lang=\"en\" alpha=\"50%\">x</span>").unwrap();
    let mut ignored = parsed.ignored.clone();
    ignored.sort();
    assert_eq!(ignored, ["alpha", "font_features", "lang"]);
}

// ---------------------------------------------------------------------------
// Finding faces

#[test]
fn faces_are_found_by_family_weight_and_style() {
    let fonts = fonts();
    let find = |family: &str, weight: u16, style: Style| {
        full_name(&fonts, fonts.find(family, Weight(weight), style))
    };
    assert_eq!(
        find("Liberation Sans", 400, Style::Normal),
        "Liberation Sans"
    );
    assert_eq!(
        find("liberation  SANS", 400, Style::Normal),
        "Liberation Sans"
    );
    assert_eq!(
        find("LiberationSans", 400, Style::Normal),
        "Liberation Sans"
    );
    assert_eq!(
        find("Liberation Sans", 700, Style::Italic),
        "Liberation Sans Bold Italic"
    );
    assert_eq!(
        find("Liberation Sans", 400, Style::Oblique),
        "Liberation Sans Italic"
    );
    // CSS: above 500 heavier first, below 400 lighter first and then
    // heavier.
    assert_eq!(
        find("Liberation Sans", 600, Style::Normal),
        "Liberation Sans Bold"
    );
    assert_eq!(
        find("Liberation Sans", 300, Style::Normal),
        "Liberation Sans"
    );
    assert_eq!(
        find("Liberation Sans", 900, Style::Normal),
        "Liberation Sans Bold"
    );
    assert_eq!(
        find("Liberation Sans", 500, Style::Normal),
        "Liberation Sans"
    );
    // A variable face covers every weight on its axis.
    assert_eq!(find("Inter Variable", 300, Style::Normal), "Inter Variable");
    assert_eq!(
        find("Inter Variable", 800, Style::Italic),
        "Inter Variable Italic"
    );
    assert!(
        fonts
            .find("No Such Family", Weight::NORMAL, Style::Normal)
            .is_none()
    );

    assert_eq!(
        fonts.generic("sans-serif"),
        ["Liberation Sans", "Inter Variable"]
    );
    assert_eq!(fonts.generic("serif"), ["Liberation Serif"]);
    assert_eq!(fonts.generic("monospace"), ["Liberation Mono"]);
    assert!(fonts.generic("emoji").is_empty());
    assert!(fonts.generic("Liberation Sans").is_empty());
    assert_eq!(find("sans-serif", 400, Style::Normal), "Liberation Sans");
    assert_eq!(
        find("Monospace", 700, Style::Normal),
        "Liberation Mono Bold"
    );
}

#[test]
fn a_file_is_added_once_and_a_non_font_not_at_all() {
    let mut fonts = fonts();
    let again = tree_fonts().join("liberation/LiberationSans-Regular.ttf");
    assert_eq!(fonts.add_file(&again), 0);
    assert_eq!(fonts.add_file(&tree_fonts().join("README.md")), 0);
    assert_eq!(fonts.add_file(Path::new("/nonexistent/font.ttf")), 0);
    assert_eq!(fonts.add_bytes(b"not a font".to_vec(), "junk"), 0);
    assert_eq!(fonts.faces().len(), 14);
    let face = &fonts.faces()[9];
    assert_eq!(face.style_name, "Regular");
    assert!(face.path.ends_with("LiberationSans-Regular.ttf"));
    assert!(fonts.faces()[5].monospace);
}

#[test]
fn a_description_resolves_to_a_chain_of_faces() {
    let fonts = fonts();
    let font = fonts.resolve(&FontDescription::pango(
        "Liberation Serif, Inter Variable Bold 12",
    ));
    assert!(close(font.px, 16.0));
    let names: Vec<String> = font
        .faces
        .iter()
        .map(|face| fonts.info(*face).unwrap().full_name.clone())
        .collect();
    assert_eq!(names[0], "Liberation Serif Bold");
    assert_eq!(names[1], "Inter Variable");
    // Then sans-serif's, then every other family.
    assert_eq!(names[2], "Liberation Sans Bold");
    assert!(names.contains(&"Liberation Mono Bold".to_owned()));
    let unique: std::collections::HashSet<usize> =
        font.faces.iter().map(|face| face.index()).collect();
    assert_eq!(unique.len(), font.faces.len());
    // Inter is instanced at the weight asked for; Liberation Serif Bold
    // is drawn as it is.
    assert_eq!(font.faces[0].instance().weight, None);
    assert_eq!(font.faces[1].instance().weight, Some(700));
    assert!(!font.faces[1].instance().bold);

    let nothing = Fonts::new().resolve(&FontDescription::pango("Sans 10"));
    assert!(nothing.faces.is_empty());
}

#[test]
fn a_face_lighter_than_asked_is_emboldened_and_an_upright_one_slanted() {
    let fonts = only("liberation/LiberationSans-Regular.ttf");
    let mut description = FontDescription::new(&["Liberation Sans"], Size::Pixels(32.0));
    description.weight = Weight::BOLD;
    description.style = Style::Italic;
    let font = fonts.resolve(&description);
    let instance = font.faces[0].instance();
    assert!(instance.bold);
    assert!(instance.oblique);
    assert_eq!(font.faces[0].index(), 0);
    assert_eq!(
        fonts.info(font.faces[0]).unwrap().full_name,
        "Liberation Sans"
    );

    description.weight = Weight::MEDIUM;
    description.style = Style::Normal;
    let instance = fonts.resolve(&description).faces[0].instance();
    assert!(!instance.bold);
    assert!(!instance.oblique);
}

// ---------------------------------------------------------------------------
// Shaping

#[test]
fn advances_are_the_faces_own() {
    let mut fonts = fonts();
    let font = fonts.resolve(&FontDescription::new(
        &["Liberation Sans"],
        Size::Pixels(2048.0),
    ));
    let run = fonts.shape(&font, "H");
    assert_eq!(run.glyphs.len(), 1);
    assert!(close(run.width, 1479.0));
    assert!(close(run.glyphs[0].advance, 1479.0));
    assert!(close(fonts.measure(&font, "HH"), 2958.0));
    // Liberation Sans's hhea: ascender 1854, descender -434.
    assert!(close(run.ascent, 1854.0));
    assert!(close(run.descent, 434.0));
    assert!(close(run.px, 2048.0));
    // 16 points is 21⅓ pixels, where H's 15.40 is placed as a whole 15, as
    // Pango rounds each glyph's advance, and its ascent and descent too.
    let small = fonts.resolve(&FontDescription::new(
        &["Liberation Sans"],
        Size::Points(16.0),
    ));
    assert!(close(
        fonts.measure(&small, "H"),
        (1479.0_f32 * (64.0 / 3.0) / 2048.0).round()
    ));
    assert!(close(fonts.measure(&small, "HHH"), 45.0));
    let small_run = fonts.shape(&small, "H");
    assert!(close(
        small_run.ascent,
        (1854.0_f32 * (64.0 / 3.0) / 2048.0).round()
    ));
    assert!(close(small_run.descent, 5.0));
    assert!(fonts.shape(&font, "").glyphs.is_empty());
}

#[test]
fn kerning_pulls_pairs_together() {
    let mut fonts = fonts();
    let font = fonts.resolve(&FontDescription::new(
        &["Liberation Sans"],
        Size::Pixels(2048.0),
    ));
    let apart = fonts.measure(&font, "A") + fonts.measure(&font, "V");
    let together = fonts.measure(&font, "AV");
    assert!(together < apart - 50.0, "{together} against {apart}");
    let run = fonts.shape(&font, "AV");
    assert_eq!(run.glyphs[1].cluster, 1);
    assert!(close(run.glyphs[1].x, run.glyphs[0].advance));
}

#[test]
fn a_character_the_first_face_lacks_comes_from_the_next() {
    let mut fonts = fonts();
    let font = fonts.resolve(&FontDescription::new(
        &["Liberation Sans"],
        Size::Pixels(20.0),
    ));
    let liberation = font.faces[0];
    // U+2713 CHECK MARK: Inter has it, Liberation Sans does not.
    let run = fonts.shape(&font, "a\u{2713}b");
    assert_eq!(run.glyphs.len(), 3);
    assert_eq!(run.glyphs[0].face, liberation);
    assert_ne!(run.glyphs[1].face, liberation);
    assert_eq!(
        fonts.info(run.glyphs[1].face).unwrap().families[0],
        "Inter Variable"
    );
    assert_ne!(run.glyphs[1].id, 0);
    assert_eq!(run.glyphs[2].face, liberation);
    assert_eq!(
        run.glyphs
            .iter()
            .map(|glyph| glyph.cluster)
            .collect::<Vec<_>>(),
        [0, 1, 4]
    );
    // Each glyph starts where the one before it ended.
    assert!(close(run.glyphs[1].x, run.glyphs[0].advance));
    assert!(close(
        run.glyphs[2].x,
        run.glyphs[0].advance + run.glyphs[1].advance
    ));
    let check = fonts.glyph_for(&font, '\u{2713}').unwrap();
    assert_eq!(check.face, run.glyphs[1].face);
    assert_eq!(fonts.glyph_for(&font, 'a').unwrap().face, liberation);
    // What no face has stays the first face's .notdef.
    let none = fonts.shape(&font, "\u{10fffd}");
    assert_eq!(none.glyphs[0].id, 0);
    assert_eq!(none.glyphs[0].face, liberation);
    assert!(fonts.glyph_for(&font, '\u{10fffd}').is_none());
}

#[test]
fn a_variable_face_is_instanced_at_the_weight() {
    let mut fonts = fonts();
    let at = |fonts: &mut Fonts, weight: u16| {
        let mut description = FontDescription::new(&["Inter Variable"], Size::Pixels(100.0));
        description.weight = Weight(weight);
        let font = fonts.resolve(&description);
        fonts.measure(&font, "Hamburgefonstiv")
    };
    let thin = at(&mut fonts, 100);
    let regular = at(&mut fonts, 400);
    let black = at(&mut fonts, 900);
    assert!(
        thin < regular && regular < black,
        "{thin} {regular} {black}"
    );
}

#[test]
fn ellipsizing_cuts_to_the_width() {
    let mut fonts = fonts();
    let font = fonts.resolve(&FontDescription::new(
        &["Liberation Sans"],
        Size::Pixels(20.0),
    ));
    let text = "The quick brown fox jumps over the lazy dog";
    let whole = fonts.measure(&font, text);
    let ellipsis = fonts.glyph_for(&font, '\u{2026}').unwrap();
    for at in [Ellipsize::End, Ellipsize::Start, Ellipsize::Middle] {
        let run = fonts.shape_ellipsized(&font, text, 150.0, at);
        assert!(run.width <= 150.0, "{at:?} {}", run.width);
        assert!(run.width > 120.0, "{at:?} {}", run.width);
        let position = run
            .glyphs
            .iter()
            .position(|glyph| glyph.id == ellipsis.id)
            .unwrap();
        match at {
            Ellipsize::End => assert_eq!(position, run.glyphs.len() - 1),
            Ellipsize::Start => assert_eq!(position, 0),
            _ => assert!(position > 3 && position < run.glyphs.len() - 3),
        }
        let pen: f32 = run.glyphs.iter().map(|glyph| glyph.advance).sum();
        assert!(close(pen, run.width));
    }
    let short = fonts.shape_ellipsized(&font, "short", 150.0, Ellipsize::End);
    assert_eq!(short, fonts.shape(&font, "short"));
    assert!(whole > 150.0);
    let tiny = fonts.shape_ellipsized(&font, text, 1.0, Ellipsize::End);
    assert_eq!(tiny.glyphs.len(), 1);
}

// ---------------------------------------------------------------------------
// Glyphs

#[test]
fn a_glyph_is_drawn_into_its_bounding_box() {
    let mut fonts = fonts();
    let font = fonts.resolve(&FontDescription::new(
        &["Liberation Sans"],
        Size::Pixels(32.0),
    ));
    let h = fonts.glyph_for(&font, 'H').unwrap();
    let mask = fonts.glyph(h.face, h.id, 32.0, 0.0).unwrap().clone();
    // Liberation Sans's cap height is 1409 units: 22 pixels at 32.
    assert!((23..=26).contains(&mask.height), "{mask:?}");
    assert!((22..=25).contains(&mask.top));
    assert!((0..=3).contains(&(mask.left + 1)));
    assert!((20..=25).contains(&mask.width));
    assert_eq!(mask.coverage.len(), (mask.width * mask.height) as usize);
    assert!(mask.coverage.contains(&255));
    // The margin is empty.
    assert!(
        mask.coverage[..mask.width as usize]
            .iter()
            .all(|&value| value == 0)
    );

    let shifted = fonts.glyph(h.face, h.id, 32.0, 0.5).unwrap().clone();
    assert_ne!(shifted.coverage, mask.coverage);
    let space = fonts.glyph_for(&font, ' ').unwrap();
    assert!(fonts.glyph(space.face, space.id, 32.0, 0.0).is_none());
    // Sizes no one draws at give nothing, not a panic.
    assert!(fonts.glyph(h.face, h.id, 1.0e9, 0.0).is_none());
    assert!(fonts.glyph(h.face, h.id, f32::NAN, 0.0).is_none());
    assert!(fonts.glyph(h.face, h.id, 0.0, 0.0).is_none());
}

#[test]
fn synthetic_bold_and_oblique_change_the_outline() {
    let mut fonts = only("liberation/LiberationSans-Regular.ttf");
    let draw = |fonts: &mut Fonts, weight: Weight, style: Style| {
        let mut description = FontDescription::new(&["Liberation Sans"], Size::Pixels(40.0));
        description.weight = weight;
        description.style = style;
        let font = fonts.resolve(&description);
        let glyph = fonts.glyph_for(&font, 'l').unwrap();
        fonts
            .glyph(glyph.face, glyph.id, 40.0, 0.0)
            .unwrap()
            .clone()
    };
    let regular = draw(&mut fonts, Weight::NORMAL, Style::Normal);
    let bold = draw(&mut fonts, Weight::BOLD, Style::Normal);
    let oblique = draw(&mut fonts, Weight::NORMAL, Style::Italic);
    let ink = |mask: &crate::Mask| {
        mask.coverage
            .iter()
            .map(|&value| u32::from(value))
            .sum::<u32>()
    };
    assert!(ink(&bold) > ink(&regular) * 11 / 10);
    assert!(oblique.width > regular.width + 4);
}

#[test]
fn a_run_is_blended_into_a_pixmap() {
    let mut fonts = fonts();
    let font = fonts.resolve(&FontDescription::new(
        &["Liberation Sans"],
        Size::Pixels(24.0),
    ));
    let run = fonts.shape(&font, "Hi");
    let mut pixmap = tiny_skia::Pixmap::new(60, 40).unwrap();
    fonts.draw_run(&run, &mut pixmap.as_mut(), 2.0, 30.0, Rgba::rgb(255, 0, 0));
    let pixels = pixmap.pixels();
    assert!(
        pixels
            .iter()
            .any(|pixel| pixel.alpha() == 255 && pixel.red() == 255)
    );
    assert!(
        pixels
            .iter()
            .all(|pixel| pixel.green() == 0 && pixel.red() <= pixel.alpha())
    );
    // Nothing below the baseline for "Hi", nothing above the cap height.
    for y in 31..40 {
        assert!(
            (0..60).all(|x| pixmap.pixel(x, y).unwrap().alpha() == 0),
            "row {y}"
        );
    }
    assert!((0..60).all(|x| pixmap.pixel(x, 0).unwrap().alpha() == 0));
    // Clipped, not a panic, when it runs off the edges.
    fonts.draw_run(&run, &mut pixmap.as_mut(), -10.0, 5.0, Rgba::WHITE);
    fonts.draw_run(&run, &mut pixmap.as_mut(), 50.0, 60.0, Rgba::WHITE);
}

// ---------------------------------------------------------------------------
// Layout

fn options(size: f32) -> LayoutOptions {
    LayoutOptions {
        font: FontDescription::new(&["Liberation Sans"], Size::Pixels(size)),
        ..LayoutOptions::default()
    }
}

#[test]
fn a_line_is_as_high_as_its_faces_ascent_and_descent() {
    let mut fonts = fonts();
    let layout = fonts.layout(&plain_spans("Hello"), &options(2048.0));
    assert_eq!(layout.lines.len(), 1);
    // Pango's logical rectangle: ascent and descent, not the line gap.
    assert!(close(layout.height, 1854.0 + 434.0));
    assert!(close(layout.baseline, 1854.0));
    let font = fonts.resolve(&options(2048.0).font);
    assert!(close(layout.width, fonts.measure(&font, "Hello")));
    let metrics = fonts.metrics(&font);
    assert!(close(metrics.ascent, 1854.0));
    assert!(close(metrics.descent, 434.0));
    assert!(close(metrics.line_gap, 67.0));
    assert!(close(metrics.height, 1854.0 + 434.0 + 67.0));
    assert!(metrics.underline_position > 0.0);
    assert!(metrics.strikeout_position > 0.0);
    assert!(metrics.approximate_char_width > 800.0 && metrics.approximate_char_width < 1200.0);
    assert!(close(metrics.approximate_digit_width, 1139.0));

    let empty = fonts.layout(&[], &options(2048.0));
    assert_eq!(empty.lines.len(), 1);
    assert!(close(empty.width, 0.0));
    assert!(close(empty.height, 1854.0 + 434.0));
    assert_eq!(empty.pixel_size(), (0, 2288));
}

#[test]
fn newlines_break_lines_and_alignment_places_them() {
    let mut fonts = fonts();
    let spans = plain_spans("a\nwide line\n\nb");
    let left = fonts.layout(&spans, &options(20.0));
    assert_eq!(left.lines.len(), 4);
    let line_height = left.lines[0].height;
    assert!(close(left.height, 4.0 * line_height));
    assert!(close(left.lines[2].height, line_height));
    assert!(close(left.lines[3].y, 3.0 * line_height));
    assert!(close(left.lines[1].baseline, line_height + left.baseline));
    assert!(close(left.width, left.lines[1].width));
    assert!(left.lines.iter().all(|line| close(line.x, 0.0)));

    let centre = fonts.layout(
        &spans,
        &LayoutOptions {
            align: Align::Center,
            ..options(20.0)
        },
    );
    assert!(close(
        centre.lines[0].x,
        (centre.width - centre.lines[0].width) / 2.0
    ));
    let right = fonts.layout(
        &spans,
        &LayoutOptions {
            align: Align::Right,
            ..options(20.0)
        },
    );
    assert!(close(right.lines[3].x + right.lines[3].width, right.width));
    let crlf = fonts.layout(&plain_spans("a\r\nb"), &options(20.0));
    assert_eq!(crlf.lines.len(), 2);
    assert!(close(crlf.lines[0].width, left.lines[0].width));
}

#[test]
fn line_height_follows_pango_and_fuzzel() {
    let mut fonts = fonts();
    let natural = fonts.layout(&plain_spans("x"), &options(20.0));
    let doubled = markup::parse("<span line_height=\"2\">x</span>").unwrap();
    let layout = fonts.layout(&doubled.spans, &options(20.0));
    assert!(close(layout.height, 2.0 * natural.height));
    // Half the extra above the baseline, half below.
    assert!(close(
        layout.baseline,
        natural.baseline + natural.height / 2.0
    ));

    let fixed = fonts.layout(
        &plain_spans("x\ny"),
        &LayoutOptions {
            line_height: Some(30.0),
            ..options(20.0)
        },
    );
    assert!(close(fixed.height, 60.0));
    assert!(close(fixed.lines[1].y, 30.0));
    assert!(close(
        fixed.baseline,
        natural.baseline + (30.0 - natural.height) / 2.0
    ));
}

#[test]
fn spans_are_pieces_side_by_side() {
    let mut fonts = fonts();
    let parsed = markup::parse(
        "a<span size=\"200%\" foreground=\"red\" background=\"blue\">B</span><u>c</u><s>d</s>",
    )
    .unwrap();
    let layout = fonts.layout(&parsed.spans, &options(20.0));
    let line = &layout.lines[0];
    assert_eq!(line.pieces.len(), 4);
    assert!(close(line.pieces[1].x, line.pieces[0].run.width));
    assert!(close(line.pieces[1].run.px, 40.0));
    assert_eq!(line.pieces[1].color, Rgba::rgb(255, 0, 0));
    assert_eq!(line.pieces[1].background, Some(Rgba::rgb(0, 0, 255)));
    assert_eq!(line.pieces[2].underline, Underline::Single);
    assert!(line.pieces[3].strikethrough);
    // The big piece makes the line higher.
    let small = fonts.layout(&plain_spans("a"), &options(20.0));
    assert!(layout.height > 1.9 * small.height);
    assert!(close(
        line.width,
        line.pieces.iter().map(|piece| piece.run.width).sum()
    ));

    let rise = markup::parse("a<sup>2</sup>").unwrap();
    let raised = fonts.layout(&rise.spans, &options(20.0));
    assert!(raised.lines[0].pieces[1].rise > 0.0);
    assert!(raised.height > small.height);
}

#[test]
fn trailing_spaces_are_part_of_a_line() {
    let mut fonts = fonts();
    let bare = fonts.layout(&plain_spans("ab"), &options(20.0));
    let padded = fonts.layout(&plain_spans(" ab  "), &options(20.0));
    let font = fonts.resolve(&options(20.0).font);
    let space = fonts.measure(&font, " ");
    assert!(space > 1.0);
    assert!(close(padded.width, bare.width + 3.0 * space));
    assert!(close(padded.lines[0].width, padded.width));
}

#[test]
fn letter_spacing_goes_between_characters() {
    let mut fonts = fonts();
    let plain = fonts.layout(&plain_spans("abc"), &options(20.0));
    let spaced = fonts.layout(
        &plain_spans("abc"),
        &LayoutOptions {
            letter_spacing: 3.0,
            ..options(20.0)
        },
    );
    assert!(close(spaced.width, plain.width + 6.0));
    let glyphs = &spaced.lines[0].pieces[0].run.glyphs;
    assert!(close(glyphs[1].x, glyphs[0].advance));
}

#[test]
fn wrapping_breaks_at_spaces() {
    let mut fonts = fonts();
    let font = fonts.resolve(&options(20.0).font);
    let word = fonts.measure(&font, "alpha beta");
    let wrapped = fonts.layout(
        &plain_spans("alpha beta alpha beta"),
        &LayoutOptions {
            wrap: true,
            max_width: Some(word + 1.0),
            ..options(20.0)
        },
    );
    assert_eq!(wrapped.lines.len(), 2);
    assert!(close(wrapped.width, word + 1.0));
    assert!(close(wrapped.lines[0].width, word));
    assert!(wrapped.lines.iter().all(|line| line.width <= word + 1.0));
    assert!(close(wrapped.height, 2.0 * wrapped.lines[0].height));

    // A word wider than the line is broken inside.
    let long = fonts.layout(
        &plain_spans("abcdefghijklmnopqrstuvwxyz"),
        &LayoutOptions {
            wrap: true,
            max_width: Some(60.0),
            ..options(20.0)
        },
    );
    assert!(long.lines.len() > 3);
    assert!(long.lines.iter().all(|line| line.width <= 60.0));
}

#[test]
fn ellipsizing_a_layout_cuts_each_paragraph_to_one_line() {
    let mut fonts = fonts();
    let parsed = markup::parse("The <b>quick brown</b> fox jumps over\nthe dog").unwrap();
    let layout = fonts.layout(
        &parsed.spans,
        &LayoutOptions {
            max_width: Some(100.0),
            ellipsize: Ellipsize::End,
            wrap: true,
            ..options(20.0)
        },
    );
    assert!(layout.ellipsized);
    assert_eq!(layout.lines.len(), 2);
    assert!(layout.width <= 100.0);
    let first = &layout.lines[0];
    assert!(
        first.width <= 100.0 && first.width > 70.0,
        "{}",
        first.width
    );
    let last = first.pieces.last().unwrap();
    assert_eq!(last.run.glyphs.len(), 1);
    let fits = fonts.layout(
        &plain_spans("short"),
        &LayoutOptions {
            max_width: Some(100.0),
            ellipsize: Ellipsize::End,
            ..options(20.0)
        },
    );
    assert!(!fits.ellipsized);
}

#[test]
fn a_layout_renders_into_a_pixmap_its_own_size() {
    let mut fonts = fonts();
    let parsed =
        markup::parse("<span background=\"#0000ff\" underline=\"single\">Hg</span> <s>x</s>")
            .unwrap();
    let layout = fonts.layout(&parsed.spans, &options(30.0));
    let pixmap = fonts.render(&layout).unwrap();
    assert_eq!((pixmap.width(), pixmap.height()), layout.pixel_size());
    // The background covers the piece's logical rectangle: its top-left
    // pixel is blue.
    let corner = pixmap.pixel(0, 0).unwrap();
    assert_eq!((corner.red(), corner.blue(), corner.alpha()), (0, 255, 255));
    // White text lands over it somewhere.
    assert!(
        pixmap
            .pixels()
            .iter()
            .any(|pixel| pixel.red() == 255 && pixel.blue() == 255)
    );
    // The space after the piece has no background.
    let piece = &layout.lines[0].pieces[0];
    let after = (piece.run.width.ceil() as u32 + 1).min(pixmap.width() - 1);
    assert_eq!(pixmap.pixel(after, 0).unwrap().alpha(), 0);

    let empty = fonts.layout(&[Span::default()], &options(0.0));
    let pixmap = fonts.render(&empty).unwrap();
    assert_eq!((pixmap.width(), pixmap.height()), (1, 1));
}

// ---------------------------------------------------------------------------
// The host's own fonts, by hand

#[test]
#[ignore = "reads the host's fonts; run by hand"]
#[expect(
    clippy::print_stderr,
    reason = "a probe run by hand prints what it finds"
)]
fn probe_host_fonts() {
    let mut fonts = Fonts::system();
    eprintln!("{} faces", fonts.faces().len());
    for text in [
        "Ubuntu Light 96",
        "GFS Didot 16",
        "Sans 12",
        "DejaVu Sans Bold 10",
    ] {
        let description = FontDescription::pango(text);
        let font = fonts.resolve(&description);
        let primary = font.primary().unwrap();
        let info = fonts.info(primary).unwrap().clone();
        let metrics = fonts.metrics(&font);
        let run = fonts.shape(&font, "Hamburg \u{2026} \u{2713}");
        eprintln!(
            "{text:?}: {} ({}) instance {:?}, px {}, metrics {metrics:?}",
            info.full_name,
            info.path.display(),
            primary.instance(),
            font.px,
        );
        for glyph in &run.glyphs {
            eprintln!(
                "    {:>3} {:>5} {:>8.2} {}",
                glyph.cluster,
                glyph.id,
                glyph.advance,
                fonts.info(glyph.face).unwrap().full_name
            );
        }
    }
    let fc = FontDescription::fontconfig("GFS Didot:size=16");
    let font = fonts.resolve(&fc);
    let ellipsis = fonts.glyph_for(&font, '\u{2026}').unwrap();
    eprintln!(
        "GFS Didot:size=16 -> {} at {} px; its \u{2026} from {}",
        fonts.info(font.primary().unwrap()).unwrap().full_name,
        font.px,
        fonts.info(ellipsis.face).unwrap().full_name
    );
    let run = fonts.shape_ellipsized(
        &font,
        "An entry far too long for its row",
        120.0,
        Ellipsize::End,
    );
    eprintln!("ellipsized: {} px, {} glyphs", run.width, run.glyphs.len());
    let options = LayoutOptions {
        font: FontDescription::pango("Ubuntu Light 96"),
        ..LayoutOptions::default()
    };
    let layout = fonts.layout(&plain_spans("12:34"), &options);
    eprintln!("Ubuntu Light 96 \"12:34\": {:?}", layout.pixel_size());
    let pixmap = fonts.render(&layout).unwrap();
    let inked = pixmap
        .pixels()
        .iter()
        .filter(|pixel| pixel.alpha() > 0)
        .count();
    eprintln!("    {inked} pixels inked");

    // waybar: `<span line_height='2.0'>` at Ubuntu 15px (and 15pt).
    for size in [Size::Pixels(15.0), Size::Points(15.0)] {
        let ubuntu = LayoutOptions {
            font: FontDescription::new(&["Ubuntu"], size),
            ..LayoutOptions::default()
        };
        let natural = fonts.layout(&plain_spans("Tooltip"), &ubuntu);
        let doubled = markup::parse("<span line_height='2.0'>Tooltip</span>").unwrap();
        let tall = fonts.layout(&doubled.spans, &ubuntu);
        eprintln!(
            "Ubuntu {size:?}: natural height {} baseline {}; line_height 2.0: height {} \
             baseline {} (extra above {}, below {})",
            natural.height,
            natural.baseline,
            tall.height,
            tall.baseline,
            tall.baseline - natural.baseline,
            (tall.height - tall.baseline) - (natural.height - natural.baseline),
        );
    }
    let start = std::time::Instant::now();
    let again = Fonts::system();
    eprintln!(
        "scan of {} faces: {:?}",
        again.faces().len(),
        start.elapsed()
    );
}

/// Pango's own widths on nazuna (1.57, 96 dpi, GTK3's `font-size: 15px` on
/// Ubuntu, `~/.local/share/ferrix/logs/waybar/pango-reference.txt`): a
/// line of each is 17 high with its baseline at 14.
#[test]
#[ignore = "reads the host's Ubuntu font; run by hand"]
fn widths_are_pangos_on_the_hosts_ubuntu() {
    let mut fonts = Fonts::system();
    for (description, text, width) in [
        ("Ubuntu 15px", " ", 3.0),
        ("Ubuntu 15px", "vol 0%", 44.0),
        ("Ubuntu 15px", "cpu 19%", 57.0),
        ("Ubuntu 15px", "ram 25%", 59.0),
        ("Ubuntu 15px", "eth 0.0b/s", 67.0),
        ("Ubuntu 15px", "abc", 24.0),
        ("Ubuntu Bold 15px", "1", 8.0),
        ("Ubuntu Bold 15px", "10", 16.0),
    ] {
        let font = fonts.resolve(&FontDescription::pango(description));
        let run = fonts.shape(&font, text);
        assert!(
            close(run.width, width),
            "{description} {text:?}: {}",
            run.width
        );
        assert!(
            close(run.ascent, 14.0) && close(run.descent, 3.0),
            "{text:?}"
        );
        assert!(close(fonts.metrics(&font).approximate_digit_width, 8.0));
    }
}
