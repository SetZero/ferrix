//! A stream's samples as the card takes them (`docs/AUDIO.md`, U2c): decoded
//! to floats, its channels mapped onto the card's, converted to the card's
//! rate by `media-resample`, and scaled by its volume, ready to be summed
//! with every other stream's.
//!
//! Volumes are `PulseAudio`'s: a channel's volume is cubed to give its linear
//! gain, so that half the slider is an eighth of the power, and a stream's
//! gain is its own times the sink's. The sum is clipped once, at the end, to
//! the card's 16 bits.

use std::collections::VecDeque;

use media_resample::Resampler;
use pulseaudio::protocol::{ChannelMap, ChannelPosition, PulseError, SampleFormat, SampleSpec};

/// What one stream contributes to the card.
#[derive(Debug)]
pub(crate) struct Converter {
    format: SampleFormat,
    /// Bytes in one of the stream's frames.
    frame_bytes: usize,
    /// For each of the stream's channels, its weight in each of the card's.
    weights: Vec<Vec<f32>>,
    card_channels: usize,
    /// The stream's rate and the card's.
    from: u32,
    to: u32,
    resampler: Option<Resampler>,
    /// Converted frames, the card's channels interleaved, not yet taken.
    pending: VecDeque<f32>,
}

impl Converter {
    /// A converter from `spec` in the positions `map` names to the card's
    /// `card`.
    ///
    /// # Errors
    ///
    /// [`PulseError::NotSupported`] for a sample format it cannot read, or a
    /// pair of rates the resampler cannot convert between.
    pub(crate) fn new(
        spec: &SampleSpec,
        map: &ChannelMap,
        card: &SampleSpec,
    ) -> Result<Converter, PulseError> {
        let sample_bytes = match spec.format {
            SampleFormat::U8 => 1,
            SampleFormat::S16Le | SampleFormat::S16Be => 2,
            SampleFormat::S32Le
            | SampleFormat::S32Be
            | SampleFormat::Float32Le
            | SampleFormat::Float32Be => 4,
            _ => return Err(PulseError::NotSupported),
        };
        let channels = usize::from(spec.channels);
        let card_channels = usize::from(card.channels);
        if channels == 0 || card_channels == 0 {
            return Err(PulseError::NotSupported);
        }
        let positions: Vec<ChannelPosition> = map.into_iter().collect();
        let weights = (0..channels)
            .map(|channel| {
                weights(
                    positions.get(channel).copied(),
                    channel,
                    channels,
                    card_channels,
                )
            })
            .collect();
        let resampler = if spec.sample_rate == card.sample_rate {
            None
        } else {
            Some(
                Resampler::new(spec.sample_rate, card.sample_rate, card_channels)
                    .map_err(|_| PulseError::NotSupported)?,
            )
        };
        Ok(Converter {
            format: spec.format,
            frame_bytes: sample_bytes * channels,
            weights,
            card_channels,
            from: spec.sample_rate,
            to: card.sample_rate,
            resampler,
            pending: VecDeque::new(),
        })
    }

    /// Converted frames waiting to be taken.
    pub(crate) fn pending_frames(&self) -> usize {
        self.pending.len() / self.card_channels
    }

    /// The stream's bytes to read for about `frames` more of the card's: a
    /// whole number of the stream's frames, at least one.
    pub(crate) fn bytes_for(&self, frames: usize) -> usize {
        let from = u64::from(self.from);
        let to = u64::from(self.to).max(1);
        let input = (frames as u64 * from).div_ceil(to).max(1);
        usize::try_from(input).unwrap_or(usize::MAX) * self.frame_bytes
    }

