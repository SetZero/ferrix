//! Finding F-23's negative control: allocations fail, on the paths a program
//! drives, and the kernel carries on.
//!
//! The certified item allocates through `crate::fallible`, which reports a
//! failure as an error its caller turns into `NO_MEMORY` or `ENOMEM`. That is
//! a claim about every caller, and a gate that reads the source
//! (`scripts/check-fallible-alloc.py`) can say only that the calls are the
//! fallible ones, not that a failure is handled well where it lands. This
//! runs the failure. Two parts:
//!
//! * **The reserve serves.** An `Arc` and a map insert cannot report failure
//!   once they have started; they run inside a reserved section, which fills
//!   this processor's reserve first. With the heap made to refuse every
//!   allocation inside a section, both must still complete, on the reserve
//!   alone -- and with the reserve refused its filling, both must fail before
//!   they start.
//! * **The native ABI survives.** One process drives a round of native calls
//!   that allocate -- make a VMO, a port, a channel and a job, duplicate a
//!   handle, register on a port, write a message carrying a handle, wait on
//!   the port, read the message, write the VMO, close everything -- over and
//!   over while every `n`th fallible allocation of this task is made to fail,
//!   for several `n`. Every call must succeed or answer `NO_MEMORY` (or the
//!   status that follows from an earlier call having failed); at least one
//!   must have been refused for memory, and at least one allowed through;
//!   the machine must still be here; and afterwards, with nothing failing, a
//!   round must succeed whole and the rounds must have leaked no frame.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::nr;
use ferrix_native_abi::rights::{Rights, SAME_RIGHTS};
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::status;

use crate::fallible;
use crate::mm;
use crate::object::check::{SCRATCH, Side, reg};
use crate::object::job::Job;
use crate::object::{self, Object};

/// Where each call's user memory is, in the side's scratch region.
const PAIR: u64 = SCRATCH + 0x400;
/// The message's bytes.
const PAYLOAD: u64 = SCRATCH + 0x500;
/// The handle a message carries, going out.
const SENT: u64 = SCRATCH + 0x600;
/// Where a read puts what arrives.
const INBOX: u64 = SCRATCH + 0x700;
/// Where a read puts the handles that arrive.
const RECEIVED: u64 = SCRATCH + 0x800;
/// Where a read reports what it delivered.
const ACTUAL: u64 = SCRATCH + 0x880;
/// A registration's key, and a VMO offset.
const WORDS: u64 = SCRATCH + 0x900;
/// A port wait's deadline: one nanosecond after boot, long past.
const DEADLINE: u64 = SCRATCH + 0x910;
/// Where a port wait puts its packet.
const PACKET: u64 = SCRATCH + 0x940;

/// Every how many fallible allocations one is failed, a pass each. Primes, so
/// that the failures land on a different call of the round each time.
const PERIODS: [u32; 6] = [2, 3, 5, 7, 11, 13];

/// Rounds per period.
const ROUNDS: usize = 12;

/// Bytes in the message a round writes.
const MESSAGE: u64 = 16;

/// What the check did, for the boot line.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Report {
    /// Native calls made with allocations failing.
    pub(crate) calls: u32,
    /// Of those, answered `NO_MEMORY`.
    pub(crate) refused: u32,
    /// Allocations the injection failed.
    pub(crate) injected: u64,
    /// Allocations served from a reserve while the heap refused.
    pub(crate) drawn: u64,
}

/// Run both parts.
pub(crate) fn run() -> Result<Report, &'static str> {
    let drawn = check_a_section_completes_on_the_reserve()?;
    let mut report = check_the_native_calls_survive()?;
    report.drawn = drawn;
    if object::abandoned() != 0 {
        return Err("an object was given up rather than disposed of");
    }
    Ok(report)
}

