//! Lines of attributed text, measured and placed.

use crate::markup::{LineHeight, Rgba, Span, SpanSize, SpanStyle, Underline};
use crate::{Font, FontDescription, Fonts, Run};

/// How the lines of a paragraph line up with each other.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Align {
    /// Ragged right.
    #[default]
    Left,
    /// Centred.
    Center,
    /// Ragged left.
    Right,
}

/// Where a line too long for the width loses its text.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ellipsize {
    /// It does not: it wraps at spaces if [`LayoutOptions::wrap`], and is
    /// otherwise as wide as it is.
    #[default]
    None,
    /// `…text`.
    Start,
    /// `te…xt`.
    Middle,
    /// `text…`, waybar's `max-length` and fuzzel's rows.
    End,
}

/// How to lay text out.
#[derive(Clone, PartialEq, Debug)]
pub struct LayoutOptions {
    /// The font a span with no font attributes is in.
    pub font: FontDescription,
    /// The colour a span with no `foreground` is in.
    pub color: Rgba,
    /// How lines line up.
    pub align: Align,
    /// The widest a line may be, in pixels.
    pub max_width: Option<f32>,
    /// What happens to a line wider than that.
    pub ellipsize: Ellipsize,
    /// Whether a line wider than that breaks at spaces instead.
    pub wrap: bool,
    /// Extra space between characters, in pixels (fuzzel's
    /// `letter-spacing`).
    pub letter_spacing: f32,
    /// A fixed line height in pixels that replaces the font's (fuzzel's
    /// `line-height`), before any `line_height` span.
    pub line_height: Option<f32>,
    /// Dots per inch that points are read at.
    pub dpi: f32,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            font: FontDescription::new(&["sans-serif"], crate::Size::default()),
            color: Rgba::WHITE,
            align: Align::Left,
            max_width: None,
            ellipsize: Ellipsize::None,
            wrap: false,
            letter_spacing: 0.0,
            line_height: None,
            dpi: crate::DPI,
        }
    }
}

/// A shaped piece of one line: a run of one style.
#[derive(Clone, PartialEq, Debug)]
pub struct Piece {
    /// Its glyphs.
    pub run: Run,
    /// Where it starts along the line, in pixels from the line's left.
    pub x: f32,
    /// How far above the line's baseline it sits (`rise`).
    pub rise: f32,
    /// Its colour.
    pub color: Rgba,
    /// Its background, drawn over the piece's logical rectangle.
    pub background: Option<Rgba>,
    /// Its underline.
    pub underline: Underline,
    /// The underline's colour, the text's where `None`.
    pub underline_color: Option<Rgba>,
    /// Whether it is struck through.
    pub strikethrough: bool,
}

/// One line of a [`Layout`].
#[derive(Clone, PartialEq, Debug)]
pub struct Line {
    /// Its pieces, left to right.
    pub pieces: Vec<Piece>,
    /// Where its left edge is in the layout, after alignment.
    pub x: f32,
    /// Where its top is in the layout.
    pub y: f32,
    /// Where its baseline is, from the layout's top.
    pub baseline: f32,
    /// Its logical width.
    pub width: f32,
    /// Its logical height, `line_height` applied.
    pub height: f32,
}

/// Text laid out: every line placed, and the logical rectangle of all of
/// them, which is Pango's `pango_layout_get_pixel_extents` logical rect.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Layout {
    /// The lines, top first.
    pub lines: Vec<Line>,
    /// The widest line's logical width.
    pub width: f32,
    /// The lines' heights summed.
    pub height: f32,
    /// The first line's baseline from the top: what GTK aligns a label's
    /// text by.
    pub baseline: f32,
    /// Whether any line was ellipsized.
    pub ellipsized: bool,
}

