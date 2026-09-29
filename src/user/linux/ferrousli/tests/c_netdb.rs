//! The C programs that exercise name resolution and the interface list.
//!
//! None of them needs a network. Every lookup is of a numeric address, a
//! numeric service, or `localhost`, and where a program reads a file of the
//! machine's — `/etc/services`, `/etc/protocols`, `/etc/hosts` — it checks
//! that the answers agree with one another rather than what they are.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

#[test]
fn addresses_resolve_and_are_named_again() {
    check(&Case::named("netdb/addrinfo"));
}

#[test]
fn the_legacy_lookups_and_databases_answer() {
    check(&Case::named("netdb/databases"));
}

#[test]
fn the_interfaces_are_listed_and_ethernet_addresses_convert() {
    check(&Case::named("netdb/interfaces"));
}
