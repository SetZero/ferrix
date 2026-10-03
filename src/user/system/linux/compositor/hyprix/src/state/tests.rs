//! Who may unlock the session: only the client holding the lock, on the
//! lock it was given, and nobody once that client has gone.
//!
//! A lock whose program died leaves the screen locked (`docs/AUTH.md` §3.7).
//! The compositor holds clients by their place in its list, and the place a
//! dead client held is soon another client's; these are the cases where that
//! other client must not be taken for the holder.

use compositor_wire::ObjectId;

use super::{Lock, Slot, lock_changed};

/// The `ext_session_lock_v1` the holder locked with.
const HELD: ObjectId = ObjectId(10);

/// One a second client asks with.
const OTHER: ObjectId = ObjectId(20);

/// What a step of the loop does with one client's lock requests: `locking`
/// a new lock, `unlocking` one, from the client at `index`.
fn step(
    lock: &mut Option<Lock>,
    slots: &mut [Slot],
    index: usize,
    locking: Option<ObjectId>,
    covered: &[(ObjectId, ObjectId, usize)],
    unlocking: Option<(ObjectId, bool)>,
) -> Vec<String> {
    let mut said = Vec::new();
    let _ = lock_changed(
        lock,
        slots,
        index,
        &[],
        locking,
        covered,
        unlocking,
        &mut |line| said.push(line.to_owned()),
    );
    said
}

/// Two clients, the first holding the lock on [`HELD`].
fn held() -> (Option<Lock>, Vec<Slot>) {
    let mut slots = vec![Slot::for_test(), Slot::for_test()];
    let mut lock = None;
    let said = step(&mut lock, &mut slots, 0, Some(HELD), &[], None);
    assert_eq!(said, ["hyprix: the session is locked"]);
    (lock, slots)
}

/// A holder that only moved, because a client before it went, still holds
/// the lock at its new place.
#[test]
fn a_holder_that_moved_keeps_the_lock() {
    let mut slots = vec![Slot::for_test(), Slot::for_test()];
    let mut lock = None;
    let _ = step(&mut lock, &mut slots, 1, Some(HELD), &[], None);
    if let Some(held) = lock.as_mut() {
        held.renumber(&[None, Some(0)]);
        assert!(held.held_by(0), "the holder lost the lock by moving");
    }
    let _ = slots.remove(0);
    let _ = step(&mut lock, &mut slots, 0, None, &[], Some((HELD, true)));
    assert!(
        lock.is_none(),
        "the holder could not unlock at its new place"
    );
}

#[test]
fn the_holder_unlocks_with_its_own_lock() {
    let (mut lock, mut slots) = held();
    let said = step(&mut lock, &mut slots, 0, None, &[], Some((HELD, true)));
    assert!(lock.is_none(), "the holder's own unlock was refused");
    assert_eq!(said, ["hyprix: the session is unlocked"]);
}

#[test]
fn another_client_cannot_unlock() {
    let (mut lock, mut slots) = held();
    // Refused its own lock, then unlocking it as if it held the session's.
    let said = step(&mut lock, &mut slots, 1, Some(OTHER), &[], None);
    assert_eq!(
        said,
        ["hyprix: a second program asked to lock the session and was refused"]
    );
    let _ = step(&mut lock, &mut slots, 1, None, &[], Some((OTHER, true)));
    let _ = step(&mut lock, &mut slots, 1, None, &[], Some((HELD, true)));
    assert!(
        lock.is_some(),
        "a client that does not hold the lock unlocked it"
    );
}

#[test]
fn the_holder_cannot_unlock_with_a_lock_it_was_refused() {
    let (mut lock, mut slots) = held();
    let _ = step(&mut lock, &mut slots, 0, Some(OTHER), &[], None);
    let _ = step(&mut lock, &mut slots, 0, None, &[], Some((OTHER, true)));
    assert!(
        lock.is_some(),
        "a refused lock's unlock let the session's go"
    );
}

/// The case that was open: the holder dies, and whoever comes to sit at its
/// place in the list -- the next client, moved down by one -- asks for a
/// lock, is refused, and unlocks.
#[test]
fn nobody_at_a_dead_holders_place_can_unlock() {
    let (mut lock, mut slots) = held();
    // The holder, at 0, goes; the client after it moves down to 0. Only the
    // renumbering, as the loop does it once the gone are taken out, without
    // the loop's own note that the holder went.
    if let Some(held) = lock.as_mut() {
        assert!(held.held_by(0));
        held.renumber(&[None, Some(0)]);
        assert!(
            !held.held_by(0),
            "a dead holder's place still holds the lock"
        );
    }
    let _ = slots.remove(0);
    let _ = step(&mut lock, &mut slots, 0, Some(OTHER), &[], None);
    let _ = step(&mut lock, &mut slots, 0, None, &[], Some((OTHER, true)));
    // Even with the very object id the dead holder used.
    let _ = step(&mut lock, &mut slots, 0, None, &[], Some((HELD, true)));
    assert!(
        lock.is_some(),
        "a client at the dead holder's place unlocked"
    );
    // Nor may it put its own surfaces up as the lock's.
    let _ = step(
        &mut lock,
        &mut slots,
        0,
        None,
        &[(ObjectId(30), ObjectId(31), 0)],
        None,
    );
    assert!(
        lock.as_ref().is_some_and(|held| held.surfaces.is_empty()),
        "a client at the dead holder's place covered the screen"
    );
}
