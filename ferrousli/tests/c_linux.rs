//! The C program that exercises Linux's own calls and the process, host name,
//! ownership and time calls busybox needs beside them.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn linux_and_process_calls_reach_the_kernel_without_changing_the_system() {
    check(&Case::named("linux/calls"));
}
