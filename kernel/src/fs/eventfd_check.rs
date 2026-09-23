//! The self-check of eventfds.
//!
//! A counter made by number with an initial value reads that value back and
//! is then empty: `EAGAIN` when non-blocking. Writes add up, and one read takes
//! the sum; with `EFD_SEMAPHORE` each read takes one. The counter stops one
//! short of `u64::MAX`: the write that would reach it is `EAGAIN`, and `poll`
//! stops answering writable exactly there. A blocking read waits, and a write
//! from elsewhere ends that wait by waking it, not by the wait's own recheck.
//! Registered edge-triggered in an epoll set, the eventfd is reported again
//! after a second write although it never stopped being readable: that is the
//! wake count at work, the way an event loop is woken by another thread.
//!
//! Refused as Linux refuses: a flag `eventfd2` does not take, a read or write
//! buffer shorter than eight bytes, a write of `u64::MAX`, and `lseek`. The run
//! is done twice and must keep no frame.

use alloc::sync::Arc;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::{
    EFD_CLOEXEC, EFD_NONBLOCK, EFD_SEMAPHORE, EPOLL_CTL_ADD, EPOLLET, EPOLLIN, F_GETFD, F_GETFL,
    FD_CLOEXEC, MAP_ANONYMOUS, MAP_PRIVATE, O_NONBLOCK, PROT_READ, PROT_WRITE, SEEK_CUR,
};
use ferrix_vfs::{Errno, OpenFile};

use crate::fs;
use crate::fs::eventfd::{self, VALUE_BYTES};
use crate::mm;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::syscall::check as syscall_check;
use crate::syscall::memory::{self, MmapRequest, OffsetUnit};
use crate::syscall::process::{self, Process};
use crate::syscall::{epoll, fd, file, poll, uaccess};

/// Where a value to write is staged, and where reads land.
const AT_VALUE: u64 = 0;
/// Where an epoll event is staged.
const AT_EVENT: u64 = 16;
/// Where epoll waits write.
const AT_EVENTS: u64 = 64;

/// How long the check gives a waiting reader to start waiting, and to come
/// back once woken.
const PATIENCE_NANOS: u64 = 2_000_000_000;
/// How long a reader must still be waiting before the write, in nanoseconds.
const STILL_WAITING_NANOS: u64 = 20_000_000;
/// How often the check looks for a waiter on the queue, in nanoseconds.
const LISTED_LOOK_NANOS: u64 = 1_000_000;

/// How long the quiet poll waits.
const QUIET_POLL_MILLIS: i32 = 300;
/// The most looks a quiet poll may take in that time: a wait trusting its
/// queues takes two or three, one looking every 5 ms about a hundred and
/// twenty.
const QUIET_POLL_LOOKS: u64 = 12;
/// The waiting `poll` or `epoll_wait`'s own timeout, far past the patience.
const WAITER_TIMEOUT_MILLIS: i32 = 10_000;

/// What the waiting `poll` or `epoll_wait` watches: the process, its page, the
/// descriptor, and which call.
static WAITER_SUBJECT: SpinLock<Option<Subject>> = SpinLock::new(None);

/// What a waiting task is to wait on.
type Subject = (Arc<Process>, u64, i32, HowWaits);
/// What it answered.
static WAITER_ANSWER: SpinLock<Option<Result<usize, Errno>>> = SpinLock::new(None);

/// The eventfd the waiting reader reads.
static READER_FILE: SpinLock<Option<Arc<OpenFile>>> = SpinLock::new(None);
/// What its read answered.
static READER_ANSWER: SpinLock<Option<Result<u64, Errno>>> = SpinLock::new(None);
/// Woken when it has answered.
static READER_DONE: WaitQueue = WaitQueue::new();

/// What the check measured, for the boot log.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Report {
    /// Values read back.
    pub(crate) reads: u32,
    /// Calls refused as Linux refuses them.
    pub(crate) refusals: u32,
    /// Frames the second run cost.
    pub(crate) leaked: i64,
}

