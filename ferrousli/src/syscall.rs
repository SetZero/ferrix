//! Raw system calls: the only way this library reaches the kernel.
//!
//! These return the kernel's value unchanged, where `-4095..=-1` is `-errno`.
//! [`crate::errno`] turns that into C's convention of `-1` plus `errno`.
//!
//! Each architecture has its own instruction, its own registers and its own
//! table of numbers; the functions here are the same on all three. A call
//! whose arguments differ between architectures -- a 64-bit offset split
//! across two registers on ARMv7-A, say -- is spelled out where it is made.

// Explicit paths, because the loader includes this file as a module of its
// own name, `sys`, beside which the default lookup would search.
#[cfg(target_arch = "aarch64")]
#[path = "syscall/aarch64.rs"]
mod aarch64;
#[cfg(target_arch = "arm")]
#[path = "syscall/arm.rs"]
mod arm;
#[cfg(target_arch = "x86_64")]
#[path = "syscall/x86_64.rs"]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
#[cfg(target_arch = "arm")]
pub use arm::*;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

/// `EINVAL`, which [`mmap`] returns itself on ARMv7-A. This file is also the
/// loader's, which has no `errno` module to take it from.
#[cfg(target_arch = "arm")]
const EINVAL: isize = 22;

/// The low half of a 64-bit argument, as ARMv7-A passes it in the first of a
/// pair of registers. The pair starts at an even register: a call whose pair
/// would start at r1 or r3 leaves that register unused.
#[cfg(target_arch = "arm")]
pub const fn low(x: i64) -> usize {
    x as u32 as usize
}

/// The high half of a 64-bit argument, in the second register of its pair.
#[cfg(target_arch = "arm")]
pub const fn high(x: i64) -> usize {
    (x >> 32) as u32 as usize
}

/// Maps memory: `mmap`, with `offset` in bytes. The kernel's value comes back
/// unchanged.
///
/// ARMv7-A has no `mmap` taking a 64-bit offset; its `mmap2` counts the
/// offset in 4096-byte units whatever the page size, so an offset not a
/// multiple of that is refused here with `EINVAL`, as the kernel refuses one
/// not a multiple of the page.
///
/// # Safety
///
/// As [`syscall6`]: the mapping must be sound to make.
pub unsafe fn mmap(
    addr: usize,
    len: usize,
    prot: usize,
    flags: usize,
    fd: usize,
    offset: i64,
) -> isize {
    #[cfg(target_arch = "arm")]
    {
        if offset < 0 || offset & 4095 != 0 {
            return -EINVAL;
        }
        // SAFETY: the caller vouches for the mapping.
        unsafe {
            syscall6(
                nr::MMAP2,
                addr,
                len,
                prot,
                flags,
                fd,
                (offset >> 12) as usize,
            )
        }
    }
    #[cfg(not(target_arch = "arm"))]
    {
        // SAFETY: the caller vouches for the mapping.
        unsafe { syscall6(nr::MMAP, addr, len, prot, flags, fd, offset as usize) }
    }
}

/// Moves `fd`'s offset, and returns the new offset or a negated error number
/// in `-4095..=-1`, which a 32-bit return register could not carry on
/// ARMv7-A: there `_llseek` writes the result through a pointer instead.
pub fn lseek(fd: usize, offset: i64, whence: usize) -> i64 {
    #[cfg(target_arch = "arm")]
    {
        let mut result = 0_i64;
        // SAFETY: the kernel writes the result, a live local, and reads no
        // other memory.
        let ret = unsafe {
            syscall6(
                nr::LLSEEK,
                fd,
                (offset >> 32) as usize,
                offset as usize,
                (&raw mut result).addr(),
                whence,
                0,
            )
        };
        if ret < 0 { ret as i64 } else { result }
    }
    #[cfg(not(target_arch = "arm"))]
    {
        // SAFETY: `lseek` reads no memory.
        unsafe { syscall3(nr::LSEEK, fd, offset as usize, whence) as i64 }
    }
}

/// System call numbers, generated from each architecture's `asm/unistd*.h` by
/// `tools/gen-abi.py`. To use another call, add its name there and run it;
/// never write a number here by hand.
///
/// A call an architecture does not have is not in its table, so naming one
/// fails to compile there rather than dispatching to whatever shares the
/// number.
pub mod nr {
    #[cfg(target_arch = "aarch64")]
    include!("generated/nr_aarch64.rs");
    #[cfg(target_arch = "arm")]
    include!("generated/nr_arm.rs");
    #[cfg(target_arch = "x86_64")]
    include!("generated/nr_x86_64.rs");
}
