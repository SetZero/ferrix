//! A recording account for host tests.
//!
//! Tests run on many threads of one process, and the account is one per
//! process, so a job here is per thread: [`Job::enter`] makes one with a
//! limit and charges the calling thread's allocations to it until it is
//! dropped. Each job counts what is charged to it, what was refused, and
//! the holds on it, so a test can require that everything it made was
//! charged and that everything it dropped gave its charge back.

use core::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{Account, NOBODY, Refused};

/// One job's counts.
#[derive(Debug, Clone, Copy, Default)]
struct Counts {
    used: u64,
    limit: u64,
    refused: u64,
    holds: u64,
}

/// Every job there is, by id.
static JOBS: Mutex<BTreeMap<u32, Counts>> = Mutex::new(BTreeMap::new());

/// The next id to hand out.
static NEXT: AtomicU32 = AtomicU32::new(1);

std::thread_local! {
    /// The job this thread's charges go to.
    static CURRENT: Cell<u32> = const { Cell::new(NOBODY) };
}

/// The account: it looks the thread's job up.
#[derive(Debug)]
struct Recorder;

static RECORDER: Recorder = Recorder;

/// Run `with` over the job `owner`'s counts.
fn counts<R>(owner: u32, with: impl FnOnce(&mut Counts) -> R) -> Option<R> {
    let mut jobs = JOBS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    jobs.get_mut(&owner).map(with)
}

impl Account for Recorder {
    fn current(&self) -> u32 {
        CURRENT.with(Cell::get)
    }

    fn charge(&self, owner: u32, bytes: u64) -> Result<(), Refused> {
        counts(owner, |job| match job.used.checked_add(bytes) {
            Some(then) if then <= job.limit => {
                job.used = then;
                Ok(())
            }
            _ => {
                job.refused += 1;
                Err(Refused)
            }
        })
        .unwrap_or(Ok(()))
    }

    fn uncharge(&self, owner: u32, bytes: u64) {
        let _ = counts(owner, |job| job.used = job.used.saturating_sub(bytes));
    }

    fn hold(&self, owner: u32) {
        let _ = counts(owner, |job| job.holds += 1);
    }

    fn release(&self, owner: u32) {
        let _ = counts(owner, |job| job.holds = job.holds.saturating_sub(1));
    }
}

/// Install the recording account for this process. Every test that
/// charges calls it; the first call wins, and they are all the same.
pub fn install() {
    crate::install(&RECORDER);
}

/// A job the calling thread's charges go to while it lives.
#[derive(Debug)]
pub struct Job {
    id: u32,
    before: u32,
}

impl Job {
    /// Make a job with a limit of `limit` bytes, and charge this thread's
    /// allocations to it until it is dropped. Installs the account.
    #[must_use]
    pub fn enter(limit: u64) -> Job {
        install();
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut jobs = JOBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = jobs.insert(
            id,
            Counts {
                limit,
                ..Counts::default()
            },
        );
        drop(jobs);
        let before = CURRENT.with(|current| current.replace(id));
        Job { id, before }
    }

    /// Its id, which the charges made in it name.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Bytes charged to it now.
    #[must_use]
    pub fn used(&self) -> u64 {
        counts(self.id, |job| job.used).unwrap_or(0)
    }

    /// Charges its limit refused.
    #[must_use]
    pub fn refused(&self) -> u64 {
        counts(self.id, |job| job.refused).unwrap_or(0)
    }

    /// Charges that name it now.
    #[must_use]
    pub fn holds(&self) -> u64 {
        counts(self.id, |job| job.holds).unwrap_or(0)
    }

    /// Change its limit.
    pub fn set_limit(&self, limit: u64) {
        let _ = counts(self.id, |job| job.limit = limit);
    }

    /// Stop charging this thread to it, for the rest of `with`.
    pub fn outside<R>(&self, with: impl FnOnce() -> R) -> R {
        let inside = CURRENT.with(|current| current.replace(self.before));
        let result = with();
        CURRENT.with(|current| current.set(inside));
        result
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        CURRENT.with(|current| current.set(self.before));
    }
}