/// What one run counts.
#[derive(Debug, Default)]
struct Counts {
    reads: u32,
    refusals: u32,
}

/// Run it twice, measured on the second.
pub(crate) fn run() -> Result<Report, &'static str> {
    let process =
        process::new_for_check().map_err(|_| "could not make a process for the eventfd check")?;
    let _warm = check_once(&process)?;
    crate::sched::wait_until_reaper_quiet(crate::sched::REAPER_PATIENCE_NANOS)?;
    let window = mm::FrameWindow::open();
    let counts = check_once(&process)?;
    crate::sched::wait_until_reaper_quiet(crate::sched::REAPER_PATIENCE_NANOS)?;
    let leaked = window.kept();
    if leaked != 0 {
        window.report("eventfd");
        crate::console::println!("  eventfd  {leaked} frames across the second run");
        return Err("the eventfd check did not give back every frame it took");
    }
    Ok(Report {
        reads: counts.reads,
        refusals: counts.refusals,
        leaked,
    })
}

/// One run.
fn check_once(process: &Arc<Process>) -> Result<Counts, &'static str> {
    let page = memory::sys_mmap(
        process,
        &MmapRequest {
            addr: 0,
            len: PAGE_SIZE,
            prot: PROT_READ | PROT_WRITE,
            flags: MAP_ANONYMOUS | MAP_PRIVATE,
            fd: -1,
            offset: 0,
            unit: OffsetUnit::Bytes,
        },
    )
    .map_err(|_| "a page for the eventfd check was refused")?;
    let page = u64::try_from(page).map_err(|_| "mmap returned an impossible address")?;
    let mut counts = Counts::default();
    let outcome = check_counting(process, page, &mut counts)
        .and_then(|()| check_the_ceiling(process, page, &mut counts))
        .and_then(|()| check_a_blocked_read_is_woken(&mut counts))
        .and_then(|()| check_edges_in_epoll(process, page, &mut counts))
        .and_then(|()| check_a_quiet_poll_sleeps(process, page))
        .and_then(|()| check_waits_are_woken(process, page, &mut counts));
    for fd in 3..32 {
        let _ = fd::sys_close(process, fd);
    }
    let _ = READER_FILE.lock().take();
    let _ = memory::sys_munmap(process, page, PAGE_SIZE);
    outcome.map(|()| counts)
}

/// The initial value, adding up, the semaphore, the flags and the refusals.
fn check_counting(process: &Process, page: u64, counts: &mut Counts) -> Result<(), &'static str> {
    let counter = create(process, 3, EFD_NONBLOCK | EFD_CLOEXEC)?;
    if fd::sys_fcntl(process, counter, F_GETFD, 0) != Ok(FD_CLOEXEC as usize) {
        return Err("eventfd2's EFD_CLOEXEC did not reach the descriptor");
    }
    if fd::sys_fcntl(process, counter, F_GETFL, 0).map(|flags| flags as u32 & O_NONBLOCK)
        != Ok(O_NONBLOCK)
    {
        return Err("eventfd2's EFD_NONBLOCK did not reach the open file");
    }
    expect_read(
        process,
        page,
        counter,
        3,
        "an eventfd did not read its initial value",
    )?;
    refused(
        file::sys_read(process, counter, page + AT_VALUE, 8),
        Errno::EAGAIN,
        "an empty non-blocking eventfd did not answer EAGAIN",
        counts,
    )?;
    write_value(process, page, counter, 5)?;
    write_value(process, page, counter, 7)?;
    expect_read(
        process,
        page,
        counter,
        12,
        "two writes did not add up to one read",
    )?;
    counts.reads += 2;

    refused(
        file::sys_read(process, counter, page + AT_VALUE, 4),
        Errno::EINVAL,
        "a read into four bytes was not EINVAL",
        counts,
    )?;
    stage(process, page, 1)?;
    refused(
        file::sys_write(process, counter, page + AT_VALUE, 4),
        Errno::EINVAL,
        "a write of four bytes was not EINVAL",
        counts,
    )?;
    stage(process, page, u64::MAX)?;
    refused(
        file::sys_write(process, counter, page + AT_VALUE, 8),
        Errno::EINVAL,
        "a write of u64::MAX was not EINVAL",
        counts,
    )?;
    refused(
        fd::sys_lseek(process, counter, 0, SEEK_CUR),
        Errno::ESPIPE,
        "an eventfd could be sought",
        counts,
    )?;
    refused(
        by_number(process, Syscall::Eventfd2, [0, 2, 0, 0, 0, 0]),
        Errno::EINVAL,
        "eventfd2 accepted a flag it does not take",
        counts,
    )?;

    let semaphore = create(process, 2, EFD_SEMAPHORE | EFD_NONBLOCK)?;
    expect_read(
        process,
        page,
        semaphore,
        1,
        "a semaphore eventfd did not read one",
    )?;
    expect_read(
        process,
        page,
        semaphore,
        1,
        "a semaphore eventfd did not read one again",
    )?;
    refused(
        file::sys_read(process, semaphore, page + AT_VALUE, 8),
        Errno::EAGAIN,
        "a semaphore eventfd read more than its count",
        counts,
    )?;
    counts.reads += 2;
    closed(process, counter)?;
    closed(process, semaphore)
}