/// An `Arc`, a large `Arc` and a run of map inserts complete with the heap
/// refusing every allocation inside their sections; and fail before they
/// start when their section cannot be entered.
fn check_a_section_completes_on_the_reserve() -> Result<u64, &'static str> {
    let (drawn_before, _) = mm::reserve_counts();
    mm::bypass_heap_in_sections(true);
    let small = fallible::try_arc([7_u64; 8]);
    let large = fallible::try_arc([9_u8; 3000]);
    let mut map = BTreeMap::new();
    let mut inserted = Ok(());
    for key in 0..200_u64 {
        if let Err(error) = fallible::insert(&mut map, key, key.wrapping_mul(3)) {
            inserted = Err(error);
            break;
        }
    }
    mm::bypass_heap_in_sections(false);
    let (drawn_after, _) = mm::reserve_counts();

    let small = small.map_err(|_| "an Arc was refused although its section was entered")?;
    let large = large.map_err(|_| "a large Arc was refused although its section was entered")?;
    inserted.map_err(|_| "a map insert was refused although its section was entered")?;
    if *small != [7; 8] || large.iter().any(|&byte| byte != 9) {
        return Err("an Arc built on the reserve does not hold its value");
    }
    if map.len() != 200 || !map.iter().all(|(key, value)| *value == key.wrapping_mul(3)) {
        return Err("a map built on the reserve does not hold what was inserted");
    }
    // Two `Arc`s, and a leaf and the nodes its splits made.
    let drawn = drawn_after.saturating_sub(drawn_before);
    if drawn < 4 {
        return Err("the reserve was not drawn on while the heap refused");
    }
    drop((small, large, map));

    // A section whose reserve cannot be filled refuses before anything runs.
    let task = crate::sched::current_id().ok_or("the allocation check runs outside a task")?;
    fallible::inject(task, 1);
    let refused_arc = fallible::try_arc(1_u64).is_err();
    let mut refused_map = BTreeMap::new();
    let refused_insert = fallible::insert(&mut refused_map, 1_u8, 1_u8).is_err();
    let failed = fallible::stop_injecting();
    if !refused_arc || !refused_insert || failed < 2 || !refused_map.is_empty() {
        return Err("a section whose reserve could not be filled went ahead");
    }
    Ok(drawn)
}

/// The handles one round has made and not yet closed.
#[derive(Debug, Default)]
struct Round {
    /// Every handle made, to close at the end.
    open: Vec<Handle>,
}

/// Rounds of native calls with allocations failing, then one without.
fn check_the_native_calls_survive() -> Result<Report, &'static str> {
    let side = Side::new()?;
    let root = Job::new_root().map_err(|_| "no memory for the check's job")?;
    let root = side
        .process
        .with_handles(|table| table.insert(Object::Job(root), Rights::JOB))
        .map_err(|_| "no room for the check's job")?;
    side.put(PAYLOAD, &[0xA5; MESSAGE as usize])?;
    side.put(WORDS, &0_u64.to_ne_bytes())?;
    side.put(DEADLINE, &1_u64.to_ne_bytes())?;

    let task = crate::sched::current_id().ok_or("the allocation check runs outside a task")?;
    // One round before the window, for the size classes and the table's first
    // slots, which stay.
    let mut tally = Report::default();
    let mut warm = Report::default();
    round(&side, root, &mut warm, false)?;
    crate::sched::wait_until_reaper_quiet(crate::sched::REAPER_PATIENCE_NANOS)?;
    let window = mm::FrameWindow::open();
    for period in PERIODS {
        fallible::inject(task, period);
        let mut outcome = Ok(());
        for _ in 0..ROUNDS {
            outcome = round(&side, root, &mut tally, true);
            if outcome.is_err() {
                break;
            }
        }
        tally.injected += fallible::stop_injecting();
        outcome?;
    }
    // Nothing failing: every call of a round succeeds, so nothing the failures
    // left behind stands in the way.
    let mut clean = Report::default();
    round(&side, root, &mut clean, false)?;
    crate::sched::wait_until_reaper_quiet(crate::sched::REAPER_PATIENCE_NANOS)?;
    let leaked = window.kept();
    if leaked != 0 {
        mm::print_frame_delta("allocation failure", leaked);
        window.report("allocation failure");
        return Err("rounds with allocations failing did not give every frame back");
    }
    side.close_everything();

    if tally.refused == 0 || tally.injected == 0 {
        return Err("no native call was refused for memory while allocations failed");
    }
    if tally.refused >= tally.calls {
        return Err("every native call was refused while allocations failed");
    }
    Ok(tally)
}

