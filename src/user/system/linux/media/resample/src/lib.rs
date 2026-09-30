//! Sample-rate conversion by a rational factor, with a polyphase windowed-sinc
//! filter.
//!
//! Ferrix's sound card takes 48 kHz and nothing else (`docs/AUDIO.md` §3),
//! so every source at another rate is converted before it is written: Bad
//! Apple!!'s 44.1 kHz AAC (a factor of 160/147) and Doom's 11025 Hz sound
//! effects (640/147). Output sample `j` sits at input position `j × M / L`;
//! its integer part picks the input frames and its fraction, which is one of
//! `L` values, picks one of `L` precomputed filter phases. There is no
//! floating-point drift, however long the stream: positions are counted in
//! integers.

use core::f64::consts::PI;
use core::fmt;

/// Filter taps per phase: each output sample is a weighted sum of this many
/// input frames. 32 puts the transition band at a few hundred hertz at 48 kHz.
pub const TAPS: usize = 32;

/// What can't be converted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A rate of zero, no channels, or a ratio whose phase table would be
    /// larger than [`MAX_PHASES`].
    Rates,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rates this resampler can't convert between")
    }
}

impl std::error::Error for Error {}

/// The most phases a table may have: 44.1 kHz and 11025 Hz to 48 kHz need
/// 160 and 640.
pub const MAX_PHASES: u64 = 4096;

const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// A converter from one rate to another, for interleaved `f32` frames of a
/// fixed number of channels.
#[derive(Clone, Debug)]
pub struct Resampler {
    channels: usize,
    /// The factor, `up / down`, in lowest terms.
    up: u64,
    down: u64,
    /// `up` phases of [`TAPS`] weights each.
    table: Vec<f32>,
    /// Input frames not yet wholly used, interleaved; `pending[0]` is the
    /// absolute input frame `start`.
    pending: Vec<f32>,
    start: u64,
    /// The next output frame's number.
    next: u64,
}

/// Frames of history before an output position: the filter is centred
/// between frame `HALF - 1` and frame `HALF` of its window.
const HALF: usize = TAPS / 2;

impl Resampler {
    /// A converter from `from` Hz to `to` Hz for `channels` channels.
    ///
    /// # Errors
    ///
    /// [`Error::Rates`] for a zero rate, no channels, or a ratio too fine.
    pub fn new(from: u32, to: u32, channels: usize) -> Result<Self, Error> {
        if from == 0 || to == 0 || channels == 0 {
            return Err(Error::Rates);
        }
        let divisor = gcd(u64::from(from), u64::from(to));
        let (up, down) = (u64::from(to) / divisor, u64::from(from) / divisor);
        if up > MAX_PHASES {
            return Err(Error::Rates);
        }
        // The cut-off, in cycles per input frame: below the lower Nyquist
        // frequency of the two, with a margin for the transition band.
        let cutoff = 0.5 * (f64::from(to) / f64::from(from)).min(1.0) * 0.92;
        let mut table = Vec::with_capacity(up as usize * TAPS);
        for phase in 0..up {
            let fraction = phase as f64 / up as f64;
            let start = table.len();
            for tap in 0..TAPS {
                // How far input frame `tap` of the window is from the output
                // position, in input frames.
                let distance = tap as f64 - (HALF as f64 - 1.0) - fraction;
                table.push(kernel(distance, cutoff) as f32);
            }
            // Each phase passes a constant through unchanged.
            let sum: f32 = table.iter().skip(start).sum();
            if sum != 0.0 {
                for weight in table.iter_mut().skip(start) {
                    *weight /= sum;
                }
            }
        }
        Ok(Self {
            channels,
            up,
            down,
            table,
            // History before the first frame is silence.
            pending: vec![0.0; (HALF - 1) * channels],
            start: 0,
            next: 0,
        })
    }

    /// Convert `input`, whole interleaved frames, appending to `output` every
    /// frame whose window of input is now complete.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        self.pending.extend_from_slice(input);
        let have = (self.pending.len() / self.channels) as u64;
        loop {
            let at = self.next * self.down;
            // The first input frame of the window, counted in `pending`
            // (which starts HALF - 1 frames of silence early).
            let first = at / self.up;
            if first + TAPS as u64 > have + self.start {
                break;
            }
            let phase = (at % self.up) as usize;
            let weights = self
                .table
                .get(phase * TAPS..(phase + 1) * TAPS)
                .unwrap_or(&[]);
            let offset = (first - self.start) as usize * self.channels;
            for channel in 0..self.channels {
                let mut sum = 0.0_f32;
                for (tap, weight) in weights.iter().enumerate() {
                    let sample = self
                        .pending
                        .get(offset + tap * self.channels + channel)
                        .copied()
                        .unwrap_or(0.0);
                    sum += sample * weight;
                }
                output.push(sum);
            }
            self.next += 1;
        }
        // Drop the frames no later window reaches.
        let keep_from = (self.next * self.down / self.up).max(self.start);
        let drop = ((keep_from - self.start) as usize * self.channels).min(self.pending.len());
        let _ = self.pending.drain(..drop);
        self.start = keep_from;
    }

    /// Convert what is left, as if the input ended in silence.
    pub fn flush(&mut self, output: &mut Vec<f32>) {
        let silence = vec![0.0; HALF * self.channels];
        self.process(&silence, output);
    }

    /// The factor, output frames per input frame, in lowest terms.
    #[must_use]
    pub const fn ratio(&self) -> (u64, u64) {
        (self.up, self.down)
    }
}

/// A windowed sinc: an ideal low-pass at `cutoff` cycles per frame,
/// Blackman-windowed to the filter's span.
fn kernel(distance: f64, cutoff: f64) -> f64 {
    let span = HALF as f64;
    if distance.abs() >= span {
        return 0.0;
    }
    let x = 2.0 * cutoff * distance;
    let sinc = if x == 0.0 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    };
    let w = PI * distance / span;
    let window = 0.42 + 0.5 * w.cos() + 0.08 * (2.0 * w).cos();
    2.0 * cutoff * sinc * window
}

/// A 16-bit sample from a float in `-1.0..=1.0`, clipped.
#[must_use]
pub fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[cfg(test)]
mod tests;
