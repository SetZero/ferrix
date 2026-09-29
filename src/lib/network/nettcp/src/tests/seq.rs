//! Sequence arithmetic, including across the wrap.

use crate::seq::SeqNumber;

#[test]
fn a_number_precedes_the_ones_within_half_the_space_after_it() {
    let base = SeqNumber(1_000);
    assert!(base.precedes(base.advance(1)));
    assert!(base.precedes(base.advance(0x7FFF_FFFF)));
    assert!(!base.precedes(base));
    assert!(!base.precedes(base.advance(0x8000_0000)));
}

#[test]
fn the_order_survives_the_wrap() {
    let before = SeqNumber(0xFFFF_FF00);
    let after = SeqNumber(0x0000_0100);
    assert!(
        before.precedes(after),
        "{before:?} should precede {after:?}"
    );
    assert!(after.follows(before));
    assert_eq!(after.distance_from(before), 0x200);
}

#[test]
fn a_range_is_half_open_and_an_empty_one_holds_nothing() {
    let start = SeqNumber(10);
    let end = SeqNumber(20);
    assert!(start.is_within(start, end));
    assert!(SeqNumber(19).is_within(start, end));
    assert!(!end.is_within(start, end));
    assert!(!SeqNumber(9).is_within(start, end));
    assert!(!start.is_within(start, start));
}

#[test]
fn adding_wraps_rather_than_overflowing() {
    let near = SeqNumber(u32::MAX);
    assert_eq!(near.advance(1), SeqNumber(0));
    assert_eq!(near.advance(2), SeqNumber(1));
    assert_eq!((near + 5).distance_from(near), 5);
}

#[test]
fn min_and_max_follow_the_modular_order_not_the_integer_one() {
    let low = SeqNumber(0xFFFF_FFF0);
    let high = SeqNumber(0x0000_0010);
    assert_eq!(low.min(high), low);
    assert_eq!(low.max(high), high);
}