/// One round of calls. With `failing`, a call may answer `NO_MEMORY`, or what
/// follows from an earlier one having done so; without, every call must
/// succeed.
fn round(side: &Side, root: Handle, tally: &mut Report, failing: bool) -> Result<(), &'static str> {
    let mut made = Round::default();
    let outcome = calls(side, root, tally, failing, &mut made);
    // A port's promises must have held: a packet with no room is one lost.
    let lost: u64 = made
        .open
        .iter()
        .filter_map(|&handle| {
            side.process.with_handles(|table| match table.get(handle) {
                Ok((Object::Port(port), _)) => Some(port.lost()),
                _ => None,
            })
        })
        .sum();
    for handle in made.open {
        let _ = side.call(nr::HANDLE_CLOSE, &[reg(handle)]);
    }
    if lost != 0 {
        return Err("a port found no room for a packet its registration had promised");
    }
    outcome
}

/// Makes the calls of a round, and says whether each outcome is allowed.
struct Caller<'a> {
    /// The process calling.
    side: &'a Side,
    /// What the calls came to.
    tally: &'a mut Report,
    /// Whether allocations are failing.
    failing: bool,
}

impl Caller<'_> {
    /// Make one call: its value if it succeeded, `None` if it was refused in
    /// a way `failing` allows -- `NO_MEMORY`, or one of `also`, which follow
    /// from an earlier call having been refused -- and `what` otherwise.
    fn call(
        &mut self,
        number: usize,
        args: &[u64],
        also: &[Errno],
        what: &'static str,
    ) -> Result<Option<usize>, &'static str> {
        self.tally.calls += 1;
        match self.side.call(number, args) {
            Ok(value) => Ok(Some(value)),
            Err(status::NO_MEMORY) if self.failing => {
                self.tally.refused += 1;
                Ok(None)
            }
            Err(other) if self.failing && also.contains(&other) => Ok(None),
            Err(other) => {
                crate::console::println!("  no-mem   {what}: status {}", other.0);
                Err(what)
            }
        }
    }

    /// Make a call that answers a handle, and note the handle in `made`.
    fn handle(
        &mut self,
        number: usize,
        args: &[u64],
        what: &'static str,
        made: &mut Round,
    ) -> Result<Option<Handle>, &'static str> {
        let value = self.call(number, args, &[], what)?;
        let handle = value
            .and_then(|value| u32::try_from(value).ok())
            .map(Handle);
        if let Some(handle) = handle {
            made.open.push(handle);
        }
        Ok(handle)
    }
}

/// What a round made, where the call making it succeeded.
#[derive(Debug, Default, Clone, Copy)]
struct Objects {
    /// A VMO.
    vmo: Option<Handle>,
    /// A port.
    port: Option<Handle>,
    /// A channel's end the round writes into.
    writer: Option<Handle>,
    /// Its other end, which the round reads.
    reader: Option<Handle>,
}

/// The calls of [`round`], recording every handle made in `made`.
fn calls(
    side: &Side,
    root: Handle,
    tally: &mut Report,
    failing: bool,
    made: &mut Round,
) -> Result<(), &'static str> {
    let mut caller = Caller {
        side,
        tally,
        failing,
    };
    let objects = make(&mut caller, root, made)?;
    exchange(&mut caller, objects, made)
}

