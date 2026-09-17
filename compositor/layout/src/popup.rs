//! Where a popup goes: `xdg_positioner`'s rules, as arithmetic.
//!
//! A menu, a tooltip and a dropdown are all `xdg_popup`s, and where each one
//! lands is decided entirely by the numbers its client put in an
//! `xdg_positioner`: a rectangle on the parent to hang off, a corner of that
//! rectangle to anchor to, a direction to grow in, an offset, and what to do
//! when the result falls off the screen.
//!
//! The protocol is specific about all of it, and none of it involves a
//! surface, a buffer or a socket -- so it is written here, where it can be
//! tested against the rules rather than against a screenshot.
//!
//! # The order the rules are applied in
//!
//! `xdg_positioner`'s own, from its description:
//!
//! 1. the anchor point on the anchor rectangle, which is a corner, an edge's
//!    middle, or the rectangle's centre;
//! 2. the offset, added to it;
//! 3. the gravity, which says which way the popup grows from that point --
//!    `bottom_right` puts its top-left corner there, `top_left` its
//!    bottom-right, and `none` centres it;
//! 4. the constraint adjustments, in the order `flip`, `slide`, `resize`,
//!    each on the axis that is off the screen and only if the client asked
//!    for it.
//!
//! Everything is in the parent's surface-local coordinates, which is what
//! `xdg_popup.configure` carries, so the anchor rectangle and the answer are
//! both relative to the parent's window geometry.

use crate::Rect;

/// `xdg_positioner.anchor`, in the protocol's own order.
///
/// Named rather than numbered so that the arithmetic below reads as the
/// rules do; [`Anchor::from_wire`] is where the numbers stop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    /// The rectangle's centre.
    #[default]
    None,
    /// The middle of an edge.
    Top,
    /// The middle of an edge.
    Bottom,
    /// The middle of an edge.
    Left,
    /// The middle of an edge.
    Right,
    /// A corner.
    TopLeft,
    /// A corner.
    BottomLeft,
    /// A corner.
    TopRight,
    /// A corner.
    BottomRight,
}

impl Anchor {
    /// The value the wire carries, or `None` for one the protocol does not
    /// have -- which is `invalid_input` and the caller's to refuse.
    #[must_use]
    pub const fn from_wire(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::None,
            1 => Self::Top,
            2 => Self::Bottom,
            3 => Self::Left,
            4 => Self::Right,
            5 => Self::TopLeft,
            6 => Self::BottomLeft,
            7 => Self::TopRight,
            8 => Self::BottomRight,
            _ => return None,
        })
    }

    /// Which way the anchor point is pulled: -1, 0 or 1 on each axis.
    const fn pull(self) -> (i64, i64) {
        match self {
            Self::None => (0, 0),
            Self::Top => (0, -1),
            Self::Bottom => (0, 1),
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
            Self::TopLeft => (-1, -1),
            Self::BottomLeft => (-1, 1),
            Self::TopRight => (1, -1),
            Self::BottomRight => (1, 1),
        }
    }

    /// The same anchor with its horizontal half turned over, which is what
    /// `flip_x` does.
    const fn flipped_x(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::TopLeft => Self::TopRight,
            Self::TopRight => Self::TopLeft,
            Self::BottomLeft => Self::BottomRight,
            Self::BottomRight => Self::BottomLeft,
            other => other,
        }
    }

    /// The same, vertically.
    const fn flipped_y(self) -> Self {
        match self {
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
            Self::TopLeft => Self::BottomLeft,
            Self::BottomLeft => Self::TopLeft,
            Self::TopRight => Self::BottomRight,
            Self::BottomRight => Self::TopRight,
            other => other,
        }
    }
}

/// `xdg_positioner.gravity`: which way the popup grows from the anchor
/// point.
pub type Gravity = Anchor;

/// `xdg_positioner.constraint_adjustment`, as its bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Adjust(pub u32);

impl Adjust {
    /// Slide along the x axis until it fits.
    pub const SLIDE_X: u32 = 1;
    /// Slide along the y axis.
    pub const SLIDE_Y: u32 = 2;
    /// Turn the anchor and the gravity over, horizontally.
    pub const FLIP_X: u32 = 4;
    /// The same, vertically.
    pub const FLIP_Y: u32 = 8;
    /// Make it narrower.
    pub const RESIZE_X: u32 = 16;
    /// Make it shorter.
    pub const RESIZE_Y: u32 = 32;

