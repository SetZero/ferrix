use super::{STEPS, judge, negative_failed_there, verdict};

// Recorded from the first run under Ferrix (2026-10-02, x86-64 under KVM),
// the lines `boot` returns.
const PASSED_ON_X86_64: &str = "\
  7.28 | FERRIX-BOOT-OK stages 1-12\n\
  7.28 |   init     19351 KiB program built in, starting `sh -c` with a built-in script\n\
  7.28 | nvidia-uvm:  Built-in UVM tests are enabled. This is a security risk.\n\
  7.28 | uvm-selftest: module init 0\n\
  7.28 | uvm-selftest: UVM_INITIALIZE                     PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.29 | uvm-selftest: UVM_TEST_RNG_SANITY                PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.29 | uvm-selftest: UVM_TEST_RANGE_TREE_DIRECTED       PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.29 | uvm-selftest: UVM_TEST_LOCK_SANITY               PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.29 | uvm-selftest: UVM_TEST_PERF_UTILS_SANITY         PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.29 | uvm-selftest: UVM_TEST_KVMALLOC                  PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.29 | uvm-selftest: UVM_TEST_PERF_EVENTS_SANITY        PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.89 | uvm-selftest: UVM_TEST_NV_KTHREAD_Q              PASS (ioctl 0, status Success [NV_OK], 601 ms)\n\
  7.89 | uvm-selftest: UVM_TEST_RB_TREE_DIRECTED          PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.89 | uvm-selftest: UVM_TEST_CPU_CHUNK_API             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.93 | uvm-selftest: UVM_TEST_RANGE_ALLOCATOR_SANITY    PASS (ioctl 0, status Success [NV_OK], 37 ms)\n\
 14.21 | uvm-selftest: UVM_TEST_RB_TREE_RANDOM            PASS (ioctl 0, status Success [NV_OK], 6272 ms)\n\
 14.21 | uvm-selftest: UVM_CREATE_RANGE_GROUP             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 14.21 | uvm-selftest: UVM_CREATE_RANGE_GROUP             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 14.21 | uvm-selftest: UVM_CREATE_RANGE_GROUP             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 14.21 | uvm-selftest: UVM_CREATE_RANGE_GROUP             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 14.21 | uvm-selftest: UVM_TEST_RANGE_GROUP_TREE          PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 14.21 | uvm-selftest: UVM_TEST_THREAD_CONTEXT_SANITY     PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 14.26 | uvm-selftest: UVM_TEST_THREAD_CONTEXT_PERF       PASS (ioctl 0, status Success [NV_OK], 48 ms)\n\
 14.26 | uvm-selftest: UVM_TEST_GET_CPU_CHUNK_ALLOC_SIZES PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 14.26 | uvm-selftest: PASS, 0 failed\n\
 14.26 |   init     the shell exited with 0\n\
";

fn lines(transcript: &str) -> Vec<String> {
    transcript.lines().map(str::to_owned).collect()
}

#[test]
fn the_recorded_run_passes() {
    assert_eq!(judge(&lines(PASSED_ON_X86_64)), None);
}

#[test]
fn the_recorded_run_is_not_a_negative_control() {
    assert!(!negative_failed_there(&lines(PASSED_ON_X86_64)));
}

#[test]
fn fifteen_of_the_steps_are_uvm_tests() {
    assert_eq!(
        STEPS
            .iter()
            .filter(|name| name.starts_with("UVM_TEST_"))
            .count(),
        15
    );
}

#[test]
fn verdicts_are_read_and_other_lines_are_not() {
    assert_eq!(
        verdict(
            " 7.29 | uvm-selftest: UVM_TEST_KVMALLOC                  PASS (ioctl 0, status Success [NV_OK], 0 ms)"
        ),
        Some(("UVM_TEST_KVMALLOC", true))
    );
    assert_eq!(
        verdict(
            "uvm-selftest: UVM_TEST_KVMALLOC FAIL (ioctl 0, status Generic Error: Invalid state [NV_ERR_INVALID_STATE], 0 ms)"
        ),
        Some(("UVM_TEST_KVMALLOC", false))
    );
    assert_eq!(verdict(" 7.28 | uvm-selftest: module init 0"), None);
    assert_eq!(verdict("14.26 | uvm-selftest: PASS, 0 failed"), None);
}

#[test]
fn a_failed_test_is_named() {
    let run = PASSED_ON_X86_64.replace(
        "UVM_TEST_LOCK_SANITY               PASS",
        "UVM_TEST_LOCK_SANITY               FAIL",
    );
    assert_eq!(
        judge(&lines(&run)),
        Some("UVM_TEST_LOCK_SANITY failed".to_owned())
    );
}

