//! Restarting (§5.4): whether an end restarts a service, after how long,
//! and when to give up.
//!
//! The policy is `ferrix-restart`'s, a crate of its own that allocates
//! nothing, because `devmgr` has the same problem with no clock and no
//! allocator (`docs/DEVMGR.md` §4) and shares this code. Its names are
//! re-exported here, where the manager has always found them.

pub use ferrix_restart::{Backoff, Budget, Decision, Ended, Policy, wanted};
