//! The C program that exercises `termios.h` and the terminal calls in
//! `unistd.h`, on a pseudo-terminal it opens.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn a_pseudo_terminal_is_configured_sized_and_named() {
    check(&Case::named("termios/terminal"));
}
