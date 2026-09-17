//! The windows on their way from where they were to where they are.
//!
//! `compositor/anim` holds the curves and the tree and knows nothing about a
//! window; this joins the two. Each pass round the compositor's loop the
//! layout says where every window belongs, and this says where each one *is*:
//! on its way there along `windowsMove`'s curve, or already arrived.
//!
//! # What is animated and what is not
//!
//! A window's rectangle: where it is and how big. A window that has just
//! opened starts at its goal -- `windowsIn` is a style (`slide`, `popin`)
//! rather than a move from somewhere, and a style is not written yet -- so
//! what moves is a window the layout put somewhere else: a swap, a focus
//! change that re-tiles, a gap that changed.
//!
//! The client is configured at the *goal* size and draws once, as Hyprland's
//! is; the renderer scales its surface into the rectangle while it moves.
//! Configuring it once a frame would make every animation a storm of round
//! trips and a client that never caught up.

use std::collections::BTreeMap;

use compositor_anim::{Bezier, Curves, Moving, Tree};
use compositor_config::Config;
use compositor_layout::{MonitorLayout, Rect, WindowId};

/// A rectangle on its way somewhere.
#[derive(Clone, Copy, Debug)]
struct MovingRect {
    x: Moving,
    y: Moving,
    width: Moving,
    height: Moving,
}

impl MovingRect {
    fn still(rect: Rect) -> Self {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a screen coordinate; f64 holds every one exactly"
        )]
        Self {
            x: Moving::still(rect.x as f64),
            y: Moving::still(rect.y as f64),
            width: Moving::still(rect.width as f64),
            height: Moving::still(rect.height as f64),
        }
    }

    fn towards(self, rect: Rect, now: u64, duration: f32, curve: &Bezier) -> Self {
        #[expect(clippy::cast_precision_loss, reason = "as in `still`")]
        Self {
            x: self.x.towards(rect.x as f64, now, duration, curve),
            y: self.y.towards(rect.y as f64, now, duration, curve),
            width: self.width.towards(rect.width as f64, now, duration, curve),
            height: self
                .height
                .towards(rect.height as f64, now, duration, curve),
        }
    }

    fn at(&self, now: u64, curve: &Bezier) -> Rect {
        let round = |value: f64| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a screen coordinate, rounded to the pixel it is drawn at"
            )]
            let whole = value.round() as i64;
            whole
        };
        Rect::new(
            round(self.x.at(now, curve)),
            round(self.y.at(now, curve)),
            round(self.width.at(now, curve)).max(0),
            round(self.height.at(now, curve)).max(0),
        )
    }

    fn finished(&self, now: u64) -> bool {
        self.x.finished(now)
            && self.y.finished(now)
            && self.width.finished(now)
            && self.height.finished(now)
    }
}

/// Every window that is moving, and the curves they move along.
#[derive(Clone, Debug)]
pub struct Animations {
    curves: Curves,
    tree: Tree,
    /// `animations:enabled`: the master switch, which turns every animation
    /// off whatever the tree says.
    enabled: bool,
    moving: BTreeMap<WindowId, MovingRect>,
    /// What could not be read from the configuration, for the log.
    said: Vec<String>,
}

impl Animations {
    /// Read `config`'s `bezier` and `animation` lines.
    #[must_use]
    pub fn new(config: &Config) -> Self {
        let (curves, tree, diagnostics) = compositor_anim::read(&config.animations);
        Self {
            curves,
            tree,
            enabled: config.bool("animations:enabled").unwrap_or(true),
            moving: BTreeMap::new(),
            said: diagnostics
                .into_iter()
                .map(|one| format!("{}: {}", one.line, one.reason))
                .collect(),
        }
    }

    /// What could not be read, which the compositor says once.
    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.said
    }

    /// Whether any window is still moving at `now`.
    #[must_use]
    pub fn busy(&self, now: u64) -> bool {
        self.moving.values().any(|rect| !rect.finished(now))
    }

    /// Take the layout the tiling worked out, and give back the one to draw.
    ///
    /// A window whose goal changed is sent towards it; every window's
    /// rectangle is then whatever it is at `now`. A window that has gone is
    /// forgotten.
    #[must_use]
    pub fn follow(&mut self, layout: &MonitorLayout, now: u64) -> MonitorLayout {
        let settings = self.tree.get("windowsMove");
        let curve = self.curves.get(&settings.curve).clone();
        let duration = if self.enabled {
            self.tree.duration("windowsMove")
        } else {
            0.0
        };

        let mut drawn = layout.clone();
        for placed in &mut drawn.windows {
            let moving = match self.moving.get(&placed.window).copied() {
                Some(moving) => {
                    if moving_goal(&moving) == placed.rect {
                        moving
                    } else {
                        moving.towards(placed.rect, now, duration, &curve)
                    }
                }
                // A window that has just opened is where the layout put it:
                // `windowsIn` is a style, and a style is not written yet.
                None => MovingRect::still(placed.rect),
            };
            let _ = self.moving.insert(placed.window, moving);
            placed.rect = moving.at(now, &curve);
        }
        self.moving
            .retain(|window, _| layout.windows.iter().any(|placed| placed.window == *window));
        drawn
    }
}

/// Where a moving rectangle is going.
fn moving_goal(moving: &MovingRect) -> Rect {
    let round = |value: f64| {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a screen coordinate, as in `at`"
        )]
        let whole = value.round() as i64;
        whole
    };
    Rect::new(
        round(moving.x.goal()),
        round(moving.y.goal()),
        round(moving.width.goal()),
        round(moving.height.goal()),
    )
}
