//! `ipc-bench`: how long a message takes to go to another process and back.
//!
//! The figure an IPC design is quoted by -- Zircon's "about a microsecond" is
//! this -- and the one `docs/OPAQUE-KERNEL.md`'s trip cannot give, because a
//! trip there includes a disk. Run from a shell, with no bootstrap handle, it
//! is the client: it starts a copy of itself as a native process in the root
//! cgroup's job, giving it one end of a channel, and then times
//! [`ROUNDS`] round trips of an eight-byte message, each a `channel_write`,
//! an `object_wait_one` for `READABLE` and a `channel_read`. Started with a
//! bootstrap channel it is the server, and echoes every message back until
//! the client closes its end.
//!
//! Then the same trip again with `channel_write_read`, one call a side and
//! the message in registers (`call`).
//!
//! Before the round trips it times [`ROUNDS`] `object_wait_one`s on a signal
//! already asserted, so the floor -- a native call that does not sleep -- is
//! printed beside the trip. Times are the processor's counter (`ferrix_rt::counter`),
//! converted to nanoseconds by the monotonic clock read around the run.
//! Each line is `ipc-bench <what> n=<count> min=<ns> p50=<ns> p90=<ns>
//! p99=<ns> mean=<ns>`, the percentiles to an eighth of a power of two.
//! Exit 0 is a run that finished; any other status names the step that did
//! not.

#![no_std]
#![no_main]

use core::fmt::Write as _;

use ferrix_native_abi::rights::{Requested, Rights};
use ferrix_rt::linux::{self, numbers};
use ferrix_rt::native::channel::{self, Channel, ReadError};
use ferrix_rt::native::job::for_cgroup;
use ferrix_rt::native::pending::create_process;
use ferrix_rt::native::{Deadline, Error, Handle, Object, Signals, vmo};
use ferrix_rt::{Bootstrap, Kernel};

ferrix_rt::entry!(main);

/// Round trips timed, after [`WARMUP`] untimed ones.
const ROUNDS: u32 = 20_000;
/// Round trips run first and not timed: caches, first faults, the server's
/// first sleep.
const WARMUP: u32 = 1_000;
/// Where this program is, to start a copy of it.
const SELF: &[u8] = b"/sbin/ipc-bench\0";
/// The cgroup whose job the server runs in, which `bench-ipc`'s script makes.
const CGROUP: &[u8] = b"/sys/fs/cgroup/ipc-bench\0";
/// `AT_FDCWD`.
const AT_FDCWD: usize = -100_isize as usize;
/// `O_RDONLY | O_CLOEXEC`.
const O_READ: usize = 0o2_000_000;
/// `O_DIRECTORY | O_RDONLY | O_CLOEXEC`.
const O_DIR: usize = 0o2_000_000 | 0o200_000;
/// `SEEK_END`.
const SEEK_END: usize = 2;

/// Client or server, by whether there is a bootstrap handle.
fn main(bootstrap: Bootstrap) -> i32 {
    match bootstrap {
        Some(channel) => serve(&channel),
        None => match client() {
            Ok(()) => 0,
            Err(step) => {
                say(format_args!("ipc-bench failed at step {step}"));
                step
            }
        },
    }
}

/// Echo every message until the client lets go.
fn serve(channel: &Channel<Kernel>) -> i32 {
    let mut bytes = [0_u8; 64];
    let mut handles = [Handle::INVALID; 1];
    loop {
        match channel.read(&mut bytes, &mut handles) {
            Ok(received) => {
                let len = received.bytes.min(bytes.len());
                let message = bytes.get(..len).unwrap_or_default();
                if message == FAST {
                    return serve_fast(channel);
                }
                if channel.write(message).is_err() {
                    return 31;
                }
            }
            Err(ReadError::Failed(Error::ShouldWait)) => {
                if channel
                    .wait_one(Signals::READABLE | Signals::PEER_CLOSED, Deadline::Never)
                    .is_err()
                {
                    return 32;
                }
            }
            Err(ReadError::Failed(Error::PeerClosed)) => return 0,
            Err(_) => return 33,
        }
    }
}

