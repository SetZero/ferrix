//! The sound control protocol, and the PCM stream it feeds: what the kernel's
//! audio core and a ring-3 sound driver say to each other, and what the core
//! keeps of a stream between a program's writes and the device's completions.
//!
//! `docs/AUDIO.md` §3.1 and §3.2 are the specification. Messages are small and
//! come at most once a period, so there is no ring: one control channel per
//! card, carrying fixed little-endian messages in `libs/inputctl`'s shape, and
//! the samples themselves in a buffer the core allocates and the driver pins
//! for the device to read.
//!
//! [`message`] is the bytes: every message a fixed structure, decoded
//! strictly. [`session`] is the core's half of the conversation for one card:
//! it judges the driver's HELLO, chooses what to publish, and routes the
//! driver's reports to the stream, refusing any that lies. [`pcm`] is the
//! stream: ALSA's states, pointers and thresholds as Linux keeps them, what
//! is submitted to the driver and when, and what `STATUS`, `SYNC_PTR` and
//! `poll` answer. [`refine`] is `HW_REFINE` and `HW_PARAMS`: Linux's interval
//! and mask rules against a card's one configuration.
//!
//! # Where the behaviour comes from
//!
//! Where `docs/AUDIO.md` fixes a rule, it is followed. Where it leaves a rule
//! to Linux -- the refine, the pointers, when a stream starts, stops or
//! underruns, what each request answers in each state -- the rule is Linux's
//! `sound/core/pcm_native.c` and `pcm_lib.c`, copied to
//! `~/.local/share/ferrix/audio-ref/kernel/` on 2026-09-26 and cited by
//! function. Where the two part, because the core has no device to program
//! and no mapping to offer, the item says so, and `docs/AUDIO.md` §6 lists
//! the deviations.
//!
//! Nothing here sends, maps, copies samples, waits, reads a clock or
//! allocates; the glue does.

#![no_std]
#![forbid(unsafe_code)]

pub mod message;
pub mod pcm;
pub mod refine;
pub mod session;

#[cfg(test)]
mod tests;
