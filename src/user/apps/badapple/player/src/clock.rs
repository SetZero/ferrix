//! The time the picture is shown by: the sound card's, when there is one.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The furthest the clock runs on from the card's last word. The sound
/// thread speaks after every write, a period or so apart; if it goes quiet
/// for longer, the card has stalled and the picture should wait for it.
pub const COAST: Duration = Duration::from_millis(100);

/// Where the song is, shared by the thread that plays it and the one that
/// shows the picture.
#[derive(Debug)]
pub struct Clock {
    start: Instant,
    rate: u32,
    heard: Mutex<Heard>,
}

#[derive(Clone, Copy, Debug)]
struct Heard {
    /// The card is the clock; otherwise the wall is.
    card: bool,
    /// Frames the speaker had played when last asked.
    played: u64,
    /// When that was, from `start`; `None` before the card started.
    at: Option<Duration>,
}

impl Clock {
    /// A clock for a song at `rate` frames a second, which follows the card
    /// until told otherwise.
    pub fn new(rate: u32) -> Self {
        Self {
            start: Instant::now(),
            rate,
            heard: Mutex::new(Heard {
                card: true,
                played: 0,
                at: None,
            }),
        }
    }

    fn heard(&self) -> std::sync::MutexGuard<'_, Heard> {
        self.heard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The card says the speaker has played `played` frames.
    pub fn played(&self, played: u64) {
        let now = self.start.elapsed();
        let mut heard = self.heard();
        if played > 0 {
            heard.at = Some(now);
        }
        heard.played = played;
    }

    /// There is no card, or it went away: keep time by the wall from here.
    pub fn follow_the_wall(&self) {
        let now = self.start.elapsed();
        let mut heard = self.heard();
        if heard.card {
            heard.card = false;
            // Carry on from where the song was, not from the start.
            heard.at = Some(now);
        }
    }

    /// Microseconds into the song.
    pub fn micros(&self) -> u64 {
        let now = self.start.elapsed();
        let heard = *self.heard();
        let base = heard.played * 1_000_000 / u64::from(self.rate.max(1));
        let since = heard.at.map_or(Duration::ZERO, |at| now.saturating_sub(at));
        position(base, since, heard.card)
    }
}

/// Where the song is, `since` after the card said `base` microseconds: the
/// card's word plus the time since, which runs on at most [`COAST`] while
/// the card is the clock and without bound once the wall is.
pub fn position(base: u64, since: Duration, card: bool) -> u64 {
    let since = if card { since.min(COAST) } else { since };
    base + u64::try_from(since.as_micros()).unwrap_or(u64::MAX / 2)
}
