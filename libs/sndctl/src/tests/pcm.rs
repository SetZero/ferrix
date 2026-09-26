//! The stream, driven the way the glue drives it: a program writing into a
//! model of the buffer, a device completing submissions by reading from it,
//! and every rule of `docs/AUDIO.md` §3.1 checked against what comes out.

use super::std::collections::VecDeque;
use super::std::vec;
use super::std::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::socket::Width;
use ferrix_linux_abi::sound::{
    HwParams, Interval, Mask, PCM_VERSION, STATE_DISCONNECTED, STATE_DRAINING, STATE_OPEN,
    STATE_PREPARED, STATE_RUNNING, STATE_SETUP, STATE_XRUN, SYNC_PTR_APPL, SYNC_PTR_AVAIL_MIN,
    SYNC_PTR_HWSYNC, SwParams,
};

use crate::pcm::{Drain, Effects, Lie, Poll, Stream, Submit, VERSION_1, boundary};
use crate::refine::ANY;

const PERIOD: usize = 960;
const BUFFER: usize = 3840;

fn any() -> HwParams {
    HwParams {
        flags: 0,
        masks: [Mask {
            bits: [u32::MAX; 8],
        }; 3],
        mres: [Mask { bits: [0; 8] }; 5],
        intervals: [ANY; 12],
        ires: [Interval {
            min: 0,
            max: 0,
            flags: 0,
        }; 9],
        rmask: u32::MAX,
        cmask: 0,
        info: 0,
        msbits: 0,
        rate_num: 0,
        rate_den: 0,
        fifo_size: 0,
        sync: [0; 16],
        reserved: [0; 48],
    }
}

fn software(start: u64, stop: u64, avail_min: u64) -> SwParams {
    SwParams {
        tstamp_mode: 0,
        period_step: 1,
        sleep_min: 0,
        avail_min,
        xfer_align: 1,
        start_threshold: start,
        stop_threshold: stop,
        silence_threshold: 0,
        silence_size: 0,
        boundary: 0,
        proto: PCM_VERSION,
        tstamp_type: 1,
        reserved: [0; 56],
    }
}

/// A program, a buffer and a device around one stream.
struct Rig {
    stream: Stream,
    /// The buffer, one frame id per frame.
    buffer: Vec<u32>,
    /// Submissions the device holds, oldest first.
    device: VecDeque<Submit>,
    /// Frame ids in the order the device played them.
    played: Vec<u32>,
    halts: usize,
    now: u64,
}

impl Rig {
    fn new(width: Width) -> Rig {
        let mut stream = Stream::new(VERSION_1);
        stream.open(width);
        let mut effects = Effects::default();
        stream
            .hw_params(&mut any(), &mut effects)
            .expect("HW_PARAMS");
        stream.prepare(&mut effects).expect("PREPARE");
        let mut rig = Rig {
            stream,
            buffer: vec![0; BUFFER],
            device: VecDeque::new(),
            played: Vec::new(),
            halts: 0,
            now: 1_000,
        };
        rig.take(&effects);
        rig
    }

    fn take(&mut self, effects: &Effects) {
        for submit in effects.submits() {
            assert_eq!(submit.offset % 4, 0, "a submission starts on a frame");
            assert_eq!(submit.bytes % 4, 0, "a submission is whole frames");
            let first = submit.offset as usize / 4;
            let frames = submit.bytes as usize / 4;
            assert!(frames > 0 && frames <= PERIOD, "at most a period");
            assert_eq!(
                first / PERIOD,
                (first + frames - 1) / PERIOD,
                "{submit:?} crosses a period boundary"
            );
            self.device.push_back(submit);
        }
        if effects.halt {
            self.halts += 1;
        }
    }

    fn sw(&mut self, mut params: SwParams) {
        let mut effects = Effects::default();
        self.stream
            .sw_params(&mut params, &mut effects)
            .expect("SW_PARAMS");
        self.take(&effects);
    }

