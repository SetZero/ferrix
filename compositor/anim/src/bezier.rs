//! Hyprland's animation curve: a cubic bezier, baked and looked up.
//!
//! `bezier = NAME, X0, Y0, X1, Y1` in a `hyprland.conf` names the two control
//! points of a cubic bezier from (0, 0) to (1, 1). An animation asks it, for
//! the fraction of its time that has passed, what fraction of its distance it
//! has covered.
//!
//! # Why it is baked
//!
//! A cubic bezier is a curve in `t`, and both `x` and `y` are cubics of it:
//! there is no closed form for "the `y` at this `x`" that is worth writing.
//! Hyprland bakes 255 points at `t = (i + 1) / 255` and then binary-searches
//! the baked `x`s and interpolates between two of them
//! (`hyprutils/src/animation/BezierCurve.cpp`). This does the same, to the
//! same 255 points, because a curve that is nearly Hyprland's is an animation
//! that looks nearly right and cannot be compared against anything.

/// How many points are baked, as `BAKEDPOINTS` in hyprutils.
pub const BAKED: usize = 255;

/// A cubic bezier from (0, 0) to (1, 1) through two control points.
#[derive(Clone, Debug, PartialEq)]
pub struct Bezier {
    control: [(f32, f32); 2],
    baked: Vec<(f32, f32)>,
}

impl Bezier {
    /// Hyprland's `linear`: the straight line, added by its animation
    /// manager and by `removeAllBeziers`.
    #[must_use]
    pub fn linear() -> Self {
        Self::new((0.0, 0.0), (1.0, 1.0))
    }

    /// Hyprland's `default`, whose control points are `DEFAULTBEZIERPOINTS`
    /// in `hyprutils/src/animation/AnimationManager.cpp`: a curve that
    /// starts fast and eases out.
    #[must_use]
    pub fn default_curve() -> Self {
        Self::new((0.0, 0.75), (0.15, 1.0))
    }

    /// The curve through `first` and `second`.
    #[must_use]
    pub fn new(first: (f32, f32), second: (f32, f32)) -> Self {
        let control = [first, second];
        let baked = (0..BAKED)
            .map(|index| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "255 points; every index is exact in f32"
                )]
                let t = (index + 1) as f32 / BAKED as f32;
                (x_at(&control, t), y_at(&control, t))
            })
            .collect();
        Self { control, baked }
    }

    /// The control points, as the configuration gave them.
    #[must_use]
    pub const fn control(&self) -> &[(f32, f32); 2] {
        &self.control
    }

    /// How far along the animation is at `x`, the fraction of its time that
    /// has passed.
    ///
    /// Outside 0 to 1 the answer is the end it is past, which is what
    /// `getYForPoint` returns and what keeps a late tick from overshooting.
    #[must_use]
    pub fn y_for_x(&self, x: f32) -> f32 {
        if x >= 1.0 {
            return 1.0;
        }
        if x <= 0.0 {
            return 0.0;
        }
        // hyprutils' own search: halve the step each time, stepping towards
        // or away from the end depending on which side of `x` the point is.
        let mut index: isize = 0;
        let mut below = true;
        // `(BAKEDPOINTS + 1) / 2` in hyprutils, which is this.
        let mut step = BAKED.div_ceil(2);
        while step > 0 {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "the step is at most 128; an isize holds it"
            )]
            let by = step as isize;
            index = if below { index + by } else { index - by };
            index = index.clamp(0, BAKED as isize - 1);
            below = self
                .baked
                .get(index.unsigned_abs())
                .is_some_and(|point| point.0 < x);
            step /= 2;
        }
        let lower = index - isize::from(!below || index == BAKED as isize - 1);
        let lower = lower.clamp(0, BAKED as isize - 2).unsigned_abs();
        let (Some(low), Some(high)) = (self.baked.get(lower), self.baked.get(lower + 1)) else {
            return 1.0;
        };
        let dx = high.0 - low.0;
        // Two baked points with almost the same `x`: the lower one, rather
        // than a division that would be a very large number or a NaN.
        if dx <= 1e-6 {
            return low.1;
        }
        let along = (x - low.0) / dx;
        if !along.is_finite() {
            return low.1;
        }
        low.1 + (high.1 - low.1) * along
    }
}

/// The cubic's `x` at `t`.
fn x_at(control: &[(f32, f32); 2], t: f32) -> f32 {
    cubic(0.0, control[0].0, control[1].0, 1.0, t)
}

/// The cubic's `y` at `t`.
fn y_at(control: &[(f32, f32); 2], t: f32) -> f32 {
    cubic(0.0, control[0].1, control[1].1, 1.0, t)
}

/// One coordinate of a cubic bezier at `t`, written as hyprutils writes it.
fn cubic(start: f32, first: f32, second: f32, end: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    let u = 1.0 - t;
    (u * u * u * start) + (3.0 * t * u * u * first) + (3.0 * t2 * u * second) + (t3 * end)
}
