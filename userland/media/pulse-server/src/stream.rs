//! One playback stream: the bytes a client has written and the card has not
//! yet taken, what it has been asked for and not yet sent, and where playback
//! is.
//!
//! The accounting is `pa_memblockq`'s, as `PulseAudio`'s `protocol-native.c`
//! drives it for a playback stream. The server asks for what brings the queue
//! up to `tlength`, counting what it has asked for and not been sent, and only
//! once that is at least `minreq`, so that a client is not woken for a few
//! bytes at a time. Playback waits for `prebuf` bytes before it starts, and
//! again after an underrun; a stream created with `prebuf` 0 starts at once.
//! `STARTED` goes to the client when the card first takes bytes after a start,
//! as `PulseAudio` sends it once the sink renders the stream.

use std::collections::VecDeque;

use pulseaudio::protocol::stream::BufferAttr;
use pulseaudio::protocol::{ChannelMap, SampleSpec};

/// Where a stream's playback is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Waiting for `prebuf` bytes, or for a trigger.
    Prebuffering,
    /// Taken by the card as it asks.
    Playing,
    /// Taken by the card to the last byte, then `seq` acknowledged.
    Draining {
        /// The `DRAIN_PLAYBACK_STREAM` to answer.
        seq: u32,
    },
}

/// What a stream owes its client, for the server to send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owed {
    /// `REQUEST`: this many more bytes.
    Request(u32),
    /// `STARTED`: the card has begun to take the stream.
    Started,
    /// `UNDERFLOW`, at this read offset: the card asked for more than there
    /// was.
    Underflow(i64),
    /// `OVERFLOW`: this many bytes were written past `maxlength` and dropped.
    Overflow(u32),
    /// The acknowledgement of the drain whose sequence number this is.
    Drained(u32),
}

/// A playback stream.
#[derive(Debug)]
pub(crate) struct Stream {
    /// The sink input it is, which introspection names it by.
    pub(crate) index: u32,
    /// Its format, one the card takes as it is.
    pub(crate) spec: SampleSpec,
    /// Its channels' positions.
    pub(crate) map: ChannelMap,
    /// Its buffer attributes, every `-1` resolved.
    pub(crate) attr: BufferAttr,
    /// Whether the client has paused it.
    pub(crate) corked: bool,
    /// Its properties, as the client last set them.
    pub(crate) props: pulseaudio::protocol::Props,
    queue: VecDeque<u8>,
    /// Bytes asked for and not yet written.
    asked: u64,
    state: State,
    /// Whether `STARTED` has gone since the last start.
    started: bool,
    /// Whether the current underrun has been reported.
    underrun_reported: bool,
    /// Bytes ever written by the client.
    pub(crate) written: u64,
    /// Bytes ever taken by the card.
    pub(crate) read: u64,
    /// Bytes the card asked for and did not get, since the last start.
    pub(crate) underrun_for: u64,
    /// Bytes the card has taken since the last start.
    pub(crate) playing_for: u64,
}

impl Stream {
    /// A stream with `attr` already resolved, corked when `corked`, and
    /// what it asks for first.
    pub(crate) fn new(
        index: u32,
        spec: SampleSpec,
        map: ChannelMap,
        attr: BufferAttr,
        corked: bool,
        props: pulseaudio::protocol::Props,
    ) -> (Stream, u32) {
        let state = if attr.pre_buffering == 0 {
            State::Playing
        } else {
            State::Prebuffering
        };
        let mut stream = Stream {
            index,
            spec,
            map,
            attr,
            corked,
            props,
            queue: VecDeque::new(),
            asked: 0,
            state,
            started: false,
            underrun_reported: false,
            written: 0,
            read: 0,
            underrun_for: 0,
            playing_for: 0,
        };
        let first = stream.missing();
        stream.asked = u64::from(first);
        (stream, first)
    }

    /// Whether the card should be taking this stream now.
    pub(crate) fn playing(&self) -> bool {
        !self.corked && self.state != State::Prebuffering
    }

    /// Bytes queued and not yet taken.
    pub(crate) fn queued(&self) -> usize {
        self.queue.len()
    }

    /// What would bring the queue to `tlength`, counting what is asked for.
    fn missing(&self) -> u32 {
        let have = (self.queue.len() as u64).saturating_add(self.asked);
        u32::try_from(u64::from(self.attr.target_length).saturating_sub(have)).unwrap_or(u32::MAX)
    }

