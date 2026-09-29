//! GTK3's box model and box layout, for the bar's tree.
//!
//! Every widget with a CSS box (a window, a box, a label, an image) is a
//! `GtkCssGadget`: its content is at least `min-width` by `min-height`, and
//! padding, border and margin are added around it, in that order outwards
//! (`gtk_css_gadget_get_preferred_size`). Margins may be negative, and the
//! user's desktop chips' `margin: 4px -2px` is: a chip's allocation is then
//! 4 pixels narrower than its border box, which spills 2 pixels over each
//! neighbour -- which is how the chips come to be 2 pixels apart with the
//! bar's 6-pixel spacing between them.
//!
//! A `GtkEventBox` has no box of its own and gives its child its whole
//! allocation.
//!
//! A horizontal `GtkBox` gives each visible child its natural width, with
//! `spacing` between them, and every child the box's whole height (the
//! default `valign` is fill). When the children want more than there is,
//! each is shrunk towards its minimum, the ones closest to it first
//! (`gtk_distribute_natural_allocation`). The bar's own box packs the left
//! section at the start, the right at the end, and the centre as its centre
//! widget: centred on the whole bar, and pushed aside only where it would
//! overlap one of the others (`gtk_box_size_allocate_with_center`).
//!
//! A `GtkLabel`'s natural width is its text's logical width; an ellipsizing
//! one (`max-length`) is at least the ellipsis and at most `max-width-chars`
//! times the font's approximate character width, and a wrapping one (a
//! tooltip) at most that. Its text sits at `floor(x + (width - text) / 2)`,
//! `floor(y + (height - text) / 2)` in its content box: `xalign` and
//! `yalign` 0.5 (`gtk_label_get_layout_location`).

use crate::css::style::Style;
use crate::tree::{Kind, Tree};

/// A rectangle in the surface's logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// A rectangle.
    #[must_use]
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Shrunk by `sides` (top, right, bottom, left); a negative side grows it.
    #[must_use]
    pub fn inset(&self, sides: [f32; 4]) -> Self {
        let [top, right, bottom, left] = sides;
        Self {
            x: self.x + left,
            y: self.y + top,
            width: (self.width - left - right).max(0.0),
            height: (self.height - top - bottom).max(0.0),
        }
    }

    /// Whether a point is inside.
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// A label's text, measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextSize {
    /// Its logical width, whole.
    pub width: f32,
    /// Its logical height.
    pub height: f32,
    /// The narrowest it can be: the ellipsis alone, or the widest word when
    /// it wraps.
    pub min_width: f32,
    /// The font's approximate character width: the larger of Pango's
    /// approximate char and digit widths, which `max-width-chars` counts in.
    pub char_width: f32,
}

/// What lays text out, so the layout does not depend on the fonts.
pub trait Measure {
    /// `markup` in a label styled `style`, as wide as it is, and, when
    /// `wrap_at` is given, wrapped at that width.
    fn text(&mut self, markup: &str, style: &Style, wrap_at: Option<f32>) -> TextSize;
}

/// Where one node is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Placed {
    /// What its parent gave it: its margin box.
    pub allocation: Rect,
    /// Its border box: the allocation less the margins.
    pub border: Rect,
    /// Its content box: less border and padding too.
    pub content: Rect,
    /// A label's text: where the text's logical rectangle goes, and its
    /// size (narrower than natural when the label was squeezed, which is
    /// when it ellipsizes).
    pub text: Rect,
}

/// The whole tree placed, by node index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placement {
    /// One per node; hidden nodes are all zero.
    pub nodes: Vec<Placed>,
    /// Each label's text size as measured.
    pub texts: Vec<Option<TextSize>>,
    /// The window's size.
    pub size: (f32, f32),
}

/// A node's minimum and natural widths and its height, margins included.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Want {
    min: f32,
    natural: f32,
    height: f32,
}

fn extra_width(style: &Style) -> f32 {
    let [_, mr, _, ml] = style.margin;
    let [_, br, _, bl] = style.border_width;
    let [_, pr, _, pl] = style.padding;
    ml + mr + bl + br + pl + pr
}

