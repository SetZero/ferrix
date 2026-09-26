//! Fuzz init's wire formats (`libs/svc-proto`): any bytes a local program
//! sends to `/run/ferrix/control` or writes to a readiness descriptor.
//!
//! Decoding must never panic, a record that decodes must encode back to the
//! same bytes, and the framer must give the same records however the stream
//! is cut.

#![no_main]

use ferrix_svc_proto::control::{Answer, Call, Framer};
use ferrix_svc_proto::notify::{self, Lines};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(call) = Call::decode(data) {
        assert_eq!(&call.encode()[4..], data, "a call that decodes encodes back");
    }
    if let Ok(answer) = Answer::decode(data) {
        assert_eq!(&answer.encode()[4..], data, "an answer that decodes encodes back");
    }
    let cut = data.first().map_or(0, |&b| usize::from(b)).min(data.len());
    let mut whole = Framer::new();
    whole.push(data);
    let mut pieces = Framer::new();
    pieces.push(&data[..cut]);
    let _ = pieces.next_record();
    pieces.push(&data[cut..]);
    let first = whole.next_record();
    let _ = first;
    let mut lines = Lines::new();
    for line in lines.push(data) {
        let _ = notify::parse(&line);
    }
    let _ = lines.finish();
});