    const fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

/// Everything an `xdg_positioner` holds.
///
/// The defaults are the protocol's: a popup whose client set nothing is an
/// empty rectangle at the parent's top-left, which the compositor refuses --
/// `set_size` and `set_anchor_rect` are both required before `get_popup`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Positioner {
    /// What the popup asked to be.
    pub size: (i64, i64),
    /// The rectangle on the parent it hangs off, in the parent's
    /// surface-local coordinates.
    pub anchor_rect: Rect,
    /// Which point of that rectangle it hangs from.
    pub anchor: Anchor,
    /// Which way it grows from there.
    pub gravity: Gravity,
    /// What to do when it does not fit.
    pub adjust: Adjust,
    /// Added to the anchor point.
    pub offset: (i64, i64),
    /// Whether it is to be placed again when the parent moves, which the
    /// caller obeys and this arithmetic does not care about.
    pub reactive: bool,
}

impl Positioner {
    /// Whether the client said enough for `get_popup` to be answered.
    ///
    /// The protocol makes an unset size or anchor rectangle
    /// `xdg_wm_base.invalid_positioner`, which is the one error a popup can
    /// earn before it exists.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.size.0 > 0
            && self.size.1 > 0
            && self.anchor_rect.width > 0
            && self.anchor_rect.height > 0
    }
}

/// Where the popup goes, in the parent's surface-local coordinates.
///
/// `parent` is the parent's window geometry and `room` is what the popup
/// must fit inside -- the monitor, in the same coordinates -- which is what
/// the constraint adjustments are measured against.
#[must_use]
pub fn place(positioner: &Positioner, parent: Rect, room: Rect) -> Rect {
    let (mut anchor, mut gravity) = (positioner.anchor, positioner.gravity);
    let mut rect = at(positioner, anchor, gravity);

    // The room, in the same coordinates the answer is in: the popup's
    // rectangle is relative to the parent's geometry, so the screen is too.
    let room = Rect::new(
        room.x.saturating_sub(parent.x),
        room.y.saturating_sub(parent.y),
        room.width,
        room.height,
    );

    // Flip first, and only if flipping actually helps: the protocol says a
    // flip that would still not fit is not made, which stops a popup
    // jumping to the other side for no gain.
    if positioner.adjust.has(Adjust::FLIP_X) && !fits_x(rect, room) {
        let flipped = at(positioner, anchor.flipped_x(), gravity.flipped_x());
        if fits_x(flipped, room) {
            anchor = anchor.flipped_x();
            gravity = gravity.flipped_x();
            rect = flipped;
        }
    }
    if positioner.adjust.has(Adjust::FLIP_Y) && !fits_y(rect, room) {
        let flipped = at(positioner, anchor.flipped_y(), gravity.flipped_y());
        if fits_y(flipped, room) {
            rect = flipped;
        }
    }

    // Then slide, which moves it along the axis until an edge is inside.
    if positioner.adjust.has(Adjust::SLIDE_X) {
        rect.x = slide(rect.x, rect.width, room.x, room.width);
    }
    if positioner.adjust.has(Adjust::SLIDE_Y) {
        rect.y = slide(rect.y, rect.height, room.y, room.height);
    }

    // And last resize, which is the only one that changes what the client
    // asked to be.
    if positioner.adjust.has(Adjust::RESIZE_X) {
        rect.width = shrink(rect.x, rect.width, room.x, room.width);
    }
    if positioner.adjust.has(Adjust::RESIZE_Y) {
        rect.height = shrink(rect.y, rect.height, room.y, room.height);
    }
    rect
}

/// The popup's rectangle for one anchor and gravity, before any constraint.
fn at(positioner: &Positioner, anchor: Anchor, gravity: Gravity) -> Rect {
    let rect = positioner.anchor_rect;
    let (pull_x, pull_y) = anchor.pull();
    // The anchor point: an edge for -1 or 1, the middle for 0.
    let edge = |start: i64, size: i64, pull: i64| match pull {
        -1 => start,
        1 => start.saturating_add(size),
        _ => start.saturating_add(size / 2),
    };
    let point = (
        edge(rect.x, rect.width, pull_x),
        edge(rect.y, rect.height, pull_y),
    );
    let point = (
        point.0.saturating_add(positioner.offset.0),
        point.1.saturating_add(positioner.offset.1),
    );
    // The gravity: which corner of the popup lands on that point.
    let (grow_x, grow_y) = gravity.pull();
    let top_left = |at: i64, size: i64, grow: i64| match grow {
        1 => at,
        -1 => at.saturating_sub(size),
        _ => at.saturating_sub(size / 2),
    };
    Rect::new(
        top_left(point.0, positioner.size.0, grow_x),
        top_left(point.1, positioner.size.1, grow_y),
        positioner.size.0,
        positioner.size.1,
    )
}

