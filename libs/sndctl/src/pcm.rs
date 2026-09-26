//! One playback stream as the audio core keeps it: ALSA's states, pointers
//! and thresholds, and what reaches the driver.
//!
//! `docs/AUDIO.md` §3.1 is the design. A program writes frames into the
//! core's buffer and moves `appl_ptr`; the core submits what it holds to the
//! driver in [`Submit`]s; each completion the driver reports moves `hw_ptr`
//! by the frames it carried. Everything a program sees -- `avail`, `delay`,
//! `poll`, an underrun -- follows from those two pointers exactly as Linux's
//! `sound/core/pcm_lib.c` and `pcm_native.c` compute it, and the functions
//! below name the Linux function each follows. Where the core differs from
//! Linux, because it has no device to program and no mapping to offer, the
//! item says so.
//!
//! # What reaches the driver
//!
//! Only frames the program has written since the last prepare are ever
//! submitted, so the device never plays a stale period. Whole periods go as
//! they fill. When nothing is in flight, whatever is queued goes at once, even
//! part of a period, so a device is never left idle with frames waiting; and
//! at a drain everything left goes. A submission never crosses a period
//! boundary, and so never the end of the buffer.
//!
//! When the stream stops -- a drop, an underrun, the end of a drain, a close
//! -- the core asks the driver to [`Effects::halt`] the device and hand back
//! what is posted. Submissions still in flight then belong to the stream that
//! stopped: their completions are accepted in order and move nothing, and no
//! new submission goes until the driver says the device has halted.
//!
//! # Time
//!
//! The glue passes the time into every call that may need it, in the clock
//! the program chose ([`Stream::clock`]), as nanoseconds. Nothing here reads
//! a clock, waits or allocates.

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::socket::Width;
use ferrix_linux_abi::sound::{
    ACCESS_RW_INTERLEAVED, FORMAT_S16_LE, HwParams, INFO_BATCH, INFO_BLOCK_TRANSFER,
    INFO_INTERLEAVED, INFO_PERFECT_DRAIN, INTERVALS, MmapStatus, PCM_VERSION, STATE_DISCONNECTED,
    STATE_DRAINING, STATE_OPEN, STATE_PAUSED, STATE_PREPARED, STATE_RUNNING, STATE_SETUP,
    STATE_SUSPENDED, STATE_XRUN, SUBFORMAT_STD, SYNC_PTR_APPL, SYNC_PTR_AVAIL_MIN, SYNC_PTR_HWSYNC,
    Status, SwParams, TSTAMP_ENABLE, TSTAMP_TYPE_MONOTONIC, TSTAMP_TYPE_MONOTONIC_RAW, Timespec,
    protocol_version,
};

use crate::refine::{self, ANY, Constraints, single};

// ---------------------------------------------------------------------------
// The configuration
// ---------------------------------------------------------------------------

/// A stream's one configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Frames a second.
    pub rate: u32,
    /// Channels.
    pub channels: u32,
    /// `FORMAT_*`.
    pub format: u32,
    /// Bits in one sample.
    pub sample_bits: u32,
    /// Frames in one period.
    pub period_frames: u32,
    /// Periods in the buffer.
    pub periods: u32,
}

/// Version 1's configuration (`docs/AUDIO.md` §3.1): `S16_LE`, two channels,
/// 48 kHz, four periods of 960 frames. 20 ms a period, a whole number of
/// microseconds, so that every interval of the refine is closed.
pub const VERSION_1: Config = Config {
    rate: 48_000,
    channels: 2,
    format: FORMAT_S16_LE,
    sample_bits: 16,
    period_frames: 960,
    periods: 4,
};

/// `INFO_*` a card here reports: interleaved read-write access in blocks,
/// a pointer that moves in periods, and a drain that needs no silence.
pub const INFO: u32 = INFO_INTERLEAVED | INFO_BLOCK_TRANSFER | INFO_BATCH | INFO_PERFECT_DRAIN;

/// The most submissions in flight at once: every period, and a partial one
/// at each end.
pub const MAX_IN_FLIGHT: usize = 8;

impl Config {
    /// Bytes in one frame.
    #[must_use]
    pub const fn frame_bytes(&self) -> u32 {
        self.sample_bits / 8 * self.channels
    }

    /// Frames in the buffer.
    #[must_use]
    pub const fn buffer_frames(&self) -> u32 {
        self.period_frames * self.periods
    }

    /// Bytes in one period.
    #[must_use]
    pub const fn period_bytes(&self) -> u32 {
        self.period_frames * self.frame_bytes()
    }

    /// Bytes in the buffer: the size of the stream's VMO's contents.
    #[must_use]
    pub const fn buffer_bytes(&self) -> u32 {
        self.buffer_frames() * self.frame_bytes()
    }

