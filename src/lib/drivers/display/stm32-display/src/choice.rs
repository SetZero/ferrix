//! Which of the modes a monitor offers the board can run, and which to run.
//!
//! # What limits it
//!
//! * **The LTDC's pixel clock tops out at 90 MHz.** The STM32MP157A/D
//!   datasheet (DS12504 Rev 4, table 94, "LTDC characteristics") gives
//!   `fCLK` a maximum of 90 MHz at 2.7 to 3.6 V with the pins at high or
//!   very high speed, and sells the controller as "up to WXGA (1366 × 768)
//!   @60 fps"; Linux's STM32 DRM driver gives this LTDC version the same
//!   `pad_max_freq_hz` (`drivers/gpu/drm/stm/drv.c`) and refuses every mode
//!   above it (`ltdc_crtc_mode_valid`). 1920x1200 at 60 Hz needs 154 MHz
//!   even with reduced blanking, and 1920x1080 at 60 Hz 148.5: neither is in
//!   reach, on this board or any STM32MP15.
//! * **The clock comes from a divider.** The pixel clock is PLL4's Q output,
//!   and the only part of PLL4 the kernel may change is Q's own divider
//!   (`docs/DISPLAY.md` §6): with the firmware's 594 MHz VCO the rates are
//!   594 MHz / k, so 84.857, 74.25, 66, 59.4, 54, 49.5 ... MHz. The kernel
//!   rounds a rate for the driver (`device_clock`), which is the `round`
//!   every function here takes, so nothing here assumes the VCO -- nor the
//!   kernel's own ceiling, which is lower than 90 MHz on a DK board: its
//!   device tree sets the LTDC's pins to medium speed, for which the
//!   datasheet gives no rate at all, and the kernel holds those to the
//!   74.25 MHz the board is seen to run.
//! * **A listed mode must be hit within half a percent**, the tolerance
//!   CEA-861 gives a sink and the one the kernel checked the firmware's rate
//!   against. Linux's LTDC driver asks for 50 Hz, which on this board comes
//!   to the same list: every 74.25 MHz mode, and the 27 and 54 MHz ones.
//! * **The bridge carries 25 to 165 MHz** ([`crate::sii9022`]).
//!
//! # What is run
//!
//! Every mode the monitor offers whose clock the board makes exactly is
//! runnable as it is ([`How::Listed`]). A monitor whose EDID says it takes
//! any timing inside its range limits ([`crate::edid::continuous`]) also
//! gets each size it lists at the fastest rate the board can make, with the
//! size's own blanking or CVT's reduced blanking, whichever refreshes
//! faster, if that lands inside the limits ([`How::Retimed`]): 1920x1200
//! with reduced blanking at 74.25 MHz is about 29 Hz, which a monitor whose
//! range starts at 24 Hz takes and one whose range starts at 49 does not.
//! And CEA-861's
//! 1280x720 at 60 Hz, which the board ran before it read any EDID and every
//! HDMI sink takes, is always runnable ([`How::Assumed`]), so no monitor
//! gets less than it had.
//!
//! One mode a size -- the best by exactness, then refresh -- and the sizes
//! largest first: the first is the one to run.

use crate::edid::{Offers, RangeLimits};
use crate::ltdc::Timing;
use crate::mode::Mode;

/// The fastest pixel clock the STM32MP15's LTDC drives its pins at.
pub const MAX_PIXEL_KHZ: u32 = 90_000;

/// The slowest pixel clock the bridge takes.
pub const MIN_PIXEL_KHZ: u32 = 25_000;

/// The most modes a [`Runnable`] keeps: what the display protocol's HELLO
/// carries.
pub const MAX_RUNNABLE: usize = 16;

/// Sizes considered at once, before the list is cut to [`MAX_RUNNABLE`].
const MAX_SIZES: usize = 40;

/// Whether `got` is within half a percent of `want`.
#[must_use]
pub const fn close_enough(want_khz: u32, got_khz: u32) -> bool {
    (want_khz.abs_diff(got_khz) as u64) * 200 <= want_khz as u64
}

/// Whether the board can run a mode, and if not, why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// It runs, at this rate.
    Runs {
        /// The rate the clock will make.
        clock_khz: u32,
    },
    /// Its clock is above [`MAX_PIXEL_KHZ`].
    TooFast,
    /// Its clock is below [`MIN_PIXEL_KHZ`].
    TooSlow,
    /// The nearest rate the clock makes is more than half a percent off.
    Clock {
        /// That rate.
        nearest_khz: u32,
    },
    /// The kernel will not say what rate it would make: no clock to set.
    NoClock,
    /// Its totals do not fit the LTDC's counters.
    Counters,
}

