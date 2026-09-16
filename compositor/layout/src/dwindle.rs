//! The dwindle layout: Hyprland's `DwindleLayout.cpp`, which Hyprland 0.54
//! moved to `CDwindleAlgorithm` in `layout/algorithm/tiled/dwindle`.
//!
//! A workspace's tiled windows are the leaves of a binary tree. The root's
//! box is the workspace's work area, the monitor less its reserved strips
//! and `general:gaps_out`; each split divides its box in two, side by side
//! or stacked, the first child taking `ratio` times half. A new window
//! splits the leaf of the window it opens beside, and a closed window's
//! sibling takes its parent's place, keeping whatever subtree it has.
//!
//! A split's direction is chosen from its box when it is made: side by side
//! when the box is wider than its height times
//! `dwindle:split_width_multiplier`, stacked otherwise. Unless
//! `dwindle:preserve_split` is set, it is chosen again from the box whenever
//! the tree is laid out, as Hyprland's `recalcSizePosRecursive` does, which
//! uses the opposite comparison (stacked when the height times the
//! multiplier exceeds the width), so a square box ends up side by side.
//!
//! `movewindow` does what Hyprland's `moveTargetInDirection` does: it takes
//! the window out and puts it back beside the window whose box is nearest a
//! point one pixel past its edge in the direction of the move, on the half
//! of that box the point is in, which may be on another monitor's
//! workspace. When the window's sibling is a single window in that
//! direction, it lands on that sibling's far side. Either way the new split
//! gets `dwindle:default_split_ratio`.
//!
//! Where it departs from Hyprland: a new window's split is beside the
//! workspace's most recently focused tiled window, where Hyprland's
//! `use_active_for_splits` takes the focused window only if it is tiled and
//! otherwise the one under the cursor; with `dwindle:force_split` at 0
//! Hyprland puts the new window on the half of the split the cursor is
//! over, and there is no cursor here, so the new window always takes the
//! second half; `smart_split`, `split_bias`,
//! `permanent_direction_override` and pseudotiling are not implemented.

use crate::WindowId;
use crate::dispatch::Direction;
use crate::geometry::Area;
use crate::settings::{ForceSplit, Settings};

/// Which side of a new split a new window takes.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Place {
    /// As `dwindle:force_split` says.
    Configured,
    /// The half of the box this point is in.
    Point(f64, f64),
    /// First for a move left or up, second for right or down, and the split
    /// along that direction's axis.
    Toward(Direction),
}

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

    /// Split the leaf of `target` into it and `new`, `new` first when
    /// `place` says so. Returns whether the leaf was found.
    fn split_leaf(
        &mut self,
        target: WindowId,
        new: WindowId,
        area: Area,
        settings: &Settings,
        place: Place,
    ) -> bool {
        match self {
            Self::Leaf(id) => {
                if *id != target {
                    return false;
                }
                let old = *id;
                let side_by_side = area.w > area.h * settings.dwindle.split_width_multiplier;
                let new_first = match place {
                    Place::Configured => settings.dwindle.force_split == ForceSplit::First,
                    // Hyprland's `force_split` 0 branch, which a moved window
                    // always takes: the half of the box the point is in.
                    Place::Point(x, y) => {
                        if side_by_side {
                            x < area.x + area.w / 2.0
                        } else {
                            y < area.y + area.h / 2.0
                        }
                    }
                    Place::Toward(direction) => {
                        matches!(direction, Direction::Left | Direction::Up)
                    }
                };
                let stacked = match place {
                    Place::Toward(direction) => {
                        matches!(direction, Direction::Up | Direction::Down)
                    }
                    Place::Configured | Place::Point(..) => !side_by_side,
                };
                let (first, second) = if new_first { (new, old) } else { (old, new) };
                *self = Self::Split(Box::new(Split {
                    stacked,
                    ratio: settings.dwindle.default_split_ratio,
                    first: Self::Leaf(first),
                    second: Self::Leaf(second),
                }));
                true
            }
            Self::Split(split) => {
                let (first, second) = split.children(area, settings);
                split.first.split_leaf(target, new, first, settings, place)
                    || split
                        .second
                        .split_leaf(target, new, second, settings, place)
            }
        }
    }

    /// Whether `window`'s sibling is a single window lying in `direction`
    /// from it, across a split in that direction's axis.
    fn faces_lone_sibling(&self, window: WindowId, direction: Direction) -> bool {
        let Self::Split(split) = self else {
            return false;
        };
        let leaf = |node: &Self| matches!(node, Self::Leaf(id) if *id == window);
        let lone = |node: &Self| matches!(node, Self::Leaf(_));
        let across = match direction {
            Direction::Up | Direction::Down => split.stacked,
            Direction::Left | Direction::Right => !split.stacked,
        };
        let faces = match direction {
            Direction::Up | Direction::Left => leaf(&split.second) && lone(&split.first),
            Direction::Down | Direction::Right => leaf(&split.first) && lone(&split.second),
        };
        (across && faces)
            || split.first.faces_lone_sibling(window, direction)
            || split.second.faces_lone_sibling(window, direction)
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
}

