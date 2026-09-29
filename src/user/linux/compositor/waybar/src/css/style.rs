//! The computed style: every property a node has once the cascade and
//! inheritance are done, colours resolved and lengths in pixels.
//!
//! Initial values are GTK3's (`gtkcssstylepropertyimpl.c`): `color` white,
//! `font-family` `Sans`, `font-size` `medium` (which is the GTK font
//! setting's size and is 11pt, 14.67 px, here), backgrounds transparent
//! with a `repeat`ed, `auto`-sized layer at `0 0`, clipped to the border box
//! and positioned in the padding box, no border, no shadow, `min-width` and
//! `min-height` 0.
//!
//! Inherited: `color`, the font properties and `text-shadow`. A border
//! whose style is `none` or `hidden` has a width of zero, whatever width
//! was declared, as CSS computes it.

use std::path::PathBuf;

use super::Stylesheet;
use super::property::{Declared, FontStyle, Id, Prop};
use super::selector::Node;
use super::value::{
    self, Angle, Area, BorderStyle, Color, FontSize, FontWeight, Image, Length, Repeat, Rgba, Size,
};

/// `medium`: GTK's default font size, 11pt at 96 dpi.
pub const MEDIUM_PX: f32 = 11.0 * 96.0 / 72.0;

/// A shadow in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowPx {
    /// `inset`.
    pub inset: bool,
    /// Right.
    pub x: f32,
    /// Down.
    pub y: f32,
    /// Blur radius.
    pub blur: f32,
    /// Spread.
    pub spread: f32,
    /// Colour.
    pub color: Rgba,
}

/// A gradient with its colours resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// The direction.
    pub angle: Angle,
    /// Colours and where they are along the line.
    pub stops: Vec<(Rgba, Option<Length>)>,
    /// `repeating-`.
    pub repeating: bool,
}

/// A layer's image, resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerImage {
    /// A file, its path absolute.
    File(PathBuf),
    /// A linear gradient.
    Linear(Gradient),
    /// An image kind this does not draw, by function name.
    Other(String),
}

/// One background layer, with the lists of sizes, positions and repeats
/// repeated to the number of images as CSS does.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// The image.
    pub image: LayerImage,
    /// Its size.
    pub size: Size,
    /// Its position: offsets, each a percentage of the room left and a length.
    pub position: (Length, Length),
    /// How it repeats, per axis.
    pub repeat: (Repeat, Repeat),
    /// Where it is clipped.
    pub clip: Area,
    /// Where it is positioned.
    pub origin: Area,
}

/// Every property of one node, computed.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    /// `color`.
    pub color: Rgba,
    /// `opacity`.
    pub opacity: f32,
    /// `font-family`, in order.
    pub font_family: Vec<String>,
    /// `font-size`, in pixels.
    pub font_size: f32,
    /// `font-weight`: 100 to 1000.
    pub font_weight: u16,
    /// `font-style`.
    pub font_style: FontStyle,
    /// `text-shadow`.
    pub text_shadow: Vec<ShadowPx>,
    /// `box-shadow`.
    pub box_shadow: Vec<ShadowPx>,
    /// `margin`, top, right, bottom, left.
    pub margin: [f32; 4],
    /// `padding`, top, right, bottom, left.
    pub padding: [f32; 4],
    /// Border widths, zero where the style is `none` or `hidden`.
    pub border_width: [f32; 4],
    /// Border styles.
    pub border_style: [BorderStyle; 4],
    /// Border colours.
    pub border_color: [Rgba; 4],
    /// Corner radii, horizontal and vertical, top left first; percentages
    /// are of the border box and resolved when it is known.
    pub radius: [(Length, Length); 4],
    /// `background-color`.
    pub background_color: Rgba,
    /// The background layers, the first listed first (frontmost).
    pub layers: Vec<Layer>,
    /// `min-width`.
    pub min_width: f32,
    /// `min-height`.
    pub min_height: f32,
    /// `transition-duration`s; nonzero means a change is animated.
    pub transition: Vec<f32>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            opacity: 1.0,
            font_family: vec!["Sans".to_owned()],
            font_size: MEDIUM_PX,
            font_weight: 400,
            font_style: FontStyle::Normal,
            text_shadow: Vec::new(),
            box_shadow: Vec::new(),
            margin: [0.0; 4],
            padding: [0.0; 4],
            border_width: [0.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [Rgba::TRANSPARENT; 4],
            radius: [(Length::ZERO, Length::ZERO); 4],
            background_color: Rgba::TRANSPARENT,
            layers: Vec::new(),
            min_width: 0.0,
            min_height: 0.0,
            transition: Vec::new(),
        }
    }
}