    /// Microseconds in one period.
    #[must_use]
    pub const fn period_time(&self) -> u32 {
        (self.period_frames as u64 * 1_000_000 / self.rate as u64) as u32
    }

    /// Microseconds in the buffer.
    #[must_use]
    pub const fn buffer_time(&self) -> u32 {
        (self.buffer_frames() as u64 * 1_000_000 / self.rate as u64) as u32
    }

    /// Whether every time in it is a whole number of microseconds, so that
    /// its intervals can be closed. A configuration that is not would need
    /// open intervals, which this crate does not offer.
    #[must_use]
    pub const fn times_are_whole(&self) -> bool {
        (self.period_frames as u64 * 1_000_000).is_multiple_of(self.rate as u64)
    }

    /// What `HW_REFINE` narrows a space to: this configuration and nothing
    /// else, with `TICK_TIME` left as it came, as Linux leaves it.
    #[must_use]
    pub const fn constraints(&self) -> Constraints {
        let intervals: [ferrix_linux_abi::sound::Interval; INTERVALS] = [
            single(self.sample_bits),
            single(self.sample_bits * self.channels),
            single(self.channels),
            single(self.rate),
            single(self.period_time()),
            single(self.period_frames),
            single(self.period_bytes()),
            single(self.periods),
            single(self.buffer_time()),
            single(self.buffer_frames()),
            single(self.buffer_bytes()),
            ANY,
        ];
        Constraints {
            masks: [
                refine::only(ACCESS_RW_INTERLEAVED),
                refine::only(self.format),
                refine::only(SUBFORMAT_STD),
            ],
            intervals,
            info: INFO,
        }
    }
}

// ---------------------------------------------------------------------------
// What the glue does next
// ---------------------------------------------------------------------------

/// A range of the buffer the driver is to play: `docs/AUDIO.md` §3.2's
/// SUBMIT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Submit {
    /// Its sequence number; completions come back in this order.
    pub sequence: u32,
    /// Where it starts in the buffer, in bytes.
    pub offset: u32,
    /// Its length in bytes, a whole number of frames.
    pub bytes: u32,
}

/// What a call asks of the glue: submissions to send, in order, whether to
/// ask the driver to halt the device, and whether to wake the stream's
/// waiters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Effects {
    submits: [Option<Submit>; MAX_IN_FLIGHT],
    count: usize,
    /// Send HALT: the device is to stop and hand back what it holds.
    pub halt: bool,
    /// Wake writers, pollers and a drain.
    pub wake: bool,
}

impl Effects {
    /// The submissions, in the order they go.
    pub fn submits(&self) -> impl Iterator<Item = Submit> + '_ {
        self.submits.iter().take(self.count).flatten().copied()
    }

    fn push(&mut self, submit: Submit) {
        if let Some(slot) = self.submits.get_mut(self.count) {
            *slot = Some(submit);
            self.count += 1;
        }
    }
}

/// Why a driver's report about a submission was refused: the driver lied, and
/// the session is broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lie {
    /// A completion when nothing is in flight.
    NothingInFlight,
    /// A completion for a sequence number other than the oldest in flight.
    Sequence {
        /// The one expected.
        expected: u32,
        /// The one reported.
        reported: u32,
    },
    /// HALTED when no HALT was asked for.
    NotHalting,
    /// HALTED with a count other than what was in flight.
    Unplayed {
        /// What was in flight.
        in_flight: u32,
        /// What the driver said came back unplayed.
        reported: u32,
    },
}

/// What `poll` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Poll {
    /// `POLLOUT | POLLWRNORM`.
    pub writable: bool,
    /// `POLLERR`.
    pub error: bool,
}

/// What a drain leaves the caller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drain {
    /// Nothing: the stream is set up again, or was never running.
    Done,
    /// Wait until the stream leaves `STATE_DRAINING`, or [`Stream::drain_expired`].
    Wait,
}

/// A frame count's contiguous room in the buffer: where the next copy goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    /// Where it starts in the buffer, in bytes.
    pub offset: u32,
    /// Frames that fit there before the end of the buffer or of `avail`.
    pub frames: u32,
}

// ---------------------------------------------------------------------------
// The stream
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlight {
    sequence: u32,
    frames: u32,
    /// Submitted before the last halt: its completion moves nothing.
    void: bool,
}

/// The software parameters a program set, and `HW_PARAMS`' defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Software {
    tstamp_mode: i32,
    tstamp_type: u32,
    avail_min: u64,
    start_threshold: u64,
    stop_threshold: u64,
    silence_threshold: u64,
    silence_size: u64,
    period_step: u32,
}