    /// Write `frames` as far as the buffer takes them, as `WRITEI_FRAMES`
    /// does in its copy loop; say how many went.
    fn write(&mut self, frames: &[u32]) -> Result<usize, Errno> {
        let mut done = 0;
        while done < frames.len() {
            let room = self.stream.room()?;
            let count = (room.frames as usize).min(frames.len() - done);
            if count == 0 {
                break;
            }
            let at = room.offset as usize / 4;
            self.buffer[at..at + count].copy_from_slice(&frames[done..done + count]);
            let mut effects = Effects::default();
            self.stream.wrote(count as u32, self.now, &mut effects)?;
            self.take(&effects);
            done += count;
        }
        Ok(done)
    }

    /// The device finishes its oldest submission, reading it from the buffer
    /// only now, so a program that overwrote it would be caught.
    fn complete(&mut self) -> Submit {
        let submit = self.device.pop_front().expect("something is in flight");
        let first = submit.offset as usize / 4;
        self.played
            .extend_from_slice(&self.buffer[first..first + submit.bytes as usize / 4]);
        self.now += 5_000_000;
        let mut effects = Effects::default();
        self.stream
            .elapsed(submit.sequence, true, self.now, &mut effects)
            .expect("the oldest completes");
        self.take(&effects);
        submit
    }

    fn halted(&mut self) {
        let unplayed = self.device.len() as u32;
        self.device.clear();
        let mut effects = Effects::default();
        self.stream.halted(unplayed, &mut effects).expect("HALTED");
        self.take(&effects);
    }
}

fn counter(from: u32, count: usize) -> Vec<u32> {
    (from..).take(count).collect()
}

#[test]
fn a_second_of_a_counter_plays_whole_in_order_and_drains() {
    let mut rig = Rig::new(Width::Bits64);
    let frames = counter(1, 48_000);
    let mut written = 0;
    while written < frames.len() {
        let end = (written + 700).min(frames.len());
        let went = rig.write(&frames[written..end]).expect("RUNNING");
        written += went;
        if went == 0 {
            let _ = rig.complete();
        }
    }
    assert_eq!(rig.stream.state(), STATE_RUNNING);
    let mut effects = Effects::default();
    assert_eq!(rig.stream.drain(rig.now, &mut effects), Ok(Drain::Wait));
    rig.take(&effects);
    while !rig.device.is_empty() {
        let _ = rig.complete();
    }
    assert_eq!(rig.stream.state(), STATE_SETUP, "the drain ended");
    assert_eq!(rig.halts, 1, "and the device was halted");
    assert_eq!(rig.played, frames, "every frame, once, in order");
    rig.halted();
}

#[test]
fn a_write_starts_at_the_threshold_and_a_partial_period_goes_only_when_idle() {
    let mut rig = Rig::new(Width::Bits64);
    // HW_PARAMS' default start threshold is 1: the first write starts it,
    // and with nothing in flight its 700 frames go at once.
    assert_eq!(rig.write(&counter(1, 700)), Ok(700));
    assert_eq!(rig.stream.state(), STATE_RUNNING);
    assert_eq!(rig.device.len(), 1);
    assert_eq!(rig.device[0].bytes, 700 * 4);
    // The next 700 fill that period (260) and start another (440), which
    // waits while something is in flight.
    assert_eq!(rig.write(&counter(701, 700)), Ok(700));
    assert_eq!(rig.device.len(), 2);
    assert_eq!(rig.device[1].bytes, 260 * 4);
    let _ = rig.complete();
    assert_eq!(rig.device.len(), 1, "the 440 still wait");
    let _ = rig.complete();
    assert_eq!(rig.device.len(), 1, "idle: the 440 go");
    assert_eq!(rig.device[0].bytes, 440 * 4);
    assert_eq!(rig.stream.hw_ptr(), 960);
}

