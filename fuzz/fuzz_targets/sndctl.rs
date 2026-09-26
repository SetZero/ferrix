//! Fuzz the sound control protocol and the PCM stream behind it.
//!
//! The driver runs in ring 3 and the program in user space; the core believes
//! neither. The input is first taken as one message, then as a script: a
//! program's requests, a device's completions and a driver's reports, in any
//! order, some of them lies.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **A message is its bytes**: whatever decodes encodes back to exactly
//!    the bytes it came from.
//! 2. **The program never overruns the device**: frames queued never exceed
//!    the buffer, and `avail` plus queued is the buffer.
//! 3. **What reaches the driver is sane**: at most `MAX_IN_FLIGHT` in flight,
//!    each a whole number of frames, never empty, never across a period
//!    boundary, and nothing new while a HALT is outstanding.
//! 4. **A lie is refused**: a completion that is not the oldest in flight,
//!    or a HALTED with the wrong count, is an error, never an effect.

#![no_main]

use std::collections::VecDeque;

use ferrix_linux_abi::socket::Width;
use ferrix_linux_abi::sound::{HwParams, Interval, Mask, PCM_VERSION, SwParams};
use ferrix_sndctl::message::Message;
use ferrix_sndctl::pcm::{Effects, MAX_IN_FLIGHT, Stream, Submit, VERSION_1};
use ferrix_sndctl::refine::ANY;
use libfuzzer_sys::fuzz_target;

const PERIOD: u32 = 960;
const BUFFER: u64 = 3840;

fn any() -> HwParams {
    HwParams {
        flags: 0,
        masks: [Mask { bits: [u32::MAX; 8] }; 3],
        mres: [Mask { bits: [0; 8] }; 5],
        intervals: [ANY; 12],
        ires: [Interval { min: 0, max: 0, flags: 0 }; 9],
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

struct Device {
    queue: VecDeque<Submit>,
    halting: bool,
}

impl Device {
    fn take(&mut self, effects: &Effects) {
        for submit in effects.submits() {
            assert!(!self.halting, "a submission while HALT is outstanding");
            assert!(submit.bytes > 0 && submit.bytes % 4 == 0 && submit.offset % 4 == 0);
            let first = submit.offset / 4;
            let last = first + submit.bytes / 4 - 1;
            assert_eq!(first / PERIOD, last / PERIOD, "across a period boundary");
            self.queue.push_back(submit);
        }
        if effects.halt {
            self.halting = true;
        }
        assert!(self.queue.len() <= MAX_IN_FLIGHT);
    }
}

fn step(stream: &mut Stream, device: &mut Device, op: u8, arg: u8, now: u64) {
    let mut effects = Effects::default();
    match op % 13 {
        0 => {
            let mut left = u32::from(arg) * 16;
            while left > 0 {
                let Ok(room) = stream.room() else { break };
                let frames = room.frames.min(left);
                if frames == 0 {
                    break;
                }
                stream.wrote(frames, now, &mut effects).expect("a write that fits its room");
                left -= frames;
            }
        }
        1 => {
            if let Some(submit) = device.queue.pop_front() {
                stream
                    .elapsed(submit.sequence, arg != 0, now, &mut effects)
                    .expect("the oldest completes");
            }
        }
        2 => {
            // A lie: any sequence but the oldest's.
            let oldest = device.queue.front().map(|s| s.sequence);
            let told = u32::from(arg);
            let result = stream.elapsed(told, true, now, &mut effects);
            if oldest != Some(told) {
                assert!(result.is_err(), "a completion out of turn was taken");
                assert_eq!(effects, Effects::default(), "a lie had an effect");
            } else {
                let _ = device.queue.pop_front();
            }
        }
        3 => {
            let unplayed = device.queue.len() as u32;
            let told = if arg & 1 == 0 { unplayed } else { unplayed + 1 };
            let result = stream.halted(told, &mut effects);
            if device.halting && told == unplayed {
                result.expect("a true HALTED");
                device.queue.clear();
                device.halting = false;
            } else {
                assert!(result.is_err(), "a false HALTED was taken");
            }
        }
        4 => { let _ = stream.drop_stream(now, &mut effects); }
        5 => { let _ = stream.prepare(&mut effects); }
        6 => { let _ = stream.drain(now, &mut effects); }
        7 => { let _ = stream.drain_expired(now, &mut effects); }
        8 => { let _ = stream.start(now, &mut effects); }
        9 => { let _ = stream.reset(&mut effects); }
        10 => {
            let value = u64::from(arg) * 64;
            let mut params = SwParams {
                tstamp_mode: i32::from(arg & 1),
                period_step: 1,
                sleep_min: 0,
                avail_min: value.max(1),
                xfer_align: 1,
                start_threshold: value,
                stop_threshold: if arg & 2 == 0 { BUFFER } else { value },
                silence_threshold: 0,
                silence_size: 0,
                boundary: 0,
                proto: PCM_VERSION,
                tstamp_type: 1,
                reserved: [0; 56],
            };
            let _ = stream.sw_params(&mut params, &mut effects);
        }
        11 => {
            let _ = stream.sync_ptr(u32::from(arg & 7), u64::from(arg), u64::from(arg));
            let _ = stream.status(now);
            let _ = stream.poll();
            let _ = stream.delay();
        }
        _ => {
            if arg & 1 == 0 {
                stream.close(&mut effects);
                stream.open(if arg & 2 == 0 { Width::Bits64 } else { Width::Bits32 });
            } else {
                let _ = stream.hw_params(&mut any(), &mut effects);
            }
        }
    }
    device.take(&effects);
    assert_eq!(stream.halting(), device.halting, "the core and the device agree on HALT");
    assert!(stream.queued() <= BUFFER, "the program overran the device");
    assert_eq!(stream.avail() + stream.queued(), BUFFER);
}

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = Message::decode(data) {
        assert_eq!(message.encode().as_bytes(), data, "a message is its bytes");
    }

    let mut stream = Stream::new(VERSION_1);
    stream.open(Width::Bits64);
    let mut device = Device { queue: VecDeque::new(), halting: false };
    let mut effects = Effects::default();
    stream.hw_params(&mut any(), &mut effects).expect("HW_PARAMS");
    stream.prepare(&mut effects).expect("PREPARE");
    device.take(&effects);
    for (index, pair) in data.chunks_exact(2).enumerate() {
        step(&mut stream, &mut device, pair[0], pair[1], index as u64 * 1_000_000);
    }
});
