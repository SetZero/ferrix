//! The C programs that exercise startup, exit, thread-local storage, `errno`,
//! the auxiliary vector, the stack protector and the first string functions.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, Ending, SIGABRT, check};

#[test]
fn hello() {
    check(&Case {
        stdout: "hello, world\n",
        ..Case::named("hello")
    });
}

#[test]
fn arguments_environment_and_constructors_reach_main() {
    check(&Case {
        args: &["one", "two words"],
        env: &[("FERROUSLI_TEST", "a value")],
        stdout: "one\ntwo words\na value\n",
        ending: Ending::Code(3),
        ..Case::named("startup")
    });
}

#[test]
fn string_and_memory_functions() {
    check(&Case::named("strings"));
}

#[test]
fn a_failed_call_sets_errno() {
    check(&Case::named("errno"));
}

#[test]
fn thread_local_storage_in_the_static_block() {
    check(&Case::named("tls"));
}

#[test]
fn thread_local_storage_too_large_for_the_static_block() {
    check(&Case::named("tls_large"));
}

#[test]
fn exit_runs_handlers_newest_first_then_destructors() {
    check(&Case {
        stdout: "main\nsecond\nfirst\ndestructor\n",
        ending: Ending::Code(7),
        ..Case::named("exit_handlers")
    });
}

#[test]
fn the_auxiliary_vector_reaches_getauxval() {
    check(&Case::named("auxv"));
}

#[test]
fn the_stack_protector_has_a_canary_and_catches_a_smashed_stack() {
    const ALL: &[&str] = &["-fstack-protector-all"];
    check(&Case {
        cflags: ALL,
        ..Case::named("canary")
    });
    check(&Case {
        cflags: ALL,
        args: &["smash"],
        stderr: "*** stack smashing detected ***: terminated\n",
        ending: Ending::Signal(SIGABRT),
        ..Case::named("canary")
    });
}

#[test]
fn abort_ends_the_process_with_sigabrt() {
    check(&Case {
        ending: Ending::Signal(SIGABRT),
        ..Case::named("abort")
    });
}

#[test]
fn check_h_reports_a_failed_check_on_standard_error() {
    check(&Case {
        stderr: "check_failure.c:9: check failed: 1 + 1 == 3\n",
        ending: Ending::Code(1),
        ..Case::named("check_failure")
    });
}
