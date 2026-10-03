//! What `sessiond` passes between the compositor and `authd`, and what it
//! refuses to (`docs/AUTH.md` §3.7, P2.5): the rules, with no descriptor in
//! sight, so a test drives them line by line.
//!
//! * The compositor numbers its locks, and the numbers only grow. A lock
//!   that does not is a compositor that is not behaving, and ends the
//!   session.
//! * A lock is armed at `authd` for the session's uid and that number, one
//!   at a time; the one before it is disarmed.
//! * A grant goes to the compositor only for the session's uid and the
//!   number armed now, once.
//! * Anything the compositor says that is not `locked` or `unlocked` ends
//!   the session: nothing grant-shaped is ever taken from its side.

use compositor_seat::lock::{FromCompositor, ToCompositor};

/// What a step asks of `sessiond`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Act {
    /// ARM at `authd`.
    Arm {
        /// The session's uid.
        uid: u32,
        /// The lock.
        epoch: u64,
    },
    /// DISARM at `authd`.
    Disarm(u64),
    /// A line to the compositor.
    Tell(ToCompositor),
    /// End the session, saying why.
    End(String),
}

/// The relay's state for one session.
#[derive(Debug)]
pub(crate) struct Relay {
    /// The session's user.
    uid: u32,
    /// The highest lock number the compositor has said.
    last: Option<u64>,
    /// The lock armed at `authd`, or to arm once its channel is up.
    armed: Option<u64>,
    /// Whether `authd`'s seat channel is up: grants can come.
    ready: bool,
}

impl Relay {
    /// A session of `uid`'s, with no lock and no seat channel yet.
    pub(crate) const fn new(uid: u32) -> Relay {
        Relay {
            uid,
            last: None,
            armed: None,
            ready: false,
        }
    }

    /// A line from the compositor, without its newline.
    pub(crate) fn compositor_said(&mut self, line: &str) -> Vec<Act> {
        match FromCompositor::parse(line) {
            Some(FromCompositor::Locked(epoch)) => {
                if self.last.is_some_and(|last| epoch <= last) {
                    return vec![Act::End(format!(
                        "the compositor numbered a lock {epoch}, not past {}",
                        self.last.unwrap_or_default()
                    ))];
                }
                self.last = Some(epoch);
                let mut acts: Vec<Act> = self.armed.take().map(Act::Disarm).into_iter().collect();
                self.armed = Some(epoch);
                if self.ready {
                    acts.push(Act::Arm {
                        uid: self.uid,
                        epoch,
                    });
                }
                acts
            }
            Some(FromCompositor::Unlocked(epoch)) => {
                if self.armed == Some(epoch) {
                    self.armed = None;
                    vec![Act::Disarm(epoch)]
                } else {
                    Vec::new()
                }
            }
            None => vec![Act::End(format!(
                "the compositor said `{}` on the lock channel",
                line.escape_debug()
            ))],
        }
    }

    /// `authd`'s seat channel came up: grants can come, and a lock already
    /// up is armed now.
    pub(crate) fn seat_ready(&mut self) -> Vec<Act> {
        self.ready = true;
        let mut acts = vec![Act::Tell(ToCompositor::Grants(true))];
        if let Some(epoch) = self.armed {
            acts.push(Act::Arm {
                uid: self.uid,
                epoch,
            });
        }
        acts
    }

    /// It went: nothing armed there survives, and the compositor takes no
    /// new lock until it is back.
    pub(crate) fn seat_gone(&mut self) -> Vec<Act> {
        let was = self.ready;
        self.ready = false;
        if was {
            vec![Act::Tell(ToCompositor::Grants(false))]
        } else {
            Vec::new()
        }
    }

    /// A GRANT from `authd`: passed on only for this session's uid and the
    /// lock armed now, once.
    pub(crate) fn grant(&mut self, uid: u32, epoch: u64) -> Vec<Act> {
        if uid != self.uid || self.armed != Some(epoch) {
            return Vec::new();
        }
        self.armed = None;
        vec![Act::Tell(ToCompositor::Grant(epoch))]
    }

