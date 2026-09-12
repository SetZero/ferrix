//! What the system calls itself.
//!
//! # Why `sysname` says `Linux`
//!
//! Because a program that asks is deciding what to do, not printing a label.
//! `uname -s` is what configure scripts branch on, what a libc checks before
//! using a system call it thinks is new enough, and what build systems use to
//! choose a code path. Answering `Ferrix` would send every one of them down
//! the path nobody has tested — and Ferrix's whole claim is that the Linux
//! system call interface *is* its interface, not an emulation of somebody
//! else's. `libs/linux-abi` says the same thing at [`Utsname::sysname`].
//!
//! The identity goes where it does no harm and is still visible: `nodename`,
//! `release` and `version`. `uname -a` reads
//!
//! ```text
//! Linux ferrix 6.1.0-ferrix #1 Ferrix 0.1.0 x86_64 GNU/Linux
//! ```
//!
//! which tells a person exactly what they are running while telling a script
//! what it needs to hear.

use ferrix_bootinfo::Arch;
use ferrix_linux_abi::errno::Errno;

use crate::arch;
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// Bytes in each of `utsname`'s six fields.
const FIELD: usize = 65;

/// The kernel release.
///
/// Not arbitrary: a configure script compares this against a minimum, and
/// glibc refuses to start under a kernel it reads as older than the one it was
/// built for. So it is a plausible modern Linux version with Ferrix named in
/// the suffix, which is exactly what a distribution kernel does.
pub(crate) const RELEASE: &str = "6.1.0-ferrix";

/// The version string, which by convention starts with a build number.
pub(crate) const VERSION: &str = "#1 Ferrix 0.1.0";

/// `uname`.
pub(crate) fn sys_uname(process: &Process, at: u64) -> Result<usize, Errno> {
    // The name Linux uses for the machine, which is not always the name this
    // tree uses for the architecture: 32-bit Arm is `armv7l` to `uname` and
    // `armv7a` here.
    let machine = match arch::ARCH {
        Arch::X86_64 => "x86_64",
        Arch::AArch64 => "aarch64",
        Arch::Armv7a => "armv7l",
    };
    let fields = ["Linux", "ferrix", RELEASE, VERSION, machine, "(none)"];

    // Built whole and copied once. Every field is NUL-padded because the
    // structure is fixed-width and a reader stops at the first NUL.
    let mut buffer = [0_u8; FIELD * 6];
    for (slot, text) in buffer.chunks_mut(FIELD).zip(fields) {
        for (byte, source) in slot.iter_mut().zip(text.bytes()) {
            *byte = source;
        }
    }
    uaccess::copy_to_user(process.space(), at, &buffer).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}