fn extra_height(style: &Style) -> f32 {
    let [mt, _, mb, _] = style.margin;
    let [bt, _, bb, _] = style.border_width;
    let [pt, _, pb, _] = style.padding;
    mt + mb + bt + bb + pt + pb
}

struct Layouter<'a> {
    tree: &'a Tree,
    measure: &'a mut dyn Measure,
    wants: Vec<Want>,
    texts: Vec<Option<TextSize>>,
    placed: Vec<Placed>,
}

impl Layouter<'_> {
    /// Compute every node's wants, children first.
    fn want(&mut self, index: usize) -> Want {
        let Some(node) = self.tree.get(index) else {
            return Want::default();
        };
        if !node.visible {
            return Want::default();
        }
        let style = &node.style;
        let (min, natural, height) = match &node.kind {
            Kind::Label {
                markup,
                max_chars,
                ellipsize,
                wrap,
            } => self.label(index, markup, style, *max_chars, *ellipsize || *wrap, *wrap),
            Kind::Image => (0.0, 0.0, 0.0),
            Kind::EventBox | Kind::Window => {
                let mut total = Want::default();
                for &child in &node.children {
                    let want = self.want(child);
                    total.min = total.min.max(want.min);
                    total.natural = total.natural.max(want.natural);
                    total.height = total.height.max(want.height);
                }
                (total.min, total.natural, total.height)
            }
            Kind::Box {
                spacing, center, ..
            } => {
                let mut min = 0.0f32;
                let mut natural = 0.0f32;
                let mut height = 0.0f32;
                let mut shown = 0usize;
                let mut before = 0.0f32;
                let mut after = 0.0f32;
                let mut middle = 0.0f32;
                for (at, &child) in node.children.iter().enumerate() {
                    let want = self.want(child);
                    if !self.tree.get(child).is_some_and(|c| c.visible) {
                        continue;
                    }
                    shown += 1;
                    height = height.max(want.height);
                    match center {
                        Some(c) if *c == at => middle = want.natural,
                        Some(c) if at > *c => after += want.natural,
                        Some(_) => before += want.natural,
                        None => {}
                    }
                    min += want.min;
                    natural += want.natural;
                }
                #[expect(clippy::cast_precision_loss, reason = "a count of children")]
                let gaps = spacing * shown.saturating_sub(1) as f32;
                if center.is_some() {
                    // A centred child needs the same room on both sides.
                    natural = natural.max(2.0 * before.max(after) + middle);
                }
                (min + gaps, natural + gaps, height)
            }
        };
        let content_min = min.max(style.min_width);
        let content_natural = natural.max(style.min_width);
        let content_height = height.max(style.min_height);
        let want = Want {
            min: (content_min + extra_width(style)).max(0.0),
            natural: (content_natural + extra_width(style)).max(0.0),
            height: (content_height + extra_height(style)).max(0.0),
        };
        if let Some(slot) = self.wants.get_mut(index) {
            *slot = want;
        }
        want
    }

    /// A label's minimum and natural widths and its height, as
    /// `gtk_label_get_preferred_layout_size` makes them.
    fn label(
        &mut self,
        index: usize,
        markup: &str,
        style: &Style,
        max_chars: Option<u32>,
        shrinks: bool,
        wrap: bool,
    ) -> (f32, f32, f32) {
        let size = self.measure.text(markup, style, None);
        let mut kept = size;
        let mut min = size.width;
        let mut natural = size.width;
        let mut height = size.height;
        if shrinks {
            min = size.min_width;
            if let Some(chars) = max_chars {
                #[expect(clippy::cast_precision_loss, reason = "a count of characters")]
                let cap = size.char_width * chars as f32;
                natural = natural.min(cap.ceil()).max(min);
            }
            if wrap && natural < size.width {
                kept = self.measure.text(markup, style, Some(natural));
                height = kept.height;
                natural = kept.width.max(min);
                min = natural;
            }
        }
        if let Some(slot) = self.texts.get_mut(index) {
            *slot = Some(kept);
        }
        (min, natural, height)
    }

    fn want_of(&self, index: usize) -> Want {
        self.wants.get(index).copied().unwrap_or_default()
    }

    /// Place `index` in `allocation`, and its children within it.
    fn place(&mut self, index: usize, allocation: Rect) {
        let Some(node) = self.tree.get(index) else {
            return;
        };
        if !node.visible {
            return;
        }
        let style = &node.style;
        let border = allocation_less(allocation, style.margin, true);
        let content = border.inset(style.border_width).inset(style.padding);
        let mut placed = Placed {
            allocation,
            border,
            content,
            text: Rect::default(),
        };
        match &node.kind {
            Kind::Label { .. } => {
                let size = self.texts.get(index).copied().flatten().unwrap_or_default();
                let width = size.width.min(content.width);
                let height = size.height;
                placed.text = Rect::new(
                    (content.x + ((content.width - width) * 0.5).max(0.0)).floor(),
                    (content.y + ((content.height - height) * 0.5).max(0.0)).floor(),
                    width,
                    height,
                );
            }
            Kind::Image => {}
            Kind::EventBox | Kind::Window => {
                let children = node.children.clone();
                for child in children {
                    self.place(child, content);
                }
            }
            Kind::Box {
                spacing,
                center,
                end,
            } => {
                let children = node.children.clone();
                let (spacing, center, end) = (*spacing, *center, *end);
                self.place_box(&children, content, spacing, center, end);
            }
        }
        if let Some(slot) = self.placed.get_mut(index) {
            *slot = placed;
        }
    }

    fn place_box(
        &mut self,
        children: &[usize],
        content: Rect,
        spacing: f32,
        center: Option<usize>,
        end: usize,
    ) {
        let visible: Vec<(usize, usize)> = children
            .iter()
            .copied()
            .enumerate()
            .filter(|&(_, c)| self.tree.get(c).is_some_and(|n| n.visible))
            .collect();
        #[expect(clippy::cast_precision_loss, reason = "a count of children")]
        let gaps = spacing * visible.len().saturating_sub(1) as f32;
        let wants: Vec<Want> = visible.iter().map(|&(_, c)| self.want_of(c)).collect();
        let widths = distribute(&wants, (content.width - gaps).max(0.0));
        let height = content.height;
        let start_count = children.len().saturating_sub(end);
        // Packed at the start, in order; at the end, from the right edge.
        let mut x = content.x;
        let mut right = content.x + content.width;
        let mut left_edge = content.x;
        let mut right_edge = content.x + content.width;
        let mut centred = None;
        for (slot, &(at, child)) in visible.iter().enumerate() {
            let width = widths.get(slot).copied().unwrap_or(0.0);
            if center == Some(at) {
                centred = Some((child, width));
                continue;
            }
            if at < start_count {
                self.place(child, Rect::new(x, content.y, width, height));
                x += width + spacing;
                left_edge = x;
            }
        }
        for (slot, &(at, child)) in visible.iter().enumerate().rev() {
            if center == Some(at) || at < start_count {
                continue;
            }
            let width = widths.get(slot).copied().unwrap_or(0.0);
            right -= width;
            self.place(child, Rect::new(right, content.y, width, height));
            right_edge = right - spacing;
            right -= spacing;
        }
        if let Some((child, width)) = centred {
            let mut cx = content.x + ((content.width - width) / 2.0).floor();
            if cx < left_edge {
                cx = left_edge;
            }
            if cx + width > right_edge {
                cx = (right_edge - width).max(left_edge);
            }
            self.place(child, Rect::new(cx, content.y, width, height));
        }
    }
}

