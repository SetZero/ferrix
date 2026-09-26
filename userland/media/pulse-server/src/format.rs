//! What a new stream plays in, and its buffer: a client's sample
//! specification or its format list read, and every `-1` in its buffer
//! attributes given the value `PulseAudio`'s `fix_playback_buffer_attr` would.
//!
//! A client may name its format either way. libpulse sends a sample
//! specification; since protocol 21 it may instead send an invalid one and a
//! list of formats, each PCM entry naming its rate, channels, sample format and
//! channel map as properties. mpv does the second (recorded with `patrace` on
//! 2026-09-27), with the values as JSON: `"format.sample_format"` is
//! `"\"s16le\""`, `"format.rate"` is `"48000"`.

use std::ffi::CStr;
use std::time::Duration;

use pulseaudio::protocol::stream::BufferAttr;
use pulseaudio::protocol::{
    ChannelMap, ChannelPosition, FormatEncoding, FormatInfo, PulseError, SampleFormat, SampleSpec,
};

/// The format a stream asked for, from its sample specification or, when
/// that is invalid, the first PCM entry of its format list that can be read.
///
/// # Errors
///
/// [`PulseError::Invalid`] when neither names a format.
pub(crate) fn requested(
    spec: SampleSpec,
    map: &ChannelMap,
    formats: &[FormatInfo],
) -> Result<(SampleSpec, ChannelMap), PulseError> {
    if spec.format != SampleFormat::Invalid && spec.channels != 0 && spec.sample_rate != 0 {
        let map = if map.num_channels() == spec.channels {
            *map
        } else {
            default_map(spec.channels).ok_or(PulseError::Invalid)?
        };
        return Ok((spec, map));
    }
    formats
        .iter()
        .filter(|format| format.encoding == FormatEncoding::Pcm)
        .find_map(from_format)
        .ok_or(PulseError::Invalid)
}

/// A PCM format entry read as a sample specification and channel map.
fn from_format(format: &FormatInfo) -> Option<(SampleSpec, ChannelMap)> {
    let prop = |key: &CStr| {
        format
            .props
            .get_bytes(key)
            .map(|bytes| unquote(bytes).to_owned())
    };
    let sample_format = sample_format(&prop(c"format.sample_format")?)?;
    let sample_rate = prop(c"format.rate")?.parse().ok()?;
    let channels: u8 = prop(c"format.channels")?.parse().ok()?;
    let map = match prop(c"format.channel_map") {
        Some(names) => {
            let map = ChannelMap::new(names.split(',').map(position).collect::<Option<Vec<_>>>()?);
            (map.num_channels() == channels).then_some(map)?
        }
        None => default_map(channels)?,
    };
    Some((
        SampleSpec {
            format: sample_format,
            channels,
            sample_rate,
        },
        map,
    ))
}

/// A property's text: its bytes to the first NUL, and the quotes a JSON
/// string has taken off.
fn unquote(bytes: &[u8]) -> &str {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    let text = std::str::from_utf8(bytes.get(..end).unwrap_or(bytes)).unwrap_or("");
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text)
}

/// `PulseAudio`'s name for a sample format.
fn sample_format(name: &str) -> Option<SampleFormat> {
    Some(match name {
        "u8" => SampleFormat::U8,
        "aLaw" => SampleFormat::Alaw,
        "uLaw" => SampleFormat::Ulaw,
        "s16le" => SampleFormat::S16Le,
        "s16be" => SampleFormat::S16Be,
        "float32le" => SampleFormat::Float32Le,
        "float32be" => SampleFormat::Float32Be,
        "s32le" => SampleFormat::S32Le,
        "s32be" => SampleFormat::S32Be,
        "s24le" => SampleFormat::S24Le,
        "s24be" => SampleFormat::S24Be,
        "s24-32le" => SampleFormat::S24In32Le,
        "s24-32be" => SampleFormat::S24In32Be,
        _ => return None,
    })
}

