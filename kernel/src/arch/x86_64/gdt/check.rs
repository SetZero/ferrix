//! A saved selector is loaded only if it names a segment ring 3 may hold
//! (certification review, T.ESCALATE path 7): every selector a program could
//! have left, and every descriptor a thread-local slot could hold, through
//! `gdt::loadable`, with the answer each must get.
//!
//! Verification, not the entry: a file of its own so that the manifest counts
//! it as the test it is (`scripts/certification-item.json`,
//! `test_file_patterns`). A child of `gdt`, so it reaches the descriptor bits
//! without the table exporting them.

use super::{
    DPL_MASK, EXECUTABLE, KERNEL_CODE, KERNEL_DATA, LONG_MODE, PRESENT, TLS_FIRST_SLOT,
    TSS_SELECTOR, USER_CODE, USER_CODE32, USER_DATA, USER_DATA_DESCRIPTOR, WRITABLE, loadable,
};
use crate::console::println;

/// A thread-local slot's selector, RPL 3.
const fn tls_selector(index: usize) -> u16 {
    (((TLS_FIRST_SLOT + index) as u16) << 3) | 3
}

/// Check every case, and say how many were refused.
///
/// # Errors
///
/// The first selector answered otherwise than it must be.
pub(crate) fn run() -> Result<(), &'static str> {
    // A thread's own data segment in the first slot, as `set_thread_area`
    // builds one.
    let tls = [USER_DATA_DESCRIPTOR, 0, 0];
    let loads: [(u16, u16, &'static str); 7] = [
        (USER_DATA | 3, USER_DATA | 3, "user data"),
        (
            USER_DATA,
            USER_DATA | 3,
            "user data with RPL 0, which must load at RPL 3",
        ),
        (USER_CODE32 | 3, USER_CODE32 | 3, "32-bit user code"),
        (
            tls_selector(0),
            tls_selector(0),
            "a thread-local data segment",
        ),
        (0, 0, "the null selector"),
        (3, 0, "the null selector at RPL 3"),
        (
            tls_selector(0) & !3,
            tls_selector(0),
            "a thread-local segment at RPL 0",
        ),
    ];
    for (selector, wanted, what) in loads {
        if loadable(selector, &tls) != wanted {
            println!("  gdt      {what} ({selector:#x}) was not loaded as {wanted:#x}");
            return Err("a selector ring 3 may hold was not loaded as it should be");
        }
    }

    // Each must load the null selector: every kernel slot, the 64-bit user
    // code (not readable, so no data register holds it), the TSS, the LDT,
    // slots nothing fills, and a thread-local slot holding each kind of
    // descriptor ring 3 may not.
    let forged = [
        (0, 0, "nothing"),
        (1, USER_DATA_DESCRIPTOR & !DPL_MASK, "a DPL 0 data segment"),
        (
            1,
            0x0000_8c00_0000_0000,
            "a 64-bit call gate, a system descriptor",
        ),
        (
            1,
            0x0000_8900_0000_0000,
            "an available TSS, a system descriptor",
        ),
        (
            1,
            USER_DATA_DESCRIPTOR | LONG_MODE,
            "a segment with the L bit",
        ),
        (
            1,
            USER_DATA_DESCRIPTOR & !PRESENT,
            "a data segment marked absent",
        ),
        // Bit 41 is "writable" for data and "readable" for code, so it is
        // cleared to make the code unreadable.
        (
            1,
            (USER_DATA_DESCRIPTOR | EXECUTABLE) & !WRITABLE,
            "a code segment that is not readable",
        ),
        (
            1,
            USER_DATA_DESCRIPTOR | EXECUTABLE | (1 << 42),
            "conforming code",
        ),
    ];
    let mut refused = 0;
    for (index, descriptor, what) in forged {
        let mut slots = tls;
        if let Some(slot) = slots.get_mut(index) {
            *slot = descriptor;
        }
        if loadable(tls_selector(index), &slots) != 0 {
            println!("  gdt      a thread-local slot holding {what} was loadable");
            return Err("a selector naming a descriptor ring 3 may not hold was loaded");
        }
        refused += 1;
    }
    let kernel = [
        KERNEL_CODE,
        KERNEL_DATA,
        KERNEL_DATA | 3,
        USER_CODE | 3,
        TSS_SELECTOR | 3,
        (1 << 3) | 3,
        (10 << 3) | 3,
        (15 << 3) | 3,
        (USER_DATA | 3) | (1 << 2),
        0xFFFF,
    ];
    for selector in kernel {
        if loadable(selector, &tls) != 0 {
            println!("  gdt      selector {selector:#x} was loadable");
            return Err("a selector naming a kernel slot, the TSS or the LDT was loaded");
        }
        refused += 1;
    }
    println!(
        "  gdt      {} selectors ring 3 may hold loaded at RPL 3, {refused} naming kernel slots, \
         the TSS, the LDT or a forged thread-local descriptor refused",
        loads.len()
    );
    Ok(())
}
