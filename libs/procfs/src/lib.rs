//! The text of `/proc`, as pure functions of what the kernel knows.
//!
//! A `/proc` file is an interface, not a report. `ps`, `free`, a C library's
//! `sysconf`, a debugger and a garbage collector all read these files with
//! `sscanf` and fixed columns written against what Linux prints, and a field
//! one column off or a label one space short is a program that silently reads
//! the wrong number. So the formats are pinned here, byte for byte, against
//! lines taken from a real Linux, where `cargo test` can hold them — rather
//! than in the kernel, where the first test of a format would be a program
//! misreading its own memory map.
//!
//! Nothing here knows where the numbers come from. The kernel gathers them —
//! an address space's regions, the frame allocator's counts, a process's
//! identity — and these functions only arrange them. That is also what keeps
//! the kernel honest about a field it cannot fill: it has to pass a value,
//! and the place it does so is the place a comment says why.
//!
//! # What is here
//!
//! * [`maps`] — `/proc/<pid>/maps`, and the parser the kernel's boot check
//!   reads its own output back with.
//! * [`meminfo`] — `/proc/meminfo`.
//! * [`status`] — `/proc/<pid>/status`, and the CPU mask and list formats.
//! * [`stat`] — `/proc/<pid>/stat`, fifty-two fields on one line.
//! * [`mounts`] — `/proc/mounts`, with the octal escapes a mount point with a
//!   space in it needs.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod maps;
pub mod meminfo;
pub mod mounts;
pub mod stat;
pub mod status;
mod text;

#[cfg(test)]
mod tests;

pub use status::State;
