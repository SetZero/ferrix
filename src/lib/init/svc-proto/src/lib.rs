//! The init's wire formats (`docs/INIT.md` §5.3, §10).
//!
//! Two, both between `/sbin/init` and programs it does not trust:
//!
//! * [`control`] -- what `svc` and init say on `/run/ferrix/control`: one
//!   [`Call`](control::Call) from the client, then one or more
//!   [`Answer`](control::Answer)s from init, the last of them final. Each is
//!   a length-prefixed record, not text, so `svc`'s output can change
//!   without breaking another client.
//! * [`notify`] -- the lines a `Type=notify` service writes to the
//!   descriptor `NotifyFd=` names: `sd_notify`'s words (`READY=1`,
//!   `STATUS=…`) over s6's readiness descriptor.
//!
//! Pure functions over bytes, `no_std` with `alloc`, tested on the host and
//! fuzzed: init decodes what any local program sends it.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod control;
pub mod notify;

#[cfg(test)]
mod tests;