/// The declared background lists of one node, before they are resolved.
#[derive(Clone, Debug, Default)]
struct Backgrounds {
    images: Vec<Image>,
    sizes: Vec<Size>,
    positions: Vec<(Length, Length)>,
    repeats: Vec<(Repeat, Repeat)>,
    clips: Vec<Area>,
    origins: Vec<Area>,
}

/// The values one node declares, by longhand, in cascade order.
#[derive(Debug, Default)]
struct Winners {
    set: Vec<(Id, Declared)>,
}

impl Winners {
    fn get(&self, id: &Id) -> Option<&Declared> {
        self.set
            .iter()
            .rev()
            .find(|(at, _)| at == id)
            .map(|(_, d)| d)
    }
}

/// What a declared value is to the computation: its own, the parent's, or
/// the initial one.
enum Source<'a> {
    Own(&'a Prop),
    Parent,
    Initial,
}

fn source<'a>(winners: &'a Winners, id: &Id) -> Source<'a> {
    match winners.get(id) {
        Some(Declared::Value(prop)) => Source::Own(prop),
        Some(Declared::Inherit(_)) => Source::Parent,
        Some(Declared::Unset(_)) if super::property::inherited(id) => Source::Parent,
        Some(Declared::Initial(_) | Declared::Unset(_)) => Source::Initial,
        None if super::property::inherited(id) => Source::Parent,
        None => Source::Initial,
    }
}

impl Stylesheet {
    /// Resolve a colour: `@name`s looked up, functions applied, and
    /// `currentColor` as `current`. `None` for a name never defined, or
    /// names defined in terms of each other.
    #[must_use]
    pub fn resolve(&self, color: &Color, current: Rgba) -> Option<Rgba> {
        self.resolve_depth(color, current, 0)
    }

    fn resolve_depth(&self, color: &Color, current: Rgba, depth: usize) -> Option<Rgba> {
        if depth > 64 {
            return None;
        }
        Some(match color {
            Color::Rgba(rgba) => *rgba,
            Color::Current => current,
            Color::Named(name) => self.resolve_depth(self.color(name)?, current, depth + 1)?,
            Color::Alpha(base, by) => {
                let mut rgba = self.resolve_depth(base, current, depth + 1)?;
                rgba.a = (rgba.a * by).clamp(0.0, 1.0);
                rgba
            }
            Color::Shade(base, by) => {
                value::shade(self.resolve_depth(base, current, depth + 1)?, *by)
            }
            Color::Mix(a, b, by) => {
                let a = self.resolve_depth(a, current, depth + 1)?;
                let b = self.resolve_depth(b, current, depth + 1)?;
                let mix = |x: f32, y: f32| (x + (y - x) * by).clamp(0.0, 1.0);
                Rgba {
                    r: mix(a.r, b.r),
                    g: mix(a.g, b.g),
                    b: mix(a.b, b.b),
                    a: mix(a.a, b.a),
                }
            }
        })
    }