impl Layout {
    /// The logical size rounded up to whole pixels.
    #[must_use]
    pub fn pixel_size(&self) -> (u32, u32) {
        let up = |value: f32| {
            let rounded = value.ceil();
            if rounded <= 0.0 {
                0
            } else if rounded >= u32::MAX as f32 {
                u32::MAX
            } else {
                rounded as u32
            }
        };
        (up(self.width), up(self.height))
    }
}

/// Spans for plain text in one style, for a caller that has no markup.
#[must_use]
pub fn plain_spans(text: &str) -> Vec<Span> {
    crate::markup::plain(text).spans
}

/// A line's height as a piece asks for it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Height {
    /// A factor of the piece's natural height.
    Factor(f32),
    /// Pixels.
    Pixels(f32),
}

/// A span's attributes with the layout's defaults filled in and its font
/// resolved.
#[derive(Clone, Debug)]
struct Resolved {
    font: Font,
    color: Rgba,
    background: Option<Rgba>,
    underline: Underline,
    underline_color: Option<Rgba>,
    strikethrough: bool,
    rise: f32,
    spacing: f32,
    line_height: Option<Height>,
}

/// A size times `factor`.
fn scaled(size: crate::Size, factor: f32) -> crate::Size {
    match size {
        crate::Size::Points(points) => crate::Size::Points(points * factor),
        crate::Size::Pixels(pixels) => crate::Size::Pixels(pixels * factor),
    }
}

/// The description a span's text is in: the layout's font, then the
/// span's `font`, family, size, style and weight over it.
fn description(style: &SpanStyle, options: &LayoutOptions) -> FontDescription {
    let mut description = options.font.clone();
    if let Some(font) = &style.font {
        if !font.families.is_empty() {
            description.families.clone_from(&font.families);
        }
        description.weight = font.weight;
        description.style = font.style;
        // A size of nothing is a description that gave none.
        if font.size.pixels(options.dpi) > 0.0 {
            description.size = font.size;
        }
    }
    if let Some(family) = &style.family {
        description.families = family
            .split(',')
            .map(str::trim)
            .filter(|family| !family.is_empty())
            .map(str::to_owned)
            .collect();
    }
    if style.monospace {
        description.families = vec!["monospace".to_owned()];
    }
    match style.size {
        Some(SpanSize::Size(size)) => description.size = size,
        Some(SpanSize::Scale(factor)) => description.size = scaled(description.size, factor),
        None => {}
    }
    if let Some(slant) = style.style {
        description.style = slant;
    }
    if let Some(weight) = style.weight {
        description.weight = weight;
    }
    description
}

/// `style` resolved, fonts shared through `cache`.
fn resolve(
    fonts: &Fonts,
    cache: &mut Vec<(FontDescription, Font)>,
    style: &SpanStyle,
    options: &LayoutOptions,
) -> Resolved {
    let description = description(style, options);
    let font = if let Some((_, font)) = cache.iter().find(|(known, _)| *known == description) {
        font.clone()
    } else {
        let mut font = fonts.resolve(&description);
        font.px = description.size.pixels(options.dpi);
        cache.push((description, font.clone()));
        font
    };
    Resolved {
        font,
        color: style.foreground.unwrap_or(options.color),
        background: style.background,
        underline: style.underline.unwrap_or_default(),
        underline_color: style.underline_color,
        strikethrough: style.strikethrough.unwrap_or(false),
        rise: style.rise.unwrap_or(0.0),
        spacing: style.letter_spacing.unwrap_or(options.letter_spacing),
        line_height: style
            .line_height
            .map(|height| match height {
                LineHeight::Factor(factor) => Height::Factor(factor),
                LineHeight::Size(size) => Height::Pixels(size.pixels(options.dpi)),
            })
            .or(options.line_height.map(Height::Pixels)),
    }
}

