//! The translation into the core's terms, held to both tables.

use ferrix_sndctl::message::RATES_HZ;
use ferrix_virtio::snd;

use crate::format::{FORMATS, to_virtio};

#[test]
fn the_two_rate_tables_are_one() {
    assert_eq!(RATES_HZ, snd::RATES_HZ, "same rates, same order, same bits");
}

#[test]
fn every_format_maps_both_ways_and_s16_is_s16() {
    for (virtio, alsa) in FORMATS {
        assert_eq!(to_virtio(alsa), Some(virtio));
    }
    assert_eq!(to_virtio(2), Some(snd::FORMAT_S16), "S16_LE");
    assert_eq!(to_virtio(3), None, "S16_BE has no virtio format");
}
