//! A value on its way from one place to another.
//!
//! Hyprland's `CAnimatedVariable` is where it began, where it is going and
//! when it started; what it *is* at any moment is the fraction of its time
//! that has passed, put through the curve, applied to the distance
//! (`hyprutils/src/animation/AnimatedVariable.cpp`). This is that, with the
//! clock passed in rather than read, so a test can run an animation without
//! waiting for one.

use crate::Bezier;

/// A value moving from `begun` to `goal`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moving {
    begun: f64,
    goal: f64,
    /// When it started, in milliseconds on the caller's own clock.
    started: u64,
    /// How long it takes, in milliseconds. Zero warps.
    duration: f32,
}

impl Moving {
    /// A value that is already where it is going.
    #[must_use]
    pub const fn still(value: f64) -> Self {
        Self {
            begun: value,
            goal: value,
            started: 0,
            duration: 0.0,
        }
    }

    /// Send a value to `goal` over `duration` milliseconds, starting now.
    ///
    /// The value it starts from is where it is at `now`, not where it was
    /// going: a window that is asked to move again half-way through a move
    /// carries on from where it is, which is what keeps a fast succession of
    /// keybinds from making it jump.
    #[must_use]
    pub fn towards(self, goal: f64, now: u64, duration: f32, curve: &Bezier) -> Self {
        let begun = self.at(now, curve);
        if duration <= 0.0 || (goal - begun).abs() < f64::EPSILON {
            return Self::still(goal);
        }
        Self {
            begun,
            goal,
            started: now,
            duration,
        }
    }

    /// Where it is at `now`.
    #[must_use]
    pub fn at(&self, now: u64, curve: &Bezier) -> f64 {
        if self.duration <= 0.0 {
            return self.goal;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a millisecond count within an animation; f32 holds it as Hyprland's does"
        )]
        let passed = now.saturating_sub(self.started) as f32;
        // `getPercent`: the milliseconds over a hundred, over the speed in
        // deciseconds -- which is the same as the milliseconds over the
        // duration.
        let spent = (passed / self.duration).clamp(0.0, 1.0);
        if spent >= 1.0 {
            return self.goal;
        }
        let along = f64::from(curve.y_for_x(spent));
        self.begun + (self.goal - self.begun) * along
    }

    /// Where it is going.
    #[must_use]
    pub const fn goal(&self) -> f64 {
        self.goal
    }

    /// Whether it has arrived by `now`.
    #[must_use]
    pub fn finished(&self, now: u64) -> bool {
        self.duration <= 0.0 || {
            #[expect(clippy::cast_precision_loss, reason = "as in `at`")]
            let passed = now.saturating_sub(self.started) as f32;
            passed >= self.duration
        }
    }
}
