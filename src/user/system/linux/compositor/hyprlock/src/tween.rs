//! A value moving on one of hyprlock's animation nodes: upstream's
//! `PHLANIMVAR`, over `src/user/linux/compositor/anim`'s curves and clock-free values.

use compositor_anim::{Bezier, Moving};

use crate::config::Animations;

/// One animated number.
#[derive(Clone, Debug)]
pub struct Tween {
    moving: Moving,
    curve: Bezier,
    duration: f32,
}

impl Tween {
    /// A value at `value`, animated as the node `node` says.
    #[must_use]
    pub fn new(value: f64, animations: &Animations, node: &str) -> Self {
        let settings = animations.get(node);
        Self {
            moving: Moving::still(value),
            curve: animations.curves.get(&settings.bezier).clone(),
            duration: animations.duration(node),
        }
    }

    /// Send it to `goal` from where it is at `now`.
    pub fn set(&mut self, goal: f64, now: u64) {
        if (self.moving.goal() - goal).abs() < f64::EPSILON {
            return;
        }
        self.moving = self.moving.towards(goal, now, self.duration, &self.curve);
    }

    /// Put it at `value` at once.
    pub fn warp(&mut self, value: f64) {
        self.moving = Moving::still(value);
    }

    /// Where it is at `now`.
    #[must_use]
    pub fn at(&self, now: u64) -> f64 {
        self.moving.at(now, &self.curve)
    }

    /// Where it is going.
    #[must_use]
    pub const fn goal(&self) -> f64 {
        self.moving.goal()
    }

    /// Whether it is still on its way at `now`.
    #[must_use]
    pub fn moving(&self, now: u64) -> bool {
        !self.moving.finished(now)
    }
}
