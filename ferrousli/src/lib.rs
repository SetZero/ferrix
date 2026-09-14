//! Ferrousli: a C library for Linux, written in Rust.
//!
//! The name is *ferrous* and *musl*. The model is musl: small, correct, and
//! built to be linked statically. The destination is further. A program built
//! against glibc should one day be able to load this library in glibc's place,
//! which means matching glibc's exported symbols, structure layouts and startup
//! contract rather than only its API.
//!
//! The library talks to the kernel only through system calls, so it runs on
//! any Linux, and it is tested on the host that builds it.
//!
//! # Building for C and for tests
//!
//! Built normally, this is a `no_std` static library whose functions carry
//! their C names. Built for `cargo test`, it links std, and every export keeps
//! its Rust mangled name, so the functions can be unit tested without
//! colliding with the host libc's `strlen` in the same test binary. That is
//! what each `cfg_attr(not(test), unsafe(no_mangle))` is for.

#![cfg_attr(not(test), no_std)]
// LLVM recognises a byte-copying loop and replaces it with a call to `memcpy`.
// Inside `memcpy` that call is the function calling itself until the stack
// runs out. `no_builtins` turns the recognition off, and it can only be turned
// off for a whole crate. It does not stop rustc copying large values with
// `memcpy`; `string`'s module documentation covers that half.
#![no_builtins]

#[cfg(not(target_arch = "x86_64"))]
compile_error!("ferrousli supports only x86-64 so far");

pub mod arch;
pub mod arith;
pub mod assert;
pub mod auxv;
pub mod cancel;
pub mod cond;
pub mod crypt;
pub mod ctype;
pub mod dirent;
pub mod endian;
pub mod env;
pub mod errno;
pub mod ether;
pub mod exit;
pub mod fcntl;
pub mod float;
pub mod fnmatch;
pub mod futex;
pub mod getopt;
pub mod growable;
pub mod grp;
#[cfg(test)]
mod host_glibc;
pub mod ifaddrs;
pub mod inet;
pub mod ioctl;
pub mod ipc;
pub mod key;
pub mod libgen;
pub mod linux;
pub mod locale;
pub mod lock;
pub mod malloc;
pub mod mman;
pub mod mntent;
pub mod mount;
pub mod multibyte;
pub mod mutex;
pub mod netdb;
pub mod poll;
pub mod process;
pub mod pthread;
pub mod pthread_attr;
pub mod pwd;
pub mod qsort;
pub mod rand;
pub mod random;
pub mod realpath;
pub mod rename;
pub mod resource;
pub mod rwlock;
pub mod scan;
pub mod sched;
pub mod select;
#[cfg(not(test))]
pub mod setjmp;
pub mod shadow;
pub mod sigaction;
pub mod signal;
pub mod sigset;
pub mod socket;
pub mod spawn;
#[cfg(not(test))]
pub mod start;
pub mod stat;
pub mod statvfs;
pub mod stdio;
pub mod stdlib;
pub mod strerror;
pub mod strftime;
pub mod string;
pub mod strings;
pub mod strptime;
pub mod strtod;
pub mod strtol;
pub mod stubs;
pub mod syscall;
pub mod sysconf;
pub mod syslog;
pub mod temp;
pub mod termios;
pub mod thread;
pub mod time;
pub mod times;
pub mod tm;
pub mod tz;
pub mod uio;
pub mod unistd;
pub mod utmpx;
pub mod utsname;
pub mod va;
pub mod wait;
pub mod wchar;
pub mod wctype;

/// A panic inside the library has nothing to unwind into, because every frame
/// above it is C. Trapping stops the program at the fault, where a debugger or
/// a core dump can still see it.
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    syscall::trap()
}

/// The unwinder's personality routine, which nothing ever calls.
///
/// This target's precompiled `core` was built to unwind, and its unwind tables
/// name this symbol even though this library aborts instead. Without a
/// definition, every C program fails to link. A frame is never unwound through,
/// so reaching this is a bug, and it traps.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {
    syscall::trap()
}
