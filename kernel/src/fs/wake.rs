//! Sleeping on the files a `poll` or an `epoll_wait` watches.
//!
//! Each pollable object names the wait queues it wakes when its readiness may
//! have changed, through [`ferrix_vfs::Inode::poll_queues`]. A wait gathers
//! them into [`Sources`] and sleeps on all of them at once, so a pipe written,
//! a datagram arriving or an eventfd bumped ends the wait at that moment, as
//! it does on Linux, rather than at the next look of a wait that asks again
//! every few milliseconds.
//!
//! A wait still looks again of its own accord, as every wait queue's does, in
//! case a wake is missing. How often depends on whether it can trust its
//! queues: when every watched object says every change wakes one of them, a
//! second; otherwise the few milliseconds every other wait uses. A regular
//! file names no queue and is always ready, so it never makes a wait sleep.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_vfs::{OpenFile, WakeSource};

use crate::sched::WaitQueue;

/// How long a wait whose every queue is trusted sleeps between its own looks.
pub(crate) const TRUSTED_RECHECK_NANOS: u64 = 1_000_000_000;

/// How long any other wait sleeps between them: a wait queue's own recheck.
pub(crate) const UNTRUSTED_RECHECK_NANOS: u64 = 5_000_000;

/// Every look a wait gathered here has taken at its condition, for the check
/// that a wait on quiet files sleeps rather than looking every few
/// milliseconds.
static LOOKS: AtomicU64 = AtomicU64::new(0);

/// How many looks waits have taken, since boot.
pub(crate) fn looks() -> u64 {
    LOOKS.load(Ordering::Relaxed)
}

/// A queue a wait holds on to for as long as it sleeps on it.
enum Held {
    /// One that lives as long as the kernel.
    Static(&'static WaitQueue),
    /// One inside an object that can go.
    Shared(Arc<WaitQueue>),
}

impl Held {
    /// The queue.
    fn queue(&self) -> &WaitQueue {
        match self {
            Held::Static(queue) => queue,
            Held::Shared(queue) => queue,
        }
    }
}

/// The queues of the files one wait watches.
pub(crate) struct Sources {
    held: Vec<Held>,
    /// Whether every file said every change wakes one of its queues.
    trusted: bool,
}

impl Sources {
    /// No queue yet, and nothing untrusted.
    pub(crate) fn new() -> Sources {
        Sources {
            held: Vec::new(),
            trusted: true,
        }
    }

    /// Add a file's queues.
    pub(crate) fn add(&mut self, file: &OpenFile) {
        let mut offered = Vec::new();
        let trusted = file.poll_queues(&mut |source| offered.push(source));
        self.trusted &= trusted;
        for source in offered {
            self.add_source(source);
        }
    }

    /// Add one queue an object named, once however often it is named.
    pub(crate) fn add_source(&mut self, source: WakeSource) {
        let held = match source {
            WakeSource::Static(any) => match any.downcast_ref::<WaitQueue>() {
                Some(queue) => Held::Static(queue),
                None => {
                    self.trusted = false;
                    return;
                }
            },
            WakeSource::Shared(any) => match any.downcast::<WaitQueue>() {
                Ok(queue) => Held::Shared(queue),
                Err(_) => {
                    self.trusted = false;
                    return;
                }
            },
        };
        let already = self
            .held
            .iter()
            .any(|kept| core::ptr::eq(kept.queue(), held.queue()));
        if !already {
            self.held.push(held);
        }
    }

    /// How long this wait may sleep between its own looks.
    pub(crate) fn recheck(&self) -> u64 {
        if self.trusted && !self.held.is_empty() {
            TRUSTED_RECHECK_NANOS
        } else {
            UNTRUSTED_RECHECK_NANOS
        }
    }

    /// Sleep until `ready`, a wake of any of the queues, or `deadline`.
    pub(crate) fn wait(&self, mut ready: impl FnMut() -> bool, deadline: u64) -> bool {
        let queues: Vec<&WaitQueue> = self.held.iter().map(Held::queue).collect();
        let counted = || {
            let _ = LOOKS.fetch_add(1, Ordering::Relaxed);
            ready()
        };
        WaitQueue::wait_on_any(&queues, counted, deadline, self.recheck())
    }
}

/// A queue inside an object, as a source a wait can hold.
pub(crate) fn shared(queue: &Arc<WaitQueue>) -> WakeSource {
    WakeSource::Shared(Arc::clone(queue) as Arc<dyn core::any::Any + Send + Sync>)
}

/// A queue that lives as long as the kernel, as a source.
pub(crate) fn lent(queue: &'static WaitQueue) -> WakeSource {
    WakeSource::Static(queue)
}