/// Whether the board runs `mode` as it is, with `round` the kernel's
/// rounding of a pixel clock.
pub fn verdict(mode: &Mode, round: &mut impl FnMut(u32) -> Option<u32>) -> Verdict {
    if mode.clock_khz > MAX_PIXEL_KHZ {
        return Verdict::TooFast;
    }
    if mode.clock_khz < MIN_PIXEL_KHZ {
        return Verdict::TooSlow;
    }
    if Timing::of(mode).is_none() {
        return Verdict::Counters;
    }
    match round(mode.clock_khz) {
        None => Verdict::NoClock,
        Some(got) if close_enough(mode.clock_khz, got) => Verdict::Runs { clock_khz: got },
        Some(got) => Verdict::Clock { nearest_khz: got },
    }
}

/// How a runnable mode came to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum How {
    /// The monitor listed it, and the clock makes it.
    Listed,
    /// The monitor listed its size and takes any timing inside its range
    /// limits; this is that size at a rate the clock makes.
    Retimed,
    /// 720p60, which the board ran before it read EDIDs.
    Assumed,
}

/// A mode the board can run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Candidate {
    /// The mode. A retimed one carries the rate the clock makes.
    pub mode: Mode,
    /// How it came to be.
    pub how: How,
}

impl Candidate {
    /// Within one size, which is better: a listed or assumed mode over a
    /// retimed one, then the faster refresh, then listed over assumed.
    fn rank(&self) -> (bool, u64, bool) {
        (
            self.how != How::Retimed,
            self.mode.refresh_millihz(),
            self.how == How::Listed,
        )
    }

    fn pixels(&self) -> u32 {
        u32::from(self.mode.hdisplay) * u32::from(self.mode.vdisplay)
    }
}

/// The modes the board can run on one monitor, a size each, the one to run
/// first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Runnable {
    list: [Candidate; MAX_RUNNABLE],
    len: usize,
}

impl Runnable {
    /// The modes.
    #[must_use]
    pub fn as_slice(&self) -> &[Candidate] {
        self.list.get(..self.len).unwrap_or(&[])
    }

    /// The mode to run: the largest.
    #[must_use]
    pub fn chosen(&self) -> Option<Candidate> {
        self.as_slice().first().copied()
    }

    /// The mode of size `width` × `height`, if one is runnable.
    #[must_use]
    pub fn of_size(&self, width: u32, height: u32) -> Option<Candidate> {
        self.as_slice()
            .iter()
            .find(|c| (u32::from(c.mode.hdisplay), u32::from(c.mode.vdisplay)) == (width, height))
            .copied()
    }
}

/// Every size the board can run on a monitor that offers `offers`, with
/// `limits` its range limits and `continuous` whether it takes any timing
/// inside them; `round` is the kernel's rounding of a pixel clock.
pub fn runnable(
    offers: &Offers,
    limits: Option<RangeLimits>,
    continuous: bool,
    mut round: impl FnMut(u32) -> Option<u32>,
) -> Runnable {
    let mut sizes = [None::<Candidate>; MAX_SIZES];
    let mut keep = |candidate: Candidate| {
        let size = (candidate.mode.hdisplay, candidate.mode.vdisplay);
        let slot = sizes
            .iter()
            .position(|held| held.is_some_and(|h| (h.mode.hdisplay, h.mode.vdisplay) == size))
            .or_else(|| sizes.iter().position(Option::is_none));
        if let Some(held) = slot.and_then(|at| sizes.get_mut(at))
            && held.is_none_or(|h| candidate.rank() > h.rank())
        {
            *held = Some(candidate);
        }
    };
    for offer in offers.as_slice() {
        let Ok(mode) = offer.mode else {
            continue;
        };
        if let Verdict::Runs { .. } = verdict(&mode, &mut round) {
            keep(Candidate {
                mode,
                how: How::Listed,
            });
        } else if let Some(limits) = limits.filter(|_| continuous)
            && let Some(mode) = retime(&mode, &limits, &mut round)
        {
            keep(Candidate {
                mode,
                how: How::Retimed,
            });
        }
    }
    if let Verdict::Runs { .. } = verdict(&Mode::CEA_720P60, &mut round) {
        keep(Candidate {
            mode: Mode::CEA_720P60,
            how: How::Assumed,
        });
    }

    let mut runnable = Runnable {
        list: [Candidate {
            mode: Mode::CEA_720P60,
            how: How::Assumed,
        }; MAX_RUNNABLE],
        len: 0,
    };
    // Largest first, then the better of two sizes of as many pixels; a
    // selection sort over forty entries at most.
    let mut left = sizes;
    while runnable.len < MAX_RUNNABLE {
        let best = left
            .iter()
            .enumerate()
            .filter_map(|(at, held)| Some((at, (*held)?)))
            .max_by_key(|(_, c)| (c.pixels(), c.rank()));
        let Some((at, candidate)) = best else {
            break;
        };
        if let Some(slot) = left.get_mut(at) {
            *slot = None;
        }
        if let Some(slot) = runnable.list.get_mut(runnable.len) {
            *slot = candidate;
            runnable.len += 1;
        }
    }
    runnable
}