/// One published playback stream: the program's view of it while it is open,
/// and the driver's in-flight submissions, which outlive an open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream {
    config: Config,
    /// The width of the program that has it open, which bounds `boundary`.
    width: Width,
    state: i32,
    appl_ptr: u64,
    hw_ptr: u64,
    /// How far submissions reach, between `hw_ptr` and `appl_ptr`.
    submitted: u64,
    boundary: u64,
    software: Software,
    user_version: u32,
    in_flight: [Option<InFlight>; MAX_IN_FLIGHT],
    next_sequence: u32,
    /// HALT sent, HALTED not yet received.
    halting: bool,
    /// Something was submitted since the device last halted.
    active: bool,
    trigger_tstamp: u64,
    tstamp: u64,
    avail_max: u64,
}

/// `LONG_MAX` at `width`.
const fn long_max(width: Width) -> u64 {
    match width {
        Width::Bits32 => i32::MAX as u64,
        Width::Bits64 => i64::MAX as u64,
    }
}

/// `snd_pcm_hw_params`' boundary: the buffer size doubled while twice it
/// still fits below `LONG_MAX` less a buffer.
#[must_use]
pub const fn boundary(buffer_frames: u64, width: Width) -> u64 {
    let mut boundary = buffer_frames;
    if boundary == 0 {
        return 0;
    }
    let limit = long_max(width) - buffer_frames;
    while boundary <= limit / 2 {
        boundary *= 2;
    }
    boundary
}

fn nanos(timespec_ns: u64) -> Timespec {
    Timespec {
        sec: (timespec_ns / 1_000_000_000) as i64,
        nsec: (timespec_ns % 1_000_000_000) as i64,
    }
}

impl Stream {
    /// A stream of `config`, not open.
    #[must_use]
    pub const fn new(config: Config) -> Self {
        Self {
            config,
            width: Width::Bits64,
            state: STATE_OPEN,
            appl_ptr: 0,
            hw_ptr: 0,
            submitted: 0,
            boundary: 0,
            software: Software {
                tstamp_mode: 0,
                tstamp_type: 0,
                avail_min: 1,
                start_threshold: 1,
                stop_threshold: 0,
                silence_threshold: 0,
                silence_size: 0,
                period_step: 1,
            },
            user_version: 0,
            in_flight: [None; MAX_IN_FLIGHT],
            next_sequence: 0,
            halting: false,
            active: false,
            trigger_tstamp: 0,
            tstamp: 0,
            avail_max: 0,
        }
    }

    /// Its configuration.
    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// `STATE_*`.
    #[must_use]
    pub const fn state(&self) -> i32 {
        self.state
    }

    /// The program's position, in frames modulo `boundary`.
    #[must_use]
    pub const fn appl_ptr(&self) -> u64 {
        self.appl_ptr
    }

    /// The device's position, in frames modulo `boundary`.
    #[must_use]
    pub const fn hw_ptr(&self) -> u64 {
        self.hw_ptr
    }

    /// The clock the program chose for its timestamps, `TSTAMP_TYPE_*`.
    #[must_use]
    pub const fn clock(&self) -> u32 {
        self.software.tstamp_type
    }

    /// Whether HALT is outstanding.
    #[must_use]
    pub const fn halting(&self) -> bool {
        self.halting
    }

    // -- opening and closing ------------------------------------------------

    /// A program opened the node, at `width`: the stream starts again from
    /// `STATE_OPEN`, and what is still in flight from the last open stays in
    /// flight, void.
    pub fn open(&mut self, width: Width) {
        let in_flight = self.in_flight;
        let (next_sequence, halting, active) = (self.next_sequence, self.halting, self.active);
        *self = Self::new(self.config);
        self.width = width;
        self.in_flight = in_flight;
        self.next_sequence = next_sequence;
        self.halting = halting;
        self.active = active;
    }

    /// The program closed the node: whatever the device holds is halted.
    pub fn close(&mut self, effects: &mut Effects) {
        self.stop_device(effects);
        self.state = STATE_OPEN;
    }

    /// The driver went away: every request now answers `EBADFD`, and `poll`
    /// an error, as a disconnected card's do (`snd_pcm_common_ioctl`).
    pub fn disconnect(&mut self, effects: &mut Effects) {
        self.state = STATE_DISCONNECTED;
        self.in_flight = [None; MAX_IN_FLIGHT];
        self.halting = false;
        self.active = false;
        effects.wake = true;
    }

    /// Refused with `EBADFD` for a disconnected stream, the check at the top
    /// of `snd_pcm_common_ioctl`, before any request.
    ///
    /// # Errors
    ///
    /// `EBADFD` once the card is gone.
    pub const fn check_connected(&self) -> Result<(), Errno> {
        if self.state == STATE_DISCONNECTED {
            Err(Errno::EBADFD)
        } else {
            Ok(())
        }
    }

    /// `USER_PVERSION`: the protocol the program speaks.
    pub fn set_user_version(&mut self, version: u32) {
        self.user_version = version;
    }

