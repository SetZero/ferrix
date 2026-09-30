//! `stdlib.h`: `qsort`, `qsort_r` and `bsearch`.
//!
//! The sort is smoothsort, adapted from musl's `src/stdlib/qsort.c` (MIT,
//! copyright 2011 Valentin Ochs). It sorts in place with no allocation and no
//! recursion, takes O(n log n) comparisons in the worst case and close to O(n)
//! on nearly sorted input, and moves elements of any size: a rotation copies
//! through a 256-byte buffer in pieces. It is not stable, and C does not ask
//! it to be.
//!
//! The array is kept as a forest of heaps whose sizes are Leonardo numbers.
//! A bit vector `p` records which sizes are present, shifted so that its low bit
//! is the smallest tree, of order `pshift`. musl keeps `p` in two words; here
//! it is a `u128`, which is two words on a 64-bit machine and more than
//! enough on a 32-bit one.

use core::ffi::{c_int, c_void};
use core::ptr::{copy_nonoverlapping, null_mut};

/// A comparator as `qsort` takes it.
type Compare = unsafe extern "C" fn(*const c_void, *const c_void) -> c_int;
/// A comparator as `qsort_r` takes it: the third argument is the context.
type CompareWith = unsafe extern "C" fn(*const c_void, *const c_void, *mut c_void) -> c_int;

/// Leonardo numbers kept: the 96th exceeds any 64-bit array size.
const ORDERS: usize = 12 * size_of::<usize>();
/// Elements a rotation can involve: one per tree level, plus the buffer.
const ROTATION: usize = 14 * size_of::<usize>() + 1;
/// The size of the piece a rotation copies at once.
const PIECE: usize = 256;

/// One sort in progress.
struct Sorter<F> {
    width: usize,
    /// Leonardo numbers scaled by the element width: the byte size of a tree
    /// of each order.
    sizes: [usize; ORDERS],
    compare: F,
}

impl<F> core::fmt::Debug for Sorter<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sorter")
            .field("width", &self.width)
            .finish()
    }
}

/// The number of trailing zeros of `p - 1`: how far to shift `p` to bring its
/// next tree to the low bit. `p` is odd; for `p == 1` it is 0, as in musl.
const fn next_tree(p: u128) -> u32 {
    if p <= 1 { 0 } else { (p - 1).trailing_zeros() }
}

impl<F: FnMut(*const u8, *const u8) -> c_int> Sorter<F> {
    /// The byte size of a tree of `order`.
    fn size(&self, order: usize) -> usize {
        self.sizes.get(order).copied().unwrap_or(usize::MAX)
    }

    /// Compares the elements at `a` and `b`.
    fn compare(&mut self, a: *mut u8, b: *mut u8) -> c_int {
        (self.compare)(a.cast_const(), b.cast_const())
    }

    /// Moves the element at `chain[0]` to `chain[n - 1]`, each other element
    /// one place toward the front: `chain[i]` takes `chain[i + 1]`'s value.
    ///
    /// # Safety
    ///
    /// The first `n` pointers must name distinct elements of the array.
    unsafe fn rotate(&self, chain: &mut [*mut u8; ROTATION], n: usize) {
        if n < 2 {
            return;
        }
        let mut buffer = [0_u8; PIECE];
        let Some(slot) = chain.get_mut(n) else {
            return;
        };
        *slot = buffer.as_mut_ptr();
        let mut remaining = self.width;
        while remaining > 0 {
            let piece = remaining.min(PIECE);
            let (Some(&first), Some(&spare)) = (chain.first(), chain.get(n)) else {
                return;
            };
            // SAFETY: the first element has `piece` more bytes, and the buffer
            // is a separate `PIECE` bytes.
            unsafe { copy_nonoverlapping(first, spare, piece) };
            let mut i = 0;
            while i < n {
                let (Some(&to), Some(&from)) = (chain.get(i), chain.get(i + 1)) else {
                    return;
                };
                // SAFETY: both are distinct elements, or the buffer, each with
                // `piece` bytes left.
                unsafe { copy_nonoverlapping(from, to, piece) };
                if let Some(to) = chain.get_mut(i) {
                    *to = to.wrapping_add(piece);
                }
                i += 1;
            }
            remaining -= piece;
        }
    }

