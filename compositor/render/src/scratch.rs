//! Buffers kept between operations, so that a frame's working memory is
//! mapped once rather than once a frame.
//!
//! Every large buffer this renderer works in is short-lived: the float
//! planes of a blur, the block a blur is cut out of the canvas into, the
//! tight rows a padded surface is gathered into before it is blended. Each
//! was a fresh `Vec` and was dropped when its operation ended -- which on a
//! Linux host is cheap enough to go unnoticed, and on Ferrix is not. These
//! programs are linked against musl, whose `malloc` maps every block over
//! 128 KiB on its own and unmaps it on `free` (`ferrousli`'s does the same,
//! by design), so each of them was an `mmap`, a page fault for every 4 KiB
//! page written, and a `munmap` with a TLB shootdown across every
//! processor. A frame that blurs a screen at three passes works in about
//! 88 MB of planes: twenty-one thousand page faults a frame, taken under
//! the address space's one lock while the threads of the blur wait on each
//! other for it, and seven shootdowns. Measured in a guest of four
//! processors, that was most of a 70 ms frame whose arithmetic is 30.
//!
//! So the buffers are kept. A buffer is handed out at the length asked for
//! **with whatever it last held**: nothing that takes one reads a byte it
//! has not written first, which is what every caller here promises and what
//! the golden images hold. Each kind is kept per thread, because the blur's
//! planes are taken on the thread that draws the frame and a pass's mixed
//! rows on each worker, and a pool shared between them would be a lock in
//! the innermost loop of the renderer.
//!
//! A buffer taken at a smaller length than it last had is cut to it, and
//! grown again with zeros when a larger length is asked for: the cost of
//! that is a `memset` over memory that is already mapped, and the best-fit
//! choice below makes it rare -- the four planes of a three-pass blur each
//! go back to the size they were.

use std::cell::RefCell;
use std::thread::LocalKey;

/// The most buffers of one kind a thread keeps.
///
/// A three-pass blur is four planes of four sizes, and a screen's worth of
/// them is what a frame that blurs a screen needs every frame. Past this
/// many the smallest goes, so that a thread never keeps more than a handful
/// of large buffers however many sizes it has seen.
const KEPT: usize = 8;

thread_local! {
    /// Planes of premultiplied floats: the blur's pyramid and its mixed rows.
    static PLANES: RefCell<Vec<Vec<[f32; 4]>>> = const { RefCell::new(Vec::new()) };
    /// Bytes: a blurred block, a surface's gathered rows.
    static BYTES: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}

/// A plane of `len` pixels, holding whatever it last held.
pub(crate) fn planes(len: usize) -> Vec<[f32; 4]> {
    take(&PLANES, len, [0.0; 4])
}

/// Keep `plane` for the next [`planes`].
pub(crate) fn planes_back(plane: Vec<[f32; 4]>) {
    give(&PLANES, plane);
}

/// A buffer of `len` bytes, holding whatever it last held.
pub(crate) fn bytes(len: usize) -> Vec<u8> {
    take(&BYTES, len, 0)
}

/// Keep `bytes` for the next [`bytes`].
pub(crate) fn bytes_back(bytes: Vec<u8>) {
    give(&BYTES, bytes);
}

/// The smallest kept buffer that holds `len`, or a new one, at `len`.
fn take<T: Copy + 'static>(
    kept: &'static LocalKey<RefCell<Vec<Vec<T>>>>,
    len: usize,
    fill: T,
) -> Vec<T> {
    let mut buffer = kept
        .with(|kept| {
            let mut kept = kept.borrow_mut();
            let best = kept
                .iter()
                .enumerate()
                .filter(|(_, buffer)| buffer.capacity() >= len)
                .min_by_key(|(_, buffer)| buffer.capacity())
                .map(|(at, _)| at);
            best.map(|at| kept.swap_remove(at))
        })
        .unwrap_or_default();
    buffer.resize(len, fill);
    buffer
}

/// Keep `buffer`, letting the smallest go if that makes one too many.
fn give<T: 'static>(kept: &'static LocalKey<RefCell<Vec<Vec<T>>>>, buffer: Vec<T>) {
    if buffer.capacity() == 0 {
        return;
    }
    kept.with(|kept| {
        let mut kept = kept.borrow_mut();
        kept.push(buffer);
        if kept.len() > KEPT
            && let Some(at) = kept
                .iter()
                .enumerate()
                .min_by_key(|(_, buffer)| buffer.capacity())
                .map(|(at, _)| at)
        {
            drop(kept.swap_remove(at));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many planes this thread keeps.
    fn kept() -> usize {
        PLANES.with(|kept| kept.borrow().len())
    }

    /// A buffer given back is the one handed out next, at the length asked
    /// for, and no memory is mapped for it: its capacity is what it was.
    #[test]
    fn a_buffer_given_back_is_handed_out_again() {
        let first = planes(1000);
        let address = first.as_ptr();
        let capacity = first.capacity();
        planes_back(first);
        let again = planes(500);
        assert_eq!(again.len(), 500);
        assert_eq!(again.as_ptr(), address, "another buffer was made");
        assert_eq!(again.capacity(), capacity, "the buffer shrank");
        planes_back(again);
        let grown = planes(1000);
        assert_eq!(grown.as_ptr(), address);
        assert!(
            grown.iter().skip(500).all(|pixel| *pixel == [0.0; 4]),
            "what was grown back is not zero"
        );
        planes_back(grown);
    }

    /// The smallest buffer that fits is the one taken, so the four planes of
    /// a pyramid each go back to their own size rather than the largest
    /// being cut down for the smallest need.
    #[test]
    fn the_smallest_buffer_that_fits_is_taken() {
        let (small, large) = (planes(100), planes(10_000));
        let (small_at, large_at) = (small.as_ptr(), large.as_ptr());
        planes_back(large);
        planes_back(small);
        let taken = planes(50);
        assert_eq!(taken.as_ptr(), small_at, "the large buffer was cut down");
        let other = planes(5_000);
        assert_eq!(other.as_ptr(), large_at);
        planes_back(taken);
        planes_back(other);
    }

    /// Past `KEPT` the smallest goes.
    #[test]
    fn only_so_many_are_kept_and_the_smallest_goes_first() {
        let before = kept();
        for len in 1..=KEPT + 2 {
            planes_back(planes(len * 10));
        }
        assert!(kept() <= KEPT);
        assert!(kept() >= before.min(KEPT));
        let smallest =
            PLANES.with(|kept| kept.borrow().iter().map(Vec::capacity).min().unwrap_or(0));
        assert!(smallest >= 30, "a small buffer outlived a larger one");
    }
}
