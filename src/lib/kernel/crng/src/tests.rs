//! Tests for the block function, against RFC 8439 and OpenSSL's keystream,
//! and for the generator built on it.

extern crate std;

use std::format;

use super::*;

fn hex(text: &str) -> [u8; BLOCK_BYTES] {
    let mut out = [0_u8; BLOCK_BYTES];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2).unwrap(), 16).unwrap();
    }
    out
}

fn counting_key() -> [u8; KEY_BYTES] {
    core::array::from_fn(|i| i as u8)
}

#[test]
fn the_block_function_matches_rfc_8439_section_2_3_2() {
    // RFC 8439's 32-bit counter 1 and nonce 00 00 00 09 00 00 00 4a 00 00 00
    // 00 are, as words, 12 = 1, 13 = 0x09000000, 14 = 0x4a000000, 15 = 0.
    let counter = 1_u64 | (0x0900_0000_u64 << 32);
    let nonce = 0x4a00_0000_u64;
    let expected = hex(
        "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e\
         d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e",
    );
    assert_eq!(block(&counting_key(), counter, nonce), expected);
}

#[test]
fn the_all_zero_keystream_matches_openssl() {
    let zero = [0_u8; KEY_BYTES];
    assert_eq!(
        block(&zero, 0, 0),
        hex(
            "76b8e0ada0f13d90405d6ae55386bd28bdd219b8a08ded1aa836efcc8b770dc7\
             da41597c5157488d7724e03fb8d84a376a43b8f41518a11cc387b669b2ee6586"
        )
    );
    assert_eq!(
        block(&zero, 1, 0),
        hex(
            "9f07e7be5551387a98ba977c732d080dcb0f29a048e3656912c6533e32ee7aed\
             29b721769ce64e43d57133b074d839d531ed1f28510afb45ace10a1f4b794d6f"
        )
    );
}

#[test]
fn the_counter_carries_into_its_high_word_as_openssl_does() {
    let key = counting_key();
    let nonce = 0x89ab_cdef_0123_4567_u64;
    assert_eq!(
        block(&key, 0xffff_ffff, nonce),
        hex(
            "2512e52947541f05046ff9d15eb9a4a9a6b16dfe1c5032f0a68240b52fff075b\
             20a3c6e6389f75f18a36b321e326b76f66994971833aa2c0630f3ffe293d0e4a"
        )
    );
    assert_eq!(
        block(&key, 0x1_0000_0000, nonce),
        hex(
            "23bae08e4d3e84887d240c3f892adbc572504691ab5c40a5d6c4af796de31c9c\
             1c4a76876d1f4c5ccc217cdad4d79ed0e3680d0da1276e6f01fcdd7994eecc63"
        )
    );
}

#[test]
fn output_is_the_second_half_of_each_block_and_the_first_half_becomes_the_key() {
    let mut rng = Crng::new();
    let mut out = [0_u8; 40];
    rng.fill(&mut out);

    let first = block(&[0; KEY_BYTES], 0, 0);
    let mut key = [0_u8; KEY_BYTES];
    key.copy_from_slice(&first[..32]);
    assert_eq!(out[..32], first[32..]);
    let second = block(&key, 1, 0);
    assert_eq!(out[32..], second[32..40]);
}

#[test]
fn the_same_seed_gives_the_same_stream_and_a_different_one_does_not() {
    let stream = |seed: &[u8]| {
        let mut rng = Crng::new();
        rng.mix(seed);
        let mut out = [0_u8; 256];
        rng.fill(&mut out);
        out
    };
    assert_eq!(stream(b"seed"), stream(b"seed"));
    assert_ne!(stream(b"seed"), stream(b"seee"));
    assert_ne!(stream(b"seed"), stream(b""));
}

#[test]
fn an_input_repeated_across_a_key_does_not_cancel_itself() {
    let mut once = Crng::new();
    once.mix(&[0xa5; KEY_BYTES]);
    let mut twice = Crng::new();
    twice.mix(&[0xa5; 2 * KEY_BYTES]);
    let mut neither = Crng::new();
    neither.mix(&[]);
    let mut a = [0_u8; 32];
    let mut b = [0_u8; 32];
    let mut c = [0_u8; 32];
    once.fill(&mut a);
    twice.fill(&mut b);
    neither.fill(&mut c);
    assert_ne!(a, b);
    assert_ne!(b, c);
}

#[test]
fn consecutive_reads_never_repeat_and_look_uniform() {
    let mut rng = Crng::new();
    rng.mix(&[7; 32]);
    let mut counts = [0_u32; 256];
    let mut previous = [0_u8; 64];
    for _ in 0..4096 {
        let mut out = [0_u8; 64];
        rng.fill(&mut out);
        assert_ne!(out, previous);
        for byte in out {
            counts[usize::from(byte)] += 1;
        }
        previous = out;
    }
    // 262144 bytes over 256 values is 1024 each. A chi-squared statistic over
    // 255 degrees of freedom above 350 has a probability below 0.0001.
    let chi: f64 = counts
        .iter()
        .map(|&n| {
            let d = f64::from(n) - 1024.0;
            d * d / 1024.0
        })
        .sum();
    assert!(chi < 350.0, "chi-squared {chi}");
}

#[test]
fn credit_saturates_and_seeded_waits_for_256_bits() {
    let mut rng = Crng::new();
    assert!(!rng.seeded());
    rng.credit(255);
    assert!(!rng.seeded());
    rng.credit(1);
    assert!(rng.seeded());
    rng.credit(u32::MAX);
    assert_eq!(rng.credited(), u32::MAX);
}

#[test]
fn a_debug_print_does_not_show_the_key() {
    let mut rng = Crng::new();
    rng.mix(&[0x42; 32]);
    let text = format!("{rng:?}");
    assert!(!text.contains("key"), "{text}");
}