    /// Restores the heap order of the tree of `order` whose root is `head`, by
    /// sifting the root down.
    ///
    /// # Safety
    ///
    /// The tree must lie inside the array.
    unsafe fn sift(&mut self, mut head: *mut u8, mut order: usize) {
        let mut chain = [null_mut(); ROTATION];
        let mut n = 1;
        if let Some(root) = chain.first_mut() {
            *root = head;
        }
        while order > 1 {
            let right = head.wrapping_sub(self.width);
            let left = right.wrapping_sub(self.size(order - 2));
            let root = chain.first().copied().unwrap_or(head);
            if self.compare(root, left) >= 0 && self.compare(root, right) >= 0 {
                break;
            }
            if self.compare(left, right) >= 0 {
                head = left;
                order -= 1;
            } else {
                head = right;
                order -= 2;
            }
            if let Some(slot) = chain.get_mut(n) {
                *slot = head;
                n += 1;
            }
        }
        // SAFETY: the chain holds a root and descendants, all distinct.
        unsafe { self.rotate(&mut chain, n) };
    }

    /// Moves the root at `head` left among the roots of the trees `p` records
    /// until the roots are in order, then sifts it into its new tree.
    /// `trusted` says the tree at `head` is already a heap.
    ///
    /// # Safety
    ///
    /// `p` and `order` must describe the trees ending at `head`.
    unsafe fn trinkle(
        &mut self,
        mut head: *mut u8,
        mut p: u128,
        mut order: usize,
        mut trusted: bool,
    ) {
        let mut chain = [null_mut(); ROTATION];
        let mut n = 1;
        if let Some(root) = chain.first_mut() {
            *root = head;
        }
        while p != 1 {
            let stepson = head.wrapping_sub(self.size(order));
            let root = chain.first().copied().unwrap_or(head);
            if self.compare(stepson, root) <= 0 {
                break;
            }
            if !trusted && order > 1 {
                let right = head.wrapping_sub(self.width);
                let left = right.wrapping_sub(self.size(order - 2));
                if self.compare(right, stepson) >= 0 || self.compare(left, stepson) >= 0 {
                    break;
                }
            }
            if let Some(slot) = chain.get_mut(n) {
                *slot = stepson;
                n += 1;
            }
            head = stepson;
            let trail = next_tree(p);
            p >>= trail;
            order += trail as usize;
            trusted = false;
        }
        if !trusted {
            // SAFETY: the chain holds distinct roots.
            unsafe { self.rotate(&mut chain, n) };
            // SAFETY: `head` is the root of a tree of `order` in the array.
            unsafe { self.sift(head, order) };
        }
    }

    /// Sorts `count` elements at `base`.
    ///
    /// # Safety
    ///
    /// `base` must hold `count` elements of `self.width` bytes, and `count`
    /// times the width must not overflow.
    unsafe fn sort(&mut self, base: *mut u8, count: usize) {
        let Some(total) = self.width.checked_mul(count).filter(|&total| total > 0) else {
            return;
        };
        let mut order = 2;
        let [first, second, ..] = &mut self.sizes;
        *first = self.width;
        *second = self.width;
        while order < ORDERS {
            let size = self
                .size(order - 2)
                .saturating_add(self.size(order - 1))
                .saturating_add(self.width);
            if let Some(slot) = self.sizes.get_mut(order) {
                *slot = size;
            }
            if size >= total {
                break;
            }
            order += 1;
        }

        let mut head = base;
        let high = base.wrapping_add(total - self.width);
        let mut p: u128 = 1;
        let mut order = 1_usize;
        while head < high {
            if p & 3 == 3 {
                // SAFETY: the two trees before `head` and `head` make a tree.
                unsafe { self.sift(head, order) };
                p >>= 2;
                order += 2;
            } else {
                // Addresses within one array: `head < high`.
                let left = high as usize - head as usize;
                if self.size(order - 1) >= left {
                    // SAFETY: `p` and `order` describe the trees ending here.
                    unsafe { self.trinkle(head, p, order, false) };
                } else {
                    // SAFETY: the tree of `order` ending at `head` is in the
                    // array.
                    unsafe { self.sift(head, order) };
                }
                if order == 1 {
                    p <<= 1;
                    order = 0;
                } else {
                    p <<= order - 1;
                    order = 1;
                }
            }
            p |= 1;
            head = head.wrapping_add(self.width);
        }
        // SAFETY: as above.
        unsafe { self.trinkle(head, p, order, false) };

        while order != 1 || p != 1 {
            if order <= 1 {
                let trail = next_tree(p);
                p >>= trail;
                order += trail as usize;
            } else {
                p <<= 2;
                order -= 2;
                p ^= 7;
                p >>= 1;
                let left_root = head.wrapping_sub(self.size(order)).wrapping_sub(self.width);
                // SAFETY: splitting the tree at `head` leaves two heaps, whose
                // roots these are.
                unsafe { self.trinkle(left_root, p, order + 1, true) };
                p <<= 1;
                p |= 1;
                // SAFETY: as above.
                unsafe { self.trinkle(head.wrapping_sub(self.width), p, order, true) };
            }
            head = head.wrapping_sub(self.width);
        }
    }
}