#[test]
fn a_start_threshold_holds_the_stream_until_it_is_met() {
    let mut rig = Rig::new(Width::Bits64);
    rig.sw(software(BUFFER as u64, BUFFER as u64, PERIOD as u64));
    assert_eq!(rig.write(&counter(1, 3000)), Ok(3000));
    assert_eq!(rig.stream.state(), STATE_PREPARED);
    assert!(rig.device.is_empty(), "nothing goes before the start");
    assert_eq!(rig.write(&counter(3001, 840)), Ok(840));
    assert_eq!(rig.stream.state(), STATE_RUNNING);
    assert_eq!(rig.device.len(), 4, "four whole periods");
    assert_eq!(rig.write(&counter(1, 1)), Ok(0), "the buffer is full");
}

#[test]
fn a_device_that_runs_dry_is_an_underrun_that_a_prepare_recovers() {
    let mut rig = Rig::new(Width::Bits64);
    assert_eq!(rig.write(&counter(1, PERIOD)), Ok(PERIOD));
    let _ = rig.complete();
    assert_eq!(
        rig.stream.state(),
        STATE_XRUN,
        "everything played, nothing queued"
    );
    assert_eq!(rig.halts, 1);
    assert_eq!(rig.stream.room(), Err(Errno::EPIPE));
    assert!(rig.stream.poll().error);
    rig.halted();
    let mut effects = Effects::default();
    rig.stream
        .prepare(&mut effects)
        .expect("PREPARE after XRUN");
    assert_eq!(rig.stream.state(), STATE_PREPARED);
    assert_eq!(rig.stream.appl_ptr(), rig.stream.hw_ptr());
    assert_eq!(rig.write(&counter(1, 10)), Ok(10), "and it plays again");
}

#[test]
fn a_stop_threshold_at_the_boundary_never_underruns() {
    let mut rig = Rig::new(Width::Bits64);
    let wrap = boundary(BUFFER as u64, Width::Bits64);
    rig.sw(software(1, wrap, PERIOD as u64));
    assert_eq!(rig.write(&counter(1, 100)), Ok(100));
    let _ = rig.complete();
    assert_eq!(rig.stream.state(), STATE_RUNNING, "dry, and still running");
    assert_eq!(rig.write(&counter(101, 100)), Ok(100));
    assert_eq!(rig.device.len(), 1, "an idle device is fed at once");
    let _ = rig.complete();
    assert_eq!(rig.played, counter(1, 200));
    // A drain with nothing left ends at once.
    let mut effects = Effects::default();
    assert_eq!(rig.stream.drain(rig.now, &mut effects), Ok(Drain::Done));
    assert_eq!(rig.stream.state(), STATE_SETUP);
}

#[test]
fn a_drop_voids_what_is_in_flight() {
    let mut rig = Rig::new(Width::Bits64);
    assert_eq!(rig.write(&counter(1, 2000)), Ok(2000));
    let in_flight = rig.device.len();
    assert!(in_flight >= 2);
    let mut effects = Effects::default();
    rig.stream.drop_stream(rig.now, &mut effects).expect("DROP");
    rig.take(&effects);
    assert_eq!(rig.stream.state(), STATE_SETUP);
    assert_eq!(rig.halts, 1);
    // The device finishes one before it halts: accepted, and it moves nothing.
    let hw = rig.stream.hw_ptr();
    let _ = rig.complete();
    assert_eq!(rig.stream.hw_ptr(), hw);
    // The rest come back unplayed; the count must be exact.
    let mut effects = Effects::default();
    let left = rig.device.len() as u32;
    assert_eq!(
        rig.stream.halted(left + 1, &mut effects),
        Err(Lie::Unplayed {
            in_flight: left,
            reported: left + 1
        })
    );
    rig.halted();
    assert!(!rig.stream.halting());
    // A new prepare and write submit again, from where the device stopped.
    let mut effects = Effects::default();
    rig.stream.prepare(&mut effects).expect("PREPARE");
    assert_eq!(rig.write(&counter(9000, 10)), Ok(10));
    assert_eq!(rig.device.len(), 1);
}