/// The allocation less the margins: the border box. Negative margins grow
/// it, which GTK allows.
fn allocation_less(allocation: Rect, margin: [f32; 4], _grow: bool) -> Rect {
    let [top, right, bottom, left] = margin;
    Rect {
        x: allocation.x + left,
        y: allocation.y + top,
        width: (allocation.width - left - right).max(0.0),
        height: (allocation.height - top - bottom).max(0.0),
    }
}

/// Widths for children wanting `wants` in `room`: natural if it fits,
/// otherwise each shrunk towards its minimum, the ones with the least to
/// give first, as `gtk_distribute_natural_allocation` does it in reverse.
fn distribute(wants: &[Want], room: f32) -> Vec<f32> {
    let natural: f32 = wants.iter().map(|w| w.natural).sum();
    if natural <= room {
        return wants.iter().map(|w| w.natural).collect();
    }
    let minimum: f32 = wants.iter().map(|w| w.min).sum();
    let mut widths: Vec<f32> = wants.iter().map(|w| w.min).collect();
    let mut extra = (room - minimum).max(0.0);
    // Give out what is left above the minimums, the child that wants the
    // least more first, each an equal share of what remains.
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by(|&a, &b| {
        let gap = |i: usize| wants.get(i).map_or(0.0, |w| w.natural - w.min);
        gap(a).total_cmp(&gap(b))
    });
    let count = order.len();
    for (rank, index) in order.into_iter().enumerate() {
        #[expect(clippy::cast_precision_loss, reason = "a count of children")]
        let share = extra / (count - rank) as f32;
        let gap = wants.get(index).map_or(0.0, |w| w.natural - w.min);
        let given = gap.min(share);
        if let Some(width) = widths.get_mut(index) {
            *width += given;
        }
        extra -= given;
    }
    widths
}

