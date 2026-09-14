//! The C programs that exercise the user, group and shadow databases, the
//! login name, the shells and password hashing.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn users_groups_shadow_entries_login_names_and_shells_are_read() {
    check(&Case::named("pwd/database"));
}

#[test]
fn crypt_hashes_des_settings_and_refuses_what_musl_refuses() {
    check(&Case::named("pwd/crypt"));
}
