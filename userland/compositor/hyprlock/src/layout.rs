//! Where a widget goes: `IWidget::posFromHVAlign` and the rounding rules.
//!
//! Upstream draws in OpenGL's coordinates, the origin at the bottom left
//! and y growing upwards, so `position = 0, 240` moves a widget *up* and
//! `valign = bottom` puts it at y = 0. The maths here is upstream's, in
//! those coordinates; [`Placed::top`] turns the answer into the top-down
//! row a pixmap is drawn at.

/// A widget's box in upstream's coordinates, with the viewport it is in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// The left edge.
    pub x: f64,
    /// The bottom edge, from the bottom of the screen.
    pub y: f64,
    /// Its width.
    pub width: f64,
    /// Its height.
    pub height: f64,
    /// The screen's height, to turn `y` over with.
    pub viewport_height: f64,
}

impl Placed {
    /// The top edge, counted down from the top of the screen.
    #[must_use]
    pub fn top(&self) -> f64 {
        self.viewport_height - self.y - self.height
    }
}

/// `posFromHVAlign`: the bottom-left corner of a `size` box on a
/// `viewport`, moved by `offset`, aligned by `halign` and `valign`, with
/// the correction a rotation by `angle` radians makes. An alignment it does
/// not know leaves that axis at the offset and is reported, as upstream
/// logs `IWidget: invalid halign …`.
#[must_use]
pub fn place(
    viewport: (f64, f64),
    size: (f64, f64),
    offset: (f64, f64),
    halign: &str,
    valign: &str,
    angle: f64,
) -> (Placed, Option<String>) {
    let turned = if angle == 0.0 {
        (0.0, 0.0)
    } else {
        let (cos, sin) = (angle.cos().abs(), angle.sin().abs());
        let rotated = (size.0 * cos + size.1 * sin, size.0 * sin + size.1 * cos);
        ((size.0 - rotated.0) / 2.0, (size.1 - rotated.1) / 2.0)
    };
    let mut problem = None;
    let mut x = offset.0;
    match halign {
        "center" => x += viewport.0 / 2.0 - size.0 / 2.0,
        "left" => x -= turned.0,
        "right" => x += viewport.0 - size.0 + turned.0,
        "none" => {}
        other => problem = Some(format!("IWidget: invalid halign {other}")),
    }
    let mut y = offset.1;
    match valign {
        "center" => y += viewport.1 / 2.0 - size.1 / 2.0,
        "top" => y += viewport.1 - size.1 + turned.1,
        "bottom" => y -= turned.1,
        "none" => {}
        other => problem = Some(format!("IWidget: invalid valign {other}")),
    }
    (
        Placed {
            x,
            y,
            width: size.0,
            height: size.1,
            viewport_height: viewport.1,
        },
        problem,
    )
}

/// Half the shorter side, truncated as upstream's `int` truncates it.
fn half_shorter(width: f64, height: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a widget's size in pixels; upstream truncates to int the same way"
    )]
    let half = (width.min(height) / 2.0) as i64;
    half
}

/// `roundingForBox`: -1 is as round as the box allows, anything else is
/// clamped to that.
#[must_use]
pub fn rounding_for_box(width: f64, height: f64, rounding: i64) -> i64 {
    let most = half_shorter(width, height);
    if rounding == -1 {
        most
    } else {
        rounding.clamp(0, most.max(0))
    }
}

/// `roundingForBorderBox`: the outer edge of a border `thickness` wide
/// around a box rounded by `rounding`.
#[must_use]
pub fn rounding_for_border_box(width: f64, height: f64, rounding: i64, thickness: i64) -> i64 {
    let most = half_shorter(width, height);
    match rounding {
        -1 => most,
        0 => 0,
        _ => rounding.saturating_add(thickness).clamp(0, most.max(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::{place, rounding_for_border_box, rounding_for_box};

    #[test]
    fn the_customer_s_clock_panel_on_a_1080p_screen() {
        // shape: size 520x190, position 0, 240, centred both ways.
        let (placed, problem) = place(
            (1920.0, 1080.0),
            (520.0, 190.0),
            (0.0, 240.0),
            "center",
            "center",
            0.0,
        );
        assert_eq!(problem, None);
        assert!((placed.x - 700.0).abs() < 1e-9);
        assert!((placed.y - 685.0).abs() < 1e-9);
        // 240 up from the middle is 205 down from the top.
        assert!((placed.top() - 205.0).abs() < 1e-9);
    }

    #[test]
    fn bottom_left_is_the_origin_and_offsets_move_up_and_right() {
        let (placed, _) = place(
            (1000.0, 800.0),
            (100.0, 20.0),
            (24.0, 18.0),
            "left",
            "bottom",
            0.0,
        );
        assert!((placed.x - 24.0).abs() < 1e-9);
        assert!((placed.top() - 762.0).abs() < 1e-9);
        let (placed, _) = place(
            (1000.0, 800.0),
            (100.0, 20.0),
            (0.0, 0.0),
            "right",
            "top",
            0.0,
        );
        assert!((placed.x - 900.0).abs() < 1e-9);
        assert!(placed.top().abs() < 1e-9);
    }

    #[test]
    fn an_unknown_alignment_is_said() {
        let (_, problem) = place(
            (10.0, 10.0),
            (1.0, 1.0),
            (0.0, 0.0),
            "middle",
            "center",
            0.0,
        );
        assert_eq!(problem.as_deref(), Some("IWidget: invalid halign middle"));
    }

    #[test]
    fn rounding_is_clamped_to_half_the_shorter_side() {
        assert_eq!(rounding_for_box(320.0, 56.0, 28), 28);
        assert_eq!(rounding_for_box(320.0, 56.0, 40), 28);
        assert_eq!(rounding_for_box(320.0, 56.0, -1), 28);
        // The border box around it, two pixels out.
        assert_eq!(rounding_for_border_box(324.0, 60.0, 28, 2), 30);
        assert_eq!(rounding_for_border_box(324.0, 60.0, 0, 2), 0);
    }
}