/// The counter stops one short of `u64::MAX`, and `poll` says so.
fn check_the_ceiling(
    process: &Process,
    page: u64,
    counts: &mut Counts,
) -> Result<(), &'static str> {
    let counter = create(process, 0, EFD_NONBLOCK)?;
    let opened = fd::file(process, counter).map_err(|_| "the eventfd is gone")?;
    let ready = opened.poll();
    if ready.readable || !ready.writable {
        return Err("an empty eventfd did not poll writable and not readable");
    }
    write_value(process, page, counter, u64::MAX - 2)?;
    write_value(process, page, counter, 1)?;
    stage(process, page, 1)?;
    refused(
        file::sys_write(process, counter, page + AT_VALUE, 8),
        Errno::EAGAIN,
        "a write that would carry an eventfd to u64::MAX was not EAGAIN",
        counts,
    )?;
    let full = opened.poll();
    if !full.readable || full.writable {
        return Err("an eventfd one short of u64::MAX did not poll readable and not writable");
    }
    let held = eventfd::of(&opened).map(|counter| counter.count());
    if held != Some(u64::MAX - 1) {
        return Err("an eventfd did not hold u64::MAX - 1 after filling");
    }
    expect_read(
        process,
        page,
        counter,
        u64::MAX - 1,
        "a full eventfd did not read back",
    )?;
    counts.reads += 1;
    drop(opened);
    closed(process, counter)
}

/// A read with nothing to take waits, and a write wakes it.
fn check_a_blocked_read_is_woken(counts: &mut Counts) -> Result<(), &'static str> {
    let counter = eventfd::create(0, false, false).map_err(|_| "an eventfd was refused")?;
    let inner = eventfd::of(&counter).ok_or("an eventfd is not one")?;
    *READER_ANSWER.lock() = None;
    *READER_FILE.lock() = Some(Arc::clone(&counter));
    let ended_before = inner.waits_ended_by_a_wake();
    let reader = crate::sched::spawn(
        "eventfd-reader",
        blocked_reader,
        0,
        ferrix_sched::NICE_0_WEIGHT,
    )?;
    until_listed(&inner, || READER_ANSWER.lock().is_some());
    crate::sched::sleep_for(STILL_WAITING_NANOS);
    if READER_ANSWER.lock().is_some() {
        return Err("a read of an empty blocking eventfd did not wait");
    }
    if counter.write(&9_u64.to_ne_bytes()) != Ok(VALUE_BYTES) {
        return Err("a write into an eventfd with a waiting reader was refused");
    }
    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    let _ = READER_DONE.wait_until_deadline(|| READER_ANSWER.lock().is_some(), deadline);
    let answer = READER_ANSWER.lock().take();
    if answer.is_none() {
        return Err("a reader waiting on an eventfd never came back after a write");
    }
    // Gone, not only answered: its stack must be back before the window the
    // second run counts in closes, or opens.
    crate::sched::wait_until_gone(&reader, crate::sched::REAPER_PATIENCE_NANOS)?;
    drop(reader);
    match answer {
        None => return Err("a reader waiting on an eventfd never came back after a write"),
        Some(Ok(9)) => {}
        Some(_) => return Err("a woken eventfd reader did not read the value written"),
    }
    if inner.waits_ended_by_a_wake() == ended_before {
        return Err("a waiting eventfd reader was ended by its recheck, not by the write's wake");
    }
    counts.reads += 1;
    Ok(())
}

