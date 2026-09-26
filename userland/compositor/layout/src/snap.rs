//! `general:snap:*`: a dragged floating window that comes near another
//! window's edge, or the screen's, lands flush against it.
//!
//! Hyprland's `CLayoutManager::performSnap`, which its drag controller calls
//! on the rectangle a drag has worked out but before that rectangle is the
//! window's. So this takes a rectangle and gives one back, and is the last
//! thing `State::move_floating` does for a drag.
//!
//! Two kinds of snap, and the difference is the whole of why `moving` is an
//! argument. A window being *moved* keeps its size and slides to the edge
//! (Hyprland's `snapMove`); a window being *resized* moves the one edge that
//! was grabbed and changes size (`snapResize`). A move pulls on every edge
//! -- Hyprland passes `-1` for the corner, which is every bit set -- and a
//! resize only on the edges the drag grabbed.
//!
//! Where it departs from Hyprland: `general:snap:border_overlap` does
//! nothing here. It decides whether a window's *extents* -- the shadow and
//! the border drawn outside its box -- may hang over the screen's edge, and
//! this layout's rectangle is the window box with nothing outside it, so
//! Hyprland's `EXTENTS` are zero and the option has no two cases to choose
//! between. It is read and ignored rather than refused, because a
//! configuration that sets it is not wrong.

use crate::settings::Settings;
use crate::{Corner, Gaps, Rect};

/// Where one axis of the dragged window's two edges are.
#[derive(Clone, Copy, Debug)]
struct Span {
    /// The left or top edge.
    start: i64,
    /// The right or bottom edge.
    end: i64,
}

impl Span {
    /// Put the near edge at `to`: Hyprland's `snapMove` and `snapResize`,
    /// which differ in whether the far edge comes along.
    fn pull_start(&mut self, to: i64, moving: bool) {
        if moving {
            self.end = to.saturating_add(self.end - self.start);
        }
        self.start = to;
    }

    /// The same for the far edge.
    fn pull_end(&mut self, to: i64, moving: bool) {
        if moving {
            self.start = to.saturating_sub(self.end - self.start);
        }
        self.end = to;
    }

