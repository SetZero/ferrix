//! `stdio.h`: buffered streams, and the `printf` family.
//!
//! * [`file`] holds the `FILE` structure, its buffering and the three standard
//!   streams, and the list of open streams that `fflush(NULL)` and `exit`
//!   walk.
//! * [`open`] opens and closes streams on descriptors, and removes files.
//! * [`memory`] makes streams over memory and over a program's own functions:
//!   `open_memstream`, `fmemopen` and `fopencookie`.
//! * [`io`] reads, writes and positions streams, and sets their buffering and
//!   locks.
//! * [`printf`] formats, [`float`] converts floating point exactly, and
//!   [`fortify`] has glibc's checked entry points.
//! * [`wide`] reads and writes wide characters and wide strings, and orients
//!   streams with `fwide`; [`wprintf`] formats wide output and [`wscanf`] reads it.
//! * [`ext`] is `stdio_ext.h`, and glibc's `__uflow` and `__overflow`.
//!
//! # glibc's `FILE` layout, as far as its macros read it
//!
//! `FILE` is opaque: musl's header declares `struct _IO_FILE` without members,
//! and programs compiled against it reach a stream only through functions.
//! glibc's header instead exposes its structure, and binaries built against
//! it read its fields directly through the inline `getc_unlocked`,
//! `putc_unlocked`, `feof_unlocked` and `ferror_unlocked`. Ferrousli's `FILE`
//! begins with the fields those read, arranged so that each finds its buffer
//! used up and calls [`ext::__uflow`] or [`ext::__overflow`] (`file.rs`,
//! `GlibcHead`). A program that reaches further into the structure, as only
//! glibc's own `libio` should, is not served.
//!
//! # Not here yet
//!
//! `fgetln`, `gets`, `tempnam`, `ctermid` and `cuserid`.
//! `scanf` is in [`scanf`], `popen` and `pclose` in [`crate::spawn`], and
//! `tmpnam` in [`crate::temp`]; `rename` and `renameat` are with the other file
//! system calls.

pub mod ext;
pub mod file;
pub mod float;
pub mod fortify;
pub mod io;
pub mod lock;
pub mod memory;
pub mod open;
pub mod printf;
pub mod scanf;
mod sys;
#[cfg(test)]
mod tests;
pub mod wide;
pub mod wprintf;
pub mod wscanf;

pub use file::{File, exit_flush};

/// `EOF`, the error return of the character and line functions.
pub const EOF: core::ffi::c_int = -1;