/// Whether the rectangle's horizontal span is inside the room.
const fn fits_x(rect: Rect, room: Rect) -> bool {
    rect.x >= room.x && rect.x + rect.width <= room.x + room.width
}

/// The same, vertically.
const fn fits_y(rect: Rect, room: Rect) -> bool {
    rect.y >= room.y && rect.y + rect.height <= room.y + room.height
}

/// Slide `at` so that `size` is inside `start..start + room`, if it can be.
///
/// The far edge first and the near edge second, which is the order the
/// protocol gives: a popup wider than the room ends up flush with the near
/// edge rather than the far one.
fn slide(at: i64, size: i64, start: i64, room: i64) -> i64 {
    let mut at = at;
    if at.saturating_add(size) > start.saturating_add(room) {
        at = start.saturating_add(room).saturating_sub(size);
    }
    if at < start {
        at = start;
    }
    at
}

/// Shrink `size` so that it ends inside the room, never below one pixel.
fn shrink(at: i64, size: i64, start: i64, room: i64) -> i64 {
    let far = start.saturating_add(room);
    if at.saturating_add(size) <= far {
        return size;
    }
    far.saturating_sub(at).max(1)
}

#[cfg(test)]
mod tests {
    use super::{Adjust, Anchor, Positioner, place};
    use crate::Rect;

    /// A menu hanging off a 20x20 button at (100, 100) inside a parent, on a
    /// 1000x1000 screen with plenty of room.
    fn menu() -> Positioner {
        Positioner {
            size: (200, 300),
            anchor_rect: Rect::new(100, 100, 20, 20),
            anchor: Anchor::BottomLeft,
            gravity: Anchor::BottomRight,
            ..Positioner::default()
        }
    }

    const PARENT: Rect = Rect {
        x: 0,
        y: 0,
        width: 1000,
        height: 1000,
    };
    const ROOM: Rect = PARENT;

    /// The ordinary case: a dropdown under the left edge of its button,
    /// growing right and down. The anchor is the button's bottom-left, so
    /// the popup's top-left lands there.
    #[test]
    fn a_dropdown_hangs_from_the_corner_it_named() {
        let at = place(&menu(), PARENT, ROOM);
        assert_eq!(at, Rect::new(100, 120, 200, 300));
    }

    /// Every anchor names the point the protocol says it does: a corner, the
    /// middle of an edge, or the centre.
    #[test]
    fn each_anchor_is_the_point_the_protocol_names() {
        let with = |anchor| {
            let mut positioner = menu();
            positioner.anchor = anchor;
            // `bottom_right` gravity puts the popup's top-left on the point,
            // so the answer's corner *is* the anchor point.
            place(&positioner, PARENT, ROOM)
        };
        for (anchor, point) in [
            (Anchor::TopLeft, (100, 100)),
            (Anchor::Top, (110, 100)),
            (Anchor::TopRight, (120, 100)),
            (Anchor::Left, (100, 110)),
            (Anchor::None, (110, 110)),
            (Anchor::Right, (120, 110)),
            (Anchor::BottomLeft, (100, 120)),
            (Anchor::Bottom, (110, 120)),
            (Anchor::BottomRight, (120, 120)),
        ] {
            let at = with(anchor);
            assert_eq!((at.x, at.y), point, "{anchor:?}");
        }
    }

    /// The gravity decides which corner of the popup lands on the anchor
    /// point, and `none` centres it.
    #[test]
    fn the_gravity_says_which_way_the_popup_grows() {
        let with = |gravity| {
            let mut positioner = menu();
            positioner.anchor = Anchor::None;
            positioner.gravity = gravity;
            place(&positioner, PARENT, ROOM)
        };
        // The anchor point is the button's centre, (110, 110).
        assert_eq!(with(Anchor::BottomRight), Rect::new(110, 110, 200, 300));
        assert_eq!(with(Anchor::TopLeft), Rect::new(-90, -190, 200, 300));
        assert_eq!(with(Anchor::None), Rect::new(10, -40, 200, 300));
    }

    /// The offset is added to the anchor point and to nothing else.
    #[test]
    fn the_offset_moves_the_anchor_point() {
        let mut positioner = menu();
        positioner.offset = (7, -3);
        assert_eq!(
            place(&positioner, PARENT, ROOM),
            Rect::new(107, 117, 200, 300)
        );
    }