    /// Whether this span and `other` overlap at all, which is what says two
    /// windows are beside each other rather than merely on the same screen.
    const fn meets(self, other: Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// Hyprland's `canSnap`: near enough to be worth snapping to.
const fn near(a: i64, b: i64, gap: i64) -> bool {
    a.abs_diff(b) < gap.unsigned_abs()
}

/// Which edges were snapped, so that the corner pass does not undo the edge
/// pass: Hyprland's `snaps` bitfield.
#[derive(Clone, Copy, Debug, Default)]
struct Snapped {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

/// `rect`, snapped to the windows in `others` and to the monitor.
///
/// `others` is every other window on the same workspace, `monitor` is the
/// monitor's whole logical box and `reserved` the strips bars have taken out
/// of it. `corner` is the edge a resize grabbed and is ignored for a move,
/// which pulls on all four.
pub(crate) fn perform(
    rect: Rect,
    moving: bool,
    corner: Corner,
    others: &[Rect],
    monitor: Rect,
    reserved: Gaps,
    settings: &Settings,
) -> Rect {
    let snap = &settings.snap;
    if !snap.enabled {
        return rect;
    }
    // A move pulls on every edge; a resize only on what it grabbed.
    let pulls = if moving {
        Corner {
            left: true,
            right: true,
            top: true,
            bottom: true,
        }
    } else {
        corner
    };
    let mut x = Span {
        start: rect.x,
        end: rect.right(),
    };
    let mut y = Span {
        start: rect.y,
        end: rect.bottom(),
    };
    let mut done = Snapped::default();

    if snap.window_gap > 0 {
        let (gaps_x, gaps_y) = if snap.respect_gaps {
            (
                settings.gaps_in.left + settings.gaps_in.right,
                settings.gaps_in.top + settings.gaps_in.bottom,
            )
        } else {
            (0, 0)
        };
        for other in others {
            let near_x = Span {
                start: other.x - gaps_x,
                end: other.right() + gaps_x,
            };
            let near_y = Span {
                start: other.y - gaps_y,
                end: other.bottom() + gaps_y,
            };
            // Edge to edge, and only where the windows are beside each other
            // on the other axis: a window at the far end of the screen is
            // not something to line up with.
            if y.meets(near_y) {
                if pulls.left && near(x.start, near_x.end, snap.window_gap) {
                    x.pull_start(near_x.end, moving);
                    done.left = true;
                } else if pulls.right && near(x.end, near_x.start, snap.window_gap) {
                    x.pull_end(near_x.start, moving);
                    done.right = true;
                }
            }
            if x.meets(near_x) {
                if pulls.top && near(y.start, near_y.end, snap.window_gap) {
                    y.pull_start(near_y.end, moving);
                    done.top = true;
                } else if pulls.bottom && near(y.end, near_y.start, snap.window_gap) {
                    y.pull_end(near_y.start, moving);
                    done.bottom = true;
                }
            }
            // And the corners: a window already flush against another's side
            // lines its *other* pair of edges up with that window's, so two
            // windows side by side end up level rather than merely touching.
            if x.start == near_x.end || near_x.start == x.end {
                let flush = Span {
                    start: near_y.start + gaps_y,
                    end: near_y.end - gaps_y,
                };
                if pulls.top && !done.top && near(y.start, flush.start, snap.window_gap) {
                    y.pull_start(flush.start, moving);
                    done.top = true;
                } else if pulls.bottom && !done.bottom && near(y.end, flush.end, snap.window_gap) {
                    y.pull_end(flush.end, moving);
                    done.bottom = true;
                }
            }
            if y.start == near_y.end || near_y.start == y.end {
                let flush = Span {
                    start: near_x.start + gaps_x,
                    end: near_x.end - gaps_x,
                };
                if pulls.left && !done.left && near(x.start, flush.start, snap.window_gap) {
                    x.pull_start(flush.start, moving);
                    done.left = true;
                } else if pulls.right && !done.right && near(x.end, flush.end, snap.window_gap) {
                    x.pull_end(flush.end, moving);
                    done.right = true;
                }
            }
        }
    }

    if snap.monitor_gap > 0 {
        let out = if snap.respect_gaps {
            settings.gaps_out
        } else {
            Gaps::all(0)
        };
        // Two places an edge may land: against the work area, which is what
        // a bar has left of the screen, and against the screen itself. A
        // window can be put under a bar deliberately, so both are offered
        // and the work area is only tried where there is a bar to be inside
        // of.
        let work = (
            monitor.x + reserved.left + out.left,
            monitor.right() - reserved.right - out.right,
            monitor.y + reserved.top + out.top,
            monitor.bottom() - reserved.bottom - out.bottom,
        );
        let edge = (
            monitor.x + out.left,
            monitor.right() - out.right,
            monitor.y + out.top,
            monitor.bottom() - out.bottom,
        );
        let gap = snap.monitor_gap;
        // The work area first, and only where a bar has actually taken
        // something out of that side; the screen's own edge otherwise.
        let landing = |at: i64, inside: i64, outside: i64, barred: bool| {
            if barred && near(at, inside, gap) {
                Some(inside)
            } else if near(at, outside, gap) {
                Some(outside)
            } else {
                None
            }
        };
        if let Some(to) = pulls
            .left
            .then(|| landing(x.start, work.0, edge.0, reserved.left > 0))
            .flatten()
        {
            x.pull_start(to, moving);
        }
        if let Some(to) = pulls
            .right
            .then(|| landing(x.end, work.1, edge.1, reserved.right > 0))
            .flatten()
        {
            x.pull_end(to, moving);
        }
        if let Some(to) = pulls
            .top
            .then(|| landing(y.start, work.2, edge.2, reserved.top > 0))
            .flatten()
        {
            y.pull_start(to, moving);
        }
        if let Some(to) = pulls
            .bottom
            .then(|| landing(y.end, work.3, edge.3, reserved.bottom > 0))
            .flatten()
        {
            y.pull_end(to, moving);
        }
    }

    Rect::new(
        x.start,
        y.start,
        (x.end - x.start).max(1),
        (y.end - y.start).max(1),
    )
}
