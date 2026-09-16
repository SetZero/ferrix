//! The C programs that exercise threads: `pthread.h`, `semaphore.h`,
//! `threads.h` and `sched.h`, cancellation, and `fork` beside threads.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn the_program_ferrix_boots_with_threads_passes_every_step() {
    check(&Case {
        args: &["sh", "-c", "ignored"],
        stdout: "pthread: create/join\n\
                 pthread: create/join ok\n\
                 pthread: mutex counter\n\
                 pthread: mutex counter ok\n\
                 pthread: condition variable\n\
                 pthread: condition variable ok\n\
                 pthread: thread-local storage\n\
                 pthread: thread-local storage ok\n\
                 pthread: errno\n\
                 pthread: errno ok\n\
                 pthread: all ok\n",
        ..Case::named("thread/on_ferrix")
    });
}

#[test]
fn threads_return_values_and_join_and_detach_report_errors() {
    check(&Case::named("thread/create_join"));
}

#[test]
fn detached_threads_free_their_own_memory() {
    check(&Case::named("thread/detached"));
}

#[test]
fn sixty_four_threads_share_the_allocator() {
    check(&Case::named("thread/malloc_stress"));
}

#[test]
fn errno_and_thread_local_variables_are_per_thread() {
    check(&Case::named("thread/tls"));
}

#[test]
fn each_mutex_type_guards_a_counter_and_reports_its_errors() {
    check(&Case::named("thread/mutex"));
}

#[test]
fn condition_variables_queue_time_out_broadcast_and_signal() {
    check(&Case::named("thread/cond"));
}

#[test]
fn barriers_release_rounds_and_spin_locks_guard_a_counter() {
    check(&Case::named("thread/barrier"));
}

#[test]
fn scheduling_policy_priority_and_affinity_are_read_and_set_per_thread() {
    check(&Case::named("thread/sched"));
}

#[test]
fn semaphores_count_time_out_and_are_shared_named_and_unnamed() {
    check(&Case::named("thread/semaphore"));
}

#[test]
fn c11_threads_mutexes_conditions_and_storage_work_over_pthreads() {
    check(&Case::named("thread/c11"));
}

#[test]
fn keys_hold_values_per_thread_and_run_destructors_in_rounds() {
    check(&Case::named("thread/keys"));
}

#[test]
fn cancellation_acts_at_blocking_calls_in_asynchronous_threads_and_not_while_disabled() {
    check(&Case::named("thread/cancel"));
}
