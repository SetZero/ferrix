//! Stage 9's edges: the paths of the objects no program in the suite takes,
//! taken (finding F-10).
//!
//! What a job's `cgroup.subtree_control`, a node's owner and a limit are set
//! to by cgroupfs, which no gate writes; a port's packet put back when the
//! program's buffer could not take it; a registration a change does not fire
//! passed over for the one it does; a cycle walk meeting one end twice; and a
//! delivery to a line nobody holds. Each through the interface a program or
//! cgroupfs reaches it by where there is one -- the native calls, with raw
//! registers -- and by name where there is not.

use ferrix_cgroupfs::controllers::{Change, Controller, Set};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::nr;
use ferrix_native_abi::rights::SAME_RIGHTS;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::status;

use crate::object::check::{SCRATCH, Side, reg};
use alloc::sync::Weak;

use crate::object::interrupt;
use crate::object::job::{Job, JobError, NodeAttributes};
use crate::object::process;

/// Where a channel pair's two handles are written.
const PAIR: u64 = SCRATCH + 0xA00;
/// Handles a message carries.
const CARRIED: u64 = SCRATCH + 0xA10;
/// A packet, going in and coming out.
const PACKET: u64 = SCRATCH + 0xA40;
/// A registration's key.
const KEY: u64 = SCRATCH + 0xA80;
/// An address no program maps: the first page, below `MMAP_MIN_ADDR`.
const NOWHERE: u64 = 0x10;

/// Run them; answers how many were checked.
pub(crate) fn run() -> Result<u32, &'static str> {
    check_subtree_control()?;
    check_a_job_records_what_cgroupfs_sets()?;
    check_pids_wrap_past_one_still_held()?;
    let side = Side::new()?;
    check_a_packet_is_put_back(&side)?;
    check_a_change_fires_only_what_wants_it(&side)?;
    check_a_cycle_walk_meets_one_end_twice(&side)?;
    // A line nobody holds: the delivery finds nothing, and does nothing.
    interrupt::on_interrupt(u32::MAX);
    side.close_everything();
    Ok(7)
}

/// Enabling needs the controller offered; a parent may not disable what a
/// child enables; a job enabling a domain controller for its children takes
/// no process itself.
fn check_subtree_control() -> Result<(), &'static str> {
    let pids = Set::EMPTY.with(Controller::Pids);
    let memory = Set::EMPTY.with(Controller::Memory);
    let all = pids.union(memory);
    let enable = |set| Change {
        enable: set,
        disable: Set::EMPTY,
    };
    let root = Job::new_root().map_err(|_| "no memory for the subtree root")?;
    let child = root
        .new_child()
        .map_err(|_| "no memory for the subtree child")?;
    if root.change_subtree_control(enable(memory), pids) != Err(JobError::Missing) {
        return Err("a controller not offered was enabled");
    }
    root.change_subtree_control(enable(all), all)
        .and_then(|()| child.change_subtree_control(enable(pids), all))
        .map_err(|_| "enabling offered controllers was refused")?;
    if root.subtree_control() != all || child.subtree_control() != pids {
        return Err("enabled controllers were not the ones asked for");
    }
    let disable = Change {
        enable: Set::EMPTY,
        disable: pids,
    };
    if root.change_subtree_control(disable, all) != Err(JobError::Busy) {
        return Err("a controller a child enables was disabled above it");
    }
    let domain = child
        .new_child()
        .map_err(|_| "no memory for the domain job")?;
    domain
        .change_subtree_control(enable(memory), all)
        .map_err(|_| "enabling a domain controller in an empty job was refused")?;
    let side = Side::new()?;
    let home = side.process.job();
    // Counted in its job and not: a process that has left its job's count
    // -- one that has let go of what it held -- is asked the same.
    for leave in [false, true] {
        if leave {
            side.process.leave_job();
        }
        if side.process.move_to(&domain) != Err(JobError::Internal) {
            return Err("a process moved into a job enabling a domain controller");
        }
    }
    side.process
        .move_to(&home)
        .map_err(|_| "a process could not go back to its own job")?;
    side.close_everything();
    Ok(())
}

