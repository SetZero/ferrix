//! A session's lock goes on `authd`'s grant (`docs/AUTH.md` §3.7, P2.5):
//! the cases the certification consultant's G1, G3 and G8 name.
//!
//! Each drives [`lock_changed`], [`grant_arrived`] and [`unlock_waited`] as
//! the loop does, with [`Grants::for_test`] standing for the lock channel and
//! a clock the test moves.

use std::time::{Duration, Instant};

use compositor_seat::lock::FromCompositor;
use compositor_wire::ObjectId;

use super::{Lock, LockSeat, Slot, grant_arrived, lock_changed, lock_grace, unlock_waited};
use crate::grants::{Grants, LOCK_GRACE_CAP, UNLOCK_WAIT};

/// hyprlock's `ext_session_lock_v1`.
const HELD: ObjectId = ObjectId(10);

/// A second locker's.
const NEXT: ObjectId = ObjectId(20);

/// A session with two clients, the clock, and what was said.
struct Session {
    lock: Option<Lock>,
    slots: Vec<Slot>,
    grants: Grants,
    now: Instant,
    grace: Duration,
    said: Vec<String>,
}

impl Session {
    fn new() -> Session {
        Session {
            lock: None,
            slots: vec![Slot::for_test(), Slot::for_test()],
            grants: Grants::for_test(true),
            now: Instant::now(),
            grace: Duration::ZERO,
            said: Vec::new(),
        }
    }

    fn step(
        &mut self,
        index: usize,
        locking: Option<ObjectId>,
        unlocking: Option<ObjectId>,
    ) -> bool {
        let said = &mut self.said;
        lock_changed(
            &mut self.lock,
            &mut self.slots,
            index,
            &[],
            locking,
            &[],
            unlocking.map(|object| (object, true)),
            &mut LockSeat {
                grants: &mut self.grants,
                now: self.now,
                grace: self.grace,
            },
            &mut |line| said.push(line.to_owned()),
        )
    }

    fn lock(&mut self, index: usize, object: ObjectId) {
        let _ = self.step(index, Some(object), None);
    }

    fn unlock(&mut self, index: usize, object: ObjectId) {
        let _ = self.step(index, None, Some(object));
    }

    fn grant(&mut self, epoch: u64) {
        let said = &mut self.said;
        let _ = grant_arrived(&mut self.lock, epoch, &mut self.grants, &mut |line| {
            said.push(line.to_owned());
        });
    }

    fn later(&mut self, by: Duration) {
        self.now += by;
        let said = &mut self.said;
        let _ = unlock_waited(&mut self.lock, self.now, &mut |line| {
            said.push(line.to_owned());
        });
    }

    fn locked(&self) -> bool {
        self.lock.is_some()
    }

    fn epoch(&self) -> u64 {
        self.lock.as_ref().map_or(0, |held| held.epoch)
    }
}

#[test]
fn a_lock_is_numbered_and_sessiond_told() {
    let mut s = Session::new();
    s.lock(0, HELD);
    assert_eq!(s.epoch(), 1);
    assert_eq!(s.grants.outbox(), [FromCompositor::Locked(1)]);
}

#[test]
fn the_grant_before_the_unlock_lets_it_go() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.grant(1);
    assert!(s.locked(), "a grant alone unlocked");
    s.unlock(0, HELD);
    assert!(!s.locked());
    assert_eq!(
        s.grants.outbox(),
        [FromCompositor::Locked(1), FromCompositor::Unlocked(1)]
    );
}

#[test]
fn an_unlock_before_its_grant_waits_for_it() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.unlock(0, HELD);
    assert!(s.locked(), "an unlock with no grant went");
    s.later(UNLOCK_WAIT / 2);
    s.grant(1);
    assert!(!s.locked(), "the grant did not let the waiting unlock go");
}

#[test]
fn an_unlock_whose_grant_never_comes_is_refused() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.unlock(0, HELD);
    s.later(UNLOCK_WAIT);
    assert!(s.locked());
    assert!(s.lock.as_ref().is_some_and(|held| held.orphaned));
    // Its grant, late, is nothing now.
    s.grant(1);
    assert!(s.locked(), "a late grant unlocked a refused lock");
    assert!(
        s.said
            .iter()
            .any(|line| line.contains("no grant came for the unlock"))
    );
}

#[test]
fn a_grant_for_another_lock_is_nothing() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.grant(2);
    s.grant(0);
    s.unlock(0, HELD);
    assert!(s.locked());
}

#[test]
fn a_grant_is_used_once() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.grant(1);
    s.unlock(0, HELD);
    s.lock(0, NEXT);
    assert_eq!(s.epoch(), 2);
    s.unlock(0, NEXT);
    assert!(s.locked(), "the first lock's grant opened the second");
}

/// G3's stale grant: the holder dies, a new locker takes over, and the old
/// lock's grant opens nothing.
#[test]
fn a_takeover_is_a_new_lock_and_the_old_grant_is_nothing() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.grant(1);
    if let Some(held) = s.lock.as_mut() {
        held.renumber(&[None, Some(0)]);
    }
    let _ = s.slots.remove(0);
    s.lock(0, NEXT);
    assert_eq!(s.epoch(), 2, "the takeover was not a new lock");
    assert!(s.locked());
    assert!(
        s.lock
            .as_ref()
            .is_some_and(|held| held.held_by(0) && !held.told),
        "the new locker does not hold it afresh"
    );
    s.grant(1);
    s.unlock(0, NEXT);
    assert!(s.locked(), "the dead lock's grant opened the takeover");
    s.later(UNLOCK_WAIT);
    s.lock(0, ObjectId(30));
    s.grant(3);
    s.unlock(0, ObjectId(30));
    assert!(!s.locked(), "the takeover's own grant did not open it");
}