/// Wait until a task is listed on `counter`'s readable queue, `answered` says
/// the task came back without one, or the patience runs out -- whichever is
/// first. The check that follows says which it was.
///
/// The write that is to end the wait must find the waiter on the queue, and a
/// fixed sleep only assumes it got there. Under TCG on a host running other
/// gates, where the scheduler's own check ran fifteen times slower than on a
/// quiet one, a spawned task can wait longer than 20 ms for its first turn;
/// the write would then find nobody to wake and the task would read the value
/// without waiting, which the check would blame on the wake.
fn until_listed(counter: &eventfd::EventFd, answered: impl Fn() -> bool) {
    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    while counter.readers_listed() == 0 && !answered() && crate::timer::now_nanos() < deadline {
        crate::sched::sleep_for(LISTED_LOOK_NANOS);
    }
}

/// The waiting task: one blocking read of [`READER_FILE`].
fn blocked_reader(_argument: usize) {
    let subject = READER_FILE.lock().clone();
    let answer = match subject {
        Some(counter) => {
            let mut value = [0_u8; VALUE_BYTES];
            counter.read(&mut value).map(|_| u64::from_ne_bytes(value))
        }
        None => Err(Errno::ESRCH),
    };
    *READER_ANSWER.lock() = Some(answer);
    READER_DONE.wake_all();
}

/// Edge-triggered in an epoll set: a second write is a second event, although
/// the eventfd stayed readable in between.
fn check_edges_in_epoll(
    process: &Process,
    page: u64,
    counts: &mut Counts,
) -> Result<(), &'static str> {
    let counter = create(process, 0, EFD_NONBLOCK)?;
    let set = descriptor(
        epoll::sys_epoll_create1(process, 0),
        "epoll_create1 was refused",
    )?;
    uaccess::copy_to_user(
        process.space(),
        page + AT_EVENT,
        &epoll::encode(EPOLLIN | EPOLLET, 0xEF),
    )
    .map_err(|_| "could not stage an epoll event")?;
    if epoll::sys_epoll_ctl(process, set, EPOLL_CTL_ADD, counter, page + AT_EVENT) != Ok(0) {
        return Err("an eventfd could not be added to an epoll set");
    }
    let wait = || epoll::sys_epoll_wait(process, set, page + AT_EVENTS, 4, 0);
    if wait() != Ok(0) {
        return Err("an epoll set reported an empty eventfd");
    }
    write_value(process, page, counter, 1)?;
    if wait() != Ok(1) {
        return Err("an epoll set did not report an eventfd that was written");
    }
    if wait() != Ok(0) {
        return Err("an edge-triggered eventfd was reported twice for one write");
    }
    write_value(process, page, counter, 1)?;
    if wait() != Ok(1) {
        return Err(
            "an edge-triggered eventfd that stayed readable was not reported after a second write",
        );
    }
    counts.reads += 1;
    closed(process, set)?;
    closed(process, counter)
}

