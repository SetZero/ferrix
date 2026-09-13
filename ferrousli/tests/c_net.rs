//! The C programs that exercise sockets and the address conversions.
//!
//! The socket programs are three, one for each part of a kernel's Unix
//! sockets, so that a kernel adding them in steps can run each whole: pairs,
//! then names, then passing descriptors.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn socket_pairs_carry_bytes_and_messages() {
    check(&Case::named("net/sockets_pair"));
}

#[test]
fn named_sockets_listen_accept_and_send_datagrams() {
    check(&Case::named("net/sockets_names"));
}

#[test]
fn descriptors_pass_over_a_socket_pair() {
    check(&Case::named("net/sockets_rights"));
}

#[test]
fn addresses_convert_between_text_and_bytes() {
    check(&Case::named("net/inet"));
}