/// `spacing` added after every cluster of `run`.
fn spaced(mut run: Run, spacing: f32) -> Run {
    if spacing == 0.0 {
        return run;
    }
    let clusters: Vec<usize> = run.glyphs.iter().map(|glyph| glyph.cluster).collect();
    let mut extra = 0.0;
    for (at, glyph) in run.glyphs.iter_mut().enumerate() {
        glyph.x += extra;
        if clusters.get(at + 1) != Some(&glyph.cluster) {
            glyph.advance += spacing;
            extra += spacing;
        }
    }
    run.width += extra;
    run
}

/// A stretch of one run's glyphs from one cluster.
#[derive(Clone, Copy, Debug)]
struct Unit {
    /// Which run.
    run: usize,
    /// Its cluster.
    cluster: usize,
    /// Its advance.
    width: f32,
}

/// The units of `runs` in visual order, and for each glyph of each run
/// the unit it is in.
fn units(runs: &[Run]) -> (Vec<Unit>, Vec<Vec<usize>>) {
    let mut units: Vec<Unit> = Vec::new();
    let mut of_glyph = Vec::with_capacity(runs.len());
    for (index, run) in runs.iter().enumerate() {
        let mut map = Vec::with_capacity(run.glyphs.len());
        let mut previous = None;
        for glyph in &run.glyphs {
            if previous != Some(glyph.cluster) || units.is_empty() {
                units.push(Unit {
                    run: index,
                    cluster: glyph.cluster,
                    width: 0.0,
                });
                previous = Some(glyph.cluster);
            }
            if let Some(unit) = units.last_mut() {
                unit.width += glyph.advance;
            }
            map.push(units.len().saturating_sub(1));
        }
        of_glyph.push(map);
    }
    (units, of_glyph)
}

/// Which units to keep: those before the first and from the second on.
fn gap(widths: &[f32], ellipsis: f32, max_width: f32, at: Ellipsize) -> (usize, usize) {
    let count = widths.len();
    let mut prefix = Vec::with_capacity(count + 1);
    let mut total = 0.0;
    prefix.push(0.0);
    for width in widths {
        total += width;
        prefix.push(total);
    }
    let before = |index: usize| prefix.get(index).copied().unwrap_or(total);
    match at {
        Ellipsize::None => (count, count),
        Ellipsize::End => {
            let keep = (0..=count)
                .rev()
                .find(|&index| before(index) + ellipsis <= max_width)
                .unwrap_or(0);
            (keep, count)
        }
        Ellipsize::Start => {
            let from = (0..=count)
                .find(|&index| total - before(index) + ellipsis <= max_width)
                .unwrap_or(count);
            (0, from)
        }
        Ellipsize::Middle => {
            if count == 0 {
                return (0, 0);
            }
            // The gap starts as the unit across the middle and grows on
            // the side that keeps more, as Pango's does.
            let centre = (0..count)
                .find(|&index| before(index + 1) > total / 2.0)
                .unwrap_or(count - 1);
            let (mut first, mut second) = (centre, centre + 1);
            while before(first) + (total - before(second)) + ellipsis > max_width
                && (first > 0 || second < count)
            {
                let left = before(first);
                let right = total - before(second);
                if (left >= right && first > 0) || second >= count {
                    first -= 1;
                } else {
                    second += 1;
                }
            }
            (first, second)
        }
    }
}

/// `run` with only the glyphs `keep` says, placed again from zero.
fn compact(run: &Run, mut keep: impl FnMut(usize) -> bool) -> Run {
    let mut glyphs = Vec::new();
    let (mut pen, mut original) = (0.0, 0.0);
    for (index, glyph) in run.glyphs.iter().enumerate() {
        if keep(index) {
            let mut placed = *glyph;
            placed.x = pen + (glyph.x - original);
            pen += glyph.advance;
            glyphs.push(placed);
        }
        original += glyph.advance;
    }
    Run {
        glyphs,
        width: pen,
        ascent: run.ascent,
        descent: run.descent,
        px: run.px,
    }
}