    /// A popup that would fall off the right edge and asked to be flipped is
    /// put on the other side of its anchor rectangle.
    #[test]
    fn flipping_puts_it_on_the_other_side() {
        let mut positioner = menu();
        // A button hard against the right edge of a 300-wide screen.
        positioner.anchor_rect = Rect::new(250, 100, 20, 20);
        let room = Rect::new(0, 0, 300, 1000);
        // Without the adjustment it hangs off the edge.
        assert_eq!(place(&positioner, PARENT, room).x, 250);
        positioner.adjust = Adjust(Adjust::FLIP_X);
        // Flipped: the anchor becomes the button's right edge and the
        // gravity grows left, so the popup's right edge is at 270.
        assert_eq!(
            place(&positioner, PARENT, room),
            Rect::new(70, 120, 200, 300)
        );
    }

    /// A flip that would not help either is not made, which is what stops a
    /// popup jumping across for no gain.
    #[test]
    fn a_flip_that_does_not_help_is_not_made() {
        let mut positioner = menu();
        positioner.size = (400, 300);
        positioner.anchor_rect = Rect::new(150, 100, 20, 20);
        positioner.adjust = Adjust(Adjust::FLIP_X);
        // 400 wide on a 300-wide screen: neither side fits, so it stays
        // where the client asked.
        let room = Rect::new(0, 0, 300, 1000);
        assert_eq!(place(&positioner, PARENT, room).x, 150);
    }

    /// Sliding moves it along until an edge is inside, far edge first.
    #[test]
    fn sliding_moves_it_until_it_fits() {
        let mut positioner = menu();
        positioner.anchor_rect = Rect::new(250, 100, 20, 20);
        positioner.adjust = Adjust(Adjust::SLIDE_X);
        let room = Rect::new(0, 0, 300, 1000);
        // Flush with the far edge: 300 - 200.
        assert_eq!(place(&positioner, PARENT, room).x, 100);

        // A popup wider than the room ends flush with the *near* edge, which
        // is the order the two steps are written in.
        positioner.size = (400, 300);
        assert_eq!(place(&positioner, PARENT, room).x, 0);
    }

    /// Resizing is the last resort and the only one that gives the client
    /// something other than the size it asked for.
    #[test]
    fn resizing_makes_it_fit_and_never_smaller_than_a_pixel() {
        let mut positioner = menu();
        positioner.anchor_rect = Rect::new(250, 100, 20, 20);
        positioner.adjust = Adjust(Adjust::RESIZE_X);
        let room = Rect::new(0, 0, 300, 1000);
        // From 250 to the edge at 300.
        assert_eq!(place(&positioner, PARENT, room).width, 50);

        // Starting past the edge leaves one pixel rather than a negative
        // width, which is not a rectangle.
        positioner.anchor_rect = Rect::new(400, 100, 20, 20);
        assert_eq!(place(&positioner, PARENT, room).width, 1);
    }

    /// The answer is in the parent's coordinates, so a parent that is not at
    /// the screen's origin moves what counts as off the screen and not what
    /// the popup's numbers mean.
    #[test]
    fn the_room_is_measured_from_the_parent() {
        let mut positioner = menu();
        positioner.adjust = Adjust(Adjust::SLIDE_X);
        // The parent is 200 from the left of a 300-wide screen, so the
        // popup's own 100 is at 300 on the screen and has to slide back.
        let parent = Rect::new(200, 0, 500, 500);
        let room = Rect::new(0, 0, 300, 1000);
        let at = place(&positioner, parent, room);
        assert_eq!(at.x, -100, "the popup slid to the screen's left edge");
        assert_eq!(at.x + parent.x, 100, "which is 100 on the screen");
    }

    /// `set_size` and `set_anchor_rect` are both required: a positioner
    /// without them is `invalid_positioner`, which the caller refuses.
    #[test]
    fn a_positioner_without_a_size_or_a_rectangle_is_not_complete() {
        assert!(menu().is_complete());
        assert!(!Positioner::default().is_complete());
        let mut positioner = menu();
        positioner.size = (0, 300);
        assert!(!positioner.is_complete());
        positioner = menu();
        positioner.anchor_rect = Rect::new(0, 0, 0, 20);
        assert!(!positioner.is_complete());
    }

    /// Every value the wire can carry is read, and one it cannot is refused
    /// rather than becoming a corner nobody asked for.
    #[test]
    fn an_anchor_the_protocol_does_not_have_is_refused() {
        for value in 0..=8u32 {
            assert!(Anchor::from_wire(value).is_some(), "{value}");
        }
        assert_eq!(Anchor::from_wire(9), None);
    }
}