    /// The compositor exited: a lock it held is armed for nobody.
    pub(crate) fn compositor_gone(&mut self) -> Vec<Act> {
        self.armed
            .take()
            .filter(|_| self.ready)
            .map(Act::Disarm)
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FERRIX: u32 = 1000;

    fn ready() -> Relay {
        let mut relay = Relay::new(FERRIX);
        assert_eq!(relay.seat_ready(), [Act::Tell(ToCompositor::Grants(true))]);
        relay
    }

    #[test]
    fn a_lock_is_armed_and_its_grant_passed_once() {
        let mut relay = ready();
        assert_eq!(
            relay.compositor_said("locked 1"),
            [Act::Arm {
                uid: FERRIX,
                epoch: 1
            }]
        );
        assert_eq!(relay.grant(FERRIX, 1), [Act::Tell(ToCompositor::Grant(1))]);
        assert_eq!(relay.grant(FERRIX, 1), [], "a grant passed twice");
        assert_eq!(relay.compositor_said("unlocked 1"), []);
    }

    #[test]
    fn a_grant_for_another_uid_or_lock_is_dropped() {
        let mut relay = ready();
        let _ = relay.compositor_said("locked 3");
        assert_eq!(relay.grant(1001, 3), []);
        assert_eq!(relay.grant(FERRIX, 2), []);
        assert_eq!(relay.grant(FERRIX, 4), []);
        assert_eq!(relay.grant(FERRIX, 3), [Act::Tell(ToCompositor::Grant(3))]);
    }

    #[test]
    fn a_new_lock_disarms_the_one_before_and_the_old_grant_is_dropped() {
        let mut relay = ready();
        let _ = relay.compositor_said("locked 1");
        assert_eq!(
            relay.compositor_said("locked 2"),
            [
                Act::Disarm(1),
                Act::Arm {
                    uid: FERRIX,
                    epoch: 2
                }
            ]
        );
        assert_eq!(relay.grant(FERRIX, 1), []);
    }

    #[test]
    fn lock_numbers_that_do_not_grow_end_the_session() {
        let mut relay = ready();
        let _ = relay.compositor_said("locked 5");
        for line in ["locked 5", "locked 4"] {
            assert!(
                matches!(relay.compositor_said(line).as_slice(), [Act::End(_)]),
                "{line}"
            );
        }
    }

    #[test]
    fn anything_but_locked_and_unlocked_ends_the_session() {
        for line in [
            "grant 1",
            "grants on",
            "open 0 /dev/dri/card0",
            "locked x",
            "",
        ] {
            let mut relay = ready();
            assert!(
                matches!(relay.compositor_said(line).as_slice(), [Act::End(_)]),
                "{line:?}"
            );
        }
    }

    #[test]
    fn unlocked_disarms_its_own_lock_only() {
        let mut relay = ready();
        let _ = relay.compositor_said("locked 2");
        assert_eq!(relay.compositor_said("unlocked 1"), []);
        assert_eq!(relay.compositor_said("unlocked 2"), [Act::Disarm(2)]);
        assert_eq!(relay.grant(FERRIX, 2), [], "a grant after the unlock");
    }

    #[test]
    fn a_lock_taken_before_the_seat_is_up_is_armed_when_it_comes() {
        let mut relay = Relay::new(FERRIX);
        assert_eq!(relay.compositor_said("locked 1"), []);
        assert_eq!(
            relay.seat_ready(),
            [
                Act::Tell(ToCompositor::Grants(true)),
                Act::Arm {
                    uid: FERRIX,
                    epoch: 1
                }
            ]
        );
    }

    #[test]
    fn the_seat_going_says_so_and_the_compositor_going_disarms() {
        let mut relay = ready();
        let _ = relay.compositor_said("locked 1");
        assert_eq!(relay.compositor_gone(), [Act::Disarm(1)]);
        assert_eq!(relay.grant(FERRIX, 1), []);
        assert_eq!(relay.seat_gone(), [Act::Tell(ToCompositor::Grants(false))]);
        assert_eq!(relay.seat_gone(), [], "said once");
    }
}
