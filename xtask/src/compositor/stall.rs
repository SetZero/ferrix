//! What the host took from the guest while it drew: how long QEMU's virtual
//! processors sat runnable on this machine's run queues, waiting for a real
//! processor to run on.
//!
//! The compositor times its frames with the guest's clock, and under `tcg`
//! the guest's clock is the host's: a virtual processor the host did not run
//! for three seconds is a frame three seconds longer, however little the
//! compositor asked of it. On a host shared with a dozen other guests that is
//! most of a frame: at a load of 25 on nazuna, 2026-09-26, the virtual
//! processors of a guest drawing under `tcg` had waited on the run queue
//! about as long as they had run, and the slowest frame of `test-compositor`
//! came to 6.4 s at a load of 25 and 7.9 s at one of 50, against the 1.5 s it
//! takes on a quiet host.
//!
//! Linux counts that wait for every thread, in the second field of
//! `/proc/<tid>/schedstat` (`run_delay`, in nanoseconds). This reads it for
//! each virtual processor's thread every [`PERIOD`], and
//! [`wait_between`] says the most any one of them waited between two
//! moments. The moments are the frame's own: its report reached xtask at a
//! time the serial reader notes, and the frame ended before that and began
//! its length earlier, so only what the host took inside that stretch is
//! taken off it. A frame is judged by what is left.
//!
//! What it does not excuse is what the bound is for. A virtual processor that
//! is running is not waiting, so a compositor that spins or draws slowly has
//! nothing taken off; nor is a stopped one, so a guest that was frozen for
//! seconds -- the negative control SIGSTOPs QEMU mid-slide -- is judged on
//! every second of it. A host without `schedstat` (Windows, macOS, a Linux
//! built without `CONFIG_SCHED_INFO`) excuses nothing, which is the bound as
//! it was.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often the waits are read.
pub(super) const PERIOD: Duration = Duration::from_millis(100);

/// One reading: when it was taken, and each thread's run-queue wait so far,
/// in nanoseconds, in the order the threads were given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Reading {
    pub(super) at: Instant,
    pub(super) waits: Vec<u64>,
}

/// A thread reading the waits of QEMU's virtual processors until it is
/// stopped.
#[derive(Debug)]
pub(super) struct Sampler {
    /// QEMU's process, which the negative control stops and continues.
    pub(super) process: u32,
    stop: Arc<AtomicBool>,
    readings: Arc<Mutex<Vec<Reading>>>,
    thread: Option<JoinHandle<()>>,
}