/// Sorts `count` elements of `width` bytes at `base` with `compare`.
///
/// # Safety
///
/// `base` must hold `count * width` bytes, and `compare` must be safe to call
/// with pointers to any two of its elements.
unsafe fn sort_with(
    base: *mut c_void,
    count: usize,
    width: usize,
    compare: impl FnMut(*const u8, *const u8) -> c_int,
) {
    let mut sorter = Sorter {
        width,
        sizes: [0; ORDERS],
        compare,
    };
    // SAFETY: the caller vouches for the array.
    unsafe { sorter.sort(base.cast(), count) };
}

/// Sorts `count` elements of `width` bytes at `base` into the order `compare`
/// defines.
///
/// # Safety
///
/// `base` must hold `count * width` bytes, and `compare` must be safe to call
/// with pointers to any two elements.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn qsort(
    base: *mut c_void,
    count: usize,
    width: usize,
    compare: Option<Compare>,
) {
    let Some(compare) = compare else {
        return;
    };
    let order = |a: *const u8, b: *const u8| {
        // SAFETY: `sort_with` passes pointers to elements of the array.
        unsafe { compare(a.cast(), b.cast()) }
    };
    // SAFETY: the caller vouches for the array and the comparator.
    unsafe { sort_with(base, count, width, order) };
}

/// `qsort`, with a context pointer passed to `compare` as its third argument:
/// glibc's order, which musl's header declares too.
///
/// # Safety
///
/// As [`qsort`], and `compare` must accept `context`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn qsort_r(
    base: *mut c_void,
    count: usize,
    width: usize,
    compare: Option<CompareWith>,
    context: *mut c_void,
) {
    let Some(compare) = compare else {
        return;
    };
    let order = |a: *const u8, b: *const u8| {
        // SAFETY: `sort_with` passes pointers to elements of the array, and the
        // caller vouches that `compare` accepts `context`.
        unsafe { compare(a.cast(), b.cast(), context) }
    };
    // SAFETY: the caller vouches for the array and the comparator.
    unsafe { sort_with(base, count, width, order) };
}