/// Make a VMO, a port, a job and a channel.
fn make(caller: &mut Caller<'_>, root: Handle, made: &mut Round) -> Result<Objects, &'static str> {
    let vmo = caller.handle(
        nr::VMO_CREATE,
        &[4096],
        "vmo_create failed with allocations failing",
        made,
    )?;
    let port = caller.handle(
        nr::PORT_CREATE,
        &[],
        "port_create failed with allocations failing",
        made,
    )?;
    let _job = caller.handle(
        nr::JOB_CREATE,
        &[reg(root)],
        "job_create failed with allocations failing",
        made,
    )?;
    let pair = caller.call(
        nr::CHANNEL_CREATE,
        &[PAIR],
        &[],
        "channel_create failed with allocations failing",
    )?;
    let (writer, reader) = if pair.is_some() {
        let ends = (
            Handle(caller.side.get_u32(PAIR)?),
            Handle(caller.side.get_u32(PAIR + 4)?),
        );
        made.open.push(ends.0);
        made.open.push(ends.1);
        (Some(ends.0), Some(ends.1))
    } else {
        (None, None)
    };
    Ok(Objects {
        vmo,
        port,
        writer,
        reader,
    })
}

/// Duplicate the VMO's handle, register on the port for the channel, write a
/// message carrying the duplicate, wait on the port, read the message, and
/// write the VMO: whichever of those the objects made allow.
fn exchange(
    caller: &mut Caller<'_>,
    objects: Objects,
    made: &mut Round,
) -> Result<(), &'static str> {
    let copy = match objects.vmo {
        Some(vmo) => caller.call(
            nr::HANDLE_DUPLICATE,
            &[reg(vmo), u64::from(SAME_RIGHTS)],
            &[],
            "handle_duplicate failed with allocations failing",
        )?,
        None => None,
    };
    let copy = copy.and_then(|value| u32::try_from(value).ok()).map(Handle);

    if let (Some(reader), Some(port)) = (objects.reader, objects.port) {
        let _ = caller.call(
            nr::OBJECT_WAIT_ASYNC,
            &[
                reg(reader),
                reg(port),
                u64::from(Signals::READABLE.0),
                WORDS,
            ],
            &[],
            "object_wait_async failed with allocations failing",
        )?;
    }

    let sent = send(caller, objects.writer, copy, made)?;

    if let Some(port) = objects.port {
        // Empty if the registration or the write failed.
        let _ = caller.call(
            nr::PORT_WAIT,
            &[reg(port), DEADLINE, PACKET],
            &[status::TIMED_OUT],
            "port_wait failed with allocations failing",
        )?;
    }

    if let Some(reader) = objects.reader {
        let read = caller.call(
            nr::CHANNEL_READ,
            &[reg(reader), INBOX, MESSAGE, RECEIVED, 4, ACTUAL],
            // Nothing to read if the write failed.
            &[status::SHOULD_WAIT],
            "channel_read failed with allocations failing",
        )?;
        if read.is_some() && sent && copy.is_some() {
            made.open.push(Handle(caller.side.get_u32(RECEIVED)?));
        }
    }

    if let Some(vmo) = objects.vmo {
        let _ = caller.call(
            nr::VMO_WRITE,
            &[reg(vmo), PAYLOAD, MESSAGE, WORDS],
            &[],
            "vmo_write failed with allocations failing",
        )?;
    }
    Ok(())
}

/// Write the round's message through `writer`, carrying `copy` if there is
/// one. Returns whether it went; a copy that did not go stays with the
/// writer, and is noted in `made` to be closed.
fn send(
    caller: &mut Caller<'_>,
    writer: Option<Handle>,
    copy: Option<Handle>,
    made: &mut Round,
) -> Result<bool, &'static str> {
    let mut sent = false;
    if let Some(writer) = writer {
        if let Some(copy) = copy {
            caller.side.put(SENT, &copy.0.to_ne_bytes())?;
        }
        let written = caller.call(
            nr::CHANNEL_WRITE,
            &[
                reg(writer),
                PAYLOAD,
                MESSAGE,
                SENT,
                u64::from(copy.is_some()),
            ],
            &[],
            "channel_write failed with allocations failing",
        )?;
        sent = written.is_some();
    }
    if !sent && let Some(copy) = copy {
        made.open.push(copy);
    }
    Ok(sent)
}
