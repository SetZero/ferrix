//! The C programs that exercise sockets and the address conversions.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn sockets_carry_bytes_descriptors_and_addresses() {
    check(&Case::named("net/sockets"));
}

#[test]
fn addresses_convert_between_text_and_bytes() {
    check(&Case::named("net/inet"));
}