    /// The computed style of `node`, whose parent's computed style is
    /// `parent` (`None` for a root, which inherits the initial values).
    pub fn compute<N: Node>(&self, node: &N, parent: Option<&Style>) -> Style {
        let mut winners = Winners::default();
        for declaration in self.matching(node) {
            for declared in &declaration.set {
                let id = match declared {
                    Declared::Value(prop) => prop.id(),
                    Declared::Initial(id) | Declared::Inherit(id) | Declared::Unset(id) => {
                        id.clone()
                    }
                };
                winners.set.push((id, declared.clone()));
            }
        }
        self.compute_from(&winners, parent)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one property after the other, as GTK lists them"
    )]
    fn compute_from(&self, winners: &Winners, parent: Option<&Style>) -> Style {
        let initial = Style::default();
        let parent = parent.unwrap_or(&initial);
        // Font size first: `em`s everywhere else are of it.
        let font_size = match source(winners, &Id::FontSize) {
            Source::Own(Prop::FontSize(size)) => match size {
                FontSize::Length(length) => {
                    length.px + (length.em + length.percent / 100.0) * parent.font_size
                }
                FontSize::Absolute(factor) => MEDIUM_PX * factor,
                FontSize::Relative(factor) => parent.font_size * factor,
            },
            Source::Parent => parent.font_size,
            _ => MEDIUM_PX,
        };
        let mut style = Style {
            font_size,
            ..Style::default()
        };
        let em = style.font_size;
        let px = |length: &Length| length.px + length.em * em;

        style.color = match source(winners, &Id::Color) {
            Source::Own(Prop::Color(color)) => {
                self.resolve(color, parent.color).unwrap_or(parent.color)
            }
            Source::Parent => parent.color,
            _ => initial.color,
        };
        let current = style.color;
        style.font_family = match source(winners, &Id::FontFamily) {
            Source::Own(Prop::FontFamily(family)) => family.clone(),
            Source::Parent => parent.font_family.clone(),
            _ => initial.font_family.clone(),
        };
        style.font_weight = match source(winners, &Id::FontWeight) {
            Source::Own(Prop::FontWeight(weight)) => match weight {
                FontWeight::Number(n) => *n,
                // CSS Fonts 4's table.
                FontWeight::Bolder => match parent.font_weight {
                    0..350 => 400,
                    350..550 => 700,
                    _ => 900,
                },
                FontWeight::Lighter => match parent.font_weight {
                    0..550 => 100,
                    550..750 => 400,
                    _ => 700,
                },
            },
            Source::Parent => parent.font_weight,
            _ => initial.font_weight,
        };
        style.font_style = match source(winners, &Id::FontStyle) {
            Source::Own(Prop::FontStyle(font_style)) => *font_style,
            Source::Parent => parent.font_style,
            _ => initial.font_style,
        };
        let shadows = |list: &[value::Shadow]| -> Vec<ShadowPx> {
            list.iter()
                .map(|shadow| ShadowPx {
                    inset: shadow.inset,
                    x: px(&shadow.x),
                    y: px(&shadow.y),
                    blur: px(&shadow.blur),
                    spread: px(&shadow.spread),
                    color: self
                        .resolve(&shadow.color, current)
                        .unwrap_or(Rgba::TRANSPARENT),
                })
                .collect()
        };
        style.text_shadow = match source(winners, &Id::TextShadow) {
            Source::Own(Prop::TextShadow(list)) => shadows(list),
            Source::Parent => parent.text_shadow.clone(),
            _ => Vec::new(),
        };
        style.box_shadow = match source(winners, &Id::BoxShadow) {
            Source::Own(Prop::BoxShadow(list)) => shadows(list),
            Source::Parent => parent.box_shadow.clone(),
            _ => Vec::new(),
        };
        style.opacity = match source(winners, &Id::Opacity) {
            Source::Own(Prop::Opacity(opacity)) => *opacity,
            Source::Parent => parent.opacity,
            _ => 1.0,
        };
        for side in 0..4u8 {
            let index = usize::from(side);
            if let Some(slot) = style.margin.get_mut(index) {
                *slot = match source(winners, &Id::Margin(side)) {
                    Source::Own(Prop::Margin(_, length)) => px(length),
                    Source::Parent => parent.margin.get(index).copied().unwrap_or(0.0),
                    _ => 0.0,
                };
            }
            if let Some(slot) = style.padding.get_mut(index) {
                *slot = match source(winners, &Id::Padding(side)) {
                    Source::Own(Prop::Padding(_, length)) => px(length),
                    Source::Parent => parent.padding.get(index).copied().unwrap_or(0.0),
                    _ => 0.0,
                };
            }
            let border_style = match source(winners, &Id::BorderStyle(side)) {
                Source::Own(Prop::BorderStyle(_, found)) => *found,
                Source::Parent => parent
                    .border_style
                    .get(index)
                    .copied()
                    .unwrap_or(BorderStyle::None),
                _ => BorderStyle::None,
            };
            if let Some(slot) = style.border_style.get_mut(index) {
                *slot = border_style;
            }
            let width = match source(winners, &Id::BorderWidth(side)) {
                Source::Own(Prop::BorderWidth(_, length)) => px(length),
                Source::Parent => parent.border_width.get(index).copied().unwrap_or(0.0),
                // `medium`, which CSS makes 3px; GTK3's initial is 0.
                _ => 0.0,
            };
            if let Some(slot) = style.border_width.get_mut(index) {
                *slot = if matches!(border_style, BorderStyle::None | BorderStyle::Hidden) {
                    0.0
                } else {
                    width
                };
            }
            if let Some(slot) = style.border_color.get_mut(index) {
                *slot = match source(winners, &Id::BorderColor(side)) {
                    Source::Own(Prop::BorderColor(_, color)) => {
                        self.resolve(color, current).unwrap_or(current)
                    }
                    Source::Parent => parent.border_color.get(index).copied().unwrap_or(current),
                    _ => current,
                };
            }
            if let Some(slot) = style.radius.get_mut(index) {
                *slot = match source(winners, &Id::Radius(side)) {
                    Source::Own(Prop::Radius(_, h, v)) => (*h, *v),
                    Source::Parent => parent
                        .radius
                        .get(index)
                        .copied()
                        .unwrap_or((Length::ZERO, Length::ZERO)),
                    _ => (Length::ZERO, Length::ZERO),
                };
                // `em`s resolve here; percentages wait for the box.
                slot.0 = Length {
                    px: px(&slot.0),
                    em: 0.0,
                    percent: slot.0.percent,
                };
                slot.1 = Length {
                    px: px(&slot.1),
                    em: 0.0,
                    percent: slot.1.percent,
                };
            }
        }
        style.background_color = match source(winners, &Id::BackgroundColor) {
            Source::Own(Prop::BackgroundColor(color)) => {
                self.resolve(color, current).unwrap_or(Rgba::TRANSPARENT)
            }
            Source::Parent => parent.background_color,
            _ => Rgba::TRANSPARENT,
        };
        let mut lists = Backgrounds::default();
        if let Source::Own(Prop::BackgroundImage(images)) = source(winners, &Id::BackgroundImage) {
            lists.images.clone_from(images);
        }
        if let Source::Own(Prop::BackgroundSize(sizes)) = source(winners, &Id::BackgroundSize) {
            lists.sizes.clone_from(sizes);
        }
        if let Source::Own(Prop::BackgroundPosition(positions)) =
            source(winners, &Id::BackgroundPosition)
        {
            lists.positions.clone_from(positions);
        }
        if let Source::Own(Prop::BackgroundRepeat(repeats)) = source(winners, &Id::BackgroundRepeat)
        {
            lists.repeats.clone_from(repeats);
        }
        if let Source::Own(Prop::BackgroundClip(clips)) = source(winners, &Id::BackgroundClip) {
            lists.clips.clone_from(clips);
        }
        if let Source::Own(Prop::BackgroundOrigin(origins)) = source(winners, &Id::BackgroundOrigin)
        {
            lists.origins.clone_from(origins);
        }
        style.layers = self.layers(&lists, current, em);
        // The background colour is clipped as the last layer is.
        style.min_width = match source(winners, &Id::MinWidth) {
            Source::Own(Prop::MinWidth(length)) => px(length),
            Source::Parent => parent.min_width,
            _ => 0.0,
        };
        style.min_height = match source(winners, &Id::MinHeight) {
            Source::Own(Prop::MinHeight(length)) => px(length),
            Source::Parent => parent.min_height,
            _ => 0.0,
        };
        style.transition = match source(winners, &Id::TransitionDuration) {
            Source::Own(Prop::TransitionDuration(list)) => list.clone(),
            Source::Parent => parent.transition.clone(),
            _ => Vec::new(),
        };
        style
    }

    /// The layers: one per image, the other lists cycled to match.
    fn layers(&self, lists: &Backgrounds, current: Rgba, em: f32) -> Vec<Layer> {
        let pick = |index: usize, len: usize| if len == 0 { None } else { Some(index % len) };
        let em_px = |length: &Length| Length {
            px: length.px + length.em * em,
            em: 0.0,
            percent: length.percent,
        };
        lists
            .images
            .iter()
            .enumerate()
            .filter_map(|(index, image)| {
                let image = match image {
                    Image::None => return None,
                    Image::Url(path) => LayerImage::File(self.base.join(path)),
                    Image::Linear(linear) => LayerImage::Linear(Gradient {
                        angle: linear.angle,
                        stops: linear
                            .stops
                            .iter()
                            .map(|(color, at)| {
                                (
                                    self.resolve(color, current).unwrap_or(Rgba::TRANSPARENT),
                                    at.as_ref().map(em_px),
                                )
                            })
                            .collect(),
                        repeating: linear.repeating,
                    }),
                    Image::Other(name) => LayerImage::Other(name.clone()),
                };
                let size = pick(index, lists.sizes.len())
                    .and_then(|at| lists.sizes.get(at))
                    .map_or(Size::Explicit(None, None), |size| match size {
                        Size::Explicit(w, h) => {
                            Size::Explicit(w.as_ref().map(em_px), h.as_ref().map(em_px))
                        }
                        other => *other,
                    });
                let position = pick(index, lists.positions.len())
                    .and_then(|at| lists.positions.get(at))
                    .map_or((Length::percent(0.0), Length::percent(0.0)), |(x, y)| {
                        (em_px(x), em_px(y))
                    });
                let repeat = pick(index, lists.repeats.len())
                    .and_then(|at| lists.repeats.get(at))
                    .copied()
                    .unwrap_or((Repeat::Repeat, Repeat::Repeat));
                let clip = pick(index, lists.clips.len())
                    .and_then(|at| lists.clips.get(at))
                    .copied()
                    .unwrap_or(Area::Border);
                let origin = pick(index, lists.origins.len())
                    .and_then(|at| lists.origins.get(at))
                    .copied()
                    .unwrap_or(Area::Padding);
                Some(Layer {
                    image,
                    size,
                    position,
                    repeat,
                    clip,
                    origin,
                })
            })
            .collect()
    }

    /// The clip of the background colour: the last layer's, as CSS says.
    #[must_use]
    pub fn color_clip(style: &Style) -> Area {
        style.layers.last().map_or(Area::Border, |layer| layer.clip)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::Stylesheet;
    use super::super::selector::Node;
    use super::super::value::{Length, Rgba};
    use super::LayerImage;
    use crate::diag::Diagnostics;

    #[derive(Debug)]
    struct N {
        element: &'static str,
        name: Option<&'static str>,
        classes: Vec<&'static str>,
        hover: bool,
        parent: Option<Box<N>>,
    }

    impl Node for &N {
        fn element(&self) -> &str {
            self.element
        }
        fn name(&self) -> Option<&str> {
            self.name
        }
        fn has_class(&self, class: &str) -> bool {
            self.classes.contains(&class)
        }
        fn in_state(&self, state: &str) -> bool {
            state == "hover" && self.hover
        }
        fn parent(&self) -> Option<Self> {
            self.parent.as_deref()
        }
        fn position(&self) -> (usize, usize) {
            (0, 1)
        }
        fn previous(&self) -> Option<Self> {
            None
        }
    }

    /// An excerpt of the user's style.css: the palette, `*`, the window,
    /// and the desktop chips in their order.
    const EXCERPT: &str = r#"
@define-color ba-paper  #0e1b2c;
@define-color ba-ink    #dbe8f7;
@define-color ba-idle   #7f9bb8;
@define-color ba-deep   #16304d;
* { font-family: "Ubuntu", "DejaVu Sans", sans-serif; font-size: 15px; min-height: 0; }
window#waybar { background: alpha(@ba-paper, 0.55); color: @ba-ink; }
window#waybar .ws {
    padding: 0 14px; margin: 4px -2px; border-radius: 0; font-weight: bold; color: @ba-idle;
    background-color: transparent;
    background-image: url("icons/cap-l-soft.svg"), url("icons/cap-r-soft.svg"), linear-gradient(#16273c, #16273c);
    background-size: 12px 100%, 12px 100%, calc(100% - 24px) 100%;
    background-position: 0% 50%, 100% 50%, 12px 50%;
    background-repeat: no-repeat;
}
window#waybar .ws.active { color: @ba-deep; }
window#waybar .ws:hover { color: @ba-ink; }
"#;

    fn tree(classes: Vec<&'static str>, hover: bool) -> (N, N, N) {
        let window = N {
            element: "window",
            name: Some("waybar"),
            classes: vec!["top"],
            hover: false,
            parent: None,
        };
        let window_again = N {
            element: "window",
            name: Some("waybar"),
            classes: vec!["top"],
            hover: false,
            parent: None,
        };
        let section = N {
            element: "box",
            name: None,
            classes: vec!["modules-right"],
            hover: false,
            parent: Some(Box::new(window_again)),
        };
        let section_again = N {
            element: "box",
            name: None,
            classes: vec!["modules-right"],
            hover: false,
            parent: Some(Box::new(N {
                element: "window",
                name: Some("waybar"),
                classes: vec!["top"],
                hover: false,
                parent: None,
            })),
        };
        let chip = N {
            element: "box",
            name: Some("custom-ws-1"),
            classes,
            hover,
            parent: Some(Box::new(section_again)),
        };
        (window, section, chip)
    }

    #[test]
    fn the_users_chip_computes_as_gtk_would() {
        let mut diag = Diagnostics::default();
        let sheet = Stylesheet::parse(
            EXCERPT,
            Path::new("/home/u/.config/waybar/style.css"),
            &|_| None,
            &mut diag,
        );
        assert!(diag.lines.is_empty(), "{:?}", diag.lines);
        let (window, section, chip) = tree(vec!["module", "ws", "active"], false);
        let window_style = sheet.compute(&&window, None);
        assert_eq!(window_style.font_size, 15.0);
        assert_eq!(window_style.color, Rgba::rgb8(0xdb, 0xe8, 0xf7));
        let paper = window_style.background_color;
        assert!((paper.a - 0.55).abs() < 1e-6);
        assert!(
            window_style.layers.is_empty(),
            "background: clears the image"
        );
        let section_style = sheet.compute(&&section, Some(&window_style));
        let chip_style = sheet.compute(&&chip, Some(&section_style));
        assert_eq!(
            chip_style.color,
            Rgba::rgb8(0x16, 0x30, 0x4d),
            ".active wins over .ws by order"
        );
        assert_eq!(chip_style.font_weight, 700);
        assert_eq!(chip_style.margin, [4.0, -2.0, 4.0, -2.0]);
        assert_eq!(chip_style.padding, [0.0, 14.0, 0.0, 14.0]);
        assert_eq!(chip_style.layers.len(), 3);
        let first = chip_style.layers.first().map(|layer| layer.image.clone());
        assert_eq!(
            first,
            Some(LayerImage::File(
                "/home/u/.config/waybar/icons/cap-l-soft.svg".into()
            ))
        );
        let band = chip_style
            .layers
            .get(2)
            .map(|layer| (layer.size, layer.position));
        assert_eq!(
            band,
            Some((
                super::Size::Explicit(
                    Some(Length {
                        px: -24.0,
                        em: 0.0,
                        percent: 100.0
                    }),
                    Some(Length::percent(100.0))
                ),
                (Length::px(12.0), Length::percent(50.0))
            ))
        );
        let (_, _, hovered) = tree(vec!["module", "ws", "active"], true);
        let hovered = sheet.compute(&&hovered, Some(&section_style));
        assert_eq!(
            hovered.color,
            Rgba::rgb8(0xdb, 0xe8, 0xf7),
            ":hover comes last and wins"
        );
    }
}
