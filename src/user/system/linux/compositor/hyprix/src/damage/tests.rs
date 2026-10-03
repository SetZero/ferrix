//! Surface damage turned into buffer pixels, through the scale and the
//! viewport.

use compositor_layout::Rect;

use super::from_surface;

#[test]
fn without_a_viewport_the_scale_multiplies() {
    let rect = Rect::new(10, 20, 30, 40);
    assert_eq!(
        from_surface(rect, 2, Some((800, 600)), None, None),
        Rect::new(20, 40, 60, 80)
    );
}

#[test]
fn a_destination_stretches_the_whole_buffer() {
    // Chromium: a 1080x2400 buffer at scale 1, shown as a 540x1200 surface.
    let whole = Rect::new(0, 0, 540, 1200);
    assert_eq!(
        from_surface(whole, 1, Some((1080, 2400)), None, Some((540, 1200))),
        Rect::new(0, 0, 1080, 2400)
    );
    let part = Rect::new(100, 200, 10, 10);
    assert_eq!(
        from_surface(part, 1, Some((1080, 2400)), None, Some((540, 1200))),
        Rect::new(200, 400, 20, 20)
    );
}

#[test]
fn a_source_is_the_part_stretched() {
    // The right half of a 200x100 buffer at scale 2, shown 100x100.
    let rect = Rect::new(0, 0, 100, 100);
    let source = Some((50.0, 0.0, 50.0, 50.0));
    assert_eq!(
        from_surface(rect, 2, Some((200, 100)), source, Some((100, 100))),
        Rect::new(100, 0, 100, 100)
    );
    // No destination: the surface is the source's size.
    assert_eq!(
        from_surface(Rect::new(0, 0, 50, 50), 2, Some((200, 100)), source, None),
        Rect::new(100, 0, 100, 100)
    );
}

#[test]
fn fractions_round_outward() {
    // A 100-wide buffer over a 30-wide surface: one surface pixel is 3.33.
    assert_eq!(
        from_surface(
            Rect::new(1, 1, 1, 1),
            1,
            Some((100, 100)),
            None,
            Some((30, 30))
        ),
        Rect::new(3, 3, 4, 4)
    );
}

#[test]
fn everything_stays_everything() {
    let all = Rect::new(0, 0, i64::from(i32::MAX), i64::from(i32::MAX));
    let held = from_surface(all, 1, Some((1080, 2400)), None, Some((540, 1200)));
    assert!(held.width >= 1080 && held.height >= 2400);
}
