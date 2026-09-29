//! Sets of byte ranges: free space, pinned space, what the free-space tree says.
//!
//! Every range set is kept in one canonical form — disjoint, sorted, and with
//! touching ranges merged — so two sets holding the same bytes compare equal,
//! and a set turned into free-space items gives exactly the items btrfs
//! itself would record: one per maximal run.

use alloc::collections::BTreeMap;

/// A set of disjoint, non-adjacent `[start, end)` byte ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeSet {
    /// Start to end, exclusive.
    map: BTreeMap<u64, u64>,
}

impl RangeSet {
    /// An empty set.
    #[must_use]
    pub const fn new() -> Self {
        RangeSet {
            map: BTreeMap::new(),
        }
    }

    /// Whether the set holds no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// How many maximal runs the set holds.
    #[must_use]
    pub fn runs(&self) -> usize {
        self.map.len()
    }

    /// Total bytes in the set.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.map
            .iter()
            .fold(0u64, |sum, (start, end)| sum.saturating_add(end - start))
    }

    /// The runs, as `(start, length)`, in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.map.iter().map(|(&start, &end)| (start, end - start))
    }

    /// Remove everything.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// The run containing `at`, as `(start, end)`.
    fn run_containing(&self, at: u64) -> Option<(u64, u64)> {
        let (&start, &end) = self.map.range(..=at).next_back()?;
        (at < end).then_some((start, end))
    }

    /// Whether any byte of `[start, start + len)` is in the set.
    #[must_use]
    pub fn overlaps(&self, start: u64, len: u64) -> bool {
        let Some(end) = start.checked_add(len) else {
            return true;
        };
        if len == 0 {
            return false;
        }
        if self.run_containing(start).is_some() {
            return true;
        }
        self.map.range(start..end).next().is_some()
    }

    /// Whether every byte of `[start, start + len)` is in the set.
    #[must_use]
    pub fn contains(&self, start: u64, len: u64) -> bool {
        let Some(end) = start.checked_add(len) else {
            return false;
        };
        len == 0
            || self
                .run_containing(start)
                .is_some_and(|(_, run_end)| end <= run_end)
    }

    /// Add `[start, start + len)`, merging with runs it touches. `false`, and
    /// no change, if any of it is already in the set: adding a byte twice is
    /// always a bookkeeping error somewhere, never something to absorb.
    pub fn insert(&mut self, start: u64, len: u64) -> bool {
        let Some(mut end) = start.checked_add(len) else {
            return false;
        };
        if len == 0 {
            return true;
        }
        if self.overlaps(start, len) {
            return false;
        }
        let mut first = start;
        if let Some((&before, &before_end)) = self.map.range(..start).next_back()
            && before_end == start
        {
            first = before;
            let _ = self.map.remove(&before);
        }
        if let Some(after_end) = self.map.remove(&end) {
            end = after_end;
        }
        let _ = self.map.insert(first, end);
        true
    }

    /// Take `[start, start + len)` out of the set, splitting the run holding
    /// it. `false`, and no change, unless all of it is in one run.
    pub fn remove(&mut self, start: u64, len: u64) -> bool {
        let Some(end) = start.checked_add(len) else {
            return false;
        };
        if len == 0 {
            return true;
        }
        let Some((run_start, run_end)) = self.run_containing(start) else {
            return false;
        };
        if end > run_end {
            return false;
        }
        let _ = self.map.remove(&run_start);
        if run_start < start {
            let _ = self.map.insert(run_start, start);
        }
        if end < run_end {
            let _ = self.map.insert(end, run_end);
        }
        true
    }

    /// Add every run of `other`, which must not overlap this set.
    pub fn absorb(&mut self, other: &RangeSet) -> bool {
        other
            .iter()
            .fold(true, |ok, (start, len)| self.insert(start, len) && ok)
    }

    /// The lowest address at or after `from` where `len` bytes aligned to
    /// `align` fit inside one run and, when `boundary` is non-zero, do not
    /// cross a multiple of it. Falls back to the lowest such address before
    /// `from` when there is none after, so a cursor can wrap.
    #[must_use]
    pub fn first_fit(&self, len: u64, align: u64, boundary: u64, from: u64) -> Option<u64> {
        let fits =
            |(&start, &end): (&u64, &u64)| fit_in(start.max(from), end, len, align, boundary);
        let after = self
            .run_containing(from)
            .and_then(|(start, end)| fit_in(start.max(from), end, len, align, boundary))
            .or_else(|| self.map.range(from..).find_map(fits));
        after.or_else(|| {
            self.map
                .iter()
                .find_map(|(&start, &end)| fit_in(start, end, len, align, boundary))
        })
    }

    /// The longest prefix, up to `want` bytes and at least `min`, of the
    /// first run at or after `from` (wrapping) that holds `min` aligned
    /// bytes. Returns `(start, len)`, with `len` a multiple of `align`.
    #[must_use]
    pub fn first_prefix(&self, want: u64, min: u64, align: u64, from: u64) -> Option<(u64, u64)> {
        let take = |start: u64, end: u64| -> Option<(u64, u64)> {
            let start = start.checked_next_multiple_of(align.max(1))?;
            let room = end.checked_sub(start)?;
            let room = room - room % align.max(1);
            (room >= min && room > 0).then_some((start, room.min(want)))
        };
        let after = self
            .run_containing(from)
            .and_then(|(_, end)| take(from, end))
            .or_else(|| self.map.range(from..).find_map(|(&s, &e)| take(s, e)));
        after.or_else(|| self.map.iter().find_map(|(&s, &e)| take(s, e)))
    }
}

/// The first aligned start in `[start, end)` where `len` bytes fit without
/// crossing a multiple of `boundary` (when non-zero).
fn fit_in(start: u64, end: u64, len: u64, align: u64, boundary: u64) -> Option<u64> {
    let mut at = start.checked_next_multiple_of(align.max(1))?;
    loop {
        let stop = at.checked_add(len)?;
        if stop > end {
            return None;
        }
        if boundary == 0 || at / boundary == (stop - 1) / boundary {
            return Some(at);
        }
        at = (at / boundary)
            .checked_add(1)?
            .checked_mul(boundary)?
            .checked_next_multiple_of(align.max(1))?;
    }
}

#[cfg(test)]
mod tests;