/// A line of runs cut to fit: each kept piece with the index of the run
/// it came from, the ellipsis among them.
#[derive(Clone, Debug)]
pub(crate) struct Cut {
    /// The pieces, left to right.
    pub(crate) pieces: Vec<(usize, Run)>,
}

/// `runs`, one line, cut to fit `max_width` with an ellipsis where `at`
/// says. `ellipsis_for(run)` shapes the ellipsis in the style of run
/// `run`, which is the run the gap starts in. `end` is the cluster the
/// ellipsis gets when the gap is at the very end.
pub(crate) fn ellipsize(
    runs: &[Run],
    mut ellipsis_for: impl FnMut(usize) -> Run,
    max_width: f32,
    at: Ellipsize,
    end: usize,
) -> Cut {
    let (units, of_glyph) = units(runs);
    let widths: Vec<f32> = units.iter().map(|unit| unit.width).collect();
    let run_at = |first: usize| units.get(first).or(units.last()).map_or(0, |unit| unit.run);
    // The ellipsis is shaped in the style where the gap starts; a first
    // guess, then again if the gap turned out to start elsewhere.
    let guess = match at {
        Ellipsize::Start => 0,
        Ellipsize::Middle => runs.len() / 2,
        Ellipsize::End | Ellipsize::None => runs.len().saturating_sub(1),
    };
    let mut ellipsis = ellipsis_for(guess);
    let (mut first, mut second) = gap(&widths, ellipsis.width, max_width, at);
    if run_at(first) != guess {
        ellipsis = ellipsis_for(run_at(first));
        (first, second) = gap(&widths, ellipsis.width, max_width, at);
    }
    let cluster = units.get(first).map_or(end, |unit| unit.cluster);
    for glyph in &mut ellipsis.glyphs {
        glyph.cluster = cluster;
    }
    let unit_of = |run: usize, glyph: usize| {
        of_glyph
            .get(run)
            .and_then(|map| map.get(glyph))
            .copied()
            .unwrap_or(0)
    };
    let mut pieces = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        let kept = compact(run, |glyph| unit_of(index, glyph) < first);
        if !kept.glyphs.is_empty() {
            pieces.push((index, kept));
        }
    }
    pieces.push((run_at(first), ellipsis));
    for (index, run) in runs.iter().enumerate() {
        let kept = compact(run, |glyph| unit_of(index, glyph) >= second);
        if !kept.glyphs.is_empty() {
            pieces.push((index, kept));
        }
    }
    Cut { pieces }
}

/// Runs laid end to end as one.
pub(crate) fn join(runs: impl IntoIterator<Item = Run>) -> Run {
    let mut joined = Run::default();
    let mut first = true;
    for run in runs {
        if first {
            joined.px = run.px;
            first = false;
        }
        for mut glyph in run.glyphs {
            glyph.x += joined.width;
            joined.glyphs.push(glyph);
        }
        joined.width += run.width;
        joined.ascent = joined.ascent.max(run.ascent);
        joined.descent = joined.descent.max(run.descent);
    }
    joined
}

/// A paragraph's text in its styles.
type Paragraph = Vec<(usize, String)>;