/// `mode`'s size at a rate the clock makes, inside `limits`: its own
/// blanking at the rate nearest its own clock (no faster than the LTDC or
/// the monitor take), or CVT's reduced blanking at the fastest rate there
/// is, whichever refreshes faster. `None` when neither lands inside.
fn retime(
    mode: &Mode,
    limits: &RangeLimits,
    round: &mut impl FnMut(u32) -> Option<u32>,
) -> Option<Mode> {
    let cap = MAX_PIXEL_KHZ.min(limits.max_clock_khz);
    let own = round(mode.clock_khz.min(cap)).map(|clock_khz| Mode {
        clock_khz,
        vic: 0,
        ..*mode
    });
    let reduced =
        round(cap).and_then(|clock| reduced_blanking(mode.hdisplay, mode.vdisplay, clock));
    [own, reduced]
        .into_iter()
        .flatten()
        .filter(|m| fits(m, limits, cap))
        .max_by_key(Mode::refresh_millihz)
}

/// Whether a retimed mode is inside the monitor's limits and the board's.
fn fits(mode: &Mode, limits: &RangeLimits, cap: u32) -> bool {
    let clock = u64::from(mode.clock_khz);
    let (htotal, vtotal) = (u64::from(mode.htotal), u64::from(mode.vtotal));
    let (v_min, v_max) = limits.vertical_hz;
    let (h_min, h_max) = limits.horizontal_khz;
    // Vertical rate: clock * 1000 / (htotal * vtotal) Hz; horizontal:
    // clock / htotal kHz. Compared multiplied out, so nothing rounds.
    let frame = htotal * vtotal;
    mode.clock_khz <= cap
        && mode.clock_khz >= MIN_PIXEL_KHZ
        && u64::from(v_min) * frame <= clock * 1000
        && clock * 1000 <= u64::from(v_max) * frame
        && u64::from(h_min) * htotal <= clock
        && clock <= u64::from(h_max) * htotal
        && Timing::of(mode).is_some()
}

/// CVT 1.2's reduced blanking (version 1) for `width` × `height` at a pixel
/// clock of `clock_khz`: 160 pixels of horizontal blanking (front porch 48,
/// sync 32), a vertical front porch of 3 lines, a sync as long as the aspect
/// ratio says, and at least 460 µs of vertical blanking with a back porch of
/// at least 6 lines. The standard fixes the refresh and works out the clock;
/// here the clock is what the divider makes and the refresh is what it comes
/// to, which is the same arithmetic the other way round.
#[must_use]
pub fn reduced_blanking(width: u16, height: u16, clock_khz: u32) -> Option<Mode> {
    let (w, h) = (u32::from(width), u32::from(height));
    let vsync: u16 = if w * 3 == h * 4 {
        4
    } else if w * 9 == h * 16 {
        5
    } else if w * 10 == h * 16 {
        6
    } else if w * 4 == h * 5 || w * 9 == h * 15 {
        7
    } else {
        10
    };
    let htotal = width.checked_add(160)?;
    // Lines in 460 µs: 0.46 ms * clock / htotal, rounded up.
    let lines = (46 * u64::from(clock_khz)).div_ceil(100 * u64::from(htotal));
    let vblank = u16::try_from(lines).ok()?.max(3 + vsync + 6);
    let mode = Mode {
        clock_khz,
        hdisplay: width,
        hsync_start: width.checked_add(48)?,
        hsync_end: width.checked_add(80)?,
        htotal,
        vdisplay: height,
        vsync_start: height.checked_add(3)?,
        vsync_end: height.checked_add(3 + vsync)?,
        vtotal: height.checked_add(vblank)?,
        hsync_high: true,
        vsync_high: false,
        vic: 0,
    };
    mode.is_consistent().then_some(mode)
}
