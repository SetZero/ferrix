//! The manager's clock: a point on a monotonic clock the backend reads.
//!
//! The core never reads a clock. Every [`step`](crate::Manager::step) is
//! told the time, so a test replays time as it replays events, and a
//! native root task can serve it from a timer object as well as a Linux
//! init serves it from `clock_gettime(CLOCK_MONOTONIC)`.

pub use ferrix_restart::Instant;
