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

/// The fewest items a band is worth a thread for.
///
/// Starting a thread costs about what this much work does, so a pointer's
/// worth of drawing stays on the thread that asked for it and a screen's
/// worth is spread over every core there is.
pub(crate) const BAND: usize = 32 * 1024;

/// How many threads an operation may use.
///
/// Every processor of a small machine, and half of a large one's to at most
/// eight. Measured on twelve cores and twenty-four threads, with a video
/// behind a full-screen terminal so that every frame owed a whole blur: six
/// threads drew a frame in 25 ms, eight in 22, twelve in 20 and sixteen in
/// 20, and each step cost another core of the machine for the millisecond
/// or two it bought. Half the processors are the cores where each core is
/// two, and past eight the planes' memory is what is waited for.
///
/// One where the machine has one or will not say, which is one thread and
/// none started: a guest with a single processor draws exactly as it did.
pub(crate) fn cores() -> usize {
    static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    #[cfg(test)]
    if let Some(forced) = tests::forced() {
        return forced;
    }
    *CORES.get_or_init(|| {
        let processors = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        if processors <= 4 {
            processors
        } else {
            (processors / 2).clamp(4, 8)
        }
    })
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
    std::thread::scope(|scope| {
        for (at, band) in rows.chunks_mut(tall.saturating_mul(width)).enumerate() {
            let each = &each;
            let _ = scope.spawn(move || each(at.saturating_mul(tall), band));
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
