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
// C's types are not the same Rust types on every target: `char` is `i8` on
// x86-64 and `u8` on Arm, and `long` is 64 bits on the 64-bit targets and 32
// on ARMv7-A. A cast or `from` between one of them and a fixed-width type is
// needed on some targets and the identity on the others, where these two
// lints would reject it.
#![allow(
    clippy::unnecessary_cast,
    clippy::useless_conversion,
    reason = "C's char and long differ between the targets"
)]

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm")))]
compile_error!("ferrousli supports x86-64, AArch64 and ARMv7-A");

pub mod arch;
pub mod arith;
#[cfg(target_arch = "arm")]
mod arm_names;
pub mod assert;
pub mod auxv;
pub mod barrier;
pub mod cancel;
pub mod complex;
pub mod cond;
pub mod crypt;
pub mod ctype;
pub mod dirent;
pub mod endian;
pub mod env;
pub mod epoll;
pub mod errno;
pub mod ether;
pub mod eventfd;
pub mod exit;
pub mod fcntl;
pub mod fenv;
pub mod float;
pub mod fnmatch;
pub mod fortify;
pub mod futex;
pub mod getopt;
#[cfg(target_arch = "arm")]
pub mod glibc_time64;
pub mod glob;
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
pub mod link;
pub mod linux;
pub mod linux_ext;
pub mod loader;
pub mod locale;
pub mod lock;
pub mod malloc;
pub mod math;
pub mod mman;
pub mod mntent;
pub mod mount;
pub mod multibyte;
pub mod mutex;
pub mod netdb;
pub mod nl_types;
pub mod poll;
pub mod posix_spawn;
pub mod process;
pub mod pthread;
pub mod pthread_attr;
pub mod pty;
pub mod pwd;
pub mod qsort;
pub mod rand;
pub mod random;
pub mod random_r;
pub mod realpath;
pub mod regex;
pub mod rename;
pub mod resource;
pub mod rwlock;
pub mod scan;
pub mod sched;
pub mod search;
pub mod select;
pub mod semaphore;
#[cfg(not(test))]
pub mod setjmp;
pub mod shadow;
pub mod sigaction;
pub mod signal;
pub mod signalfd;
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
pub mod syscall;
pub mod sysconf;
pub mod syslog;
pub mod sysmacros;
pub mod temp;
pub mod termios;
pub mod thread;
pub mod threads;
pub mod time;
pub mod timerfd;
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
pub mod wcsto;
pub mod wctype;
pub mod xattr;

/// A panic inside the library has nothing to unwind into, because every frame
/// above it is C. Trapping stops the program at the fault, where a debugger or
/// a core dump can still see it.
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    syscall::trap()
}

// The unwinder's personality routine, which nothing ever calls.
//
// This target's precompiled `core` was built to unwind, and its unwind tables
// name this symbol even though this library aborts instead. Without a
// definition, every C program fails to link. A frame is never unwound
// through, so reaching this is a bug, and it traps: `ud2`, which is what
// `syscall::trap` emits.
//
// It is weak, and it is assembly because of that. A Rust program linked
// against this library brings its own `std`, which defines
// `rust_eh_personality` too, and two strong definitions is a link error — the
// first thing that stopped uutils/coreutils linking here. A weak one loses to
// `std`'s, which is the right outcome: a program that really unwinds should
// use the personality routine belonging to the code that laid the unwind
// tables down. stable Rust cannot mark a `#[no_mangle]` function weak, and in
// assembly `.weak` is one directive.
#[cfg(all(not(test), target_arch = "x86_64"))]
core::arch::global_asm!(
    ".pushsection .text.rust_eh_personality,\"ax\",@progbits",
    ".p2align 4",
    ".weak rust_eh_personality",
    ".type rust_eh_personality, @function",
    "rust_eh_personality:",
    "ud2",
    ".size rust_eh_personality, . - rust_eh_personality",
    ".popsection",
);

// The same on AArch64 and ARMv7-A, where `syscall::trap` emits `udf`.
#[cfg(all(not(test), any(target_arch = "aarch64", target_arch = "arm")))]
core::arch::global_asm!(
    ".pushsection .text.rust_eh_personality,\"ax\",%progbits",
    ".p2align 2",
    ".weak rust_eh_personality",
    ".type rust_eh_personality, %function",
    "rust_eh_personality:",
    "udf #0",
    ".size rust_eh_personality, . - rust_eh_personality",
    ".popsection",
);

// ARMv7-A's unwind tables, `.ARM.exidx`, name the EHABI personality routines
// instead: every function this target's `core` was compiled with refers to
// `__aeabi_unwind_cpp_pr0` or `pr1`, and a C program linked without libgcc's
// unwinder has none. They are weak for the same reason, and trap for the same
// reason: nothing here unwinds, and a program that does brings its own.
#[cfg(all(not(test), target_arch = "arm"))]
core::arch::global_asm!(
    ".pushsection .text.__aeabi_unwind_cpp_pr,\"ax\",%progbits",
    ".p2align 2",
    ".arm",
    ".weak __aeabi_unwind_cpp_pr0",
    ".type __aeabi_unwind_cpp_pr0, %function",
    ".weak __aeabi_unwind_cpp_pr1",
    ".type __aeabi_unwind_cpp_pr1, %function",
    ".weak __aeabi_unwind_cpp_pr2",
    ".type __aeabi_unwind_cpp_pr2, %function",
    "__aeabi_unwind_cpp_pr0:",
    "__aeabi_unwind_cpp_pr1:",
    "__aeabi_unwind_cpp_pr2:",
    "udf #0",
    ".size __aeabi_unwind_cpp_pr0, . - __aeabi_unwind_cpp_pr0",
    ".popsection",
);
