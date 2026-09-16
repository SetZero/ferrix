//! The input control protocol, and the evdev queues it feeds: what the
//! kernel's input core and a ring-3 input driver say to each other, and what
//! the core does with the events until a program reads them.
//!
//! `docs/INPUT.md` §3.1 and §3.2 are the specification. Events are small and
//! rare next to a disk's traffic, so there is no ring: one control channel per
//! device, carrying fixed little-endian messages in `libs/displayctl`'s shape.
//!
//! [`message`] is the bytes: every message a fixed structure, decoded
//! strictly. [`session`] is the core's half of the conversation for one
//! device: it judges the driver's HELLO, publishes the capabilities the core
//! supports, checks every event against what the driver declared, keeps the
//! device's state the way Linux's input core does, assembles events into
//! reports and hands each finished report to the glue, together with who may
//! read it under a grab. [`queue`] is one open file's queue: Linux evdev's
//! ring, its `SYN_DROPPED` rule and its packet boundary, and the read path
//! that turns queued events into `struct input_event`s at either width.
//!
//! # Where the behaviour comes from
//!
//! Where `docs/INPUT.md` fixes a rule, it is followed. Where it leaves a rule
//! to Linux — the drop rule, what a read returns, what a grab refuses, which
//! events change the state — the rule is Linux's `drivers/input/evdev.c` and
//! `drivers/input/input.c`, read at the commits named in each module and cited
//! by function. Where both are silent, the crate follows `libs/displayctl`:
//! fixed capacity, reserved bytes that must be zero, a session that stays
//! broken once a driver lies. Where this crate departs from `docs/INPUT.md`
//! because Linux does otherwise, the item says so.
//!
//! Nothing here sends, maps, waits or allocates; the glue does.

#![no_std]
#![forbid(unsafe_code)]

pub mod message;
pub mod queue;
pub mod session;

#[cfg(test)]
mod tests;