#[test]
fn a_living_holders_lock_is_not_taken_over() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.lock(1, NEXT);
    assert!(s.lock.as_ref().is_some_and(|held| held.held_by(0)));
    assert_eq!(s.grants.outbox(), [FromCompositor::Locked(1)]);
}

#[test]
fn no_lock_is_taken_while_no_grant_can_come() {
    let mut s = Session::new();
    s.grants = Grants::for_test(false);
    s.lock(0, HELD);
    assert!(!s.locked(), "a lock nothing could open was taken");
    assert_eq!(s.grants.outbox(), []);
}

#[test]
fn a_fresh_lock_is_let_go_within_its_grace() {
    let mut s = Session::new();
    s.grace = Duration::from_secs(5);
    s.lock(0, HELD);
    s.later(Duration::from_secs(4));
    s.unlock(0, HELD);
    assert!(!s.locked(), "the grace was not honoured");
}

#[test]
fn a_fresh_lock_past_its_grace_needs_its_grant() {
    let mut s = Session::new();
    s.grace = Duration::from_secs(5);
    s.lock(0, HELD);
    s.later(Duration::from_secs(5));
    s.unlock(0, HELD);
    assert!(s.locked(), "the grace outlived itself");
}

/// G1: killing the locker and taking over its lock must not buy a grace.
#[test]
fn a_takeover_has_no_grace() {
    let mut s = Session::new();
    s.grace = Duration::from_secs(5);
    s.lock(0, HELD);
    if let Some(held) = s.lock.as_mut() {
        held.orphan();
    }
    s.lock(1, NEXT);
    s.unlock(1, NEXT);
    assert!(s.locked(), "a takeover unlocked within the grace");
}

#[test]
fn the_grace_is_capped() {
    assert_eq!(lock_grace(None), Duration::ZERO);
    assert_eq!(lock_grace(Some(-5)), Duration::ZERO);
    assert_eq!(lock_grace(Some(3)), Duration::from_secs(3));
    assert_eq!(lock_grace(Some(3600)), LOCK_GRACE_CAP);
    assert_eq!(LOCK_GRACE_CAP, Duration::from_secs(10));
}

/// S2: hyprlock exits the moment it has asked to unlock. Its unlock still
/// waits, and the grant for its lock lets the session go.
#[test]
fn a_waiting_unlock_outlives_its_holder_and_its_grant_lets_it_go() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.unlock(0, HELD);
    if let Some(held) = s.lock.as_mut() {
        held.orphan();
        assert!(held.waiting.is_some() && !held.held_by(0));
    }
    s.grant(2);
    assert!(s.locked(), "another lock's grant completed the wait");
    s.grant(1);
    assert!(
        !s.locked(),
        "the right grant did not let the waiting unlock go"
    );
}

/// And with no grant, its wait still ends, and nothing is left waiting.
#[test]
fn a_waiting_unlock_whose_holder_went_is_refused_at_its_timeout() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.unlock(0, HELD);
    if let Some(held) = s.lock.as_mut() {
        held.orphan();
    }
    s.later(UNLOCK_WAIT);
    assert!(s.lock.as_ref().is_some_and(|held| held.waiting.is_none()));
    s.grant(1);
    assert!(s.locked(), "a grant after the wait ended unlocked");
}

/// A holder that dies with a grant but no unlock leaves no grant behind.
#[test]
fn a_holder_that_dies_granted_leaves_no_grant() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.grant(1);
    if let Some(held) = s.lock.as_mut() {
        held.orphan();
        assert!(!held.granted);
    }
    s.lock(1, NEXT);
    s.unlock(1, NEXT);
    assert!(s.locked());
}

/// A takeover replaces a waiting unlock: the dead holder's grant opens
/// nothing of the new lock's.
#[test]
fn a_takeover_replaces_a_waiting_unlock() {
    let mut s = Session::new();
    s.lock(0, HELD);
    s.unlock(0, HELD);
    if let Some(held) = s.lock.as_mut() {
        held.orphan();
    }
    s.lock(1, NEXT);
    assert!(
        s.lock
            .as_ref()
            .is_some_and(|held| held.waiting.is_none() && held.epoch == 2)
    );
    s.grant(1);
    assert!(s.locked(), "the dead holder's grant opened the takeover");
}

/// S1: a compositor that `sessiond` named a lock channel for, which could
/// not be taken, is a session all the same, and takes no lock.
#[test]
fn a_named_channel_that_could_not_be_taken_refuses_locks() {
    let mut s = Session::new();
    s.grants = Grants::new(true, None);
    assert!(s.grants.in_session() && !s.grants.on());
    s.lock(0, HELD);
    assert!(!s.locked(), "a lock nothing could ever grant was taken");
    let mut none = Session::new();
    none.grants = Grants::new(false, None);
    assert!(!none.grants.in_session());
}
