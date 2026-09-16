//! The dwindle layout: Hyprland's `DwindleLayout.cpp`.
//!
//! A workspace's tiled windows are the leaves of a binary tree. The root's
//! box is the workspace's usable area; each split divides its box in two,
//! side by side or stacked, the first child taking `ratio` times half. A new
//! window splits the leaf of the window it opens beside, and a closed
//! window's sibling takes its parent's place, keeping whatever subtree it
//! has.
//!
//! A split's direction is chosen from its box when it is made: side by side
//! when the box is wider than its height times
//! `dwindle:split_width_multiplier`, stacked otherwise. Unless
//! `dwindle:preserve_split` is set, it is chosen again from the box whenever
//! the tree is laid out, as Hyprland's `recalcSizePosRecursive` does, which
//! uses the opposite comparison (stacked when the height times the
//! multiplier exceeds the width), so a square box ends up side by side.
//!
//! Where it departs from Hyprland: with `dwindle:force_split` at 0 Hyprland
//! puts the new window on the side of the split the cursor is over, and
//! there is no cursor here, so the new window always takes the second side;
//! `movewindow` exchanges two leaves, where Hyprland's `moveWindowTo`
//! takes the window out and splits the neighbour's leaf with it again, which
//! can change the tree's shape as well as who is where; `smart_split`,
//! `split_bias`, `permanent_direction_override` and pseudotiling are not
//! implemented.

use crate::WindowId;
use crate::geometry::Area;
use crate::settings::{ForceSplit, Settings};

/// A node of the tree.
#[derive(Debug, Clone, PartialEq)]
enum Node {
    /// A window.
    Leaf(WindowId),
    /// A box divided in two.
    Split(Box<Split>),
}

/// A divided box.
#[derive(Debug, Clone, PartialEq)]
struct Split {
    /// Whether the children are one above the other, rather than side by
    /// side: Hyprland's `splitTop`.
    stacked: bool,
    /// The first child's share, as a multiple of half the box.
    ratio: f64,
    /// The left or top child.
    first: Node,
    /// The right or bottom child.
    second: Node,
}

impl Split {
    /// The direction this split lays out with in `area`.
    fn stacked_in(&self, area: Area, settings: &Settings) -> bool {
        if settings.dwindle.preserve_split {
            self.stacked
        } else {
            area.h * settings.dwindle.split_width_multiplier > area.w
        }
    }

    /// The children's boxes within `area`.
    fn children(&self, area: Area, settings: &Settings) -> (Area, Area) {
        if self.stacked_in(area, settings) {
            let first = (area.h / 2.0 * self.ratio).clamp(0.0, area.h.max(0.0));
            (
                Area { h: first, ..area },
                Area {
                    y: area.y + first,
                    h: (area.h - first).max(0.0),
                    ..area
                },
            )
        } else {
            let first = (area.w / 2.0 * self.ratio).clamp(0.0, area.w.max(0.0));
            (
                Area { w: first, ..area },
                Area {
                    x: area.x + first,
                    w: (area.w - first).max(0.0),
                    ..area
                },
            )
        }
    }
}

impl Node {
    /// Whether `window` is a leaf of this subtree.
    fn contains(&self, window: WindowId) -> bool {
        match self {
            Self::Leaf(id) => *id == window,
            Self::Split(split) => split.first.contains(window) || split.second.contains(window),
        }
    }

    /// The leaves, left to right and top to bottom.
    fn leaves(&self, out: &mut Vec<WindowId>) {
        match self {
            Self::Leaf(id) => out.push(*id),
            Self::Split(split) => {
                split.first.leaves(out);
                split.second.leaves(out);
            }
        }
    }

    /// Each leaf with its box, when this subtree's box is `area`.
    fn slots(&self, area: Area, settings: &Settings, out: &mut Vec<(WindowId, Area)>) {
        match self {
            Self::Leaf(id) => out.push((*id, area)),
            Self::Split(split) => {
                let (first, second) = split.children(area, settings);
                split.first.slots(first, settings, out);
                split.second.slots(second, settings, out);
            }
        }
    }

    /// Record the direction each split lays out with, so that turning
    /// `preserve_split` on keeps the directions last seen.
    fn settle(&mut self, area: Area, settings: &Settings) {
        if let Self::Split(split) = self {
            split.stacked = split.stacked_in(area, settings);
            let (first, second) = split.children(area, settings);
            split.first.settle(first, settings);
            split.second.settle(second, settings);
        }
    }

