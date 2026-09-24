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

#[test]
fn an_eventfd_written_by_one_thread_wakes_another_threads_epoll_wait() {
    check(&Case::named("linux/epoll"));
}

#[test]
fn a_timerfd_expires_into_epoll_and_counts_the_periods_that_passed() {
    check(&Case::named("linux/timerfd"));
}
