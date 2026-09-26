//! `pulsed`: the `PulseAudio`-protocol server (`docs/AUDIO.md`, U2b).
//!
//! `pulsed [SOCKET]` listens on `SOCKET`, by default
//! `$XDG_RUNTIME_DIR/pulse/native` or `/run/pulse/native`, where libpulse
//! looks, and plays what its clients send through `/dev/snd`. The protocol is
//! `media-pulse-server`'s; this is the sockets and the card around it.
//!
//! **The card is the clock.** The loop waits for the sockets at most a
//! quarter of a period, then writes the card a period at a time for as long
//! as it has room, each period taken from the stream that is playing. What a
//! stream has not got is not made up: a stream that underruns gives the card
//! less, and the card plays what it has, so nothing is heard that no client
//! sent. One stream plays at a time until U2c mixes them. Its lines start
//! `pulsed:`.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

/// Only Linux, and Ferrix through its Linux ABI, have `/dev/snd`.
#[cfg(not(target_os = "linux"))]
fn main() {}