/// A `poll` on a quiet eventfd sleeps on its queues through its whole
/// timeout, looking at the eventfd a handful of times, not every 5 ms.
fn check_a_quiet_poll_sleeps(process: &Process, page: u64) -> Result<(), &'static str> {
    let counter = create(process, 0, 0)?;
    stage_pollfd(process, page, counter)?;
    let before = fs::wake::looks();
    if poll::sys_poll(process, page + AT_EVENT, 1, QUIET_POLL_MILLIS) != Ok(0) {
        return Err("a poll on a quiet eventfd did not time out with zero");
    }
    let looked = fs::wake::looks().saturating_sub(before);
    if looked > QUIET_POLL_LOOKS {
        crate::console::println!(
            "  eventfd  a {QUIET_POLL_MILLIS} ms poll on a quiet eventfd looked {looked} times"
        );
        return Err("a poll on a quiet eventfd kept looking instead of sleeping on its queues");
    }
    closed(process, counter)
}

/// A `poll` and an `epoll_wait` waiting on an eventfd, each in a task of its
/// own, are ended by a write's wake, not by their own looking again.
fn check_waits_are_woken(
    process: &Arc<Process>,
    page: u64,
    counts: &mut Counts,
) -> Result<(), &'static str> {
    for how in [HowWaits::Poll, HowWaits::Epoll] {
        let counter = create(process, 0, 0)?;
        let file = fd::file(process, counter).map_err(|_| "the eventfd is gone")?;
        let inner = eventfd::of(&file).ok_or("an eventfd is not one")?;
        let target = match how {
            HowWaits::Poll => {
                stage_pollfd(process, page, counter)?;
                counter
            }
            HowWaits::Epoll => {
                let set = descriptor(
                    epoll::sys_epoll_create1(process, 0),
                    "epoll_create1 was refused",
                )?;
                uaccess::copy_to_user(
                    process.space(),
                    page + AT_EVENT,
                    &epoll::encode(EPOLLIN, 0x77),
                )
                .map_err(|_| "could not stage an epoll event")?;
                if epoll::sys_epoll_ctl(process, set, EPOLL_CTL_ADD, counter, page + AT_EVENT)
                    != Ok(0)
                {
                    return Err("an eventfd could not be added to an epoll set");
                }
                set
            }
        };
        *WAITER_ANSWER.lock() = None;
        *WAITER_SUBJECT.lock() = Some((Arc::clone(process), page, target, how));
        let ended_before = inner.waits_ended_by_a_wake();
        let waiter =
            crate::sched::spawn("poll-waiter", poll_waiter, 0, ferrix_sched::NICE_0_WEIGHT)?;
        until_listed(&inner, || WAITER_ANSWER.lock().is_some());
        crate::sched::sleep_for(STILL_WAITING_NANOS);
        if WAITER_ANSWER.lock().is_some() {
            return Err("a poll or epoll_wait on an empty eventfd did not wait");
        }
        write_value(process, page, counter, 1)?;
        let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
        let _ = READER_DONE.wait_until_deadline(|| WAITER_ANSWER.lock().is_some(), deadline);
        let answer = WAITER_ANSWER.lock().take();
        if answer.is_none() {
            return Err("a poll or epoll_wait never came back after a write");
        }
        crate::sched::wait_until_gone(&waiter, crate::sched::REAPER_PATIENCE_NANOS)?;
        drop(waiter);
        if answer != Some(Ok(1)) {
            return Err("a poll or epoll_wait did not answer one ready eventfd after a write");
        }
        if inner.waits_ended_by_a_wake() == ended_before {
            return Err(match how {
                HowWaits::Poll => {
                    "a waiting poll was ended by looking again, not by the write's wake"
                }
                HowWaits::Epoll => {
                    "a waiting epoll_wait was ended by looking again, not by the write's wake"
                }
            });
        }
        counts.reads += 1;
        drop(file);
        if how == HowWaits::Epoll {
            closed(process, target)?;
        }
        closed(process, counter)?;
    }
    Ok(())
}

/// Which call the waiting task makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HowWaits {
    /// `poll` on the eventfd.
    Poll,
    /// `epoll_wait` on a set holding it.
    Epoll,
}

