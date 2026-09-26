//! Which frame is due, and bringing the picture to it: what the screen and
//! the window share.

use media_bav::{Header, Picture, Video};

/// How the picture kept up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    /// Frames shown.
    pub(crate) frames: u32,
    /// Frames decoded but never shown, to keep up.
    pub(crate) skipped: u32,
    /// The most a frame was shown after its time, in microseconds.
    pub(crate) latest: u64,
    /// Frames the file could not give.
    pub(crate) bad: u32,
    /// The last frame shown.
    pub(crate) last: Option<u32>,
}

/// What [`Stepper::advance`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing is due yet; the next frame is due this many microseconds on.
    Wait(u64),
    /// The picture is at `frame`, and picture rows `rows` changed (none if
    /// `None`).
    Show {
        /// The frame now in the picture.
        frame: u32,
        /// First and last picture row that changed.
        rows: Option<(u16, u16)>,
    },
    /// Every frame up to the end has been shown.
    Ended,
}

/// The picture, brought frame by frame to whatever the clock says.
#[derive(Debug)]
pub(crate) struct Stepper<'a> {
    video: Video<'a>,
    picture: Picture,
    next: u32,
    last: u32,
    /// What has been shown, for the summary.
    pub(crate) shown: Shown,
}

impl<'a> Stepper<'a> {
    /// A stepper over `video` that stops before frame `last`.
    pub(crate) fn new(video: Video<'a>, last: u32) -> Self {
        Self {
            picture: Picture::new(&video.header),
            last: last.min(video.header.frames),
            video,
            next: 0,
            shown: Shown::default(),
        }
    }

    /// The picture as it stands.
    pub(crate) fn shades(&self) -> &[u8] {
        self.picture.shades()
    }

    /// The header.
    pub(crate) const fn header(&self) -> &Header {
        &self.video.header
    }

    /// Microseconds from the start at which frame `n` is shown.
    pub(crate) fn time_of(&self, n: u32) -> u64 {
        let header = &self.video.header;
        u64::from(n) * 1_000_000 * u64::from(header.rate_den) / u64::from(header.rate_num)
    }

    /// Bring the picture to the frame due at `now` microseconds into the
    /// song, decoding the frames it passes without showing them, and record
    /// how late the frame it stops at is.
    pub(crate) fn advance(&mut self, now: u64) -> Step {
        if self.next >= self.last {
            return Step::Ended;
        }
        let due = u32::try_from(self.video.header.frame_at(now)).unwrap_or(u32::MAX);
        if due < self.next {
            return Step::Wait(self.time_of(self.next).saturating_sub(now));
        }
        let target = due.min(self.last - 1);
        let first = self.next;
        let mut rows: Option<(u16, u16)> = None;
        while self.next <= target {
            match self
                .video
                .frame(self.next)
                .map(|frame| self.picture.apply(frame))
            {
                Some(Ok(Some((top, bottom)))) => {
                    rows = Some(rows.map_or((top, bottom), |(t, b)| (t.min(top), b.max(bottom))));
                }
                Some(Ok(None)) => {}
                Some(Err(_)) | None => self.shown.bad += 1,
            }
            self.next += 1;
        }
        self.shown.skipped += target - first;
        self.shown.frames += 1;
        self.shown.last = Some(target);
        self.shown.latest = self
            .shown
            .latest
            .max(now.saturating_sub(self.time_of(target)));
        Step::Show {
            frame: target,
            rows,
        }
    }
}
