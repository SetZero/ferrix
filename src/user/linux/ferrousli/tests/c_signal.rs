//! The C programs that exercise `signal.h` and `setjmp.h`: handlers, flags,
//! masks, waiting, the alternate stack, the reserved signals, and jumps.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn handlers_run_for_raise_and_kill_and_report_the_old_action() {
    check(&Case::named("signal/handlers"));
}

#[test]
fn sigaction_flags_reset_defer_and_mask() {
    check(&Case::named("signal/flags"));
}

#[test]
fn blocked_signals_pend_and_are_delivered_when_unblocked() {
    check(&Case::named("signal/masks"));
}

#[test]
fn sigtimedwait_times_out_and_sigqueue_carries_a_value() {
    check(&Case::named("signal/wait"));
}

#[test]
fn a_handler_with_sa_onstack_runs_on_the_alternate_stack() {
    check(&Case::named("signal/altstack"));
}

#[test]
fn reserved_and_nonexistent_signals_are_refused() {
    check(&Case::named("signal/reserved"));
}

#[test]
fn setjmp_and_longjmp_restore_registers_values_and_masks() {
    check(&Case::named("signal/setjmp"));
}

#[test]
fn siglongjmp_leaves_signal_handlers() {
    check(&Case::named("signal/siglongjmp"));
}
