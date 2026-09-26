//! A program's saved registers, copied for a fork child, are the parent's with
//! the return register zero, and read back register by register.
//!
//! Verification, not the entry: a file of its own so that the manifest counts
//! it as the test it is (`scripts/certification-item.json`,
//! `test_file_patterns`). A child of the entry's module, so it can make a
//! [`UserRegs`] from a frame, which nothing outside this architecture can.
//!
//! The register names are what a failure report shows of a thread: its
//! derived `Debug` is the only thing that prints a saved frame, and until this
//! ran no boot had printed one.

use alloc::format;

use super::{SyscallFrame, UserRegs};
use crate::console::println;

/// Copy a frame of distinct values for a child and read every register back.
///
/// # Errors
///
/// A register the copy changed, or one the printed frame does not name with
/// its value.
pub(crate) fn run() -> Result<(), &'static str> {
    let frame = SyscallFrame {
        r15: 15,
        r14: 14,
        r13: 13,
        r12: 12,
        rbp: 0x7fff_0000,
        rbx: 3,
        r9: 9,
        r8: 8,
        r10: 10,
        rdx: 2,
        rsi: 6,
        rdi: 7,
        rax: 57,
        r11: 0x202,
        rcx: 0x40_1000,
        user_rsp: 0x7fff_e000,
    };
    let child = UserRegs(frame).for_child();
    if child.stack_pointer() != frame.user_rsp {
        return Err("a fork child's registers do not keep the parent's stack pointer");
    }
    let printed = format!("{child:?}");
    let expected = [
        "r15: 15,",
        "r14: 14,",
        "r13: 13,",
        "r12: 12,",
        "rbp: 2147418112,",
        "rbx: 3,",
        "r9: 9,",
        "r8: 8,",
        "r10: 10,",
        "rdx: 2,",
        "rsi: 6,",
        "rdi: 7,",
        // The one register a child sees changed: its `fork` returned zero.
        "rax: 0,",
        "r11: 514,",
        "rcx: 4198400,",
        "user_rsp: 2147475456 ",
    ];
    if !printed.starts_with("UserRegs(SyscallFrame { ") {
        return Err("a thread's saved registers do not print as its frame");
    }
    if expected.iter().any(|field| !printed.contains(field)) {
        return Err("a fork child's saved registers are not the parent's with rax zero");
    }
    println!(
        "  regs     a fork child's saved registers are its parent's with rax zero, and all {} \
         print by name",
        expected.len()
    );
    Ok(())
}