/// The waiting task: one `poll` or `epoll_wait` on what [`WAITER_SUBJECT`]
/// names, with a timeout far past the check's patience.
fn poll_waiter(_argument: usize) {
    let subject = WAITER_SUBJECT.lock().take();
    let answer = match subject {
        Some((process, page, _, HowWaits::Poll)) => {
            poll::sys_poll(&process, page + AT_EVENT, 1, WAITER_TIMEOUT_MILLIS)
        }
        Some((process, page, target, HowWaits::Epoll)) => {
            epoll::sys_epoll_wait(&process, target, page + AT_EVENTS, 4, WAITER_TIMEOUT_MILLIS)
        }
        None => Err(Errno::ESRCH),
    };
    *WAITER_ANSWER.lock() = Some(answer);
    READER_DONE.wake_all();
}

/// Stage a `struct pollfd` asking `fd` for `POLLIN` where [`AT_EVENT`] is.
fn stage_pollfd(process: &Process, page: u64, fd: i32) -> Result<(), &'static str> {
    let mut entry = [0_u8; 8];
    for (slot, byte) in entry.iter_mut().zip(fd.to_le_bytes()) {
        *slot = byte;
    }
    for (slot, byte) in entry.iter_mut().skip(4).zip(poll::POLLIN.to_le_bytes()) {
        *slot = byte;
    }
    uaccess::copy_to_user(process.space(), page + AT_EVENT, &entry)
        .map_err(|_| "could not stage a pollfd")
}

/// `eventfd2(initial, flags)` by number, as a descriptor.
fn create(process: &Process, initial: u32, flags: u32) -> Result<i32, &'static str> {
    descriptor(
        by_number(
            process,
            Syscall::Eventfd2,
            [u64::from(initial), u64::from(flags), 0, 0, 0, 0],
        ),
        "eventfd2 was refused",
    )
}

/// Stage `value` where writes read it from.
fn stage(process: &Process, page: u64, value: u64) -> Result<(), &'static str> {
    uaccess::copy_to_user(process.space(), page + AT_VALUE, &value.to_ne_bytes())
        .map_err(|_| "could not stage a value")
}

/// Write `value` into the eventfd, requiring all eight bytes to go.
fn write_value(process: &Process, page: u64, counter: i32, value: u64) -> Result<(), &'static str> {
    stage(process, page, value)?;
    if file::sys_write(process, counter, page + AT_VALUE, 8) != Ok(VALUE_BYTES) {
        return Err("a write into an eventfd was refused");
    }
    Ok(())
}

/// Read the eventfd, requiring `want`.
fn expect_read(
    process: &Process,
    page: u64,
    counter: i32,
    want: u64,
    what: &'static str,
) -> Result<(), &'static str> {
    if file::sys_read(process, counter, page + AT_VALUE, 16) != Ok(VALUE_BYTES) {
        return Err(what);
    }
    let mut bytes = [0_u8; VALUE_BYTES];
    uaccess::copy_from_user(process.space(), page + AT_VALUE, &mut bytes)
        .map_err(|_| "could not read a value back")?;
    if u64::from_ne_bytes(bytes) != want {
        return Err(what);
    }
    Ok(())
}

/// Close a descriptor the check opened.
fn closed(process: &Process, opened: i32) -> Result<(), &'static str> {
    fd::sys_close(process, opened)
        .map(|_| ())
        .map_err(|_| "a descriptor the eventfd check opened would not close")
}

/// Make `call` by its number, as a program would.
fn by_number(process: &Process, call: Syscall, args: [u64; 6]) -> Result<usize, Errno> {
    syscall_check::call_by_number(process, call, args)
}

/// A descriptor a call returned.
fn descriptor(got: Result<usize, Errno>, what: &'static str) -> Result<i32, &'static str> {
    got.ok().and_then(|fd| i32::try_from(fd).ok()).ok_or(what)
}

/// Require `got` to be `Err(wanted)`, counting the refusal.
fn refused(
    got: Result<usize, Errno>,
    wanted: Errno,
    what: &'static str,
    counts: &mut Counts,
) -> Result<(), &'static str> {
    if got != Err(wanted) {
        return Err(what);
    }
    counts.refusals += 1;
    Ok(())
}