#[test]
fn a_driver_that_lies_about_completions_is_caught() {
    let mut rig = Rig::new(Width::Bits64);
    let mut effects = Effects::default();
    assert_eq!(
        rig.stream.elapsed(0, true, 0, &mut effects),
        Err(Lie::NothingInFlight)
    );
    assert_eq!(rig.write(&counter(1, 2000)), Ok(2000));
    let second = rig.device[1].sequence;
    assert_eq!(
        rig.stream.elapsed(second, true, 0, &mut effects),
        Err(Lie::Sequence {
            expected: rig.device[0].sequence,
            reported: second
        })
    );
    assert_eq!(rig.stream.halted(0, &mut effects), Err(Lie::NotHalting));
}

#[test]
fn a_refused_buffer_is_an_underrun() {
    let mut rig = Rig::new(Width::Bits64);
    assert_eq!(rig.write(&counter(1, 500)), Ok(500));
    let submit = rig.device.pop_front().expect("in flight");
    let mut effects = Effects::default();
    rig.stream
        .elapsed(submit.sequence, false, 0, &mut effects)
        .expect("reported in order");
    assert_eq!(rig.stream.state(), STATE_XRUN);
    assert!(effects.halt);
}

#[test]
fn each_request_is_refused_in_the_states_linux_refuses_it() {
    let mut stream = Stream::new(VERSION_1);
    stream.open(Width::Bits32);
    let mut effects = Effects::default();
    assert_eq!(stream.prepare(&mut effects), Err(Errno::EBADFD), "OPEN");
    assert_eq!(stream.room(), Err(Errno::EBADFD));
    assert_eq!(stream.hw_free(&mut effects), Err(Errno::EBADFD));
    assert_eq!(stream.drain(0, &mut effects), Err(Errno::EBADFD));
    assert_eq!(stream.drop_stream(0, &mut effects), Err(Errno::EBADFD));
    assert_eq!(
        stream.sw_params(&mut software(1, 1, 1), &mut effects),
        Err(Errno::EBADFD)
    );
    stream
        .hw_params(&mut any(), &mut effects)
        .expect("HW_PARAMS");
    assert_eq!(stream.state(), STATE_SETUP);
    assert_eq!(stream.start(0, &mut effects), Err(Errno::EBADFD), "SETUP");
    stream.prepare(&mut effects).expect("PREPARE");
    assert_eq!(
        stream.start(0, &mut effects),
        Err(Errno::EPIPE),
        "nothing queued"
    );
    assert_eq!(
        stream.sw_params(&mut software(1, 1, 0), &mut effects),
        Err(Errno::EINVAL),
        "avail_min 0"
    );
    let mut silence = software(1, 1, 1);
    silence.silence_threshold = BUFFER as u64 + 1;
    silence.silence_size = 1;
    assert_eq!(
        stream.sw_params(&mut silence, &mut effects),
        Err(Errno::EINVAL)
    );
    assert_eq!(stream.delay(), Ok(0), "not running");
    stream.disconnect(&mut effects);
    assert_eq!(stream.state(), STATE_DISCONNECTED);
    assert_eq!(stream.check_connected(), Err(Errno::EBADFD));
    assert_eq!(
        stream.poll(),
        Poll {
            writable: true,
            error: true
        }
    );
}

#[test]
fn sw_params_writes_back_the_boundary_linux_computes() {
    // `while (boundary * 2 <= LONG_MAX - buffer_size) boundary *= 2`.
    assert_eq!(boundary(3840, Width::Bits32), 3840 << 19);
    assert!(boundary(3840, Width::Bits64) * 2 > i64::MAX as u64 - 3840);
    assert!(boundary(3840, Width::Bits64) <= i64::MAX as u64 - 3840);
    let mut rig = Rig::new(Width::Bits32);
    let mut params = software(1, BUFFER as u64, PERIOD as u64);
    let mut effects = Effects::default();
    rig.stream
        .sw_params(&mut params, &mut effects)
        .expect("SW_PARAMS");
    assert_eq!(params.boundary, 3840 << 19);
}

