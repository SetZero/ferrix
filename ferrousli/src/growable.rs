//! A growable array of `Copy` values in memory from `malloc`, and an in-place
//! sort that tolerates any comparison function.
//!
//! The library has no Rust allocator, so `alloc::vec::Vec` is not available.
//! `dirent.h`'s `scandir`, `glob` and `regex.h` all collect an unknown number
//! of values, and this is the one place that does it.
//!
//! The sort is a heap sort. It needs no memory, takes O(n log n) comparisons,
//! and never panics however the comparison behaves, which matters because the
//! comparison is often a C function the program gave. `core`'s
//! `sort_unstable_by` may panic when the order is not total, and a panic here
//! traps.

use core::cmp::Ordering;
use core::mem::size_of;
use core::ptr::null_mut;

use crate::malloc::{free, realloc};

/// A growable array of `T` in memory from `malloc`. Growth that fails leaves
/// the array as it was.
#[derive(Debug)]
pub(crate) struct Growable<T: Copy> {
    /// The first element, or null while nothing was ever allocated.
    ptr: *mut T,
    /// How many elements are initialised.
    len: usize,
    /// How many elements the allocation holds.
    cap: usize,
}

impl<T: Copy> Growable<T> {
    /// An empty array, which owns no memory yet.
    pub(crate) const fn new() -> Self {
        Self {
            ptr: null_mut(),
            len: 0,
            cap: 0,
        }
    }

    /// How many elements it holds.
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    /// Makes room for at least `extra` more elements. Returns `false`, with
    /// `errno` set to `ENOMEM`, if the memory cannot be had.
    pub(crate) fn reserve(&mut self, extra: usize) -> bool {
        let Some(needed) = self.len.checked_add(extra) else {
            crate::errno::set(crate::errno::ENOMEM);
            return false;
        };
        if needed <= self.cap {
            return true;
        }
        let cap = needed.max(self.cap.saturating_mul(2)).max(4);
        let Some(bytes) = cap.checked_mul(size_of::<T>()) else {
            crate::errno::set(crate::errno::ENOMEM);
            return false;
        };
        // SAFETY: `ptr` is null or this array's own allocation, and no
        // reference into it outlives this call.
        let grown = unsafe { realloc(self.ptr.cast(), bytes.max(1)) }.cast::<T>();
        if grown.is_null() {
            return false;
        }
        self.ptr = grown;
        self.cap = cap;
        true
    }

    /// Appends `value`. Returns `false`, with `errno` set to `ENOMEM`, if the
    /// array could not grow.
    pub(crate) fn push(&mut self, value: T) -> bool {
        if !self.reserve(1) {
            return false;
        }
        // SAFETY: `reserve` made room for element `len`.
        unsafe { self.ptr.wrapping_add(self.len).write(value) };
        self.len += 1;
        true
    }

    /// The elements as a slice.
    pub(crate) fn as_slice(&self) -> &[T] {
        if self.ptr.is_null() {
            return &[];
        }
        // SAFETY: the first `len` elements are initialised, and the borrow of
        // `self` keeps the allocation alive and unaliased by a mutation.
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// The elements as a mutable slice.
    pub(crate) fn as_mut_slice(&mut self) -> &mut [T] {
        if self.ptr.is_null() {
            return &mut [];
        }
        // SAFETY: as in `as_slice`, with the borrow exclusive.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// Gives up the allocation, returning the first element and the length.
    /// The pointer is null if nothing was ever allocated, and otherwise came
    /// from `malloc`, for the caller to free.
    pub(crate) fn into_raw(self) -> (*mut T, usize) {
        let raw = (self.ptr, self.len);
        core::mem::forget(self);
        raw
    }
}

impl<T: Copy> Drop for Growable<T> {
    fn drop(&mut self) {
        // SAFETY: `ptr` is null or this array's own allocation, which nothing
        // else refers to once the array is dropped.
        unsafe { free(self.ptr.cast()) };
    }
}

/// Sorts `items` in place with a heap sort, so that `compare` finds each
/// element no greater than the next. A `compare` that is not a total order
/// leaves the elements in some order, never out of the slice.
pub(crate) fn sort_by<T: Copy>(items: &mut [T], mut compare: impl FnMut(T, T) -> Ordering) {
    let len = items.len();
    let mut start = len / 2;
    while start > 0 {
        start -= 1;
        sift_down(items, start, len, &mut compare);
    }
    let mut end = len;
    while end > 1 {
        end -= 1;
        items.swap(0, end);
        sift_down(items, 0, end, &mut compare);
    }
}

/// Restores the max-heap property below `root` in `items[..end]`.
fn sift_down<T: Copy>(
    items: &mut [T],
    mut root: usize,
    end: usize,
    compare: &mut impl FnMut(T, T) -> Ordering,
) {
    loop {
        let Some(left) = root.checked_mul(2).and_then(|n| n.checked_add(1)) else {
            return;
        };
        if left >= end {
            return;
        }
        let mut child = left;
        let right = left + 1;
        if right < end
            && let (Some(&l), Some(&r)) = (items.get(left), items.get(right))
            && compare(l, r) == Ordering::Less
        {
            child = right;
        }
        let (Some(&parent), Some(&larger)) = (items.get(root), items.get(child)) else {
            return;
        };
        if compare(parent, larger) != Ordering::Less {
            return;
        }
        items.swap(root, child);
        root = child;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_grow_the_array_and_keep_every_element() {
        let mut v = Growable::new();
        for i in 0..1000 {
            assert!(v.push(i));
        }
        assert_eq!(v.len(), 1000);
        assert!(v.as_slice().iter().copied().eq(0..1000));
        let (ptr, len) = v.into_raw();
        assert_eq!(len, 1000);
        // SAFETY: the array came from `malloc` and is not used again.
        unsafe { free(ptr.cast()) };
    }

    #[test]
    fn the_heap_sort_orders_every_permutation_of_six() {
        let mut items = [0, 1, 2, 3, 4, 5];
        // Heap's algorithm visits all 720 orders.
        let mut c = [0usize; 6];
        let check = |items: &[i32; 6]| {
            let mut copy = *items;
            sort_by(&mut copy, |a, b| a.cmp(&b));
            assert_eq!(copy, [0, 1, 2, 3, 4, 5]);
        };
        check(&items);
        let mut i = 0;
        while i < 6 {
            if c[i] < i {
                if i % 2 == 0 {
                    items.swap(0, i);
                } else {
                    items.swap(c[i], i);
                }
                check(&items);
                c[i] += 1;
                i = 0;
            } else {
                c[i] = 0;
                i += 1;
            }
        }
    }

    #[test]
    fn an_inconsistent_comparison_still_leaves_every_element() {
        let mut items = [5, 3, 9, 1, 7, 2, 8];
        let mut flip = false;
        sort_by(&mut items, |_, _| {
            flip = !flip;
            if flip {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        });
        let mut sorted = items;
        sort_by(&mut sorted, |a, b| a.cmp(&b));
        assert_eq!(sorted, [1, 2, 3, 5, 7, 8, 9]);
    }
}