/// Where a wrapped paragraph's lines are: byte ranges of its text.
fn break_lines(paragraph: &Paragraph, runs: &[Run], max_width: f32) -> Vec<(usize, usize)> {
    let text: String = paragraph.iter().map(|(_, text)| text.as_str()).collect();
    // Each unit's start in the paragraph's text, its width, and whether
    // it is white space a line may break at.
    let mut units: Vec<(usize, f32)> = Vec::new();
    let mut offset = 0;
    for ((_, part), run) in paragraph.iter().zip(runs) {
        let (found, _) = self::units(std::slice::from_ref(run));
        units.extend(found.iter().map(|unit| (offset + unit.cluster, unit.width)));
        offset += part.len();
    }
    units.sort_by_key(|&(start, _)| start);
    let count = units.len();
    let start_of = |index: usize| units.get(index).map_or(text.len(), |&(start, _)| start);
    let is_space = |index: usize| {
        text.get(start_of(index)..start_of(index + 1))
            .is_some_and(|piece| !piece.is_empty() && piece.chars().all(char::is_whitespace))
    };
    let width_of = |index: usize| units.get(index).map_or(0.0, |&(_, width)| width);
    let mut lines = Vec::new();
    let mut line_start = 0;
    while line_start < count {
        let mut width = 0.0;
        let mut last_break: Option<(usize, usize)> = None;
        let (mut line_end, mut next) = (count, count);
        let mut index = line_start;
        while index < count {
            if is_space(index) {
                let mut after = index;
                while after < count && is_space(after) {
                    width += width_of(after);
                    after += 1;
                }
                if index > line_start {
                    last_break = Some((index, after));
                }
                index = after;
                continue;
            }
            if width + width_of(index) > max_width && index > line_start {
                (line_end, next) = last_break.unwrap_or((index, index));
                break;
            }
            width += width_of(index);
            index += 1;
        }
        lines.push((start_of(line_start), start_of(line_end)));
        line_start = next;
    }
    if lines.is_empty() {
        lines.push((0, text.len()));
    }
    lines
}

/// The parts of `paragraph` inside the byte range `from..to` of its text.
fn slice(paragraph: &Paragraph, from: usize, to: usize) -> Paragraph {
    let mut out = Vec::new();
    let mut offset = 0;
    for (style, text) in paragraph {
        let (start, end) = (offset, offset + text.len());
        offset = end;
        let (low, high) = (from.max(start), to.min(end));
        if low < high
            && let Some(part) = text.get(low - start..high - start)
        {
            out.push((*style, part.to_owned()));
        }
    }
    if out.is_empty()
        && let Some((style, _)) = paragraph.first()
    {
        out.push((*style, String::new()));
    }
    out
}

/// A line's pieces placed and measured.
fn build_line(mut pieces: Vec<(usize, Run)>, styles: &[Resolved]) -> Line {
    if pieces.iter().any(|(_, run)| !run.glyphs.is_empty()) {
        pieces.retain(|(_, run)| !run.glyphs.is_empty());
    } else {
        pieces.truncate(1);
    }
    // The spacing after the line's last cluster is not part of the line.
    if let Some((style, run)) = pieces.last_mut() {
        let spacing = styles.get(*style).map_or(0.0, |style| style.spacing);
        if spacing != 0.0
            && let Some(glyph) = run.glyphs.last_mut()
        {
            glyph.advance -= spacing;
            run.width -= spacing;
        }
    }
    let mut placed = Vec::with_capacity(pieces.len());
    let (mut x, mut top, mut bottom) = (0.0, f32::MIN, f32::MIN);
    for (style, run) in pieces {
        let Some(style) = styles.get(style) else {
            continue;
        };
        let natural = run.ascent + run.descent;
        let mut up = run.ascent + style.rise;
        let mut down = run.descent - style.rise;
        let height = match style.line_height {
            Some(Height::Factor(factor)) => Some(factor * natural),
            Some(Height::Pixels(pixels)) => Some(pixels),
            None => None,
        };
        if let Some(height) = height {
            let leading = height - natural;
            up += leading / 2.0;
            down += leading / 2.0;
        }
        top = top.max(up);
        bottom = bottom.max(down);
        let width = run.width;
        placed.push(Piece {
            run,
            x,
            rise: style.rise,
            color: style.color,
            background: style.background,
            underline: style.underline,
            underline_color: style.underline_color,
            strikethrough: style.strikethrough,
        });
        x += width;
    }
    if placed.is_empty() {
        (top, bottom) = (0.0, 0.0);
    }
    Line {
        pieces: placed,
        x: 0.0,
        y: 0.0,
        baseline: top,
        width: x,
        height: top + bottom,
    }
}

