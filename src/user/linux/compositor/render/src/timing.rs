//! Where a frame's time went, by kind of work.
//!
//! A compositor's report of its slowest frame says *that* a frame was slow;
//! on a machine a hundred times slower than the one the renderer was written
//! on -- a 650 MHz Cortex-A7 drawing in software -- what is wanted next is
//! *which* part of it. So the renderer adds the time it spends on each kind
//! of work to one counter per kind, and the compositor takes the counters
//! after each frame and keeps those of the slowest.
//!
//! Wall time on the thread that asked, which for work spread over the cores
//! ([`crate::cores`]) is the time until the last band finished: what the
//! frame waited for, which is what a person waits for.
//!
//! Atomics rather than anything per thread, because a frame is drawn on one
//! thread and taken on the same one; relaxed, because a count that is a
//! microsecond late to be seen is still counted in the next frame's.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A kind of work a frame is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The background and the layer surfaces under the windows.
    Behind,
    /// Keeping the copy of what is behind the windows for the blur.
    Backdrop,
    /// Windows' shadows.
    Shadow,
    /// Windows' borders.
    Border,
    /// The blur behind translucent windows and surfaces.
    Blur,
    /// Windows' own pixels.
    Surface,
    /// Layer surfaces over the windows, the drag icon and the pointer.
    Over,
    /// Copying the frame into the screen's buffer, turned if the monitor is.
    Present,
    /// Handing the buffer to the card: the kernel's cache clean and the flip.
    Flip,
}

impl Phase {
    /// Every kind, in the order a report lists them.
    pub const ALL: [Phase; 9] = [
        Phase::Behind,
        Phase::Backdrop,
        Phase::Shadow,
        Phase::Border,
        Phase::Blur,
        Phase::Surface,
        Phase::Over,
        Phase::Present,
        Phase::Flip,
    ];

    /// The word a report uses for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Phase::Behind => "behind",
            Phase::Backdrop => "backdrop",
            Phase::Shadow => "shadow",
            Phase::Border => "border",
            Phase::Blur => "blur",
            Phase::Surface => "surface",
            Phase::Over => "over",
            Phase::Present => "present",
            Phase::Flip => "flip",
        }
    }
}

/// Microseconds spent on each kind since the last [`take`], by
/// [`Phase::ALL`]'s order.
static SPENT: [AtomicU64; Phase::ALL.len()] = [const { AtomicU64::new(0) }; Phase::ALL.len()];

/// Run `work`, counting its time to `phase`.
pub fn timed<T>(phase: Phase, work: impl FnOnce() -> T) -> T {
    let _timer = Timer::start(phase);
    work()
}

/// Counts the time from [`Timer::start`] until it is dropped to its phase:
/// for work that is the rest of a block rather than one call.
#[derive(Debug)]
pub struct Timer {
    /// What the time is counted to.
    phase: Phase,
    /// When it started.
    began: Instant,
}

impl Timer {
    /// Start counting to `phase`.
    #[must_use]
    pub fn start(phase: Phase) -> Self {
        Self {
            phase,
            began: Instant::now(),
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let micros = u64::try_from(self.began.elapsed().as_micros()).unwrap_or(u64::MAX);
        if let Some(counter) = SPENT.get(self.phase as usize) {
            let _ = counter.fetch_add(micros, Ordering::Relaxed);
        }
    }
}

/// Microseconds spent on each kind since the last call, by
/// [`Phase::ALL`]'s order, and the counters back at zero.
#[must_use]
pub fn take() -> [u64; Phase::ALL.len()] {
    let mut spent = [0; Phase::ALL.len()];
    for (slot, counter) in spent.iter_mut().zip(&SPENT) {
        *slot = counter.swap(0, Ordering::Relaxed);
    }
    spent
}

/// `spent`, as a report prints it: each kind that took any time, by name.
#[must_use]
pub fn describe(spent: &[u64; Phase::ALL.len()]) -> String {
    let mut out = String::new();
    for (phase, micros) in Phase::ALL.iter().zip(spent) {
        if *micros == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(phase.name());
        out.push(' ');
        out.push_str(&micros.to_string());
    }
    out
}