/// What the client sends to move the server to `channel_write_read`.
const FAST: &[u8] = b"FAST";

/// Echo with `channel_write_read`: one call per request, the answer to the
/// last going out as the next comes in.
fn serve_fast(channel: &Channel<Kernel>) -> i32 {
    let mut received = match channel.write_read(None) {
        Ok(words) => words,
        Err(Error::PeerClosed) => return 0,
        Err(_) => return 34,
    };
    loop {
        let bytes = received.bytes();
        let answer = bytes.get(..received.len).unwrap_or_default();
        received = match channel.write_read(Some(answer)) {
            Ok(words) => words,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 35,
        };
    }
}

/// Start the server, time the floor and the trip, print both.
fn client() -> Result<(), i32> {
    let mine = start_server()?;

    let mut floor = Histogram::new();
    let clock = Clock::start();
    for _ in 0..ROUNDS {
        let before = ferrix_rt::counter().unwrap_or(0);
        let _ = mine
            .wait_one(Signals::WRITABLE, Deadline::Never)
            .map_err(|_| 10)?;
        floor.add(ferrix_rt::counter().unwrap_or(0).wrapping_sub(before));
    }
    let scale = clock.stop();
    floor.print("floor", scale);

    let message = 0x5EED_u64.to_ne_bytes();
    let mut back = [0_u8; 64];
    let mut handles = [Handle::INVALID; 1];
    let mut trip = Histogram::new();
    for _ in 0..WARMUP {
        round_trip(&mine, &message, &mut back, &mut handles)?;
    }
    let clock = Clock::start();
    for _ in 0..ROUNDS {
        let before = ferrix_rt::counter().unwrap_or(0);
        round_trip(&mine, &message, &mut back, &mut handles)?;
        trip.add(ferrix_rt::counter().unwrap_or(0).wrapping_sub(before));
    }
    let scale = clock.stop();
    trip.print("trip", scale);

    // The same trip as one `channel_write_read` a side.
    mine.write(FAST).map_err(|_| 23)?;
    let mut call = Histogram::new();
    for _ in 0..WARMUP {
        call_trip(&mine, &message)?;
    }
    let clock = Clock::start();
    for _ in 0..ROUNDS {
        let before = ferrix_rt::counter().unwrap_or(0);
        call_trip(&mine, &message)?;
        call.add(ferrix_rt::counter().unwrap_or(0).wrapping_sub(before));
    }
    let scale = clock.stop();
    call.print("call", scale);
    Ok(())
}

/// One message there and back by `channel_write_read`, checked.
fn call_trip(channel: &Channel<Kernel>, message: &[u8]) -> Result<(), i32> {
    let back = channel.write_read(Some(message)).map_err(|_| 24)?;
    if back.bytes().get(..back.len) != Some(message) {
        return Err(25);
    }
    Ok(())
}

/// One message there and back.
fn round_trip(
    channel: &Channel<Kernel>,
    message: &[u8],
    back: &mut [u8],
    handles: &mut [Handle],
) -> Result<(), i32> {
    channel.write(message).map_err(|_| 20)?;
    loop {
        match channel.read(back, handles) {
            Ok(_) => return Ok(()),
            Err(ReadError::Failed(Error::ShouldWait)) => {
                let _ = channel
                    .wait_one(Signals::READABLE | Signals::PEER_CLOSED, Deadline::Never)
                    .map_err(|_| 21)?;
            }
            Err(_) => return Err(22),
        }
    }
}