/// One workspace's dwindle tree.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Dwindle {
    root: Option<Node>,
}

impl Dwindle {
    /// Add `new` beside `target`, or beside the last window if `target` is
    /// not in the tree, when the workspace's work area is `area`.
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
            let _found = root.split_leaf(target, new, area, settings, Place::Configured);
        }
        self.root = Some(root);
    }

    /// Add `new` beside the window whose box is nearest the point `(x, y)`,
    /// on the half of that box the point is in, or, with `toward`, on the
    /// side of it a move in that direction heads for: how Hyprland puts back
    /// a window `movewindow` took out (`CDwindleAlgorithm::movedTarget`).
    pub(crate) fn insert_at(
        &mut self,
        new: WindowId,
        (x, y): (f64, f64),
        toward: Option<Direction>,
        area: Area,
        settings: &Settings,
    ) {
        let Some(mut root) = self.root.take() else {
            self.root = Some(Node::Leaf(new));
            return;
        };
        let mut slots = Vec::new();
        root.slots(area, settings, &mut slots);
        let mut nearest: Option<(f64, WindowId)> = None;
        for (window, slot) in slots {
            let dx = (slot.x - x).max(x - (slot.x + slot.w)).max(0.0);
            let dy = (slot.y - y).max(y - (slot.y + slot.h)).max(0.0);
            let distance = dx * dx + dy * dy;
            if nearest.is_none_or(|(best, _)| distance < best) {
                nearest = Some((distance, window));
            }
        }
        let place = toward.map_or(Place::Point(x, y), Place::Toward);
        if let Some((_, target)) = nearest {
            let _found = root.split_leaf(target, new, area, settings, place);
        }
        self.root = Some(root);
    }

    /// Whether `window`'s sibling is a single window in `direction`, the
    /// case in which Hyprland's `moveTargetInDirection` overrides where the
    /// moved window lands so that it ends up on the far side of it.
    pub(crate) fn faces_lone_sibling(&self, window: WindowId, direction: Direction) -> bool {
        self.root
            .as_ref()
            .is_some_and(|root| root.faces_lone_sibling(window, direction))
    }

    /// Remove `window`, promoting its sibling.
    pub(crate) fn remove(&mut self, window: WindowId) {
        self.root = self.root.take().and_then(|root| root.without(window));
    }

    /// Whether `window` is in the tree.
    pub(crate) fn contains(&self, window: WindowId) -> bool {
        self.root.as_ref().is_some_and(|root| root.contains(window))
    }

    /// The windows, in order.
    pub(crate) fn windows(&self) -> Vec<WindowId> {
        self.root.as_ref().map(leaves).unwrap_or_default()
    }

    /// Each window's box, when the workspace's work area is `area`.
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
