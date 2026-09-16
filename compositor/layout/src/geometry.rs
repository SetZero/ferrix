//! The arithmetic the layouts share: fractional boxes, rounding them to
//! pixels, and Hyprland's gaps and borders.

use compositor_config::Gaps;

use crate::Rect;
use crate::settings::Settings;

/// A box with fractional edges, as Hyprland's `CBox` is while a layout
/// divides a workspace, so that a split of an odd width does not lose a
/// pixel at every level of the tree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Area {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

impl Area {
    /// The box covering `rect`.
    pub(crate) fn of(rect: Rect) -> Self {
        Self {
            x: rect.x as f64,
            y: rect.y as f64,
            w: rect.width as f64,
            h: rect.height as f64,
        }
    }

    /// The box in whole pixels. Edges are rounded rather than sizes, as
    /// Hyprland's `CBox::round` does, so two boxes that share an edge still
    /// share it after rounding.
    pub(crate) fn round(self) -> Rect {
        let left = self.x.round();
        let top = self.y.round();
        let right = (self.x + self.w).round();
        let bottom = (self.y + self.h).round();
        Rect {
            x: left as i64,
            y: top as i64,
            width: (right - left).max(0.0) as i64,
            height: (bottom - top).max(0.0) as i64,
        }
    }
}

/// The centre of `rect`.
pub(crate) fn center(rect: Rect) -> (f64, f64) {
    (
        rect.x as f64 + rect.width as f64 / 2.0,
        rect.y as f64 + rect.height as f64 / 2.0,
    )
}

/// Whether two edges touch: Hyprland's `STICKS`, which allows a pixel of
/// rounding either way.
pub(crate) const fn sticks(a: i64, b: i64) -> bool {
    a.abs_diff(b) < 2
}

/// How long the spans `a0..a1` and `b0..b1` overlap, zero if they do not.
pub(crate) fn overlap(a0: i64, a1: i64, b0: i64, b1: i64) -> i64 {
    a1.min(b1).saturating_sub(a0.max(b0)).max(0)
}

/// `rect` with `gaps` taken off each side, never smaller than empty.
pub(crate) fn inset(rect: Rect, gaps: Gaps) -> Rect {
    Rect {
        x: rect.x.saturating_add(gaps.left),
        y: rect.y.saturating_add(gaps.top),
        width: rect
            .width
            .saturating_sub(gaps.left)
            .saturating_sub(gaps.right)
            .max(0),
        height: rect
            .height
            .saturating_sub(gaps.top)
            .saturating_sub(gaps.bottom)
            .max(0),
    }
}

/// The client area of a tiled window whose slot is `slot` on a workspace
/// whose usable area is `area`: Hyprland's `applyNodeDataToWindow`. An edge
/// of the slot on the edge of the area gets `gaps_out`, any other edge
/// `gaps_in`, and every edge the border inside that.
pub(crate) fn client(slot: Rect, area: Rect, settings: &Settings) -> Rect {
    let side = |touches: bool, outer: i64, inner: i64| {
        (if touches { outer } else { inner }).saturating_add(settings.border_size)
    };
    let (outer, inner) = (settings.gaps_out, settings.gaps_in);
    inset(
        slot,
        Gaps {
            top: side(sticks(slot.y, area.y), outer.top, inner.top),
            right: side(sticks(slot.right(), area.right()), outer.right, inner.right),
            bottom: side(
                sticks(slot.bottom(), area.bottom()),
                outer.bottom,
                inner.bottom,
            ),
            left: side(sticks(slot.x, area.x), outer.left, inner.left),
        },
    )
}
