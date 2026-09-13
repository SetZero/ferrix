//! The C program that exercises the user, group and shadow databases, the
//! login name and the shells.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn users_groups_shadow_entries_login_names_and_shells_are_read() {
    check(&Case::named("pwd/database"));
}