/// Finds an element equal to `*key` in `count` sorted elements of `width`
/// bytes at `base`, or returns null. `compare` gets the key first.
///
/// # Safety
///
/// `base` must hold `count * width` bytes sorted by `compare`, and `compare`
/// must be safe to call with `key` and any element.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn bsearch(
    key: *const c_void,
    base: *const c_void,
    count: usize,
    width: usize,
    compare: Option<Compare>,
) -> *mut c_void {
    let Some(compare) = compare else {
        return null_mut();
    };
    let mut base = base.cast::<u8>();
    let mut count = count;
    while count > 0 {
        let middle = base.wrapping_add(width.wrapping_mul(count / 2));
        // SAFETY: `middle` is an element of the array.
        let sign = unsafe { compare(key, middle.cast()) };
        if sign < 0 {
            count /= 2;
        } else if sign > 0 {
            base = middle.wrapping_add(width);
            count -= count / 2 + 1;
        } else {
            return middle.cast_mut().cast();
        }
    }
    null_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator for test data.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (self.0 >> 33) as u32
        }
    }

    /// Sorts `values` as elements `width` bytes wide, each holding its value in
    /// the first four bytes and a tag after, and checks the result is sorted
    /// and a permutation.
    fn check(values: &[u32], width: usize) {
        assert!(width >= 4);
        let mut bytes = vec![0_u8; values.len() * width];
        for (k, (element, value)) in bytes.chunks_mut(width).zip(values).enumerate() {
            element[..4].copy_from_slice(&value.to_be_bytes());
            for (t, byte) in element[4..].iter_mut().enumerate() {
                *byte = (k + t) as u8;
            }
        }
        let mut expected: Vec<Vec<u8>> = bytes.chunks(width).map(<[u8]>::to_vec).collect();
        let mut comparisons = 0_usize;
        let order = |a: *const u8, b: *const u8| {
            comparisons += 1;
            // SAFETY: each element has at least four bytes.
            let x = unsafe { a.cast::<[u8; 4]>().read() };
            // SAFETY: as above.
            let y = unsafe { b.cast::<[u8; 4]>().read() };
            c_int::from(x > y) - c_int::from(x < y)
        };
        // SAFETY: `bytes` holds the elements, and the comparator reads four
        // bytes of each.
        unsafe { sort_with(bytes.as_mut_ptr().cast(), values.len(), width, order) };
        let mut got: Vec<Vec<u8>> = bytes.chunks(width).map(<[u8]>::to_vec).collect();
        assert!(
            got.windows(2).all(|w| w[0][..4] <= w[1][..4]),
            "not sorted: {values:?}"
        );
        got.sort();
        expected.sort();
        assert_eq!(got, expected, "not a permutation");
        let n = values.len().max(2) as f64;
        assert!(
            comparisons as f64 <= 3.0 * n * n.log2() + 10.0,
            "{comparisons} comparisons for {} elements",
            values.len()
        );
    }

    #[test]
    fn sorts_every_order_at_many_sizes() {
        let mut rng = Lcg(7);
        for n in (0..70).chain([127, 128, 255, 256, 1000, 1023, 1024, 4097]) {
            let random: Vec<u32> = (0..n).map(|_| rng.next()).collect();
            let few: Vec<u32> = (0..n).map(|_| rng.next() % 4).collect();
            let sorted: Vec<u32> = (0..n).collect();
            let reversed: Vec<u32> = (0..n).rev().collect();
            let equal = vec![5; n as usize];
            let saw: Vec<u32> = (0..n).map(|k| k % 17).collect();
            for values in [&random, &few, &sorted, &reversed, &equal, &saw] {
                check(values, 4);
            }
        }
    }

    #[test]
    fn sorted_input_takes_linear_comparisons() {
        for n in [100_usize, 1000, 10_000] {
            let mut data: Vec<u32> = (0..n as u32).collect();
            let mut comparisons = 0_usize;
            let order = |a: *const u8, b: *const u8| {
                comparisons += 1;
                // SAFETY: each element is a `u32`.
                let x = unsafe { a.cast::<u32>().read_unaligned() };
                // SAFETY: as above.
                let y = unsafe { b.cast::<u32>().read_unaligned() };
                c_int::from(x > y) - c_int::from(x < y)
            };
            // SAFETY: `data` holds `n` four-byte elements.
            unsafe { sort_with(data.as_mut_ptr().cast(), n, 4, order) };
            assert!(
                comparisons < 4 * n,
                "{comparisons} comparisons for {n} sorted elements"
            );
        }
    }

    #[test]
    fn sorts_elements_of_any_width() {
        let mut rng = Lcg(11);
        for width in [5, 7, 16, 255, 256, 257, 600, 1000] {
            for n in [0, 1, 2, 3, 10, 57] {
                let values: Vec<u32> = (0..n).map(|_| rng.next() % 50).collect();
                check(&values, width);
            }
        }
    }

    #[test]
    fn adversarial_comparators_stay_in_bounds() {
        let mut data: Vec<u32> = (0..500).collect();
        let mut rng = Lcg(3);
        // SAFETY: the comparator ignores its arguments.
        unsafe {
            sort_with(data.as_mut_ptr().cast(), data.len(), 4, |_, _| {
                (rng.next() % 3) as c_int - 1
            });
        }
        data.sort_unstable();
        assert_eq!(data, (0..500).collect::<Vec<_>>());
    }

    unsafe extern "C" fn compare_u32(a: *const c_void, b: *const c_void) -> c_int {
        // SAFETY: the test passes pointers to `u32`s.
        let x = unsafe { a.cast::<u32>().read_unaligned() };
        // SAFETY: as above.
        let y = unsafe { b.cast::<u32>().read_unaligned() };
        c_int::from(x > y) - c_int::from(x < y)
    }

    #[test]
    fn bsearch_finds_present_keys_only() {
        let data: Vec<u32> = (0..100).map(|k| k * 2).collect();
        for key in 0..=200_u32 {
            // SAFETY: `data` is sorted and the comparator reads `u32`s.
            let found = unsafe {
                bsearch(
                    (&raw const key).cast(),
                    data.as_ptr().cast(),
                    data.len(),
                    4,
                    Some(compare_u32),
                )
            };
            if key % 2 == 0 && key < 200 {
                assert_eq!(found.cast_const(), data[key as usize / 2..].as_ptr().cast());
            } else {
                assert!(found.is_null());
            }
        }
        let key = 0_u32;
        // SAFETY: an empty array, and a `u32` key.
        let found = unsafe {
            bsearch(
                (&raw const key).cast(),
                data.as_ptr().cast(),
                0,
                4,
                Some(compare_u32),
            )
        };
        assert!(found.is_null());
    }
}