impl Sampler {
    /// Start reading the waits of `threads`, the host thread ids QMP's
    /// `query-cpus-fast` gives for the virtual processors; `None` when the
    /// host cannot say, which is every host that is not Linux.
    pub(super) fn start(threads: Vec<u32>) -> Option<Self> {
        let first = *threads.first()?;
        let process = process_of(first)?;
        let _ = waits(&threads)?;
        let stop = Arc::new(AtomicBool::new(false));
        let readings = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let (stop, readings) = (Arc::clone(&stop), Arc::clone(&readings));
            std::thread::spawn(move || sample(&threads, &stop, &readings))
        };
        Some(Self {
            process,
            stop,
            readings,
            thread: Some(thread),
        })
    }

    /// The readings so far.
    pub(super) fn readings(&self) -> Vec<Reading> {
        self.readings
            .lock()
            .map(|kept| kept.clone())
            .unwrap_or_default()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Read `threads`' waits every [`PERIOD`] into `readings` until `stop`.
/// A thread that has gone -- QEMU exiting -- ends the readings; the ones
/// before it still stand.
fn sample(threads: &[u32], stop: &AtomicBool, readings: &Mutex<Vec<Reading>>) {
    while !stop.load(Ordering::Relaxed) {
        let Some(waits) = waits(threads) else {
            return;
        };
        if let Ok(mut kept) = readings.lock() {
            kept.push(Reading {
                at: Instant::now(),
                waits,
            });
        }
        std::thread::sleep(PERIOD);
    }
}

/// The virtual processors' host thread ids in a `query-cpus-fast` reply,
/// each once: under single-threaded `tcg` they share one.
///
/// Each processor's `props` has a `thread-id` of its own, which is its place
/// in the guest's topology -- usually 0 -- and not a host thread, so every
/// `props` object is cut out first. It holds numbers only, never an object.
pub(super) fn vcpu_threads(reply: &str) -> Vec<u32> {
    let mut outside = String::new();
    let mut rest = reply;
    while let Some(at) = rest.find("\"props\"") {
        outside.push_str(rest.get(..at).unwrap_or_default());
        rest = rest
            .get(at..)
            .and_then(|props| props.find('}').and_then(|end| props.get(end + 1..)))
            .unwrap_or_default();
    }
    outside.push_str(rest);
    let mut threads = Vec::new();
    for part in outside.split("\"thread-id\"").skip(1) {
        let digits: String = part
            .trim_start_matches([':', ' '])
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(thread) = digits.parse::<u32>()
            && !threads.contains(&thread)
        {
            threads.push(thread);
        }
    }
    threads
}

/// The most any one thread waited on the run queue between `from` and `to`.
///
/// From the last reading at or before `from` to the first at or after `to`,
/// so the stretch read covers the one asked about and a little more -- a
/// reading's gap at each end -- and each thread's wait only grows, so what it
/// says is never less than what was waited between them. A stretch that
/// starts before the first reading starts at it, and one that ends after the
/// last ends at it: what was not read is not excused.
pub(super) fn wait_between(readings: &[Reading], from: Instant, to: Instant) -> Duration {
    let first = readings
        .iter()
        .rev()
        .find(|reading| reading.at <= from)
        .or(readings.first());
    let last = readings
        .iter()
        .find(|reading| reading.at >= to)
        .or(readings.last());
    let (Some(first), Some(last)) = (first, last) else {
        return Duration::ZERO;
    };
    let worst = first
        .waits
        .iter()
        .zip(&last.waits)
        .map(|(was, is)| is.saturating_sub(*was))
        .max()
        .unwrap_or(0);
    Duration::from_nanos(worst)
}

/// Each thread's run-queue wait so far, or `None` if any cannot be read.
fn waits(threads: &[u32]) -> Option<Vec<u64>> {
    threads
        .iter()
        .map(|thread| {
            let text = std::fs::read_to_string(format!("/proc/{thread}/schedstat")).ok()?;
            text.split_whitespace().nth(1)?.parse().ok()
        })
        .collect()
}

/// The process a thread belongs to.
fn process_of(thread: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{thread}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Tgid:"))?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Readings a tenth of a second apart, each thread's wait given in
    /// milliseconds.
    fn readings(waits: &[&[u64]]) -> Vec<Reading> {
        let start = Instant::now();
        waits
            .iter()
            .enumerate()
            .map(|(index, row)| Reading {
                at: start + PERIOD * u32::try_from(index).unwrap_or(0),
                waits: row.iter().map(|millis| millis * 1_000_000).collect(),
            })
            .collect()
    }

    #[test]
    fn the_threads_come_out_of_the_reply_once_each() {
        let reply = r#"{"return": [{"thread-id": 4021, "qom-path": "/machine/unattached/device[0]", "cpu-index": 0}, {"thread-id": 4022, "cpu-index": 1}, {"thread-id":4022}]}"#;
        assert_eq!(vcpu_threads(reply), [4021, 4022]);
        assert!(vcpu_threads(r#"{"return": []}"#).is_empty());
    }

    /// The topology's `thread-id` inside `props` is not a host thread: QEMU
    /// 10.2 on x86-64 puts one in every processor's, after the host's.
    #[test]
    fn a_processors_props_are_not_its_thread() {
        let reply = r#"{"return": [{"thread-id": 25627, "props": {"core-id": 0, "thread-id": 0, "socket-id": 0}, "qom-path": "/machine/unattached/device[0]", "cpu-index": 0, "target": "x86_64"}, {"props": {"thread-id": 1, "core-id": 1}, "thread-id": 25628, "cpu-index": 1}]}"#;
        assert_eq!(vcpu_threads(reply), [25627, 25628]);
    }

    /// A wait inside the stretch is counted and one outside it is not, and
    /// two threads' waits are not added together: a frame drawn on one
    /// processor waited for that one.
    #[test]
    fn the_wait_is_one_threads_inside_the_stretch() {
        // Thread 0 waits 300 ms between readings 2 and 4, and thread 1
        // 200 ms between readings 7 and 9.
        let kept = readings(&[
            &[0, 0],
            &[0, 0],
            &[0, 0],
            &[150, 0],
            &[300, 0],
            &[300, 0],
            &[300, 0],
            &[300, 0],
            &[300, 100],
            &[300, 200],
        ]);
        let at = |index: u32| kept[0].at + PERIOD * index;
        assert_eq!(
            wait_between(&kept, at(2), at(4)),
            Duration::from_millis(300)
        );
        assert_eq!(
            wait_between(&kept, at(1), at(9)),
            Duration::from_millis(300)
        );
        // Only thread 1's, late in the run: thread 0's 300 ms were before.
        assert_eq!(
            wait_between(&kept, at(6), at(9)),
            Duration::from_millis(200)
        );
        // Nothing waited in between: nothing to excuse, however long.
        assert_eq!(wait_between(&kept, at(5), at(7)), Duration::ZERO);
        // Between readings, the stretch widens to the ones around it.
        let half = PERIOD / 2;
        assert_eq!(
            wait_between(&kept, at(2) + half, at(3) + half),
            Duration::from_millis(300)
        );
    }

    /// A stretch past either end of the readings stops at the end, and no
    /// readings say nothing.
    #[test]
    fn a_stretch_past_the_readings_stops_at_them() {
        let kept = readings(&[&[100], &[200], &[450]]);
        let far = Duration::from_secs(60);
        assert_eq!(
            wait_between(&kept, kept[0].at - far, kept[2].at + far),
            Duration::from_millis(350)
        );
        assert_eq!(
            wait_between(&kept[..1], kept[0].at, kept[0].at),
            Duration::ZERO
        );
        let now = Instant::now();
        assert_eq!(wait_between(&[], now, now), Duration::ZERO);
    }

    /// This host's own threads can be read, when it is Linux.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_running_threads_wait_can_be_read() {
        let me = std::process::id();
        if std::fs::read_to_string(format!("/proc/{me}/schedstat")).is_err() {
            // A kernel without CONFIG_SCHED_INFO: nothing to excuse, and
            // `Sampler::start` says so by returning `None`.
            return;
        }
        assert_eq!(process_of(me), Some(me));
        let sampler = Sampler::start(vec![me]).expect("this process's waits");
        std::thread::sleep(PERIOD * 3);
        assert!(sampler.readings().len() >= 2);
    }
}