    /// `TTSTAMP`: the clock of the program's timestamps.
    ///
    /// # Errors
    ///
    /// `EINVAL` for a clock `TSTAMP_TYPE_*` does not name.
    pub fn set_tstamp_type(&mut self, clock: u32) -> Result<(), Errno> {
        if clock > TSTAMP_TYPE_MONOTONIC_RAW {
            return Err(Errno::EINVAL);
        }
        self.software.tstamp_type = clock;
        Ok(())
    }

    // -- the configuration ---------------------------------------------------

    /// `HW_REFINE`.
    ///
    /// # Errors
    ///
    /// `EINVAL` when some parameter has nothing left.
    pub fn hw_refine(&self, params: &mut HwParams) -> Result<(), Errno> {
        refine::refine(params, &self.config.constraints()).map_err(|_| Errno::EINVAL)
    }

    /// `HW_PARAMS`: settle the configuration and take it, with the software
    /// parameters' defaults (`snd_pcm_hw_params`).
    ///
    /// # Errors
    ///
    /// `EBADFD` outside `OPEN`, `SETUP` and `PREPARED`, and `EINVAL` when
    /// some parameter has nothing left, after which the stream is `OPEN`.
    pub fn hw_params(&mut self, params: &mut HwParams, effects: &mut Effects) -> Result<(), Errno> {
        if !matches!(self.state, STATE_OPEN | STATE_SETUP | STATE_PREPARED) {
            return Err(Errno::EBADFD);
        }
        if refine::choose(params, &self.config.constraints()).is_err() {
            self.state = STATE_OPEN;
            return Err(Errno::EINVAL);
        }
        let buffer = u64::from(self.config.buffer_frames());
        self.software = Software {
            tstamp_mode: 0,
            tstamp_type: self.software.tstamp_type,
            avail_min: u64::from(self.config.period_frames),
            start_threshold: 1,
            stop_threshold: buffer,
            silence_threshold: 0,
            silence_size: 0,
            period_step: 1,
        };
        self.boundary = boundary(buffer, self.width);
        self.stop_device(effects);
        self.state = STATE_SETUP;
        Ok(())
    }

    /// `HW_FREE`.
    ///
    /// # Errors
    ///
    /// `EBADFD` outside `SETUP` and `PREPARED`.
    pub fn hw_free(&mut self, effects: &mut Effects) -> Result<(), Errno> {
        if !matches!(self.state, STATE_SETUP | STATE_PREPARED) {
            return Err(Errno::EBADFD);
        }
        self.stop_device(effects);
        self.state = STATE_OPEN;
        Ok(())
    }

    /// `SW_PARAMS` (`snd_pcm_sw_params`): the thresholds, validated as Linux
    /// validates them, and `boundary` written back. The silence fields are
    /// kept and not acted on (`docs/AUDIO.md` §6, decision 4).
    ///
    /// # Errors
    ///
    /// `EBADFD` in `OPEN`, and `EINVAL` for a field Linux refuses.
    pub fn sw_params(&mut self, params: &mut SwParams, effects: &mut Effects) -> Result<(), Errno> {
        if self.state == STATE_OPEN {
            return Err(Errno::EBADFD);
        }
        if !(0..=TSTAMP_ENABLE).contains(&params.tstamp_mode) {
            return Err(Errno::EINVAL);
        }
        let modern = params.proto >= protocol_version(2, 0, 12);
        if modern && params.tstamp_type > TSTAMP_TYPE_MONOTONIC_RAW {
            return Err(Errno::EINVAL);
        }
        if params.avail_min == 0 {
            return Err(Errno::EINVAL);
        }
        let buffer = u64::from(self.config.buffer_frames());
        if params.silence_size >= self.boundary {
            if params.silence_threshold != 0 {
                return Err(Errno::EINVAL);
            }
        } else if params.silence_size > params.silence_threshold
            || params.silence_threshold > buffer
        {
            return Err(Errno::EINVAL);
        }
        self.software.tstamp_mode = params.tstamp_mode;
        if modern {
            self.software.tstamp_type = params.tstamp_type;
        }
        self.software.period_step = params.period_step;
        self.software.avail_min = params.avail_min;
        self.software.start_threshold = params.start_threshold;
        self.software.stop_threshold = params.stop_threshold;
        self.software.silence_threshold = params.silence_threshold;
        self.software.silence_size = params.silence_size;
        params.boundary = self.boundary;
        if self.running() {
            self.update(effects);
        }
        Ok(())
    }

    // -- pointers --------------------------------------------------------------

    fn buffer(&self) -> u64 {
        u64::from(self.config.buffer_frames())
    }

    /// `a - b` modulo `boundary`.
    fn distance(&self, a: u64, b: u64) -> u64 {
        if a >= b { a - b } else { a + self.boundary - b }
    }

    fn advance(&self, pointer: u64, frames: u64) -> u64 {
        let moved = pointer + frames;
        if self.boundary != 0 && moved >= self.boundary {
            moved - self.boundary
        } else {
            moved
        }
    }

