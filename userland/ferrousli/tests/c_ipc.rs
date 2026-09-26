//! The C program that exercises System V shared memory, semaphores and
//! message queues.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn shared_memory_semaphores_and_message_queues_work_and_are_removed() {
    check(&Case::named("ipc/sysv"));
}
