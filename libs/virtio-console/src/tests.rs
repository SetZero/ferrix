//! The driver against a virtio-console device on the host.
//!
//! Every test here drives a real [`SplitQueue`](ferrix_virtio::SplitQueue) over
//! memory a [`fake::Device`] reads and writes through the device half of the
//! same rings, so the queue arithmetic of `docs/CLIPBOARD.md` §3.2 -- the part
//! with no symptom other than silence -- is exercised rather than asserted.

mod conversation;
mod data;
mod fake;
mod hostile;
