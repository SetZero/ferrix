//! Fuzz virtio-snd's device protocol: the configuration block, the answers a
//! device gives to control requests, the status after a buffer of samples,
//! and the events it writes.
//!
//! The driver runs in ring 3 against a device it does not control, so every
//! count, every response and every status is the device's word.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **A block is believed only within bounds**: it reads exactly when it
//!    reaches `chmaps` and declares between 1 and `MAX_STREAMS` streams, and
//!    then says what the block holds.
//! 2. **Stream information keeps its promises**: an entry read has a
//!    direction that exists and a channel range that is not empty and does
//!    not start at zero, the whole response for the count asked is there,
//!    its status was OK, and what it offers is only what is defined.
//! 3. **A transmit status is eight bytes and OK**: accepted exactly when the
//!    completion wrote eight and the status word is `STATUS_OK`, and then it
//!    carries the second word.
//! 4. **Every event decodes**: eight bytes or more always give an event, an
//!    unknown code included, and fewer never do.

#![no_main]

use ferrix_virtio::snd::{
    Config, EVENT_BYTES, Event, FORMATS_DEFINED, MAX_STREAMS, PcmInfo, PcmStatus, RATES_DEFINED,
    STATUS_OK, pcm_info_response_bytes,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&count, rest)) = data.split_first() else {
        return;
    };

    // 1: the input as a configuration block.
    match Config::read(rest) {
        Ok(config) => {
            assert!(rest.len() >= 12);
            assert!((1..=MAX_STREAMS).contains(&config.streams));
            assert_eq!(config.streams.to_le_bytes(), rest[4..8]);
        }
        Err(_) => {
            let streams = rest.get(4..8).map(|word| u32::from_le_bytes(word.try_into().unwrap()));
            assert!(rest.len() < 12 || !matches!(streams, Some(1..=MAX_STREAMS)));
        }
    }

    // 2: the input as a PCM_INFO response for `count` streams, every entry.
    let count = u32::from(count % 12);
    for index in 0..=count {
        if let Ok(info) = PcmInfo::read(rest, index, count) {
            assert!(index < count);
            assert!(rest.len() >= pcm_info_response_bytes(count));
            assert_eq!(u32::from_le_bytes(rest[0..4].try_into().unwrap()), STATUS_OK);
            assert!(info.direction <= 1);
            assert!(info.channels_min >= 1 && info.channels_min <= info.channels_max);
            assert_eq!(info.formats() & !FORMATS_DEFINED, 0);
            assert_eq!(info.rates() & !RATES_DEFINED, 0);
        }
    }

    // 3: the input as a transmit status, with every length the used ring
    // could report that the input's first byte names.
    for written in [0, 7, 8, 9, u32::from(data[0])] {
        match PcmStatus::read(rest, written) {
            Ok(status) => {
                assert_eq!(written, 8);
                assert_eq!(u32::from_le_bytes(rest[0..4].try_into().unwrap()), STATUS_OK);
                assert_eq!(status.latency_bytes.to_le_bytes(), rest[4..8]);
            }
            Err(_) => assert!(
                written != 8
                    || rest.len() < 8
                    || u32::from_le_bytes(rest[0..4].try_into().unwrap()) != STATUS_OK
            ),
        }
    }

    // 4: the input as an event.
    assert_eq!(Event::read(rest).is_ok(), rest.len() >= EVENT_BYTES);
});