/// Load this program into a VMO, start it in the root cgroup's job with one
/// end of a new channel, and keep the other.
fn start_server() -> Result<Channel<Kernel>, i32> {
    // SAFETY: `SELF` is NUL-terminated and borrowed for the call.
    let fd = unsafe {
        linux::call(
            numbers::OPENAT,
            [AT_FDCWD, SELF.as_ptr().addr(), O_READ, 0, 0, 0],
        )
    }
    .map_err(|_| 1)?;
    // SAFETY: no pointer arguments.
    let size = unsafe { linux::call(numbers::LSEEK, [fd, 0, SEEK_END, 0, 0, 0]) }.map_err(|_| 2)?;
    let image = vmo::create(Kernel, size).map_err(|_| 3)?;
    // Back to the start for the reads, which then go through it in order.
    // SAFETY: no pointer arguments.
    let _ = unsafe { linux::call(numbers::LSEEK, [fd, 0, 0, 0, 0, 0]) }.map_err(|_| 2)?;
    let mut chunk = [0_u8; 4096];
    let mut at = 0_usize;
    while at < size {
        let got = linux::read(fd, &mut chunk).map_err(|_| 4)?;
        if got == 0 {
            return Err(4);
        }
        image
            .write(chunk.get(..got).unwrap_or_default(), at as u64)
            .map_err(|_| 5)?;
        at = at.saturating_add(got);
    }
    let _ = linux::close(fd);

    // Mounted and made here, what is there already being as good: the shell
    // running this may have no `mount` or `mkdir`.
    for (source, target, kind) in [
        (&b"sys\0"[..], &b"/sys\0"[..], &b"sysfs\0"[..]),
        (b"cgroup2\0", b"/sys/fs/cgroup\0", b"cgroup2\0"),
    ] {
        // SAFETY: the three strings are NUL-terminated and borrowed for the
        // call; no data argument.
        let _ = unsafe {
            linux::call(
                numbers::MOUNT,
                [
                    source.as_ptr().addr(),
                    target.as_ptr().addr(),
                    kind.as_ptr().addr(),
                    0,
                    0,
                    0,
                ],
            )
        };
    }
    // SAFETY: `CGROUP` is NUL-terminated and borrowed for the call.
    let _ = unsafe {
        linux::call(
            numbers::MKDIRAT,
            [AT_FDCWD, CGROUP.as_ptr().addr(), 0o755, 0, 0, 0],
        )
    };
    // SAFETY: `CGROUP` is NUL-terminated and borrowed for the call.
    let dir = unsafe {
        linux::call(
            numbers::OPENAT,
            [AT_FDCWD, CGROUP.as_ptr().addr(), O_DIR, 0, 0, 0],
        )
    }
    .map_err(|errno| {
        say(format_args!("ipc-bench: opening the cgroup: errno {}", errno.0));
        6
    })?;
    let job = for_cgroup(
        Kernel,
        i32::try_from(dir).unwrap_or(-1),
        Requested::Exactly(Rights::MANAGE),
    )
    .map_err(|error| {
        say(format_args!("ipc-bench: job_for_cgroup: {error:?}"));
        7
    })?;
    let _ = linux::close(dir);

    let (mine, theirs) = channel::create(Kernel).map_err(|_| 8)?;
    let server = create_process(&job, &image, "ipc-echo").map_err(|_| 9)?;
    server.start(theirs.into_owned()).map_err(|_| 9)?;
    // The process handle goes; the server lives until the channel does.
    drop(server);
    Ok(mine)
}

/// The counter against the monotonic clock, over one run.
struct Clock {
    /// The clock at the start, in nanoseconds.
    nanos: u64,
    /// The counter at the start.
    ticks: u64,
}

impl Clock {
    /// Read both now.
    fn start() -> Clock {
        Clock {
            nanos: linux::monotonic_nanos().unwrap_or(0),
            ticks: ferrix_rt::counter().unwrap_or(0),
        }
    }

