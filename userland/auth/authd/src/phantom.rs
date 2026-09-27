//! Tallies for names that are not accounts (`docs/AUTH.md` §3.5).
//!
//! A real account is throttled from its fourth failure in a row. If a name
//! that is no account never were, the fourth answer would say which names
//! exist: "wait" for one, "Authentication failed" for the other. So an
//! unknown name is counted too, the same way, here. It is kept in memory
//! only, so naming accounts never makes a file, and in a bounded table, so
//! naming a million of them costs a fixed amount of memory. It is keyed by
//! a keyed hash of the name, not by the name, so the table does not hold a
//! list of what was tried.
//!
//! What this cannot hide: a restart of `authd` forgets these tallies, and
//! not a real account's. A guesser who can restart `authd` is root.

use std::collections::VecDeque;

use ferrix_argon2::blake2b::Blake2b;

use crate::store::Tally;

/// The most names remembered; the oldest is forgotten first.
const CAPACITY: usize = 1024;

/// Bytes of the keyed hash a name is remembered by.
const TAG: usize = 16;

/// The table.
#[derive(Debug)]
pub(crate) struct Phantoms {
    key: [u8; 32],
    /// Least recently used first.
    entries: VecDeque<([u8; TAG], Tally)>,
}

impl Phantoms {
    /// An empty table under `key`, which the caller draws at random.
    pub(crate) fn new(key: [u8; 32]) -> Phantoms {
        Phantoms {
            key,
            entries: VecDeque::new(),
        }
    }

    fn tag(&self, name: &str) -> [u8; TAG] {
        let mut hasher = Blake2b::new(TAG);
        hasher.update(&self.key);
        hasher.update(name.as_bytes());
        let mut tag = [0_u8; TAG];
        hasher.finalize(&mut tag);
        tag
    }

    /// `name`'s tally; none when it has not failed, or was forgotten.
    pub(crate) fn tally(&self, name: &str) -> Tally {
        let tag = self.tag(name);
        self.entries
            .iter()
            .find(|(held, _)| *held == tag)
            .map(|(_, tally)| *tally)
            .unwrap_or_default()
    }

    /// Replace `name`'s tally, making it the most recently used.
    pub(crate) fn set(&mut self, name: &str, tally: Tally) {
        let tag = self.tag(name);
        self.entries.retain(|(held, _)| *held != tag);
        if self.entries.len() >= CAPACITY {
            let _ = self.entries.pop_front();
        }
        self.entries.push_back((tag, tally));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_counted_apart_and_the_oldest_is_forgotten() {
        let mut table = Phantoms::new([7; 32]);
        let one = Tally {
            failures: 1,
            not_before_ms: 10,
        };
        table.set("alice", one);
        assert_eq!(table.tally("alice"), one);
        assert_eq!(table.tally("bob"), Tally::default());
        for i in 0..CAPACITY {
            table.set(&format!("n{i}"), one);
        }
        assert_eq!(table.entries.len(), CAPACITY);
        assert_eq!(
            table.tally("alice"),
            Tally::default(),
            "the oldest went first"
        );
        // The table holds tags, not names.
        assert!(!format!("{:?}", table.entries).contains("n1"));
    }
}
