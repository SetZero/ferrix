//! What a frame is drawn with.
//!
//! [`crate::render_onto`] says *what* a frame is: the background, the layer
//! surfaces, each window's shadow, border, blur and pixels, in Hyprland's
//! order and with Hyprland's rules about which of them a window gets. None
//! of that is about how a rectangle gets filled. So the operations it calls
//! are this trait, and there are two of them: [`Canvas`], which fills it on
//! the CPU and whose bytes every expected image in this crate holds, and
//! [`crate::gpu::Canvas`], which writes a virgl stream that fills it on a
//! GPU.
//!
//! The software one is not a fallback in name only. A machine with no GPU
//! behind its card, a render node that would not open, a driver that died
//! half-way through a session -- each is a compositor that keeps drawing,
//! and what it draws is the reference the GPU's frames are judged against.
//!
//! Every operation draws only inside the [`Damage`] it is given, in the
//! painter's own pixels, over what is there.

use crate::backdrop::Backdrop;
use crate::{Blur, Canvas, Color, Damage, Gradient, Rect, Rounding, Shadow, Surface};

/// Something a frame can be drawn onto.
pub trait Painter {
    /// What this painter keeps of what is behind the windows, and of the
    /// blur of it: [`crate::Backdrop`]'s job, in this painter's own memory.
    type Backdrop;

    /// The whole of what is drawn onto.
    fn bounds(&self) -> Rect;

    /// Replace everything in `damage` with `color`, made opaque.
    fn clear(&mut self, color: Color, damage: &Damage);

    /// Blend `color` over `rect`.
    fn fill(&mut self, rect: Rect, color: Color, damage: &Damage);

    /// Blend `color` over `rect` with its corners cut.
    fn fill_rounded(&mut self, rect: Rect, rounding: Rounding, color: Color, damage: &Damage);

    /// Blend `gradient`, run across `rect`, over `rect` with its corners
    /// cut: a rounded window's border, before its surface goes inside it.
    fn fill_rounded_gradient(
        &mut self,
        rect: Rect,
        rounding: Rounding,
        gradient: &Gradient,
        damage: &Damage,
    );

    /// Blend a square border `width` wide *around* `rect`, in `gradient`
    /// run across the border's whole box.
    fn border_gradient(&mut self, rect: Rect, width: i64, gradient: &Gradient, damage: &Damage);

    /// Blend the shadow of a window at `rect`.
    fn shadow(&mut self, rect: Rect, shadow: &Shadow, damage: &Damage);

    /// Replace what is inside `rect`, corners cut, with the blur of what
    /// has been drawn so far.
    fn blur(&mut self, rect: Rect, rounding: Rounding, blur: &Blur, damage: &Damage);

    /// Blend `surface` at `rect`, pixel for pixel.
    fn composite(&mut self, surface: &Surface<'_>, rect: Rect, damage: &Damage);

    /// Blend `surface` stretched to `rect`, corners cut, at `opacity`.
    fn composite_scaled(
        &mut self,
        surface: &Surface<'_>,
        rect: Rect,
        rounding: Rounding,
        opacity: f32,
        nearest: bool,
        damage: &Damage,
    );

    /// Whether `backdrop` is one this painter can read: its own size.
    fn backdrop_fits(&self, backdrop: &Self::Backdrop) -> bool;

    /// Bring `backdrop` up to date with what has been drawn, inside
    /// `damage`. Called once everything behind the windows is down and
    /// before any window goes over it.
    fn keep_backdrop(&mut self, backdrop: &mut Self::Backdrop, damage: &Damage);

    /// [`Painter::blur`], of the backdrop rather than of the frame as it
    /// stands.
    fn blur_backdrop(
        &mut self,
        backdrop: &mut Self::Backdrop,
        rect: Rect,
        rounding: Rounding,
        blur: &Blur,
        damage: &Damage,
    );
}

impl Painter for Canvas {
    type Backdrop = Backdrop;

    fn bounds(&self) -> Rect {
        Self::bounds(self)
    }

    fn clear(&mut self, color: Color, damage: &Damage) {
        Self::clear(self, color, damage);
    }

    fn fill(&mut self, rect: Rect, color: Color, damage: &Damage) {
        Self::fill(self, rect, color, damage);
    }

    fn fill_rounded(&mut self, rect: Rect, rounding: Rounding, color: Color, damage: &Damage) {
        Self::fill_rounded(self, rect, rounding, color, damage);
    }

    fn fill_rounded_gradient(
        &mut self,
        rect: Rect,
        rounding: Rounding,
        gradient: &Gradient,
        damage: &Damage,
    ) {
        Self::fill_rounded_gradient(self, rect, rounding, gradient, damage);
    }

    fn border_gradient(&mut self, rect: Rect, width: i64, gradient: &Gradient, damage: &Damage) {
        Self::border_gradient(self, rect, width, gradient, damage);
    }

    fn shadow(&mut self, rect: Rect, shadow: &Shadow, damage: &Damage) {
        Self::shadow(self, rect, shadow, damage);
    }

    fn blur(&mut self, rect: Rect, rounding: Rounding, blur: &Blur, damage: &Damage) {
        Self::blur(self, rect, rounding, blur, damage);
    }

    fn composite(&mut self, surface: &Surface<'_>, rect: Rect, damage: &Damage) {
        Self::composite(self, surface, rect, damage);
    }

    fn composite_scaled(
        &mut self,
        surface: &Surface<'_>,
        rect: Rect,
        rounding: Rounding,
        opacity: f32,
        nearest: bool,
        damage: &Damage,
    ) {
        Self::composite_scaled(self, surface, rect, rounding, opacity, nearest, damage);
    }

    fn backdrop_fits(&self, backdrop: &Backdrop) -> bool {
        backdrop.fits(self)
    }

    fn keep_backdrop(&mut self, backdrop: &mut Backdrop, damage: &Damage) {
        backdrop.take(self, damage);
    }

    fn blur_backdrop(
        &mut self,
        backdrop: &mut Backdrop,
        rect: Rect,
        rounding: Rounding,
        blur: &Blur,
        damage: &Damage,
    ) {
        backdrop.blur_onto(self, rect, rounding, blur, damage);
    }
}
