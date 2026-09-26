//! Playback through `/dev/snd/pcmC0D0p`, the way alsa-lib's `hw` plugin
//! drives it, for a program that has sound to play and a picture to keep in
//! step with it.
//!
//! Ferrix's card plays one format: `S16_LE`, two channels, 48 kHz
//! (`docs/AUDIO.md` §3). [`Playback::open`] asks for exactly that, reads back
//! the period and buffer the card chose, and prepares the stream; the program
//! then writes interleaved frames, which block while the buffer is full, and
//! so are paced by the card. [`Playback::played`] is how far the speaker has
//! got -- frames written less the card's delay -- which is the clock a video
//! player shows its frames by.
//!
//! An underrun (the program fell behind and the card ran dry) is not an
//! error to a player: the stream is prepared again and the write retried,
//! and the gap is heard, counted and reported, not fatal.
//!
//! Only Linux, and Ferrix through its Linux ABI, have `/dev/snd`; elsewhere
//! the crate is empty.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Config, Playback};

/// The rate every stream plays at.
pub const RATE: u32 = 48_000;
/// The channels every stream has: interleaved left, right.
pub const CHANNELS: usize = 2;