    /// Frames queued and not yet played: `snd_pcm_playback_hw_avail`.
    #[must_use]
    pub fn queued(&self) -> u64 {
        self.distance(self.appl_ptr, self.hw_ptr)
    }

    /// Frames the program may write: `snd_pcm_playback_avail`.
    #[must_use]
    pub fn avail(&self) -> u64 {
        self.buffer().saturating_sub(self.queued())
    }

    fn running(&self) -> bool {
        matches!(self.state, STATE_RUNNING | STATE_DRAINING)
    }

    /// `DELAY`: frames between the program and the speaker, 0 unless running.
    ///
    /// # Errors
    ///
    /// `EPIPE` in `XRUN`, `EBADFD` outside `PREPARED`, `RUNNING` and
    /// `DRAINING` (`snd_pcm_delay` via `snd_pcm_hwsync`).
    pub fn delay(&self) -> Result<u64, Errno> {
        self.hwsync()?;
        Ok(if self.running() { self.queued() } else { 0 })
    }

    /// `HWSYNC`: `hw_ptr` is always current here, so only the state is
    /// judged, as `do_pcm_hwsync` judges it.
    ///
    /// # Errors
    ///
    /// `EPIPE` in `XRUN`, `EBADFD` outside `PREPARED`, `RUNNING` and
    /// `DRAINING`.
    pub const fn hwsync(&self) -> Result<(), Errno> {
        match self.state {
            STATE_DRAINING | STATE_RUNNING | STATE_PREPARED | STATE_SUSPENDED | STATE_PAUSED => {
                Ok(())
            }
            STATE_XRUN => Err(Errno::EPIPE),
            _ => Err(Errno::EBADFD),
        }
    }

    // -- starting and stopping ------------------------------------------------------

    /// `PREPARE` (`snd_pcm_prepare`): `appl_ptr` to `hw_ptr`, ready to start.
    ///
    /// # Errors
    ///
    /// `EBADFD` in `OPEN`, and `EBUSY` while running or draining.
    pub fn prepare(&mut self, effects: &mut Effects) -> Result<(), Errno> {
        match self.state {
            STATE_OPEN | STATE_DISCONNECTED => return Err(Errno::EBADFD),
            STATE_RUNNING | STATE_DRAINING => return Err(Errno::EBUSY),
            _ => {}
        }
        self.stop_device(effects);
        self.appl_ptr = self.hw_ptr;
        self.submitted = self.hw_ptr;
        self.state = STATE_PREPARED;
        Ok(())
    }

    /// `START` (`snd_pcm_start`).
    ///
    /// # Errors
    ///
    /// `EBADFD` outside `PREPARED`, and `EPIPE` with nothing queued.
    pub fn start(&mut self, now: u64, effects: &mut Effects) -> Result<(), Errno> {
        if self.state != STATE_PREPARED {
            return Err(Errno::EBADFD);
        }
        if self.queued() == 0 {
            return Err(Errno::EPIPE);
        }
        self.begin(STATE_RUNNING, now, effects);
        Ok(())
    }

    fn begin(&mut self, state: i32, now: u64, effects: &mut Effects) {
        self.state = state;
        self.trigger_tstamp = now;
        if self.software.tstamp_mode == TSTAMP_ENABLE {
            self.tstamp = now;
        }
        self.feed(effects);
    }

    /// Stop the stream in `state`, halting the device.
    fn stop(&mut self, state: i32, now: u64, effects: &mut Effects) {
        if self.running() {
            self.trigger_tstamp = now;
        }
        self.state = state;
        self.stop_device(effects);
        effects.wake = true;
    }

    /// Halt the device if it holds anything or ran since it last halted, and
    /// make everything in flight void.
    fn stop_device(&mut self, effects: &mut Effects) {
        for entry in self.in_flight.iter_mut().flatten() {
            entry.void = true;
        }
        if self.active && !self.halting {
            self.halting = true;
            effects.halt = true;
        }
        // Nothing past `hw_ptr` will play from what was submitted, so it is
        // all unsubmitted again; `feed` waits for HALTED before sending it.
        self.submitted = self.hw_ptr;
    }

    /// `DROP` (`snd_pcm_drop`): stop at once, discarding what is queued.
    ///
    /// # Errors
    ///
    /// `EBADFD` in `OPEN`.
    pub fn drop_stream(&mut self, now: u64, effects: &mut Effects) -> Result<(), Errno> {
        if matches!(self.state, STATE_OPEN | STATE_DISCONNECTED) {
            return Err(Errno::EBADFD);
        }
        self.stop(STATE_SETUP, now, effects);
        Ok(())
    }

