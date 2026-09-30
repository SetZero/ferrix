//! The Android Debug Bridge's wire protocol, the device's half: what
//! `src/user/system/linux/adbd` reads from and writes to the host's `adb`.
//!
//! `docs/ADB.md` is why Ferrix speaks adb and how much of it. The authority
//! is AOSP's `packages/modules/adb`: `protocol.txt` for [`message`] and
//! `SYNC.TXT` for [`sync`]. Every constant here was checked against the host's
//! `adb` 37 on example, which is the test that matters.
//!
//! * [`message`]: the 24-byte header every message starts with, and the
//!   banner a device answers `CNXN` with.
//! * [`sync`]: the `sync:` service's requests and replies, which carry
//!   `adb push` and `adb pull`.
//!
//! Nothing here reads, writes, spawns or waits: the program does. What is
//! here is what can be got wrong in bytes, and so what is worth testing on
//! the host.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod message;
pub mod sync;

#[cfg(test)]
mod tests;