/// Lay out `tree` in a window `width` wide and at least `height` tall (0
/// for as tall as its content).
pub fn layout(tree: &Tree, measure: &mut dyn Measure, width: f32, height: f32) -> Placement {
    let count = tree.nodes.len();
    let mut layouter = Layouter {
        tree,
        measure,
        wants: vec![Want::default(); count],
        texts: vec![None; count],
        placed: vec![Placed::default(); count],
    };
    let root = layouter.want(0);
    let height = height.max(root.height);
    let width = if width > 0.0 { width } else { root.natural };
    layouter.place(0, Rect::new(0.0, 0.0, width, height));
    Placement {
        nodes: layouter.placed,
        texts: layouter.texts,
        size: (width, height),
    }
}

/// The natural size of a tree, as a tooltip window is sized.
pub fn natural_size(tree: &Tree, measure: &mut dyn Measure) -> (f32, f32) {
    let count = tree.nodes.len();
    let mut layouter = Layouter {
        tree,
        measure,
        wants: vec![Want::default(); count],
        texts: vec![None; count],
        placed: vec![Placed::default(); count],
    };
    let root = layouter.want(0);
    (root.natural, root.height)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Measure, Rect, TextSize, layout};
    use crate::css::Stylesheet;
    use crate::css::style::Style;
    use crate::diag::Diagnostics;
    use crate::view::{BarView, ModuleView, Shape, build};

    /// Every character 8 pixels wide, lines 18 tall.
    struct Mono;

    impl Measure for Mono {
        fn text(&mut self, markup: &str, _style: &Style, wrap_at: Option<f32>) -> TextSize {
            #[expect(clippy::cast_precision_loss, reason = "a test's character count")]
            let width = markup.chars().count() as f32 * 8.0;
            let width = wrap_at.map_or(width, |w| width.min(w));
            TextSize {
                width,
                height: 18.0,
                min_width: 8.0,
                char_width: 8.0,
            }
        }
    }

    const STYLE: &str = r"
