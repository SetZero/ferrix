//! What the device offers, in the core's terms: virtio-snd's formats as
//! ALSA's, its rates as bits over `sndctl`'s table, and HELLO.
//!
//! HELLO speaks ALSA's `FORMAT_*` numbers and `ferrix_sndctl::message`'s
//! rate table, not virtio's enums (`docs/AUDIO.md` §3.2), so the driver
//! translates. A virtio format with no ALSA counterpart here -- the packed
//! 18-, 20- and 24-bit ones, the codecs, DSD, IEC958 -- is left out of HELLO:
//! the core publishes only version 1's S16 anyway, and a later landing that
//! wants one adds its line. The rate tables are the same fourteen rates in
//! the same order, which the tests hold them to.

use ferrix_linux_abi::sound::{
    FORMAT_FLOAT_LE, FORMAT_S8, FORMAT_S16_LE, FORMAT_S32_LE, FORMAT_U8, FORMAT_U16_LE,
    FORMAT_U32_LE,
};
use ferrix_sndctl::message::{
    DIRECTION_CAPTURE, DIRECTION_PLAYBACK, Hello, MAX_STREAMS, Offer, RATES_HZ, VERSION,
};
use ferrix_virtio::snd::{self, DIRECTION_INPUT, PcmInfo};

/// `FORMAT_FLOAT64_LE`, which `ferrix-linux-abi` does not name.
const FORMAT_FLOAT64_LE: u32 = 16;

/// Each virtio format with an ALSA counterpart, and that counterpart.
pub const FORMATS: [(u8, u32); 8] = [
    (snd::FORMAT_S8, FORMAT_S8),
    (snd::FORMAT_U8, FORMAT_U8),
    (snd::FORMAT_S16, FORMAT_S16_LE),
    (snd::FORMAT_U16, FORMAT_U16_LE),
    (snd::FORMAT_S32, FORMAT_S32_LE),
    (snd::FORMAT_U32, FORMAT_U32_LE),
    (snd::FORMAT_FLOAT, FORMAT_FLOAT_LE),
    (snd::FORMAT_FLOAT64, FORMAT_FLOAT64_LE),
];

/// The virtio format for ALSA's `format`, if there is one.
#[must_use]
pub fn to_virtio(format: u32) -> Option<u8> {
    FORMATS
        .iter()
        .find(|(_, alsa)| *alsa == format)
        .map(|(virtio, _)| *virtio)
}

/// ALSA's bits for the virtio formats `info` offers.
#[must_use]
pub fn alsa_formats(info: &PcmInfo) -> u64 {
    FORMATS
        .iter()
        .filter(|(virtio, _)| info.has_format(*virtio))
        .fold(0, |bits, (_, alsa)| bits | 1 << alsa)
}

/// `sndctl`'s rate bits for the rates `info` offers: the same indices.
#[must_use]
pub fn rate_bits(info: &PcmInfo) -> u32 {
    (0..RATES_HZ.len())
        .filter_map(|index| u8::try_from(index).ok())
        .filter(|index| info.has_rate(*index))
        .fold(0, |bits, index| bits | 1 << index)
}

/// The offer HELLO carries for a stream `info` describes.
#[must_use]
pub fn offer(info: &PcmInfo) -> Offer {
    Offer {
        direction: if info.direction == DIRECTION_INPUT {
            DIRECTION_CAPTURE
        } else {
            DIRECTION_PLAYBACK
        },
        channels_min: info.channels_min,
        channels_max: info.channels_max,
        rates: rate_bits(info),
        formats: alsa_formats(info),
    }
}

/// HELLO for a device whose streams `infos` describe, at `location`. Streams
/// past [`MAX_STREAMS`] do not fit and are not described; `libs/virtio::snd`
/// refuses such a device before this is reached.
#[must_use]
pub fn hello(infos: &[PcmInfo], location: u32) -> Hello {
    let mut offers = [Offer::default(); MAX_STREAMS];
    for (slot, info) in offers.iter_mut().zip(infos) {
        *slot = offer(info);
    }
    Hello {
        version: VERSION,
        location,
        // At most `MAX_STREAMS`.
        streams: infos.len().min(MAX_STREAMS) as u32,
        offers,
    }
}
