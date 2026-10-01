//! The song: decoded from its AAC track, converted to 48 kHz, written to the
//! card, and the card's position told to the clock.

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use media_pcm::{CHANNELS, Playback, RATE};
use media_resample::{Resampler, to_i16};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as Decode;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use badapple::clock::Clock;

/// What playing the song came to.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Played {
    /// Frames written to the card.
    pub(crate) frames: u64,
    /// Times the card ran dry.
    pub(crate) underruns: u32,
    /// Packets the decoder refused and that were skipped.
    pub(crate) bad_packets: u32,
}

/// Open the card, or say why not.
pub(crate) fn open() -> Result<Playback, String> {
    // Start playing once half the buffer is queued: enough in hand that a
    // slow first frame of video does not starve the card, little enough
    // that the song starts at once.
    Playback::open(|config| config.buffer / 2).map_err(|error| format!("the card: {error}"))
}

/// Play `song` on `playback`, at most `seconds` and until `stop` is set,
/// telling `clock` where the speaker is after every period.
pub(crate) fn play(
    song: &str,
    seconds: Option<u32>,
    mut playback: Playback,
    clock: &Arc<Clock>,
    stop: &AtomicBool,
) -> Result<Played, String> {
    let file = File::open(song).map_err(|error| format!("{song}: {error}"))?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    let _ = hint.with_extension("m4a");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("{song}: {error}"))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| format!("{song}: no audio track"))?;
    let track_id = track.id;
    let rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| format!("{song}: no sample rate"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|error| format!("{song}: {error}"))?;
    let mut resampler =
        Resampler::new(rate, RATE, CHANNELS).map_err(|error| format!("{song}: {error}"))?;
    let limit = seconds.map(|s| u64::from(s) * u64::from(RATE));
    let period = playback.config().period.max(1) as usize;

    let mut played = Played::default();
    let mut samples: Option<SampleBuffer<f32>> = None;
    let mut stereo: Vec<f32> = Vec::new();
    let mut converted: Vec<f32> = Vec::new();
    let mut out: Vec<i16> = Vec::new();
    let mut ended = false;
    while !ended {
        // The window was closed: stop at once, leaving what the card holds
        // to play out or not.
        if stop.load(Ordering::Relaxed) {
            played.frames = playback.written();
            played.underruns = playback.underruns();
            return Ok(played);
        }
        match format.next_packet() {
            Ok(packet) if packet.track_id() != track_id => continue,
            Ok(packet) => match decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let buffer = match &mut samples {
                        Some(buffer) if buffer.capacity() >= decoded.capacity() => buffer,
                        _ => samples.insert(SampleBuffer::new(decoded.capacity() as u64, spec)),
                    };
                    buffer.copy_interleaved_ref(decoded);
                    to_stereo(buffer.samples(), spec.channels.count(), &mut stereo);
                    resampler.process(&stereo, &mut converted);
                }
                Err(Decode::DecodeError(_)) => played.bad_packets += 1,
                Err(error) => return Err(format!("{song}: {error}")),
            },
            Err(Decode::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                resampler.flush(&mut converted);
                ended = true;
            }
            Err(error) => return Err(format!("{song}: {error}")),
        }
        // Whole periods, so the clock hears from the card at its own pace;
        // at the end, whatever is left.
        let mut whole = converted.len() / (period * CHANNELS) * period * CHANNELS;
        if ended {
            whole = converted.len();
        }
        if let Some(limit) = limit {
            let room = limit.saturating_sub(playback.written()) as usize * CHANNELS;
            if room <= whole {
                whole = room;
                ended = true;
            }
        }
        for chunk in converted
            .get(..whole)
            .unwrap_or(&[])
            .chunks(period * CHANNELS)
        {
            out.clear();
            out.extend(chunk.iter().map(|&sample| to_i16(sample)));
            playback
                .write(&out)
                .map_err(|error| format!("the card: {error}"))?;
            clock.played(
                playback
                    .played()
                    .map_err(|error| format!("the card: {error}"))?,
            );
        }
        let _ = converted.drain(..whole);
    }
    // Let the card play out what it holds, telling the clock as it goes,
    // until two periods are left; then drain, which returns once they are
    // played. Waiting for the card to run dry instead would end the song on
    // an underrun.
    let written = playback.written();
    let near = 2 * period as u64;
    loop {
        let heard = playback
            .played()
            .map_err(|error| format!("the card: {error}"))?;
        clock.played(heard);
        if heard + near >= written {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    playback
        .drain()
        .map_err(|error| format!("the card: {error}"))?;
    clock.played(written);
    played.frames = written;
    played.underruns = playback.underruns();
    Ok(played)
}

/// Interleaved frames of `channels` channels as stereo: mono twice, more
/// than two the first two.
pub(crate) fn to_stereo(samples: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    match channels {
        0 => {}
        1 => out.extend(samples.iter().flat_map(|&s| [s, s])),
        2 => out.extend_from_slice(samples),
        _ => {
            for frame in samples.chunks_exact(channels) {
                out.extend(frame.iter().take(2));
            }
        }
    }
}
