//! Fuzz authd's wire format (`libs/proto/auth-proto`): any packet a local
//! program sends to `/run/ferrix/auth`.
//!
//! Decoding must never panic, and a record that decodes must encode back to
//! exactly the packet it came from, since the format gives a record one
//! spelling.

#![no_main]

use ferrix_auth_proto::{MAX_RECORD, Record};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(record) = Record::decode(data) else {
        return;
    };
    let mut buffer = [0_u8; MAX_RECORD];
    let len = record.encode(&mut buffer).expect("a record that decoded encodes");
    assert_eq!(&buffer[..len], data, "a record has one spelling");
    let _ = record.is_final();
});
