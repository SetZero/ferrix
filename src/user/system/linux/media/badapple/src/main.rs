//! Bad Apple!!, on Ferrix, with its sound.
//!
//! `badapple VIDEO.bav SONG.m4a [SECONDS]` shows the video on
//! `/dev/dri/card0` and plays the song through `/dev/snd`, for `SECONDS` or
//! to the end. The song is the original's AAC track, decoded here; the
//! video is the original converted by the host into `.bav`
//! (`src/user/system/linux/media/bav`), since there is no H.264 decoder in Rust to run.
//!
//! **The sound card is the clock.** A thread decodes the song, converts it to
//! the card's 48 kHz and writes it; the writes block while the card's buffer
//! is full, so the song plays at the card's pace, and after each write the
//! thread records how far the speaker has got. The picture thread shows the
//! frame for that moment, skipping frames if it has fallen behind, so the
//! picture follows the song however the machine is loaded. Without a card
//! the clock is the wall clock.
//!
//! As init (`xtask test-badapple`, `run-badapple`) it has the card to
//! itself and never exits: it says what it did, holds the last frame, and
//! waits. On a desktop -- `WAYLAND_DISPLAY` set -- it is a window instead
//! (`window.rs`), which ends the program when it is closed. Its lines start
//! `badapple:`.

mod args;
mod clock;
mod scale;
mod step;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod screen;
#[cfg(target_os = "linux")]
mod sound;
#[cfg(target_os = "linux")]
mod window;

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

/// Only Linux, and Ferrix through its Linux ABI, have `/dev/dri` and
/// `/dev/snd`; elsewhere the program builds so its arithmetic is tested.
#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(test)]
mod tests;
