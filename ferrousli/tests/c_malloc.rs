//! The C programs that exercise `malloc` and its family.
//!
//! libc-test's `malloc-0.c` is adapted in `malloc/malloc_0.c`. Its
//! `malloc-oom.c` and `malloc-brk-fail.c` are not: they need `setrlimit` and
//! `brk`, which the library does not have yet.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, Ending, SIGABRT, SIGSEGV, check};

#[test]
fn malloc_0_returns_unique_pointers_and_free_null_does_nothing() {
    check(&Case::named("malloc/malloc_0"));
}

#[test]
fn allocations_of_many_sizes_are_aligned_and_do_not_overlap() {
    check(&Case::named("malloc/sizes"));
}

#[test]
fn freed_memory_is_reused() {
    check(&Case::named("malloc/reuse"));
}

#[test]
fn realloc_keeps_contents_across_the_thresholds() {
    check(&Case::named("malloc/realloc"));
}

#[test]
fn calloc_zeroes_reused_memory_and_rejects_overflow() {
    check(&Case::named("malloc/calloc"));
}

#[test]
fn aligned_allocations_at_every_power_of_two() {
    check(&Case::named("malloc/align"));
}

#[test]
fn a_random_workload_keeps_every_byte() {
    check(&Case::named("malloc/stress"));
}

/// A misuse that stops the program with `SIGABRT` and this message.
fn caught(what: &'static [&'static str], stderr: &'static str) {
    check(&Case {
        args: what,
        stderr,
        ending: Ending::Signal(SIGABRT),
        ..Case::named("malloc/corruption")
    });
}

/// Every misuse runs from one test, because the harness builds each program
/// under its own name, and parallel tests of one program would overwrite it.
#[test]
fn misuse_is_caught() {
    // A double free, through `free` or `realloc`.
    caught(&["double-free"], "ferrousli: double free\n");
    caught(&["realloc-freed"], "ferrousli: double free\n");
    // The first free erased the stub below the aligned pointer.
    caught(&["double-free-aligned"], "ferrousli: invalid pointer\n");
    // Pointers the allocator never returned.
    caught(&["interior"], "ferrousli: invalid pointer\n");
    caught(&["misaligned"], "ferrousli: invalid pointer\n");
    caught(&["static"], "ferrousli: invalid pointer\n");
    // An overflow into the next block's header.
    caught(&["overflow"], "ferrousli: invalid pointer\n");
    // A use after free that overwrote a free list's link.
    caught(
        &["use-after-free-list"],
        "ferrousli: malloc(): a free block was overwritten\n",
    );
    // `free` gives a large block back to the kernel, so touching it faults.
    check(&Case {
        args: &["large-use-after-free"],
        ending: Ending::Signal(SIGSEGV),
        ..Case::named("malloc/corruption")
    });
}
