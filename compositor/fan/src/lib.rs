//! Worker threads that are already there.
//!
//! A software renderer draws a frame in bands, a band a thread, and the bands
//! borrow the frame: each writes its own rows of one buffer, and none reads
//! another's. `std::thread::scope` says exactly that and is what
//! `compositor_render` used at first -- but it says it by *starting* the
//! threads and joining them, and a blurred frame is six passes and the
//! conversions either side of them. At 30 fps behind a wallpaper that moves,
//! that was on the order of a thousand threads started and ended a second.
//!
//! What a thread costs is not only the starting. Ferrix frees an exited
//! task's kernel stack by invalidating the address on every processor and
//! waiting for each to answer, so a program whose threads are short-lived
//! interrupts every *other* program on the machine, and the desktop stuttered
//! while a video played for that reason as much as for the drawing.
//!
//! So this crate says the same thing to threads that already exist.
//! [`fan_out`] gives each band to a worker and returns when the last has
//! finished; the workers are started once, on the first frame, and wait on a
//! condition variable between frames.
//!
//! # Why it is not in `compositor_render`
//!
//! Because that crate forbids `unsafe`, and this cannot be written without
//! one block of it: a job borrows the caller's pixels, the queue the workers
//! read is `'static`, and the bridge between those two facts is an erased
//! lifetime that a human argument has to justify -- the same argument, and
//! the same one block, that `std::thread::scope` itself is built on. Keeping
//! it here is what keeps the renderer's own `forbid(unsafe_code)` true, and
//! puts the whole of the argument in one small file rather than in the middle
//! of the blur.

#![forbid(missing_docs)]

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

/// Run the work `spread` fans out, and return once every piece of it has
/// finished.
///
/// The jobs borrow whatever `spread` borrows -- a band of the caller's pixels,
/// which is the point -- and this does not return until the last of them has
/// run, so those borrows are live throughout. An unwind out of `spread` waits
/// as well, and carries on unwinding afterwards: a job left running over a
/// stack that is being unwound would be reading pixels that are no longer
/// there.
pub fn fan_out<'env, F>(spread: F)
where
    F: for<'scope> FnOnce(&'scope Fan<'scope, 'env>),
{
    let fan = Fan {
        left: Arc::default(),
        scope: PhantomData,
        env: PhantomData,
    };
    let spread = catch_unwind(AssertUnwindSafe(|| spread(&fan)));
    fan.wait();
    if let Err(panicked) = spread {
        resume_unwind(panicked);
    }
}

/// How many workers [`fan_out`] has, counting the thread that calls it.
///
/// One more than are started, because the caller runs jobs itself while it
/// waits: a machine of one core starts no workers at all, and every job is
/// run by whoever asked for it.
#[must_use]
pub fn workers() -> usize {
    pool().workers + 1
}

/// The jobs one [`fan_out`] has given out, and the thread waiting for them.
///
/// The two lifetimes are `std::thread::Scope`'s, and for its reason:
/// `'scope` is how long a job may live and `'env` what it may borrow, and
/// both are invariant, so that neither can be widened by inference into the
/// `'static` the queue itself holds.
#[derive(Debug)]
pub struct Fan<'scope, 'env: 'scope> {
    /// How many of its jobs have still to finish.
    left: Arc<Left>,
    /// How long a job lives.
    scope: PhantomData<&'scope mut &'scope ()>,
    /// What it may borrow.
    env: PhantomData<&'env mut &'env ()>,
}

impl<'scope> Fan<'scope, '_> {
    /// Give `job` to a worker.
    pub fn spawn(&'scope self, job: impl FnOnce() + Send + 'scope) {
        // Counted before it is queued, never after: a worker that took it in
        // between would lower a count that had not been raised.
        *held(&self.left.count) += 1;
        let job: Box<dyn FnOnce() + Send + 'scope> = Box::new(job);
        // SAFETY: the lifetime is erased and nothing else about the box
        // changes; what is transmuted is one fat pointer to another of the
        // same shape. `'scope` promises that what the job borrows outlives
        // the job, and what keeps that promise is `fan_out`, which does not
        // return until `Fan::wait` has seen this count reach zero -- so the
        // job has run and been dropped before anything it borrowed can be.
        // The panic path keeps it too: `fan_out` catches the unwind, waits,
        // and only then carries on unwinding.
        let job: Box<dyn FnOnce() + Send + 'static> = unsafe { std::mem::transmute(job) };
        pool().submit(Job {
            call: job,
            left: Arc::clone(&self.left),
        });
    }

    /// Wait for every job, running whatever is queued meanwhile.
    ///
    /// The thread that asked for the work is a worker too. That is not only
    /// thrift: with a worker per core and this thread idle, a machine would
    /// have one more runnable thread than it has processors for, every frame
    /// it drew.
    fn wait(&self) {
        loop {
            while let Some(job) = pool().take() {
                job.run();
            }
            let count = held(&self.left.count);
            if *count == 0 {
                return;
            }
            // Nothing is queued and something is still out, so it is in a
            // worker's hands, and that worker lowers this count and wakes
            // this thread. It lowers it under the very lock held here, so
            // there is no wake-up to lose between the look and the wait.
            let waited = match self.left.done.wait(count) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            drop(waited);
        }
    }
}

/// How many of one fan's jobs have still to finish.
#[derive(Debug, Default)]
struct Left {
    /// The count itself.
    count: Mutex<usize>,
    /// Woken when it reaches zero.
    done: Condvar,
}

/// One band's work, waiting for a thread to run it.
struct Job {
    /// The closure. What it borrows lives on the stack of the thread that
    /// made it, which is why that thread waits for this to run.
    call: Box<dyn FnOnce() + Send + 'static>,
    /// The count to lower when it has.
    left: Arc<Left>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job").finish_non_exhaustive()
    }
}