* { font-size: 15px; min-height: 0; }
#cpu, #memory { padding: 0 16px; margin: 4px 1px; }
.ws { padding: 0 14px; margin: 4px -2px; }
#custom-launcher { min-width: 26px; padding: 0 8px; margin: 4px 2px 4px 6px; }
";

    fn sheet() -> Stylesheet {
        let mut diag = Diagnostics::default();
        Stylesheet::parse(STYLE, Path::new("/s/style.css"), &|_| None, &mut diag)
    }

    fn label(name: &str, text: &str) -> ModuleView {
        let mut view = ModuleView::new(name, Shape::Label);
        view.markup = text.to_owned();
        view
    }

    fn chip(name: &str, text: &str) -> ModuleView {
        let mut view = ModuleView::new(name, Shape::IconLabel);
        view.classes.push("ws".to_owned());
        view.markup = text.to_owned();
        view
    }

    #[test]
    fn a_bar_is_laid_out_as_gtk_does() {
        let mut launcher = ModuleView::new("custom-launcher", Shape::IconLabel);
        launcher.markup = " ".to_owned();
        let modules = vec![
            launcher,
            label("cpu", "cpu 3%"),
            chip("custom-ws-1", "1"),
            chip("custom-ws-2", "2"),
            label("memory", "ram 40%"),
        ];
        let bar = BarView {
            window_classes: vec!["top".into()],
            spacing: 6.0,
            center: true,
            fixed_center: true,
            sections: [vec![0], vec![1], vec![2, 3, 4]],
        };
        let (mut tree, built) = build(&bar, &modules);
        tree.style(&sheet());
        let placed = layout(&tree, &mut Mono, 1000.0, 40.0);
        assert_eq!(placed.size, (1000.0, 40.0));
        let border = |module: usize| {
            let (event_box, label) = built
                .modules
                .get(module)
                .copied()
                .flatten()
                .unwrap_or((0, 0));
            let named = if tree.get(label).and_then(|n| n.name.as_ref()).is_some() {
                label
            } else {
                tree.get(label).and_then(|n| n.parent).unwrap_or(event_box)
            };
            placed
                .nodes
                .get(named)
                .map(|p| p.border)
                .unwrap_or_default()
        };
        // The launcher: 26 px content (min-width beats a space's 8), 8 px
        // padding each side, margins 6 left and 2 right, 4 above and below.
        assert_eq!(border(0), Rect::new(6.0, 4.0, 42.0, 32.0));
        // The centred cpu: 48 px of text and 32 of padding, margin 1 each
        // side, so an allocation of 82 centred on 1000.
        assert_eq!(border(1), Rect::new(460.0, 4.0, 80.0, 32.0));
        // The right section ends at the right edge: memory's allocation is
        // 56+32+2 = 90, so its border box is 911..999.
        assert_eq!(border(4), Rect::new(911.0, 4.0, 88.0, 32.0));
        // Each chip's allocation is 8+28-4 = 32 wide and the border box 36,
        // spilling 2 px each way; 6 px of spacing leaves 2 px between.
        let two = border(3);
        let one = border(2);
        assert_eq!(two.width, 36.0);
        assert_eq!(two.x - (one.x + one.width), 2.0);
        assert_eq!(border(4).x - (two.x + two.width), 5.0);
    }

    #[test]
    fn a_hidden_module_takes_no_room_or_spacing() {
        let mut hidden = chip("custom-ws-2", "2");
        hidden.visible = false;
        let modules = vec![chip("custom-ws-1", "1"), hidden, label("cpu", "x")];
        let bar = BarView {
            window_classes: vec![],
            spacing: 6.0,
            center: true,
            fixed_center: true,
            sections: [vec![0, 1, 2], vec![], vec![]],
        };
        let (mut tree, built) = build(&bar, &modules);
        tree.style(&sheet());
        let placed = layout(&tree, &mut Mono, 500.0, 40.0);
        let allocation = |module: usize| {
            let (event_box, _) = built
                .modules
                .get(module)
                .copied()
                .flatten()
                .unwrap_or((0, 0));
            placed
                .nodes
                .get(event_box)
                .map(|p| p.allocation)
                .unwrap_or_default()
        };
        assert_eq!(allocation(0).x, 0.0);
        assert_eq!(allocation(2).x, allocation(0).width + 6.0);
        assert_eq!(allocation(1), Rect::default());
    }

    #[test]
    fn a_squeezed_bar_shrinks_ellipsizing_labels_first() {
        let mut title = ModuleView::new("window", Shape::IconLabel);
        title.markup = "a very long window title indeed".to_owned();
        title.ellipsize = true;
        let modules = vec![title, label("cpu", "cpu 1%")];
        let bar = BarView {
            window_classes: vec![],
            spacing: 0.0,
            center: false,
            fixed_center: true,
            sections: [vec![0, 1], vec![], vec![]],
        };
        let (mut tree, built) = build(&bar, &modules);
        tree.style(&sheet());
        let placed = layout(&tree, &mut Mono, 200.0, 40.0);
        let (_, cpu_label) = built.modules.get(1).copied().flatten().unwrap_or((0, 0));
        let cpu = placed
            .nodes
            .get(cpu_label)
            .map(|p| p.allocation)
            .unwrap_or_default();
        assert_eq!(
            cpu.width,
            48.0 + 32.0 + 2.0,
            "the cpu label keeps its natural width"
        );
        assert_eq!(cpu.x + cpu.width, 200.0);
    }
}