    /// Nanoseconds per thousand ticks since [`Clock::start`].
    fn stop(&self) -> u64 {
        let nanos = linux::monotonic_nanos()
            .unwrap_or(0)
            .saturating_sub(self.nanos);
        let ticks = ferrix_rt::counter()
            .unwrap_or(0)
            .saturating_sub(self.ticks)
            .max(1);
        nanos.saturating_mul(1000) / ticks
    }
}

/// Sub-buckets per power of two.
const STEPS: u32 = 8;
/// Buckets: every power of two a `u64` has, [`STEPS`] apiece.
const BUCKETS: usize = 64 * STEPS as usize;

/// Durations in ticks, kept to an eighth of a power of two.
struct Histogram {
    /// How many fell in each bucket.
    counts: [u32; BUCKETS],
    /// How many in all.
    total: u32,
    /// The shortest, exactly.
    least: u64,
    /// Their sum, for the mean.
    sum: u64,
}

impl Histogram {
    /// Nothing counted.
    fn new() -> Histogram {
        Histogram {
            counts: [0; BUCKETS],
            total: 0,
            least: u64::MAX,
            sum: 0,
        }
    }

    /// The bucket `ticks` falls in.
    fn bucket(ticks: u64) -> usize {
        if ticks < u64::from(STEPS) {
            return ticks as usize;
        }
        let power = 63 - ticks.leading_zeros();
        let step = (ticks >> (power - STEPS.trailing_zeros())) & u64::from(STEPS - 1);
        (power * STEPS) as usize + step as usize
    }

    /// The least duration bucket `index` holds.
    fn floor(index: usize) -> u64 {
        let steps = STEPS as usize;
        if index < steps {
            return index as u64;
        }
        let power = (index / steps) as u32;
        let step = (index % steps) as u64;
        (1_u64 << power) | (step << (power - STEPS.trailing_zeros()))
    }

    /// Count one.
    fn add(&mut self, ticks: u64) {
        if let Some(count) = self.counts.get_mut(Histogram::bucket(ticks)) {
            *count = count.saturating_add(1);
        }
        self.total = self.total.saturating_add(1);
        self.least = self.least.min(ticks);
        self.sum = self.sum.saturating_add(ticks);
    }

    /// The duration below which `per_mille` of them fell.
    fn at(&self, per_mille: u32) -> u64 {
        let wanted = u64::from(self.total) * u64::from(per_mille) / 1000;
        let mut seen = 0_u64;
        for (index, count) in self.counts.iter().enumerate() {
            seen += u64::from(*count);
            if seen > wanted {
                return Histogram::floor(index);
            }
        }
        0
    }

    /// One line, in nanoseconds, `scale` being nanoseconds per thousand ticks.
    fn print(&self, what: &str, scale: u64) {
        let ns = |ticks: u64| ticks.saturating_mul(scale) / 1000;
        let mean = self.sum / u64::from(self.total.max(1));
        say(format_args!(
            "ipc-bench {what} n={} min={} p50={} p90={} p99={} mean={}",
            self.total,
            ns(self.least),
            ns(self.at(500)),
            ns(self.at(900)),
            ns(self.at(990)),
            ns(mean),
        ));
    }
}

/// A line on standard output.
fn say(line: core::fmt::Arguments<'_>) {
    let mut out = Line {
        bytes: [0; 160],
        len: 0,
    };
    let _ = out.write_fmt(line);
    let _ = out.write_str("\n");
    let _ = linux::write(1, out.bytes.get(..out.len).unwrap_or_default());
}

/// A line being formatted, cut short rather than overflowing.
struct Line {
    /// Its bytes.
    bytes: [u8; 160],
    /// How many are used.
    len: usize,
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let end = self.len.saturating_add(text.len()).min(self.bytes.len());
        let room = end - self.len;
        if let (Some(into), Some(from)) = (
            self.bytes.get_mut(self.len..end),
            text.as_bytes().get(..room),
        ) {
            into.copy_from_slice(from);
        }
        self.len = end;
        Ok(())
    }
}
