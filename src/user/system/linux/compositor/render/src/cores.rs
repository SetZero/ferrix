//! Rows, spread over the machine's cores.
//!
//! A software renderer's large operations -- a pass of the blur, a
//! full-screen surface blended onto the frame -- write each row from
//! something that is only read, and no row of one reads another row of the
//! same one. So the rows can be written in any order by anybody, and the
//! pixels are the same bytes on one core as on sixteen: nothing here is a
//! sum whose order could move a bit.
//!
//! This is the lever `blur`'s own comment once called "a renderer that
//! spreads its rows over cores" and did not take. What took it was a
//! wallpaper that moves: everything behind the windows changes every frame
//! of the video, so the blur of it and the terminal over it are owed every
//! frame, and on one core that was a dozen frames a second.
//!
//! # Workers, not a thread per band
//!
//! The bands went to a `std::thread::scope` at first, which starts a thread
//! for each and joins them all. That is a thread started and ended per band,
//! per operation, per frame: a blurred frame is six passes and the
//! conversions either side of them, so a 30 fps video wallpaper was on the
//! order of a thousand threads a second -- and what a thread costs is not
//! only the starting. Ferrix frees an exited task's kernel stack by
//! invalidating the address everywhere, which interrupts every processor on
//! the machine and waits for each to answer; the pointer stuttered and the
//! bar stuttered while the video played, because the cost of *this* program's
//! threads was being paid by every other program on the machine.
//!
//! So the threads are started once and kept: [`fan_out`] hands each band to a
//! worker that is already there, and returns when the last of them says it
//! has finished. The thread that asked for the work is one of the workers --
//! it runs bands itself while it waits -- which is why the pool starts one
//! fewer than there are cores, and why a machine that says it has one core
//! starts none and draws exactly as it did.

use compositor_fan::fan_out;

/// The fewest items a band is worth a thread for.
///
/// Starting a thread costs about what this much work does, so a pointer's
/// worth of drawing stays on the thread that asked for it and a screen's
/// worth is spread over every core there is.
pub(crate) const BAND: usize = 32 * 1024;

/// How many threads an operation may use: the workers there are, which
/// `compositor_fan` sizes and which include the thread asking for the work.
///
/// Every processor of a small machine, and half of a large one's to at most
/// eight. Measured on twelve cores and twenty-four threads, with a video
/// behind a full-screen terminal so that every frame owed a whole blur: six
/// threads drew a frame in 25 ms, eight in 22, twelve in 20 and sixteen in
/// 20, and each step cost another core of the machine for the millisecond
/// or two it bought. Half the processors are the cores where each core is
/// two, and past eight the planes' memory is what is waited for.
///
/// One where the machine has one or will not say, which is one band and no
/// worker started: a guest with a single processor draws exactly as it did.
/// Asked of the fan rather than worked out again here, so that the number of
/// bands a frame is cut into and the number of threads there are to draw them
/// cannot drift apart.
pub(crate) fn cores() -> usize {
    #[cfg(test)]
    if let Some(forced) = tests::forced() {
        return forced;
    }
    compositor_fan::workers()
}

/// How many threads `work` items are worth.
pub(crate) fn threads_for(work: usize) -> usize {
    cores().min(work / BAND).max(1)
}

/// Run `each` over `rows`, a block of whole rows `width` items wide, in
/// bands of rows spread over the cores; `each` is given the row its band
/// begins at and the band.
pub(crate) fn bands<T: Send>(rows: &mut [T], width: usize, each: impl Fn(usize, &mut [T]) + Sync) {
    let width = width.max(1);
    let height = rows.len() / width;
    let threads = threads_for(rows.len());
    if threads == 1 {
        each(0, rows);
        return;
    }
    let tall = height.div_ceil(threads).max(1);
    fan_out(|fan| {
        for (at, band) in rows.chunks_mut(tall.saturating_mul(width)).enumerate() {
            let each = &each;
            fan.spawn(move || each(at.saturating_mul(tall), band));
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;

    use super::*;

    thread_local! {
        /// How many threads the test on this thread draws with.
        ///
        /// One unless it says otherwise, and its own rather than the
        /// process's: tests run side by side, several of them hold a frame
        /// to a time that was measured on one thread, and seventy tests
        /// each drawing on eight is a machine too busy to keep any of those
        /// times. How many threads an operation uses is asked on the thread
        /// that starts it, which is the test's.
        static THREADS: Cell<usize> = const { Cell::new(1) };
    }

    /// What [`force`] last asked for on this thread.
    pub(crate) fn forced() -> Option<usize> {
        Some(THREADS.get())
    }

    /// Draw on `threads` threads from here on, on this thread.
    pub(crate) fn force(threads: usize) {
        THREADS.set(threads.max(1));
    }

    /// One row of [`every_row_is_written_once_whatever_the_bands`]: every
    /// cell of it takes the row's number.
    fn number(row: usize, cells: &mut [u32]) {
        for cell in cells {
            *cell += u32::try_from(row).unwrap() + 1;
        }
    }

    /// Every row is handed out once, in its own place, however many bands
    /// the rows are cut into -- seven does not divide a hundred and one.
    #[test]
    fn every_row_is_written_once_whatever_the_bands() {
        let width = 1000;
        for threads in [1, 2, 7, 16] {
            force(threads);
            let mut rows = vec![0u32; width * 101];
            bands(&mut rows, width, |first, band| {
                (first..)
                    .zip(band.chunks_mut(width))
                    .for_each(|(row, cells)| number(row, cells));
            });
            let right = rows.chunks(width).enumerate().all(|(row, cells)| {
                cells
                    .iter()
                    .all(|&cell| cell == u32::try_from(row).unwrap() + 1)
            });
            assert!(
                right,
                "a row was missed or written twice on {threads} threads"
            );
        }
    }
}