/// [`Fonts::layout`].
pub(crate) fn lay_out(fonts: &mut Fonts, spans: &[Span], options: &LayoutOptions) -> Layout {
    let empty = [Span::default()];
    let spans = if spans.is_empty() { &empty[..] } else { spans };
    let mut cache = Vec::new();
    let mut styles: Vec<Resolved> = Vec::with_capacity(spans.len());
    let mut paragraphs: Vec<Paragraph> = vec![Vec::new()];
    for span in spans {
        let index = styles.len();
        styles.push(resolve(fonts, &mut cache, &span.style, options));
        let mut parts = span.text.split('\n').peekable();
        let mut first = true;
        while let Some(part) = parts.next() {
            if !first {
                paragraphs.push(Vec::new());
            }
            first = false;
            // `\r\n` is one break.
            let part = match parts.peek() {
                Some(_) => part.strip_suffix('\r').unwrap_or(part),
                None => part,
            };
            if let Some(paragraph) = paragraphs.last_mut() {
                paragraph.push((index, part.to_owned()));
            }
        }
    }
    let shape = |fonts: &mut Fonts, part: &(usize, String)| {
        let run = styles.get(part.0).map_or_else(Run::default, |style| {
            spaced(fonts.shape(&style.font, &part.1), style.spacing)
        });
        (part.0, run)
    };
    let mut lines: Vec<Line> = Vec::new();
    let mut ellipsized = false;
    for paragraph in &mut paragraphs {
        if paragraph.iter().any(|(_, text)| !text.is_empty()) {
            paragraph.retain(|(_, text)| !text.is_empty());
        } else {
            paragraph.truncate(1);
        }
        let shaped: Vec<(usize, Run)> = paragraph.iter().map(|part| shape(fonts, part)).collect();
        let width: f32 = shaped.iter().map(|(_, run)| run.width).sum();
        match options.max_width {
            Some(max_width) if options.ellipsize != Ellipsize::None && width > max_width => {
                let runs: Vec<Run> = shaped.iter().map(|(_, run)| run.clone()).collect();
                let cut = ellipsize(
                    &runs,
                    |run| {
                        let style = shaped.get(run).and_then(|(style, _)| styles.get(*style));
                        style.map_or_else(Run::default, |style| {
                            spaced(fonts.ellipsis(&style.font), style.spacing)
                        })
                    },
                    max_width,
                    options.ellipsize,
                    paragraph.last().map_or(0, |(_, text)| text.len()),
                );
                let pieces = cut
                    .pieces
                    .into_iter()
                    .map(|(run, piece)| (shaped.get(run).map_or(0, |(style, _)| *style), piece))
                    .collect();
                lines.push(build_line(pieces, &styles));
                ellipsized = true;
            }
            Some(max_width) if options.wrap && options.ellipsize == Ellipsize::None => {
                let runs: Vec<Run> = shaped.iter().map(|(_, run)| run.clone()).collect();
                for (from, to) in break_lines(paragraph, &runs, max_width) {
                    let pieces = slice(paragraph, from, to)
                        .iter()
                        .map(|part| shape(fonts, part))
                        .collect();
                    lines.push(build_line(pieces, &styles));
                }
            }
            _ => lines.push(build_line(shaped, &styles)),
        }
    }
    let widest = lines.iter().map(|line| line.width).fold(0.0_f32, f32::max);
    let width = match options.max_width {
        Some(max_width) if options.wrap && options.ellipsize == Ellipsize::None => max_width,
        _ => widest,
    };
    let mut y = 0.0;
    for line in &mut lines {
        let room = (width - line.width).max(0.0);
        line.x = match options.align {
            Align::Left => 0.0,
            Align::Center => room / 2.0,
            Align::Right => room,
        };
        line.y = y;
        line.baseline += y;
        y += line.height;
    }
    Layout {
        baseline: lines.first().map_or(0.0, |line| line.baseline),
        lines,
        width,
        height: y,
        ellipsized,
    }
}
