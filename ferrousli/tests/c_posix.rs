//! The C programs that exercise the POSIX calls that wrap system calls: files,
//! directories, descriptors, processes, time, memory, limits and identity.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn the_life_of_a_file() {
    check(&Case::named("posix/files"));
}

#[test]
fn permissions_the_creation_mask_special_files_and_timestamps() {
    check(&Case::named("posix/modes"));
}

#[test]
fn directories() {
    check(&Case::named("posix/directories"));
}

#[test]
fn pipes_descriptors_poll_and_select() {
    check(&Case {
        stdout: "to stdout\n",
        ..Case::named("posix/descriptors")
    });
}

#[test]
fn fork_wait_exec_and_sessions() {
    check(&Case::named("posix/processes"));
}

#[test]
fn clocks_and_sleeping() {
    check(&Case::named("posix/time"));
}

#[test]
fn randomness_and_identity() {
    check(&Case::named("posix/identity"));
}

#[test]
fn sysconf_resource_limits_and_priority() {
    check(&Case::named("posix/limits"));
}

#[test]
fn mappings_protection_and_memfd() {
    check(&Case::named("posix/memory"));
}

#[test]
fn record_locks_through_fcntl() {
    check(&Case::named("posix/fcntl_lock"));
}

#[test]
fn stat_on_a_directory_a_device_and_a_file() {
    check(&Case::named("posix/stat"));
}

#[test]
fn offsets_past_four_gibibytes() {
    check(&Case::named("posix/lseek_large"));
}

#[test]
fn syscall_passes_long_arguments() {
    check(&Case::named("posix/syscall"));
}

#[test]
fn glibc_large_file_and_xstat_names() {
    check(&Case::named("posix/glibc_names"));
}

#[test]
fn programs_run_through_popen_system_the_execl_family_and_daemon() {
    check(&Case::named("posix/spawn"));
}
