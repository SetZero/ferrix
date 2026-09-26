//! What the read-side files print, byte for byte as Linux prints them.

use crate::write::Limit;
use alloc::vec::Vec;
use core::fmt::Write as _;

/// A `Vec<u8>` that `write!` can format into.
struct Out<'a>(&'a mut Vec<u8>);

impl core::fmt::Write for Out<'_> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        self.0.extend_from_slice(text.as_bytes());
        Ok(())
    }
}

/// Append `cgroup.procs` or `cgroup.threads`: one id a line, in the order
/// given. cgroup v2 promises no order -- only v1's `tasks` was sorted -- and
/// Ferrix's is the registry's, ascending.
pub fn ids(out: &mut Vec<u8>, ids: &[u32]) {
    for &id in ids {
        let _ = writeln!(Out(out), "{id}");
    }
}

/// Append `cgroup.events`.
pub fn events(out: &mut Vec<u8>, populated: bool, frozen: bool) {
    let _ = write!(
        Out(out),
        "populated {}\nfrozen {}\n",
        u8::from(populated),
        u8::from(frozen)
    );
}

/// Append `cgroup.stat`: the cgroups beneath this one, and the dying ones --
/// removed but still pinned -- which Ferrix never has, since a removed
/// cgroup is a job that can be dropped at once.
pub fn stat(out: &mut Vec<u8>, descendants: u32) {
    let _ = write!(
        Out(out),
        "nr_descendants {descendants}\nnr_dying_descendants 0\n"
    );
}

/// Append a limit as `cgroup.max.depth` and `cgroup.max.descendants` print
/// it.
pub fn limit(out: &mut Vec<u8>, limit: Limit) {
    match limit {
        Limit::Max => out.extend_from_slice(b"max\n"),
        Limit::At(count) => {
            let _ = writeln!(Out(out), "{count}");
        }
    }
}

/// Append a cgroup's path as the cgroup v2 hierarchy names it: `/` for the
/// root, `/a/b` beneath it. `names` goes from the root's child down.
pub fn path<'a>(out: &mut Vec<u8>, names: impl IntoIterator<Item = &'a [u8]>) {
    let mut any = false;
    for name in names {
        out.push(b'/');
        out.extend_from_slice(name);
        any = true;
    }
    if !any {
        out.push(b'/');
    }
}

/// Append `/proc/<pid>/cgroup` for a process in the cgroup `names` leads
/// to: the unified hierarchy's line, `0::/path`, and no other, since there
/// are no v1 hierarchies.
pub fn proc_cgroup<'a>(out: &mut Vec<u8>, names: impl IntoIterator<Item = &'a [u8]>) {
    out.extend_from_slice(b"0::");
    path(out, names);
    out.push(b'\n');
}

/// Append `pids.max` or `memory.max`: `max` for no limit, else the number.
pub fn max(out: &mut Vec<u8>, limit: Option<u64>) {
    match limit {
        None => out.extend_from_slice(b"max\n"),
        Some(value) => {
            let _ = writeln!(Out(out), "{value}");
        }
    }
}

/// Append a single number and a newline: `pids.current`, `memory.current`,
/// `cpu.weight`.
pub fn number(out: &mut Vec<u8>, value: u64) {
    let _ = writeln!(Out(out), "{value}");
}

/// Append `pids.events`: how many forks the limit refused.
pub fn pids_events(out: &mut Vec<u8>, max: u64) {
    let _ = writeln!(Out(out), "max {max}");
}

/// Append `memory.stat`: of the bytes `memory.current` counts, how many are
/// kernel memory held for the cgroup's programs, under Linux's key. Linux
/// prints some forty keys; this is the one Ferrix counts, and a reader
/// looks a key up by its name.
pub fn memory_stat(out: &mut Vec<u8>, kernel: u64) {
    let _ = writeln!(Out(out), "kernel {kernel}");
}

/// Append `memory.events`, in the order Linux prints it: how many charges
/// `memory.max` refused under `max`, and the rest, which Ferrix never counts
/// (no `memory.low`, no `memory.high`, no OOM kill), as zeros.
pub fn memory_events(out: &mut Vec<u8>, max: u64) {
    let _ = write!(
        Out(out),
        "low 0\nhigh 0\nmax {max}\noom 0\noom_kill 0\noom_group_kill 0\n"
    );
}