    /// Ask for more if what is missing is worth a request.
    fn request(&mut self, owed: &mut Vec<Owed>) {
        let missing = self.missing();
        if missing != 0 && missing >= self.attr.minimum_request_length {
            self.asked = self.asked.saturating_add(u64::from(missing));
            owed.push(Owed::Request(missing));
        }
    }

    /// Start, if waiting for `prebuf` and there is that much.
    fn start_if_filled(&mut self) {
        if self.state == State::Prebuffering
            && self.queue.len() as u64 >= u64::from(self.attr.pre_buffering)
        {
            self.state = State::Playing;
        }
    }

    /// A client's write: queued up to `maxlength`, the rest dropped and
    /// reported.
    pub(crate) fn write(&mut self, data: &[u8], owed: &mut Vec<Owed>) {
        let room = (self.attr.max_length as usize).saturating_sub(self.queue.len());
        let taken = data.get(..room.min(data.len())).unwrap_or(data);
        let dropped = data.len() - taken.len();
        self.queue.extend(taken);
        self.written = self.written.saturating_add(taken.len() as u64);
        self.asked = self.asked.saturating_sub(data.len() as u64);
        if dropped != 0 {
            owed.push(Owed::Overflow(u32::try_from(dropped).unwrap_or(u32::MAX)));
        }
        self.start_if_filled();
    }

    /// The card takes up to `max` bytes into `out`: what it took. Asking for
    /// more than there is while playing is an underrun, reported once, after
    /// which playback waits for `prebuf` again.
    pub(crate) fn read(&mut self, max: usize, out: &mut Vec<u8>, owed: &mut Vec<Owed>) -> usize {
        if !self.playing() {
            return 0;
        }
        let taken = max.min(self.queue.len());
        out.extend(self.queue.drain(..taken));
        self.read = self.read.saturating_add(taken as u64);
        self.playing_for = self.playing_for.saturating_add(taken as u64);
        if taken != 0 {
            self.underrun_reported = false;
            if !self.started {
                self.started = true;
                owed.push(Owed::Started);
            }
        }
        match self.state {
            State::Draining { seq } if self.queue.is_empty() => {
                owed.push(Owed::Drained(seq));
                self.restart();
            }
            State::Playing if taken < max => {
                self.underrun_for = self.underrun_for.saturating_add((max - taken) as u64);
                if !self.underrun_reported {
                    self.underrun_reported = true;
                    owed.push(Owed::Underflow(
                        i64::try_from(self.read).unwrap_or(i64::MAX),
                    ));
                }
                if self.attr.pre_buffering != 0 {
                    self.restart();
                }
            }
            _ => {}
        }
        self.request(owed);
        taken
    }

    /// Wait for `prebuf` again, as after an underrun or a drain.
    fn restart(&mut self) {
        self.state = if self.attr.pre_buffering == 0 {
            State::Playing
        } else {
            State::Prebuffering
        };
        self.started = false;
        self.playing_for = 0;
        self.underrun_for = 0;
    }

    /// `CORK_PLAYBACK_STREAM`.
    pub(crate) fn cork(&mut self, cork: bool) {
        self.corked = cork;
        if !cork {
            self.start_if_filled();
        }
    }

    /// `FLUSH_PLAYBACK_STREAM`: what is queued is dropped, the read index
    /// moved to the write index, and playback waits for `prebuf` again.
    pub(crate) fn flush(&mut self, owed: &mut Vec<Owed>) {
        let dropped = self.queue.len() as u64;
        self.queue.clear();
        self.read = self.read.saturating_add(dropped);
        if self.attr.pre_buffering != 0 {
            self.state = State::Prebuffering;
            self.started = false;
        }
        self.request(owed);
    }

    /// `TRIGGER_PLAYBACK_STREAM`: start now, however little is queued.
    pub(crate) fn trigger(&mut self) {
        if self.state == State::Prebuffering {
            self.state = State::Playing;
        }
    }

    /// `PREBUF_PLAYBACK_STREAM`: wait for `prebuf` again.
    pub(crate) fn prebuf(&mut self) {
        if self.attr.pre_buffering != 0 {
            self.state = State::Prebuffering;
            self.started = false;
        }
    }

    /// `DRAIN_PLAYBACK_STREAM` as `seq`: acknowledged at once if nothing is
    /// queued, else once the card has taken the last byte. A drain starts a
    /// stream still waiting for `prebuf`.
    pub(crate) fn drain(&mut self, seq: u32, owed: &mut Vec<Owed>) {
        if self.queue.is_empty() {
            owed.push(Owed::Drained(seq));
        } else {
            self.state = State::Draining { seq };
        }
    }
}