    /// The stream's `bytes`, whole frames of them, converted with the
    /// per-channel `gains` into [`Converter::pending_frames`].
    pub(crate) fn feed(&mut self, bytes: &[u8], gains: &[f32]) {
        let channels = self.weights.len();
        let mut mapped = Vec::with_capacity(bytes.len() / self.frame_bytes * self.card_channels);
        for frame in bytes.chunks_exact(self.frame_bytes) {
            let mut out = vec![0.0_f32; self.card_channels];
            for (channel, raw) in frame.chunks_exact(self.frame_bytes / channels).enumerate() {
                let sample = decode(self.format, raw) * gains.get(channel).copied().unwrap_or(1.0);
                spread(sample, self.weights.get(channel), &mut out);
            }
            mapped.extend(out);
        }
        match self.resampler.as_mut() {
            Some(resampler) => {
                let mut converted = Vec::new();
                resampler.process(&mapped, &mut converted);
                self.pending.extend(converted);
            }
            None => self.pending.extend(mapped),
        }
    }

    /// Add up to `frames` pending frames into `mix`, the card's channels
    /// interleaved: how many it added.
    pub(crate) fn take(&mut self, frames: usize, mix: &mut [f32]) -> usize {
        let taken = frames.min(self.pending_frames());
        for (slot, sample) in mix
            .iter_mut()
            .zip(self.pending.drain(..taken * self.card_channels))
        {
            *slot += sample;
        }
        taken
    }

    /// Forget what is pending, as a flush does.
    pub(crate) fn clear(&mut self) {
        self.pending.clear();
    }
}

/// Add `sample`, by `weights`, into each of the card's channels in `out`.
fn spread(sample: f32, weights: Option<&Vec<f32>>, out: &mut [f32]) {
    for (slot, weight) in out.iter_mut().zip(weights.into_iter().flatten()) {
        *slot += sample * weight;
    }
}

/// A mixed sample as the card's 16 bits, clipped: the inverse of how a
/// 16-bit sample is decoded, so that a stream in the card's own format at
/// full volume reaches the card bit for bit.
pub(crate) fn to_card(sample: f32) -> i16 {
    (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

/// One sample of `format` as a float in `-1.0..=1.0`.
fn decode(format: SampleFormat, raw: &[u8]) -> f32 {
    let bytes = |n: usize| -> [u8; 4] {
        let mut word = [0; 4];
        for (to, from) in word.iter_mut().zip(raw.iter().take(n)) {
            *to = *from;
        }
        word
    };
    match format {
        SampleFormat::U8 => (f32::from(raw.first().copied().unwrap_or(128)) - 128.0) / 128.0,
        SampleFormat::S16Le => {
            let [a, b, ..] = bytes(2);
            f32::from(i16::from_le_bytes([a, b])) / 32768.0
        }
        SampleFormat::S16Be => {
            let [a, b, ..] = bytes(2);
            f32::from(i16::from_be_bytes([a, b])) / 32768.0
        }
        SampleFormat::S32Le => i32::from_le_bytes(bytes(4)) as f32 / 2_147_483_648.0,
        SampleFormat::S32Be => i32::from_be_bytes(bytes(4)) as f32 / 2_147_483_648.0,
        SampleFormat::Float32Le => f32::from_le_bytes(bytes(4)),
        SampleFormat::Float32Be => f32::from_be_bytes(bytes(4)),
        _ => 0.0,
    }
}

/// How a stream channel at `position`, number `channel` of `channels`, is
/// heard in each
/// of the card's `card` channels: a left one on the left, a right one on the
/// right, the centre, a subwoofer and mono in both at -3 dB (or whole for
/// mono itself), when the card is stereo. A mono card hears every channel
/// equally; any other card takes the stream's channels in order.
fn weights(
    position: Option<ChannelPosition>,
    channel: usize,
    channels: usize,
    card: usize,
) -> Vec<f32> {
    use ChannelPosition as P;
    const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;
    match card {
        1 => vec![1.0 / channels as f32],
        2 => match position {
            Some(P::Mono) | None if channels == 1 => vec![1.0, 1.0],
            Some(P::FrontLeft | P::RearLeft | P::SideLeft | P::FrontLeftOfCenter) => {
                vec![1.0, 0.0]
            }
            Some(P::FrontRight | P::RearRight | P::SideRight | P::FrontRightOfCenter) => {
                vec![0.0, 1.0]
            }
            _ => vec![CENTRE, CENTRE],
        },
        _ => (0..card)
            .map(|slot| f32::from(u8::from(slot == channel)))
            .collect(),
    }
}