    /// Split the leaf of `target` into it and `new`. Returns whether the
    /// leaf was found.
    fn split_leaf(
        &mut self,
        target: WindowId,
        new: WindowId,
        area: Area,
        settings: &Settings,
    ) -> bool {
        match self {
            Self::Leaf(id) => {
                if *id != target {
                    return false;
                }
                let old = *id;
                let side_by_side = area.w > area.h * settings.dwindle.split_width_multiplier;
                let (first, second) = match settings.dwindle.force_split {
                    ForceSplit::First => (new, old),
                    ForceSplit::Auto | ForceSplit::Second => (old, new),
                };
                *self = Self::Split(Box::new(Split {
                    stacked: !side_by_side,
                    ratio: settings.dwindle.default_split_ratio,
                    first: Self::Leaf(first),
                    second: Self::Leaf(second),
                }));
                true
            }
            Self::Split(split) => {
                let (first, second) = split.children(area, settings);
                split.first.split_leaf(target, new, first, settings)
                    || split.second.split_leaf(target, new, second, settings)
            }
        }
    }

    /// This subtree without the leaf of `window`: a split that loses a
    /// child becomes its other child.
    fn without(self, window: WindowId) -> Option<Self> {
        match self {
            Self::Leaf(id) if id == window => None,
            Self::Leaf(_) => Some(self),
            Self::Split(split) => {
                let Split {
                    stacked,
                    ratio,
                    first,
                    second,
                } = *split;
                match (first.without(window), second.without(window)) {
                    (Some(first), Some(second)) => Some(Self::Split(Box::new(Split {
                        stacked,
                        ratio,
                        first,
                        second,
                    }))),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    /// Exchange the leaves of `a` and `b`; if only one is here, it is
    /// renamed to the other.
    fn swap(&mut self, a: WindowId, b: WindowId) {
        match self {
            Self::Leaf(id) => {
                if *id == a {
                    *id = b;
                } else if *id == b {
                    *id = a;
                }
            }
            Self::Split(split) => {
                split.first.swap(a, b);
                split.second.swap(a, b);
            }
        }
    }
}

/// One workspace's dwindle tree.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Dwindle {
    root: Option<Node>,
}

impl Dwindle {
    /// Add `new` beside `target`, or beside the last window if `target` is
    /// not in the tree, when the workspace's usable area is `area`.
    pub(crate) fn insert(
        &mut self,
        new: WindowId,
        target: Option<WindowId>,
        area: Area,
        settings: &Settings,
    ) {
        let Some(mut root) = self.root.take() else {
            self.root = Some(Node::Leaf(new));
            return;
        };
        let target = target
            .filter(|target| root.contains(*target))
            .or_else(|| leaves(&root).last().copied());
        if let Some(target) = target {
            let _found = root.split_leaf(target, new, area, settings);
        }
        self.root = Some(root);
    }

    /// Remove `window`, promoting its sibling.
    pub(crate) fn remove(&mut self, window: WindowId) {
        self.root = self.root.take().and_then(|root| root.without(window));
    }

    /// Exchange two windows' places, or rename one to the other.
    pub(crate) fn swap(&mut self, a: WindowId, b: WindowId) {
        if let Some(root) = &mut self.root {
            root.swap(a, b);
        }
    }

    /// Whether `window` is in the tree.
    pub(crate) fn contains(&self, window: WindowId) -> bool {
        self.root.as_ref().is_some_and(|root| root.contains(window))
    }

    /// The windows, in order.
    pub(crate) fn windows(&self) -> Vec<WindowId> {
        self.root.as_ref().map(leaves).unwrap_or_default()
    }

    /// Each window's box, when the workspace's usable area is `area`.
    pub(crate) fn slots(&self, area: Area, settings: &Settings) -> Vec<(WindowId, Area)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.slots(area, settings, &mut out);
        }
        out
    }

    /// Record the directions the splits lay out with in `area`.
    pub(crate) fn settle(&mut self, area: Area, settings: &Settings) {
        if let Some(root) = &mut self.root {
            root.settle(area, settings);
        }
    }
}

/// The leaves of `root`, in order.
fn leaves(root: &Node) -> Vec<WindowId> {
    let mut out = Vec::new();
    root.leaves(&mut out);
    out
}
