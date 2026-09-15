//! The C program that exercises POSIX.1-2024's `stdatomic.h`.

mod common;

use common::{Case, check};

#[test]
fn atomic_types_operations_orders_and_fences_work_across_threads() {
    check(&Case::named("stdatomic"));
}