#[test]
fn sync_ptr_reports_and_takes_as_the_flags_say() {
    let mut rig = Rig::new(Width::Bits64);
    assert_eq!(rig.write(&counter(1, 1000)), Ok(1000));
    let _ = rig.complete();
    // alsa-lib after a write: report both.
    let (status, appl, avail_min) = rig
        .stream
        .sync_ptr(SYNC_PTR_HWSYNC | SYNC_PTR_APPL | SYNC_PTR_AVAIL_MIN, 0, 0)
        .expect("SYNC_PTR");
    assert_eq!(status.state, STATE_RUNNING);
    assert_eq!(status.hw_ptr, rig.stream.hw_ptr());
    assert_eq!(appl, 1000);
    assert_eq!(avail_min, PERIOD as u64);
    // Taking `avail_min` and the same `appl_ptr`.
    let (_, _, taken) = rig
        .stream
        .sync_ptr(0, 1000, 480)
        .expect("the same appl_ptr is taken");
    assert_eq!(taken, 480);
    // Moving `appl_ptr` is how mapped access commits, which is refused.
    assert_eq!(rig.stream.sync_ptr(0, 1200, 480), Err(Errno::EPERM));
    assert_eq!(rig.stream.sync_ptr(0, u64::MAX, 480), Err(Errno::EINVAL));
}

#[test]
fn status_delay_and_poll_follow_the_pointers() {
    let mut rig = Rig::new(Width::Bits64);
    assert_eq!(
        rig.stream.poll(),
        Poll {
            writable: true,
            error: false
        }
    );
    assert_eq!(rig.write(&counter(1, BUFFER)), Ok(BUFFER));
    assert!(!rig.stream.poll().writable, "full");
    let status = rig.stream.status(rig.now);
    assert_eq!(status.state, STATE_RUNNING);
    assert_eq!(status.avail, 0);
    assert_eq!(status.delay, BUFFER as i64);
    assert_eq!(rig.stream.delay(), Ok(BUFFER as u64));
    let _ = rig.complete();
    assert!(rig.stream.poll().writable, "a period free, avail_min met");
    assert_eq!(rig.stream.status(rig.now).avail, PERIOD as u64);
}

#[test]
fn a_drain_that_hears_nothing_ends_in_eio() {
    let mut rig = Rig::new(Width::Bits64);
    assert_eq!(rig.write(&counter(1, 500)), Ok(500));
    let mut effects = Effects::default();
    assert_eq!(rig.stream.drain(rig.now, &mut effects), Ok(Drain::Wait));
    assert_eq!(rig.stream.state(), STATE_DRAINING);
    assert_eq!(
        rig.stream.drain_timeout(),
        100_000_000,
        "max(100 ms, 88 ms)"
    );
    assert!(!rig.stream.poll().writable);
    assert_eq!(
        rig.stream.drain_expired(rig.now, &mut effects),
        Err(Errno::EIO)
    );
    assert_eq!(rig.stream.state(), STATE_SETUP);
    assert!(effects.halt);
}

#[test]
fn a_close_halts_and_the_next_open_waits_for_it() {
    let mut rig = Rig::new(Width::Bits64);
    assert_eq!(rig.write(&counter(1, 2000)), Ok(2000));
    let mut effects = Effects::default();
    rig.stream.close(&mut effects);
    assert!(effects.halt);
    rig.stream.open(Width::Bits64);
    assert_eq!(rig.stream.state(), STATE_OPEN);
    assert!(
        rig.stream.halting(),
        "the old open's device has not halted yet"
    );
    let mut effects = Effects::default();
    rig.stream
        .hw_params(&mut any(), &mut effects)
        .expect("HW_PARAMS");
    rig.stream.prepare(&mut effects).expect("PREPARE");
    assert_eq!(rig.write(&counter(1, 10)), Ok(10));
    let before = rig.device.len();
    // Nothing new goes until HALTED; then the queued frames go.
    rig.halted();
    assert!(rig.device.len() == 1 && before > 0);
    assert_eq!(rig.device[0].bytes, 40);
}