impl Job {
    /// Run it, and tell whoever is waiting whatever it does.
    ///
    /// A job that panics is still counted, because the thread waiting for it
    /// holds a frame and would otherwise wait for ever -- and, worse, would
    /// hold a borrow of pixels nothing would ever give back. The panic is not
    /// swallowed quietly: the runtime prints it as it does for any thread,
    /// and `fan_out`'s own caller is unaffected only in that it is still
    /// running.
    fn run(self) {
        let Job { call, left } = self;
        let ran = catch_unwind(AssertUnwindSafe(call));
        drop(ran);
        let mut count = held(&left.count);
        *count = count.saturating_sub(1);
        if *count == 0 {
            left.done.notify_all();
        }
    }
}

/// The workers, and the jobs they have not taken yet.
#[derive(Debug)]
struct Pool {
    /// Jobs nobody has taken.
    queue: Mutex<VecDeque<Job>>,
    /// Woken when one arrives.
    ready: Condvar,
    /// How many workers were started.
    workers: usize,
}

impl Pool {
    /// Put a job where a worker will find it.
    fn submit(&self, job: Job) {
        held(&self.queue).push_back(job);
        self.ready.notify_one();
    }

    /// Take a job if one is waiting, without blocking for one.
    fn take(&self) -> Option<Job> {
        held(&self.queue).pop_front()
    }

    /// Wait for jobs and run them: a worker's whole life.
    fn work(&self) -> ! {
        loop {
            let mut queue = held(&self.queue);
            let job = loop {
                if let Some(job) = queue.pop_front() {
                    break job;
                }
                queue = match self.ready.wait(queue) {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
            };
            drop(queue);
            job.run();
        }
    }
}

/// How many workers to start: one fewer than the processors a band is worth a
/// thread for, because the thread that asks for the work runs jobs itself.
///
/// Every processor of a small machine and half of a large one's to at most
/// eight, which is `compositor_render::cores`' measured shape, kept here
/// because this is what has to size the pool before any frame is drawn.
fn wanted() -> usize {
    let processors = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let cores = if processors <= 4 {
        processors
    } else {
        (processors / 2).clamp(4, 8)
    };
    cores.saturating_sub(1)
}

/// The pool, and the workers, started the first time work is spread.
fn pool() -> &'static Pool {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let pool: &'static Pool = Box::leak(Box::new(Pool {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            workers: wanted(),
        }));
        for _ in 0..pool.workers {
            // A worker that will not start is one worker fewer, and no more
            // than that: every job is still run, by whoever can take it.
            let started = std::thread::Builder::new()
                .name(String::from("band"))
                .spawn(|| pool.work());
            drop(started);
        }
        pool
    })
}

/// Lock a mutex, poisoned or not.
///
/// A worker runs a caller's job with nothing of the pool's locked, so the
/// only way one of these is poisoned is a panic in the pool's own few locked
/// lines, which do not panic. Taking the value regardless is what keeps a
/// renderer that met one drawing, rather than stopped for the life of the
/// process.
fn held<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn every_job_runs_once_and_is_waited_for() {
        let mut rows = vec![0u32; 1000];
        fan_out(|fan| {
            for (at, band) in rows.chunks_mut(100).enumerate() {
                let mark = u32::try_from(at).unwrap_or(0) + 1;
                fan.spawn(move || band.iter_mut().for_each(|cell| *cell += mark));
            }
        });
        for (at, band) in rows.chunks(100).enumerate() {
            let owed = u32::try_from(at).unwrap_or(0) + 1;
            assert!(
                band.iter().all(|cell| *cell == owed),
                "band {at} was missed or run twice"
            );
        }
    }

    #[test]
    fn a_job_that_panics_still_lets_its_fan_return() {
        // The count has to come down whatever the job does, or the thread
        // holding the frame waits for ever. The panic's own message is
        // printed by the runtime; this asks only that the fan came back and
        // that its other jobs ran.
        let ran = AtomicUsize::new(0);
        fan_out(|fan| {
            fan.spawn(|| {
                let _ = ran.fetch_add(1, Ordering::SeqCst);
            });
            fan.spawn(|| {
                let empty: [u8; 0] = [];
                let _ = empty.first().copied().unwrap_or_default();
                panic!("a band that gave up");
            });
            fan.spawn(|| {
                let _ = ran.fetch_add(1, Ordering::SeqCst);
            });
        });
        assert_eq!(ran.load(Ordering::SeqCst), 2, "the other two bands ran");
    }

    #[test]
    fn a_fan_inside_a_job_finishes_too() {
        // A job may itself fan out. Whoever is waiting drains the queue, so
        // the inner fan is run by its own submitter even when every worker is
        // busy with the outer one.
        let outer = AtomicUsize::new(0);
        fan_out(|fan| {
            for _ in 0..8 {
                fan.spawn(|| {
                    count_on_a_fan(4);
                    let _ = outer.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
        assert_eq!(outer.load(Ordering::SeqCst), 8, "every outer job ran");
    }

    /// Fan `jobs` out, each counting one, and require that every one ran.
    fn count_on_a_fan(jobs: usize) {
        let ran = AtomicUsize::new(0);
        fan_out(|fan| {
            for _ in 0..jobs {
                fan.spawn(|| {
                    let _ = ran.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
        assert_eq!(ran.load(Ordering::SeqCst), jobs, "every job on the fan ran");
    }

    #[test]
    fn there_is_at_least_one_worker_counting_the_caller() {
        assert!(workers() >= 1, "somebody has to run the jobs");
    }
}