/// A node's owner, set twice, is the second; a depth limit reads back; an
/// anonymous job's path names it by number above its named child.
fn check_a_job_records_what_cgroupfs_sets() -> Result<(), &'static str> {
    let root = Job::new_root().map_err(|_| "no memory for the naming root")?;
    let anonymous = root
        .new_child()
        .map_err(|_| "no memory for an anonymous job")?;
    let named = anonymous
        .new_named_child("leaf")
        .map_err(|_| "no memory for a named job")?;
    let owner = |uid| NodeAttributes {
        uid,
        gid: 5,
        permissions: 0o644,
    };
    named
        .set_node(3, owner(1))
        .and_then(|()| named.set_node(3, owner(2)))
        .map_err(|_| "no memory for a node's owner")?;
    if named.node(3).map(|node| node.uid) != Some(2) {
        return Err("a node's owner set twice was not the second");
    }
    let limit = ferrix_cgroupfs::write::parse_limit(b"4\n").map_err(|_| "a limit did not parse")?;
    named.set_max_depth(limit);
    if named.limits().0 != limit {
        return Err("a job's depth limit did not read back");
    }
    let path = named.path_names().map_err(|_| "no memory for a path")?;
    let leaf = path.get(1).map(alloc::string::String::as_str);
    let first = path.first().map(|name| name.starts_with("job-"));
    if path.len() != 2 || leaf != Some("leaf") || first != Some(true) {
        return Err("a job's path did not name its anonymous parent and then it");
    }
    drop(named);
    let _ = anonymous
        .remove_named_child("leaf")
        .map_err(|_| "removing a named job failed")?;
    Ok(())
}

/// Pids run up to `PID_MAX` and wrap to `RESERVED`, and the wrap passes over
/// a number still held rather than hand it out twice.
fn check_pids_wrap_past_one_still_held() -> Result<(), &'static str> {
    let held = process::RESERVED;
    let named = process::is_free(held);
    if named {
        let nobody: Weak<crate::syscall::process::Process> = Weak::new();
        process::name(held, nobody).map_err(|_| "no memory to hold a pid")?;
    }
    let mut previous = 0;
    let mut wrapped = None;
    for _ in 0..process::PID_MAX {
        let pid = process::allocate().ok_or("no pid left to allocate")?;
        process::release(pid);
        if pid < previous {
            wrapped = Some(pid);
            break;
        }
        previous = pid;
    }
    if named {
        process::release(held);
    }
    match wrapped {
        None => Err("pids never wrapped"),
        Some(pid) if pid <= held => {
            Err("a wrap handed out a pid still held, or one below RESERVED")
        }
        Some(_) => Ok(()),
    }
}

/// A port's packet that could not be copied out is put back, whether a
/// program queued it or a registration fired it -- on a change, or as it was
/// made -- and the next wait takes it.
fn check_a_packet_is_put_back(side: &Side) -> Result<(), &'static str> {
    let port = side.handle(nr::PORT_CREATE, &[], "port_create failed")?;
    side.put(PACKET, &[0x5A; 32])?;
    let _ = side
        .call(nr::PORT_QUEUE, &[reg(port), PACKET])
        .map_err(|_| "port_queue failed")?;
    let (near, far) = pair(side)?;
    side.put(KEY, &9_u64.to_ne_bytes())?;
    let _ = side
        .call(
            nr::OBJECT_WAIT_ASYNC,
            &[reg(near), reg(port), u64::from(Signals::PEER_CLOSED.0), KEY],
        )
        .map_err(|_| "object_wait_async failed")?;
    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(far)])
        .map_err(|_| "closing a registered channel's peer failed")?;
    // Registered once the peer has gone: it fires as it is made.
    side.put(KEY, &10_u64.to_ne_bytes())?;
    let _ = side
        .call(
            nr::OBJECT_WAIT_ASYNC,
            &[reg(near), reg(port), u64::from(Signals::PEER_CLOSED.0), KEY],
        )
        .map_err(|_| "object_wait_async on a channel whose peer had gone failed")?;
    for _ in 0..3 {
        if side.call(nr::PORT_WAIT, &[reg(port), 0, NOWHERE]) != Err(status::FAULT) {
            return Err("a port wait into memory nobody maps did not fault");
        }
        let _ = side
            .call(nr::PORT_WAIT, &[reg(port), 0, PACKET])
            .map_err(|_| "a packet put back was not there for the next wait")?;
    }
    for handle in [near, port] {
        let _ = side
            .call(nr::HANDLE_CLOSE, &[reg(handle)])
            .map_err(|_| "closing a port check's handle failed")?;
    }
    Ok(())
}

