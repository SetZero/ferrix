//! Fuzz the stored-hash string `authd` reads back from its store
//! (`libs/crypto/argon2`'s `phc`): any bytes a damaged or edited file holds.
//!
//! Parsing must never panic, a string that parses must print back as the
//! same string, and one whose costs are small enough is checked against a
//! password, which must not panic either.

#![no_main]

use ferrix_argon2::Block;
use ferrix_argon2::phc::Encoded;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Ok(encoded) = Encoded::parse(text) else {
        return;
    };
    assert_eq!(encoded.to_string(), text, "a string that parses prints back the same");
    let Ok(blocks) = encoded.params.blocks() else {
        return;
    };
    if blocks <= 64 && encoded.params.passes <= 4 {
        let mut memory = vec![Block::ZERO; blocks];
        let _ = encoded.verify(b"password", &mut memory);
    }
});
