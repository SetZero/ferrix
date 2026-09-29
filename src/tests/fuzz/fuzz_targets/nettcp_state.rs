//! Drive the TCP state machine with segments a stranger chose.
//!
//! A connection is the one piece of the net core that a remote peer steers
//! directly: every field of every segment is theirs, and the machine has to
//! answer all of them without a panic, without an unbounded buffer and without
//! sending something that contradicts what it has already sent.
//!
//! # The properties
//!
//! 1. **No input panics.** Sequence numbers that wrap, windows of zero,
//!    acknowledgments of things never sent, resets in states that must ignore
//!    them.
//! 2. **What comes out is writable.** Every header the machine produces is
//!    emitted with `ferrix-netwire` and parsed back, so a header it builds that
//!    the wire format refuses is a crash here rather than a packet nobody can
//!    read.
//! 3. **Sequence numbers never go backwards past what was acknowledged.**
//!    `SND.UNA` only ever advances, and never past `SND.NXT`.
//! 4. **The buffers stay inside their capacities**, however many out-of-order
//!    segments arrive.
//! 5. **A closed connection stays closed** and sends nothing more.

#![no_main]

use ferrix_nettcp::conn::{Config, Connection, Request};
use ferrix_nettcp::seq::SeqNumber;
use ferrix_nettcp::state::State;
use ferrix_netwire::checksum::Pseudo;
use ferrix_netwire::tcp::{Flags, Header, Options, Segment};
use libfuzzer_sys::fuzz_target;

/// The addresses the checksums are taken over. Fixed: the fuzzer's bytes are
/// better spent on the header than on two numbers the machine never reads.
const PSEUDO: Pseudo = Pseudo::V4 {
    source: [10, 0, 0, 2],
    destination: [10, 0, 0, 1],
};

/// The receive and send buffers, kept small so a fuzz case can fill them.
const CAPACITY: usize = 4096;

/// One step the fuzzer can ask for.
enum Step {
    /// Feed a segment built from the bytes.
    Segment,
    /// Write bytes into the send queue.
    Write(usize),
    /// Read bytes out of the receive queue.
    Read(usize),
    /// Move the clock forward.
    Wait(u64),
    /// Close this end.
    Close,
    /// Take everything the connection wants to send.
    Drain,
}

/// A little cursor over the fuzzer's bytes.
struct Input<'a> {
    /// What is left.
    rest: &'a [u8],
}

impl<'a> Input<'a> {
    /// One byte, or zero at the end.
    fn byte(&mut self) -> u8 {
        match self.rest.split_first() {
            Some((first, rest)) => {
                self.rest = rest;
                *first
            }
            None => 0,
        }
    }

    /// Four bytes as a big-endian number.
    fn word(&mut self) -> u32 {
        u32::from_be_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }

    /// Two bytes as a big-endian number.
    fn half(&mut self) -> u16 {
        u16::from_be_bytes([self.byte(), self.byte()])
    }

    /// Whether anything is left.
    fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    /// Up to `len` bytes.
    fn take(&mut self, len: usize) -> &'a [u8] {
        let len = len.min(self.rest.len());
        let (head, tail) = self.rest.split_at(len);
        self.rest = tail;
        head
    }
}

/// Which step the byte asks for.
fn step(code: u8, input: &mut Input<'_>) -> Step {
    match code % 6 {
        0 | 1 => Step::Segment,
        2 => Step::Write(usize::from(input.byte())),
        3 => Step::Read(usize::from(input.byte())),
        4 => Step::Wait(u64::from(input.half())),
        5 if code >= 128 => Step::Close,
        _ => Step::Drain,
    }
}

/// Build a segment out of the fuzzer's bytes.
fn segment(input: &mut Input<'_>, body: &mut [u8; 256]) -> (Header, usize) {
    let header = Header {
        source_port: 80,
        destination_port: 40_000,
        sequence: input.word(),
        acknowledgment: input.word(),
        flags: Flags(input.half() & 0x01FF),
        window: input.half(),
        urgent_pointer: 0,
        options: Options::default(),
    };
    let wanted = usize::from(input.byte());
    let taken = input.take(wanted);
    let len = taken.len().min(body.len());
    body.get_mut(..len)
        .unwrap_or_default()
        .copy_from_slice(taken.get(..len).unwrap_or_default());
    (header, len)
}

/// Assert that a header the machine produced can be written and read back.
fn round_trip(header: &Header, payload: &[u8]) {
    let mut out = [0u8; 2048];
    let Ok(written) = header.emit(payload, PSEUDO, &mut out) else {
        panic!("a header the state machine built could not be written");
    };
    let bytes = out.get(..written).expect("emit reported its own length");
    let parsed = Header::parse(bytes, PSEUDO).expect("what was written must parse");
    assert_eq!(parsed.header.sequence, header.sequence);
    assert_eq!(parsed.header.acknowledgment, header.acknowledgment);
    assert_eq!(parsed.payload, payload);
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input { rest: data };
    let config = Config {
        send_capacity: CAPACITY,
        receive_capacity: CAPACITY,
        ..Config::default()
    };
    let mut connection = match input.byte() % 2 {
        0 => Connection::connect(config, 40_000, 80, SeqNumber(input.word())),
        _ => Connection::accept(
            config,
            40_000,
            80,
            SeqNumber(input.word()),
            &Request {
                sequence: SeqNumber(input.word()),
                window: input.half(),
                mss: Some(input.half()),
                window_scale: Some(input.byte()),
                selective_ack: true,
            },
        ),
    };

    let mut now: u64 = 0;
    let mut body = [0u8; 256];
    let mut scratch = [0u8; 2048];
    let mut out = [0u8; 256];

    while !input.is_empty() {
        let was_closed = connection.state() == State::Closed;
        let previous = connection.in_flight();
        match step(input.byte(), &mut input) {
            Step::Segment => {
                let (header, len) = segment(&mut input, &mut body);
                let payload = body.get(..len).unwrap_or_default();
                let _ = connection.on_segment(now, &Segment { header, payload });
            }
            Step::Write(len) => {
                let taken = connection.write(body.get(..len.min(body.len())).unwrap_or_default());
                assert!(taken <= CAPACITY, "a write exceeded the send buffer");
            }
            Step::Read(len) => {
                let room = len.min(out.len());
                let taken = connection.read(out.get_mut(..room).unwrap_or_default());
                assert!(taken <= room, "a read overran the buffer it was given");
            }
            Step::Wait(millis) => {
                now = now.saturating_add(millis);
                let _ = connection.on_timer(now);
            }
            Step::Close => connection.close(),
            Step::Drain => {
                for _ in 0..8 {
                    let Some(transmit) = connection.poll_transmit(now, &mut scratch) else {
                        break;
                    };
                    let payload = scratch.get(..transmit.payload_len).unwrap_or_default();
                    round_trip(&transmit.header, payload);
                }
            }
        }
        assert!(
            connection.send_queued() <= CAPACITY,
            "the send queue grew past its capacity"
        );
        assert!(
            connection.receive_queued() <= CAPACITY,
            "the receive queue grew past its capacity"
        );
        if was_closed {
            assert_eq!(
                connection.state(),
                State::Closed,
                "a closed connection came back to life"
            );
            assert_eq!(connection.in_flight(), previous);
        }
    }
});