/// `PulseAudio`'s name for a channel position, for the positions a stereo or
/// surround stream has.
fn position(name: &str) -> Option<ChannelPosition> {
    Some(match name.trim() {
        "mono" => ChannelPosition::Mono,
        "front-left" | "left" => ChannelPosition::FrontLeft,
        "front-right" | "right" => ChannelPosition::FrontRight,
        "front-center" | "center" => ChannelPosition::FrontCenter,
        "rear-center" => ChannelPosition::RearCenter,
        "rear-left" => ChannelPosition::RearLeft,
        "rear-right" => ChannelPosition::RearRight,
        "lfe" | "subwoofer" => ChannelPosition::Lfe,
        "side-left" => ChannelPosition::SideLeft,
        "side-right" => ChannelPosition::SideRight,
        _ => return None,
    })
}

/// The map `PulseAudio` gives `channels` channels when a client names none:
/// mono, or stereo.
fn default_map(channels: u8) -> Option<ChannelMap> {
    match channels {
        1 => Some(ChannelMap::mono()),
        2 => Some(ChannelMap::stereo()),
        _ => None,
    }
}

/// Bytes of `spec` a duration holds, whole frames only.
pub(crate) fn bytes_for(spec: &SampleSpec, time: Duration) -> u32 {
    let frame = frame_bytes(spec);
    let frames = u128::from(spec.sample_rate) * time.as_micros() / 1_000_000;
    u32::try_from(frames * u128::from(frame)).unwrap_or(u32::MAX)
}

/// The time `bytes` of `spec` last.
pub(crate) fn duration_of(spec: &SampleSpec, bytes: u64) -> Duration {
    let frame = u64::from(frame_bytes(spec)).max(1);
    let rate = u64::from(spec.sample_rate).max(1);
    Duration::from_micros((bytes / frame).saturating_mul(1_000_000) / rate)
}

/// One frame's bytes in `spec`.
pub(crate) fn frame_bytes(spec: &SampleSpec) -> u32 {
    u32::try_from(spec.format.bytes_per_sample() * usize::from(spec.channels)).unwrap_or(0)
}

/// What `maxlength` is when a client leaves it to the server: `PulseAudio`'s
/// 4 MiB, which `PipeWire` answers too.
const MAX_LENGTH: u32 = 4 * 1024 * 1024;

/// What `tlength` is when left: 250 ms. `PulseAudio`'s two seconds is for a
/// desktop that trades latency for fewer wake-ups; Ferrix's card has 320 ms
/// of buffer of its own.
const TARGET: Duration = Duration::from_millis(250);

/// The smallest `minreq`: one of the card's 20 ms periods.
const MIN_REQUEST: Duration = Duration::from_millis(20);

/// `attr` with every `-1` given a value and every value brought within
/// `PulseAudio`'s bounds, for a stream of `spec`: `tlength` at least `minreq`
/// and at most `maxlength`, `minreq` at least one period and at most a
/// quarter of `tlength` when left, `prebuf` at most `tlength` less `minreq`
/// when left and at most `tlength` in any case, each a whole number of frames.
pub(crate) fn resolve(attr: BufferAttr, spec: &SampleSpec) -> BufferAttr {
    let frame = frame_bytes(spec).max(1);
    let whole = |bytes: u32| (bytes / frame).max(1) * frame;
    let left = |value: u32| value == u32::MAX;
    let max_length = if left(attr.max_length) {
        MAX_LENGTH
    } else {
        whole(attr.max_length).min(MAX_LENGTH)
    };
    let period = bytes_for(spec, MIN_REQUEST);
    let target_length = if left(attr.target_length) {
        bytes_for(spec, TARGET)
    } else {
        whole(attr.target_length)
    }
    .min(max_length)
    .max(period);
    let minimum_request_length = if left(attr.minimum_request_length) {
        whole(target_length / 4).max(period)
    } else {
        whole(attr.minimum_request_length).max(period)
    }
    .min(target_length);
    let pre_buffering = if left(attr.pre_buffering) {
        target_length - minimum_request_length
    } else {
        attr.pre_buffering.min(target_length) / frame * frame
    };
    BufferAttr {
        max_length,
        target_length,
        pre_buffering,
        minimum_request_length,
        fragment_size: attr.fragment_size,
    }
}