#[test]
fn a_missing_test_is_refused() {
    let run: Vec<String> = lines(PASSED_ON_X86_64)
        .into_iter()
        .filter(|line| !line.contains("UVM_TEST_RB_TREE_RANDOM"))
        .collect();
    assert!(judge(&run).is_some_and(|why| why.contains("19 ioctls")));
}

#[test]
fn a_run_that_stopped_early_is_refused() {
    let run: Vec<String> = lines(PASSED_ON_X86_64)
        .into_iter()
        .filter(|line| !line.contains("exited with"))
        .collect();
    assert_eq!(judge(&run), Some("it did not exit with 0".to_owned()));
}

// The negative control, recorded from the same day's gate.
const NEGATIVE_ON_X86_64: &str = "\
  7.18 | FERRIX-BOOT-OK stages 1-12\n\
  7.18 |   init     19350 KiB program built in, starting `sh -c` with a built-in script\n\
  7.19 | nvidia-uvm:  Built-in UVM tests are enabled. This is a security risk.\n\
  7.19 | uvm-selftest: module init 0\n\
  7.19 | uvm-selftest: UVM_INITIALIZE                     PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.19 | uvm-selftest: UVM_TEST_RNG_SANITY                PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.19 | uvm-selftest: UVM_TEST_RANGE_TREE_DIRECTED       PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.19 | uvm-selftest: UVM_TEST_LOCK_SANITY               PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.19 | uvm-selftest: UVM_TEST_PERF_UTILS_SANITY         PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.19 | nvidia-uvm: uvm_kvmalloc_test.c:171 test_uvm_kvrealloc[pid:1] Test check failed, condition 'uvm_kvrealloc(new_p, 0) == ZERO_SIZE_PTR' not true\n\
  7.20 | uvm-selftest: UVM_TEST_KVMALLOC                  FAIL (ioctl 0, status Generic Error: Invalid state [NV_ERR_INVALID_STATE], 0 ms)\n\
  7.20 | uvm-selftest: UVM_TEST_PERF_EVENTS_SANITY        PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.66 | uvm-selftest: UVM_TEST_NV_KTHREAD_Q              PASS (ioctl 0, status Success [NV_OK], 458 ms)\n\
  7.66 | uvm-selftest: UVM_TEST_RB_TREE_DIRECTED          PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.66 | uvm-selftest: UVM_TEST_CPU_CHUNK_API             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
  7.70 | uvm-selftest: UVM_TEST_RANGE_ALLOCATOR_SANITY    PASS (ioctl 0, status Success [NV_OK], 42 ms)\n\
 13.95 | uvm-selftest: UVM_TEST_RB_TREE_RANDOM            PASS (ioctl 0, status Success [NV_OK], 6249 ms)\n\
 13.95 | uvm-selftest: UVM_CREATE_RANGE_GROUP             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 13.95 | uvm-selftest: UVM_CREATE_RANGE_GROUP             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 13.95 | uvm-selftest: UVM_CREATE_RANGE_GROUP             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 13.96 | uvm-selftest: UVM_CREATE_RANGE_GROUP             PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 13.96 | uvm-selftest: UVM_TEST_RANGE_GROUP_TREE          PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 13.96 | uvm-selftest: UVM_TEST_THREAD_CONTEXT_SANITY     PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 14.01 | uvm-selftest: UVM_TEST_THREAD_CONTEXT_PERF       PASS (ioctl 0, status Success [NV_OK], 48 ms)\n\
 14.01 | uvm-selftest: UVM_TEST_GET_CPU_CHUNK_ALLOC_SIZES PASS (ioctl 0, status Success [NV_OK], 0 ms)\n\
 14.01 | uvm-selftest: FAIL, 1 failed\n\
 14.01 |   init     the shell exited with 1\n\
";

#[test]
fn the_recorded_negative_control_fails_where_it_must() {
    assert!(negative_failed_there(&lines(NEGATIVE_ON_X86_64)));
    assert_eq!(
        judge(&lines(NEGATIVE_ON_X86_64)),
        Some("UVM_TEST_KVMALLOC failed".to_owned())
    );
}

#[test]
fn a_negative_control_failing_elsewhere_too_is_refused() {
    let run = NEGATIVE_ON_X86_64.replace(
        "UVM_TEST_RB_TREE_DIRECTED          PASS",
        "UVM_TEST_RB_TREE_DIRECTED          FAIL",
    );
    assert!(!negative_failed_there(&lines(&run)));
}
