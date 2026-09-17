//! When the next frame may be drawn: no sooner than one refresh of the
//! screen after the last.
//!
//! Hyprland draws a monitor when its frame scheduler says so, which is on
//! the screen's own clock (`CMonitorFrameScheduler::onFrame` in
//! `src/output/MonitorFrameScheduler.cpp`): a pointer reporting a thousand
//! times a second still draws sixty frames. This compositor's loop draws
//! when something changed, and until a frame that changes little cost
//! little that was pacing of a kind -- a frame took a tenth of a second, so
//! there were ten. Once a pointer motion's frame cost a fraction of a
//! millisecond there was one for every event the pointer sent.
//!
//! And a frame is not only drawing. On a card it ends in a page flip, and
//! Ferrix's virtio-gpu answers a flip by setting the scanout and sending the
//! whole framebuffer to the host, waited for (`show` in
//! `kernel/src/display/drm.rs`). A guest with one processor spent it doing
//! that, and the pointer stuttered over everything -- worse than when every
//! frame was slow, which is how this was found.
//!
//! The input is still read every pass and everything it does still happens
//! at once. Only the drawing waits, and what it draws is where the pointer
//! is by then.

use std::time::{Duration, Instant};

/// The clock frames are drawn by.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Pace {
    /// When the next frame may be drawn, or `None` before the first.
    next: Option<Instant>,
}

impl Pace {
    /// Whether a frame may be drawn at `now`, on a screen whose refresh is
    /// `period` nanoseconds; and if so, that one is about to be.
    ///
    /// The next is a period after this one was *due*, not after it was
    /// drawn, so a loop that wakes a little late each time still draws at
    /// the screen's rate rather than a little under it. A loop that was
    /// away for longer than a period starts again from now: the frames it
    /// missed are not owed.
    pub(crate) fn due(&mut self, now: Instant, period: u32) -> bool {
        let period = Duration::from_nanos(u64::from(period));
        match self.next {
            Some(next) if now < next => false,
            Some(next) if now.saturating_duration_since(next) < period => {
                self.next = next.checked_add(period);
                true
            }
            _ => {
                self.next = now.checked_add(period);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sixty frames a second, as `wl_output` says it.
    const PERIOD: u32 = 16_666_666;

    /// A loop that asks every two milliseconds, which is this compositor's,
    /// draws a second's worth of frames in a second and no more, however
    /// many times it asked.
    #[test]
    fn a_second_of_asking_is_a_second_of_frames() {
        let start = Instant::now();
        let mut pace = Pace::default();
        let drawn = (0..500u64)
            .filter(|pass| pace.due(start + Duration::from_millis(pass * 2), PERIOD))
            .count();
        assert!(
            (59..=61).contains(&drawn),
            "{drawn} frames in a second at sixty a second"
        );
    }

    /// The first frame is drawn when it is asked for, and a frame asked for
    /// after a long quiet is drawn at once rather than owed the ones the
    /// quiet skipped.
    #[test]
    fn nothing_waits_that_does_not_have_to() {
        let start = Instant::now();
        let mut pace = Pace::default();
        assert!(pace.due(start, PERIOD), "the first frame waited");
        assert!(
            !pace.due(start + Duration::from_millis(5), PERIOD),
            "a second frame inside the same refresh"
        );
        let later = start + Duration::from_secs(3);
        assert!(pace.due(later, PERIOD), "a frame after a quiet waited");
        assert!(
            !pace.due(later + Duration::from_millis(1), PERIOD),
            "the quiet was paid back in frames"
        );
    }
}
