//! The C programs that exercise mounting, file system statistics and the
//! `mntent.h` table reader.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn file_system_statistics_and_the_calls_that_mount_swap_and_sync() {
    check(&Case::named("mount/filesystems"));
}

#[test]
fn mount_tables_are_read_with_their_escapes_comments_and_defaults() {
    check(&Case::named("mount/mntent"));
}