    /// `DRAIN` (`snd_pcm_drain`'s `drain_init`): stop once what is queued has
    /// played, starting a prepared stream that holds frames.
    ///
    /// # Errors
    ///
    /// `EBADFD` in `OPEN`.
    pub fn drain(&mut self, now: u64, effects: &mut Effects) -> Result<Drain, Errno> {
        match self.state {
            STATE_OPEN | STATE_DISCONNECTED | STATE_SUSPENDED => return Err(Errno::EBADFD),
            STATE_PREPARED if self.queued() != 0 => self.begin(STATE_DRAINING, now, effects),
            STATE_PREPARED | STATE_XRUN => self.state = STATE_SETUP,
            STATE_RUNNING => {
                self.state = STATE_DRAINING;
                self.feed(effects);
            }
            _ => {}
        }
        // A drain with nothing left is over at once: no completion will come
        // to end it.
        let _ = self.drain_done(effects);
        Ok(if self.state == STATE_DRAINING {
            Drain::Wait
        } else {
            Drain::Done
        })
    }

    /// How long a drain waits for a completion before it gives up, in
    /// nanoseconds: `snd_pcm_drain`'s `max(100 ms, buffer * 1100 / rate)`.
    #[must_use]
    pub const fn drain_timeout(&self) -> u64 {
        let buffer = self.config.buffer_frames() as u64 * 1100 / self.config.rate as u64;
        let millis = if buffer > 100 { buffer } else { 100 };
        millis * 1_000_000
    }

    /// No completion came within [`Stream::drain_timeout`]: the drain ends in
    /// `SETUP`, and the program is told `EIO`, as Linux's "playback drain
    /// timeout" tells it.
    ///
    /// # Errors
    ///
    /// Always `EIO`.
    pub fn drain_expired(&mut self, now: u64, effects: &mut Effects) -> Result<(), Errno> {
        if self.state == STATE_DRAINING {
            self.stop(STATE_SETUP, now, effects);
        }
        Err(Errno::EIO)
    }

    /// `RESET` (`snd_pcm_reset`): discard what is queued, keep the state.
    ///
    /// # Errors
    ///
    /// `EBADFD` outside `RUNNING`, `PREPARED`, `PAUSED` and `SUSPENDED`.
    pub fn reset(&mut self, effects: &mut Effects) -> Result<(), Errno> {
        if !matches!(
            self.state,
            STATE_RUNNING | STATE_PREPARED | STATE_PAUSED | STATE_SUSPENDED
        ) {
            return Err(Errno::EBADFD);
        }
        self.stop_device(effects);
        self.appl_ptr = self.hw_ptr;
        self.submitted = self.hw_ptr;
        if self.running() {
            self.update(effects);
        }
        Ok(())
    }

    /// `XRUN`: force an underrun.
    ///
    /// # Errors
    ///
    /// `EBADFD` outside `RUNNING`, `PREPARED` and `PAUSED`.
    pub fn xrun(&mut self, now: u64, effects: &mut Effects) -> Result<(), Errno> {
        match self.state {
            STATE_XRUN => Ok(()),
            STATE_RUNNING | STATE_PREPARED | STATE_PAUSED => {
                self.stop(STATE_XRUN, now, effects);
                Ok(())
            }
            _ => Err(Errno::EBADFD),
        }
    }

    // -- writing -------------------------------------------------------------

    /// Where the next copy of a write goes: `__snd_pcm_lib_xfer`'s room
    /// before the end of the buffer.
    ///
    /// # Errors
    ///
    /// `EPIPE` in `XRUN`, `ESTRPIPE` suspended, and `EBADFD` outside
    /// `PREPARED`, `RUNNING` and `PAUSED` (`pcm_accessible_state`).
    pub fn room(&self) -> Result<Room, Errno> {
        match self.state {
            STATE_PREPARED | STATE_RUNNING | STATE_PAUSED => {}
            STATE_XRUN => return Err(Errno::EPIPE),
            STATE_SUSPENDED => return Err(Errno::ESTRPIPE),
            _ => return Err(Errno::EBADFD),
        }
        let buffer = self.buffer();
        let at = self.appl_ptr % buffer.max(1);
        let frames = self.avail().min(buffer - at);
        Ok(Room {
            // The buffer is a few pages, so both fit a `u32`.
            offset: (at * u64::from(self.config.frame_bytes())) as u32,
            frames: frames as u32,
        })
    }

    /// The program's frames were copied into the room [`Stream::room`] gave:
    /// move `appl_ptr`, start the stream at its threshold, and submit what is
    /// due.
    ///
    /// # Errors
    ///
    /// `EINVAL` for more frames than the room held, and what
    /// [`Stream::room`] refuses, which a state changed during the copy can
    /// bring.
    pub fn wrote(&mut self, frames: u32, now: u64, effects: &mut Effects) -> Result<(), Errno> {
        let room = self.room()?;
        if frames > room.frames {
            return Err(Errno::EINVAL);
        }
        self.appl_ptr = self.advance(self.appl_ptr, u64::from(frames));
        if self.state == STATE_PREPARED && self.queued() >= self.software.start_threshold {
            self.begin(STATE_RUNNING, now, effects);
        } else {
            self.feed(effects);
        }
        self.update(effects);
        Ok(())
    }

