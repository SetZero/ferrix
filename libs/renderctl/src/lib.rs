//! The render control protocol: what the kernel's render core and a ring-3
//! GPU driver say to each other over their control channel.
//!
//! `docs/GPU.md` §3.3 is the specification, and the reason this is a crate
//! of its own rather than part of the virtio-gpu driver. The seam between
//! "what every GPU can do" and "what this GPU's command stream says" is
//! here: a message asks for a context, an object, a submission or a wait,
//! and the *contents* of a command buffer and of an object's descriptor
//! pass through as bytes the driver understands and the core does not.
//!
//! That is what lets a second driver be loaded behind the same node later
//! -- an NVIDIA one, `docs/GPU.md` §4 -- without the node, the handle
//! table, the object lifetime or this protocol being written twice. It is
//! also why no attempt is made to abstract the command stream itself:
//! virgl's TGSI and NVIDIA's methods are different languages, Linux does
//! not pretend otherwise either, and an abstraction that claimed to hide it
//! would cost a rewrite the first time it met a second device.
//!
//! The shape is `libs/displayctl`'s, because the two solve the same
//! problem: one `Channel` per device, a fixed little-endian message with
//! its type and length first, decoded strictly, handles in the channel
//! message's array, and a message on the channel as its own doorbell.
//!
//! Nothing here sends, maps or waits; the glue does.

#![no_std]
#![forbid(unsafe_code)]

pub mod message;

#[cfg(test)]
mod tests;