/// A message asserts `READABLE`, which fires the registration waiting for it
/// and not the one waiting for `PEER_CLOSED` listed ahead of it.
fn check_a_change_fires_only_what_wants_it(side: &Side) -> Result<(), &'static str> {
    let port = side.handle(nr::PORT_CREATE, &[], "port_create failed")?;
    let (near, far) = pair(side)?;
    for (key, signals) in [(1_u64, Signals::PEER_CLOSED), (2, Signals::READABLE)] {
        side.put(KEY, &key.to_ne_bytes())?;
        let _ = side
            .call(
                nr::OBJECT_WAIT_ASYNC,
                &[reg(far), reg(port), u64::from(signals.0), KEY],
            )
            .map_err(|_| "object_wait_async failed")?;
    }
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(near), PACKET, 4, CARRIED, 0])
        .map_err(|_| "a write to a watched channel failed")?;
    let _ = side
        .call(nr::PORT_WAIT, &[reg(port), 0, PACKET])
        .map_err(|_| "the registration a message fires did not fire")?;
    if side.get(PACKET, 8)? != 2_u64.to_ne_bytes() {
        return Err("a message fired the registration waiting for something else");
    }
    for handle in [near, far, port] {
        let _ = side
            .call(nr::HANDLE_CLOSE, &[reg(handle)])
            .map_err(|_| "closing a registration check's handle failed")?;
    }
    Ok(())
}

/// An end carried twice in one inbox, by two handles to it, is walked once
/// when that inbox's end is itself carried.
fn check_a_cycle_walk_meets_one_end_twice(side: &Side) -> Result<(), &'static str> {
    let (x, x_far) = pair(side)?;
    let (y, y_far) = pair(side)?;
    let (z, z_far) = pair(side)?;
    let twin = side.handle(
        nr::HANDLE_DUPLICATE,
        &[reg(x), u64::from(SAME_RIGHTS)],
        "duplicating a channel end failed",
    )?;
    put_handles(side, &[x, twin])?;
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(y), PACKET, 0, CARRIED, 2])
        .map_err(|_| "carrying one end twice failed")?;
    put_handles(side, &[y_far])?;
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(z), PACKET, 0, CARRIED, 1])
        .map_err(|_| "carrying an end whose inbox holds one end twice failed")?;
    for handle in [x_far, y, z, z_far] {
        let _ = side
            .call(nr::HANDLE_CLOSE, &[reg(handle)])
            .map_err(|_| "closing a cycle check's handle failed")?;
    }
    Ok(())
}

/// A channel in `side`: both ends.
fn pair(side: &Side) -> Result<(Handle, Handle), &'static str> {
    let _ = side
        .call(nr::CHANNEL_CREATE, &[PAIR])
        .map_err(|_| "channel_create failed")?;
    Ok((Handle(side.get_u32(PAIR)?), Handle(side.get_u32(PAIR + 4)?)))
}

/// Put `handles` at [`CARRIED`].
fn put_handles(side: &Side, handles: &[Handle]) -> Result<(), &'static str> {
    for (index, handle) in (0_u64..).zip(handles) {
        side.put(CARRIED + 4 * index, &handle.0.to_ne_bytes())?;
    }
    Ok(())
}