    /// Submit what is due (the module comment's rule).
    fn feed(&mut self, effects: &mut Effects) {
        if self.halting || !self.running() {
            return;
        }
        let period = u64::from(self.config.period_frames);
        let frame_bytes = u64::from(self.config.frame_bytes());
        loop {
            let waiting = self.distance(self.appl_ptr, self.submitted);
            let Some(slot) = self.in_flight.iter().position(Option::is_none) else {
                return;
            };
            if waiting == 0 {
                return;
            }
            let at = self.submitted % self.buffer().max(1);
            let to_boundary = period - at % period;
            let frames = waiting.min(to_boundary);
            let idle = self.in_flight.iter().all(Option::is_none);
            if frames < to_boundary && !idle && self.state != STATE_DRAINING {
                return;
            }
            let sequence = self.next_sequence;
            self.next_sequence = self.next_sequence.wrapping_add(1);
            // A submission is at most a period, and the buffer a few pages.
            let frames32 = frames as u32;
            if let Some(entry) = self.in_flight.get_mut(slot) {
                *entry = Some(InFlight {
                    sequence,
                    frames: frames32,
                    void: false,
                });
            }
            effects.push(Submit {
                sequence,
                offset: (at * frame_bytes) as u32,
                bytes: (frames * frame_bytes) as u32,
            });
            self.submitted = self.advance(self.submitted, frames);
            self.active = true;
        }
    }

    /// `snd_pcm_update_state`: the end of a drain, an underrun, and waking.
    /// Linux judges the stop threshold in whatever state the stream is when
    /// it is called, which is after a write and after the pointer moves; a
    /// prepared stream is the only other state that reaches it here.
    fn update(&mut self, effects: &mut Effects) {
        let avail = self.avail();
        self.avail_max = self.avail_max.max(avail);
        if self.drain_done(effects) {
            return;
        }
        if matches!(self.state, STATE_RUNNING | STATE_PREPARED)
            && avail >= self.software.stop_threshold
        {
            self.state = STATE_XRUN;
            self.stop_device(effects);
            effects.wake = true;
            return;
        }
        if avail >= self.software.avail_min {
            effects.wake = true;
        }
    }

    /// `snd_pcm_drain_done`, when a draining stream has played everything:
    /// set up again. The time of the stop is not known here, so the trigger
    /// keeps the drain's start.
    fn drain_done(&mut self, effects: &mut Effects) -> bool {
        if self.state != STATE_DRAINING || self.avail() < self.buffer() {
            return false;
        }
        self.state = STATE_SETUP;
        self.stop_device(effects);
        effects.wake = true;
        true
    }

    // -- the driver's reports ------------------------------------------------

    /// The oldest submission in flight, and its slot.
    fn oldest(&self) -> Option<(usize, InFlight)> {
        self.in_flight
            .iter()
            .enumerate()
            .filter_map(|(slot, entry)| entry.map(|entry| (slot, entry)))
            .min_by_key(|(_, entry)| entry.sequence.wrapping_sub(self.next_sequence))
    }

    /// ELAPSED: the device finished submission `sequence`. `played` is false
    /// for a buffer the device refused, which ends the stream in an underrun,
    /// the answer a program can recover from.
    ///
    /// # Errors
    ///
    /// A [`Lie`] for a completion that is not the oldest in flight.
    pub fn elapsed(
        &mut self,
        sequence: u32,
        played: bool,
        now: u64,
        effects: &mut Effects,
    ) -> Result<(), Lie> {
        let (slot, entry) = self.oldest().ok_or(Lie::NothingInFlight)?;
        if entry.sequence != sequence {
            return Err(Lie::Sequence {
                expected: entry.sequence,
                reported: sequence,
            });
        }
        if let Some(place) = self.in_flight.get_mut(slot) {
            *place = None;
        }
        if entry.void {
            return Ok(());
        }
        if !played {
            self.stop(STATE_XRUN, now, effects);
            return Ok(());
        }
        self.hw_ptr = self.advance(self.hw_ptr, u64::from(entry.frames));
        if self.software.tstamp_mode == TSTAMP_ENABLE {
            self.tstamp = now;
        }
        self.feed(effects);
        self.update(effects);
        Ok(())
    }

