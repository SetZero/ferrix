//! The first program: a shell, if one was built in.
//!
//! Started after the boot marker rather than before it, which is what lets one
//! kernel serve both uses. `cargo xtask test-boot` stops QEMU the moment it
//! sees the marker, so the boot test is unchanged whether or not a shell is
//! embedded; `cargo xtask run` leaves the serial port attached to the terminal,
//! so the same image hands a person a prompt.
//!
//! # Why busybox and not something written for the purpose
//!
//! Because the point is somebody else's binary. A shell written against this
//! kernel would work by construction and prove nothing about the ABI; a static
//! `busybox` was linked against Linux by people who have never heard of
//! Ferrix, and every system call it makes is one this kernel either answers
//! the way Linux does or gets wrong in a way the shell will show.

use crate::arch;
use crate::console::println;
use crate::syscall::exec;

/// The embedded program, or nothing. See `kernel/build.rs`.
pub(crate) static IMAGE: &[u8] = include_bytes!(env!("FERRIX_INIT_IMAGE"));

/// A script for the shell to run with `-c`, or nothing for an interactive
/// one. See `kernel/build.rs`.
static SCRIPT: &[u8] = include_bytes!(env!("FERRIX_INIT_SCRIPT_FILE"));

/// Start the shell, and report how it ended.
///
/// Returns when the shell exits, which on an interactive session is when
/// somebody types `exit`.
pub(crate) fn run() {
    if IMAGE.is_empty() {
        return;
    }
    let interactive: [&[u8]; 2] = [b"sh", b"-i"];
    let scripted: [&[u8]; 3] = [b"sh", b"-c", SCRIPT];
    let (args, how): (&[&[u8]], _) = if SCRIPT.is_empty() {
        (&interactive, "`sh -i`")
    } else {
        (&scripted, "`sh -c` with a built-in script")
    };
    println!(
        "  init     {} KiB program built in, starting {how}",
        IMAGE.len() / 1024
    );

    let status = exec::run(
        IMAGE,
        args,
        &[b"PATH=/bin", b"HOME=/", b"TERM=dumb", b"PS1=ferrix# "],
        random_bytes(),
    );
    match status {
        Ok(status) => println!("  init     the shell exited with {status}"),
        Err(problem) => println!("  init     the shell could not be started: {problem:?}"),
    }
}

/// Sixteen bytes for `AT_RANDOM`.
///
/// **Not random.** Two readings of the high-resolution counter, which differ
/// from boot to boot and are good enough that a libc's stack-protector canary
/// is not the same constant on every machine. They are not good enough for
/// anything an attacker is involved in, and nothing here pretends otherwise:
/// the entropy pool is a later stage's.
fn random_bytes() -> [u8; ferrix_ustack::RANDOM_BYTES] {
    let first = arch::counter_now().to_le_bytes();
    let second = arch::counter_now().rotate_left(29).to_le_bytes();
    let mut bytes = [0_u8; ferrix_ustack::RANDOM_BYTES];
    for (slot, value) in bytes.iter_mut().zip(first.iter().chain(second.iter())) {
        *slot = *value;
    }
    bytes
}
