//! The bookkeeping behind the native ABI's kernel objects.
//!
//! Stage 9 of `docs/ROADMAP.md`. A handle table and a channel's message queue
//! are data structures with rules — a closed handle must never resolve again,
//! a failed send must leave every handle where it was — and neither rule
//! needs a processor to state or to check. So they are here, generic over
//! what a handle names, where `cargo test`, Miri and a fuzzer can reach them,
//! and the kernel instantiates them over `Arc`s of its real objects.
//!
//! What is *not* here is anything that waits, wakes or locks. The kernel
//! wraps each structure in its own lock and decides who to wake; the
//! structures only answer what the state is and whether a change is allowed.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod message;
pub mod table;

#[cfg(test)]
mod tests;