    /// HALTED: the device stopped and handed back `unplayed` submissions,
    /// which must be all that was in flight.
    ///
    /// # Errors
    ///
    /// A [`Lie`] when no HALT was asked for, or the count is wrong.
    pub fn halted(&mut self, unplayed: u32, effects: &mut Effects) -> Result<(), Lie> {
        if !self.halting {
            return Err(Lie::NotHalting);
        }
        let in_flight = self.in_flight.iter().flatten().count() as u32;
        if unplayed != in_flight {
            return Err(Lie::Unplayed {
                in_flight,
                reported: unplayed,
            });
        }
        self.in_flight = [None; MAX_IN_FLIGHT];
        self.halting = false;
        self.active = false;
        // Everything past `hw_ptr` was either void or never sent: a stream
        // written to while the device halted goes on from where it played.
        self.submitted = self.hw_ptr;
        self.feed(effects);
        effects.wake = true;
        Ok(())
    }

    // -- what the program reads ---------------------------------------------------

    /// `poll` (`snd_pcm_poll`).
    #[must_use]
    pub fn poll(&self) -> Poll {
        match self.state {
            STATE_RUNNING | STATE_PREPARED | STATE_PAUSED => Poll {
                writable: self.avail() >= self.software.avail_min,
                error: false,
            },
            STATE_DRAINING => Poll {
                writable: false,
                error: false,
            },
            _ => Poll {
                writable: true,
                error: true,
            },
        }
    }

    /// `STATUS` and `STATUS_EXT` (`snd_pcm_status64`), at `now`.
    pub fn status(&mut self, now: u64) -> Status {
        let mut status = Status {
            state: self.state,
            trigger_tstamp: Timespec { sec: 0, nsec: 0 },
            tstamp: Timespec { sec: 0, nsec: 0 },
            appl_ptr: 0,
            hw_ptr: 0,
            delay: 0,
            avail: 0,
            avail_max: 0,
            overrange: 0,
            suspended_state: 0,
            audio_tstamp_data: 0,
            audio_tstamp: Timespec { sec: 0, nsec: 0 },
            driver_tstamp: Timespec { sec: 0, nsec: 0 },
            audio_tstamp_accuracy: 0,
            reserved: [0; 20],
        };
        if self.state == STATE_OPEN {
            return status;
        }
        status.trigger_tstamp = nanos(self.trigger_tstamp);
        if self.software.tstamp_mode == TSTAMP_ENABLE {
            let at = if self.running() { self.tstamp } else { now };
            status.tstamp = nanos(at);
            if self.running() {
                status.driver_tstamp = nanos(self.tstamp);
            }
        }
        status.appl_ptr = self.appl_ptr;
        status.hw_ptr = self.hw_ptr;
        status.avail = self.avail();
        status.delay = if self.running() {
            // At most a buffer, far inside an `i64`.
            self.queued() as i64
        } else {
            0
        };
        status.avail_max = self.avail_max;
        self.avail_max = 0;
        status
    }

    /// `SYNC_PTR` (`snd_pcm_sync_ptr`): take the program's `appl_ptr` and
    /// `avail_min` unless its flags ask for the core's, and answer the status
    /// half and the control half.
    ///
    /// Moving `appl_ptr` this way is how a program with mapped access
    /// commits frames, which a card here does not offer, so a value other
    /// than the current one is refused with `EPERM` rather than taken: a
    /// written deviation (`docs/AUDIO.md` §6, decision 4).
    ///
    /// # Errors
    ///
    /// What [`Stream::hwsync`] refuses when `SYNC_PTR_HWSYNC` is set,
    /// `EINVAL` for an `appl_ptr` at or past `boundary`, and `EPERM` for one
    /// that moves.
    pub fn sync_ptr(
        &mut self,
        flags: u32,
        appl_ptr: u64,
        avail_min: u64,
    ) -> Result<(MmapStatus, u64, u64), Errno> {
        if flags & SYNC_PTR_HWSYNC != 0 {
            self.hwsync()?;
        }
        if flags & SYNC_PTR_APPL == 0 && appl_ptr != self.appl_ptr {
            if appl_ptr >= self.boundary {
                return Err(Errno::EINVAL);
            }
            return Err(Errno::EPERM);
        }
        if flags & SYNC_PTR_AVAIL_MIN == 0 {
            self.software.avail_min = avail_min;
        }
        let status = MmapStatus {
            state: self.state,
            hw_ptr: self.hw_ptr,
            tstamp: nanos(self.tstamp),
            suspended_state: 0,
            audio_tstamp: Timespec { sec: 0, nsec: 0 },
        };
        Ok((status, self.appl_ptr, self.software.avail_min))
    }

    /// The protocol version the program said it speaks, or [`PCM_VERSION`]
    /// if it said none.
    #[must_use]
    pub const fn user_version(&self) -> u32 {
        if self.user_version == 0 {
            PCM_VERSION
        } else {
            self.user_version
        }
    }

    /// Whether the clock chosen is monotonic, for the glue.
    #[must_use]
    pub const fn clock_is_monotonic(&self) -> bool {
        self.software.tstamp_type == TSTAMP_TYPE_MONOTONIC
            || self.software.tstamp_type == TSTAMP_TYPE_MONOTONIC_RAW
    }
}
