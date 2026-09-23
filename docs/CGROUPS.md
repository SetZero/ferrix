# cgroups: the job tree, seen as cgroup v2

Version 1, a draft. Written on 2026-09-23. It is the cgroup half of stage 13,
which the customer put first that day, and builds on the customer's decision
of the same day that **every cgroup is backed by a `Job`** (`docs/INIT.md` §0,
C8; `docs/BACKLOG.md`, Decisions). `docs/INIT.md` §0.1 is what the first user
needs from this; `docs/ARCHITECTURE.md` §3 and §6 are the architecture. G1 and
G2 (§7) are built. §9 lists what is still the customer's to decide.

## 1. What this is, and what it is not

It is cgroup v2 as Linux defines it: one unified hierarchy, mounted as
`cgroup2`, that `mkdir`, `rmdir`, `cgroup.procs` and the controller files
drive. A program written against Linux's cgroups (systemd, a container
runtime, `docs/INIT.md`'s init) works unchanged. The controllers are
`memory`, `pids`, `cpu` and `io`, as `ARCHITECTURE.md` §6 says, and they
arrive one at a time (§7).

It is **not** a second container object. The kernel already has one: a `Job`
is "a container of processes, and where resource limits and kill authority
live" (`ARCHITECTURE.md` §3). A cgroup *is* a job, and cgroupfs is a view of
the job tree for the Linux ABI, as `/proc` is a view of processes. A native
program sees the same tree through job handles. A microkernel that someday
drops cgroupfs keeps the jobs and everything in them.

It is not cgroup v1, threaded cgroups (`cgroup.type` is `domain` and nothing
else), or the cgroup namespace. The namespace comes with the rest of stage
13's namespaces, and nothing here stands in its way.

## 2. What the job has to become

Today (read from `kernel/src/object/job.rs` and the process code on
2026-09-23):

* only `process_create` puts a process in a job, so every Linux process is in
  none, and `fork` copies no membership;
* a `Process` does not know its job;
* members and children are held weakly, and an exited process leaves its
  weak entry until it is reaped: a job cannot say whether it is populated;
* a job has no name and no identity a path could be built from;
* a killed job is killed for good, and refuses new members.

Each of these changes, as follows.

### 2.1 Every process is in exactly one job

At boot the kernel makes **the root job**, which is cgroupfs's root. A
process made by `Process::new` without a job (init, and the kernel's own
programs) starts in it. `devmgr`'s job, which the kernel makes today as a
root of its own, becomes a child of the root named `drivers.slice`, the name
`docs/INIT.md` §5.1 gives it.

`Process` gains `membership: SpinLock<Arc<Job>>`, held strongly: a process
keeps its job alive, as a Linux task keeps its `css_set`. `fork` reads the
parent's job under the parent's membership lock and joins the child to it
before the child is published (`kernel/src/syscall/family.rs`, before the
`processes` insert in `clone_with`). So a fork and a move of its parent are
ordered: the child lands in the job the parent was in when the fork took the
lock, and never half in one and half in the other. `process_create` joins the
job it is given, as it does now.

### 2.2 Membership is counted, and "populated" is exact

Each job keeps two counts under its lock:

* `live`, its own member processes that have not exited;
* `busy_children`, its children whose subtree is populated.

A job is **populated** when either count is nonzero. `live` goes up on join
and down in `Process::end`, the moment the last thread exits, not at reap. A
zombie is not a member, exactly as on Linux. When a count crosses zero, the
change walks up the tree to the parent's `busy_children`, one job lock at a
time and child before parent, and stops at the first job whose own populated
state does not change. At every job whose state flipped, the kernel wakes
the job's event queue (§4) and fires its `EMPTY` observers (§5).

A job keeps no member list at all (changed in G1 from this document's first
draft). A process's `membership` is the one truth, and `cgroup.procs` and
both kills find members by walking the process registry for the processes
whose job the job contains. A list would have had to be filled where each
process is first shared, which is several places, while the pointer is set
where every process is built, which is one. The counts are what *decide*
populated, and the registry is where members are *found*.

A fork's child is findable before it can run. A kill that began before the
child was published finds it in the registry. A kill that began after, the
fork sees at once: `clone_with` asks the child's job whether it is dying,
and ends the child before it starts.

### 2.3 Names, and who holds whom

A job gets a `name` and an `id` from a global counter. A **named** child is
made by `mkdir` in cgroupfs. Its parent holds it strongly until `rmdir`, as a
directory in a filesystem is held by the filesystem, not by who made it. An
**anonymous** child is made by native `job_create`. It is held as now, by its
handles and its members, appears in cgroupfs as `job-<id>`, and goes away
when the last handle closes and it is empty. That is how a native job has
always behaved, and cgroupfs only shows it. `rmdir` removes a named job, and
refuses one with members or children (`EBUSY`), as Linux does. An anonymous
job answers `rmdir` with `EBUSY` too (settled in G2): its handles hold it,
and it goes when they do.

A process path in `/proc/<pid>/cgroup` (`0::/system.slice/sshd.service`) is
built by walking `parent` and joining names. It is computed when read, never
stored.

### 2.4 Two kills, kept apart

`job_kill` keeps its native meaning: it kills everything and seals the job,
so nothing can join it again. `docs/DEVMGR.md` relies on that for a dead
driver's job.

`cgroup.kill` is Linux's and does not seal. It sets a `killing` mark under the
lock, takes the current members out as the job kill does, and kills them
outside the lock (`job.rs` explains why the lock must not be held across
`process::kill`). Then it clears the mark. A fork that joins a job while the
mark is set is killed as it starts, which is how Linux keeps a forking loop
from outrunning `cgroup.kill`. Both kills walk the subtree.

### 2.5 Lock order

`Process.membership` before `Job.state`, never the reverse, and never two
job locks at once. A move takes the process's membership lock, then leaves
the old job and joins the new one, each under its own lock and one after the
other. The count walk of §2.2 takes one job lock at a time going up. The
kill takes one at a time going down, and releases each before killing. So no
path holds two job locks, and none holds a job lock where the scheduler's
locks are taken (FX-0503's rule, `crate::sync::SpinLock`).

## 3. cgroupfs

A new in-kernel filesystem, `kernel/src/fs/cgroupfs.rs`, registered as
`cgroup2` in `filesystem_named` (`kernel/src/syscall/fsctl.rs`, where the
comment already says it joins that match) and listed in `/proc/filesystems`.
Every text it reads or writes is parsed and rendered by a new
`libs/cgroupfs`, a pure crate with host tests and a fuzzer. That covers
`+memory -cpu` lists, `max` or a byte count, `quota period`, `key value`
tables and the pid lists.

Every directory answers `caches_lookups() = false`, like procfs's, because a
native `job_create` adds a directory behind the VFS's back. That also forbids
mounts inside the tree, which Linux forbids too. `mkdir` and `rmdir` arrive
through the generic `Namespace::mkdir`/`rmdir`, and `chown` through
`set_attributes`, so the VFS needs no new paths.

The files in every cgroup:

| File | Read | Write |
|---|---|---|
| `cgroup.procs` | the pids of the members' thread-group leaders | a pid: move that process (§3.1) |
| `cgroup.threads` | the tids | refused, `EOPNOTSUPP` (no threaded mode) |
| `cgroup.type` | `domain` | `threaded` is `EOPNOTSUPP`, anything else `EINVAL`: Linux takes `threaded` alone, not even `domain` |
| `cgroup.events` | `populated 0/1`, `frozen 0/1`; pollable with `POLLPRI` (§4) | refused |
| `cgroup.kill` | refused | `1`: §2.4 |
| `cgroup.freeze` | `0/1` | `0/1`, from landing F1; until then `frozen` reads 0 and a write is `EOPNOTSUPP` |
| `cgroup.controllers` | what the parent's `subtree_control` enables here | refused |
| `cgroup.subtree_control` | what this cgroup enables for its children | `+name -name …`, split on single spaces; a later token for a controller overrides an earlier one, and a name not built is `EINVAL` |
| `cgroup.max.depth`, `cgroup.max.descendants` | `max` or a count | a limit on `mkdir` beneath |
| `cgroup.stat` | `nr_descendants`, `nr_dying_descendants 0` | refused |

The root has `cgroup.procs`, `cgroup.subtree_control` and the others, but
no controller limit files, as on Linux.

### 3.1 Moving a process, and delegation

A write of a pid to `cgroup.procs` moves the whole process, all its threads,
by §2.5's order. It is allowed when the writer is privileged, or when it
holds write permission on the target's `cgroup.procs` and on the
`cgroup.procs` of the common ancestor of the source and the target, and the
process's effective uid matches its own. Those are Linux's delegation rules,
and with them `chown` of a directory hands a subtree to a user (C7). A move
into a job that is sealed or being removed fails `ENOENT`.

**No internal processes.** A cgroup that enables a controller in
`subtree_control` may not hold processes, except the root. Such a write
fails `EBUSY` while it has members, and a move into such a cgroup fails
`EBUSY`. This is the rule that makes controllers' arithmetic well defined,
and it is why `docs/INIT.md`'s init moves itself into `init.scope` first.

### 3.2 `clone3` into a cgroup

`CLONE_INTO_CGROUP` stops answering `ENOSYS` (`family.rs`). The `cgroup`
field is a descriptor of a cgroupfs directory, and the child joins that job
in place of its parent's, before publication. The checks are §3.1's, with the
parent as the writer. It is init's race-free start (`docs/INIT.md` §5.2).

## 4. `POLLPRI`: the change notification

`cgroup.events` changes are announced as Linux announces them: the file polls
`POLLPRI` (and `EPOLLPRI`) once after every change, and `select` reports it
in the exception set. Nothing here polls priority yet, so:

* `Readiness` (`libs/vfs/src/node.rs`) gains `priority`;
* `poll`, `ppoll`, `select` and `epoll` map it to `POLLPRI`/`EPOLLPRI` and
  to the exception set. The constants already exist in `libs/linux-abi`;
* a job gains an `events: WaitQueue`, woken on every populated or frozen
  flip, and `cgroup.events` answers `poll_queues` with it and
  `poll_changes` with its `wakes()` count, as eventfd does.

Linux's semantics are edge-triggered in effect: `POLLPRI` is asserted from a
change until the file is read again. A `cgroup.events` open records the
`wakes()` count it last rendered, and reports `priority` while the count has
moved since. `memory.events` and `pids.events` use the same mechanism.

## 5. The native side: `EMPTY`, and a job for a cgroup

`Signals` gains `EMPTY = 1 << 4`, and `ALL` widens to `0x1F`. A job asserts
`EMPTY` while it is unpopulated and clears it when it is populated again.
Unlike `TERMINATED`, it is a level, not a latch. So `object_wait_async` on a
job for `EMPTY` fires once the job is empty, which is what a native init
watches in place of `cgroup.events` (`docs/INIT.md` §9).

`job_for_cgroup(dirfd, rights) -> handle` (0x102A) returns a handle to the
job behind a cgroupfs directory, with at most the rights the caller's access
to that directory's `cgroup.procs` allows: `MANAGE` and `WAIT` for write,
`WAIT` for read. It is the one bridge from a path to a handle, and it goes
one way only. There is no call that names a job's path from a handle,
because a handle is a capability and a path is not.

## 6. Where each thing is charged

The controllers need to know, at a choke point, which job pays. The survey
of 2026-09-23 found these:

| Controller | Charged where | Uncharged where |
|---|---|---|
| `pids` (tasks, as Linux counts) | `clone_with` and `process_create` for a process, `clone_thread` for a thread, before the pid is allocated | `Drop for Process` and `release_thread` |
| `memory` | `mm::allocate_frames` callers on the user path: `commit_page` (anonymous and file faults), the copy-on-write copies, the fork copies of held pages, `write_page`/`hold`, and the page-cache fill in `fs/pages.rs` | the frame's free, from the owner recorded at charge |
| `cpu` | the scheduler's entity for the job (§7, S1) | — |
| `io` | a block request `Part` carries the job that submitted it (`libs/block/src/schedule.rs` already names this as stage 13's place) | — |

**Memory has no owner today.** A VMO carries no charge, and `AddressSpace::
resident_pages` is computed on demand. So landing M1 gives each committed
page a charge to one job. Anonymous memory is charged to the job of the
process that faulted it in. A page-cache page is charged to the first job
that brought it in, and stays charged there, which is Linux's
first-touch rule and its known imprecision. The charge is recorded in
`PageInfo`, which already carries a refcount and the owning VMO
(`ARCHITECTURE.md` §4), so an uncharge at free needs no lookup.

A charge that would exceed `memory.max` first tries reclaim inside the job
(landing M2), and then **OOM-kills inside the job**. The victim is the
member with the most charged memory in the subtree, killed as `SIGKILL`.
`memory.events` counts `max`, `oom` and `oom_kill`. The fault that could not
be charged is retried after the kill, and gets `SIGBUS` if the job is empty
and still over. That is the scoped OOM kill of stage 13's exit, and the one
`docs/INIT.md` §5.5 shows as `oom-kill`.

## 7. The landings

In story points, each landing gated by what it names. The kernel's in-boot
checks are the pattern `kernel/src/syscall/check.rs` sets; a user-level
check runs in `test-shell` with zinc and uutils. The G landings are what
`docs/INIT.md` needs before init boots.

| | Landing | Gives | Gate | Points |
|---|---|---|---|---|
| G1 | Every process in one job: root job, `membership`, fork inherits, `live`/`busy_children` counts, names and ids, the `cgroup.kill` kill beside `job_kill`, `drivers.slice` | C2's inheritance, C5's mechanism | boot checks: a fork's child in its parent's job; populated flips at the last exit, not the reap; a killed forking loop ends | 8 |
| G2 | `libs/cgroupfs` and cgroupfs: mount, `mkdir`/`rmdir`, `cgroup.procs` read and move, `cgroup.kill`, `cgroup.events` (without `POLLPRI`), `subtree_control` with no controllers yet, `/proc/<pid>/cgroup` | C1, C2, C5 | host tests, Miri and the `cgroupfs_write` fuzzer; the `cgroups` boot check, which drives cgroupfs through the VFS as a program's calls would (mount, `mkdir`, a move by pid, `/proc/<pid>/cgroup`, `cgroup.events`, `cgroup.kill`, the limits, nine refusals), and its negative control, a `cgroup.kill` that does not kill. The user-level run moved to G4, whose delegation needs a user anyway | 8 |
| G3 | `POLLPRI` through `Readiness`, `poll`, `select`, `epoll`; `cgroup.events` pollable | C4 | boot check: an `epoll` on `cgroup.events` wakes at the last exit and not before | 3 |
| G4 | `CLONE_INTO_CGROUP`; delegation by ownership; the no-internal-process rule | C3, C7 | `test-shell` as a non-root user in a chowned subtree; a refused move outside it | 5 |
| G5 | `EMPTY`; `job_for_cgroup`; native jobs as `job-<id>`; `devmgr`'s jobs under `drivers.slice` | C8 | boot check: a native wait for `EMPTY` fires with the populated flip; `test-restart` still passes | 3 |
| P1 | `pids`: `pids.max`, `pids.current`, `pids.events` | C6 | a fork bomb in a `pids.max 16` cgroup fails `EAGAIN` at 16 | 3 |
| M1 | `memory` charging: `memory.current`, `memory.max`, `memory.events`, `memory.stat` (anon, file); the scoped OOM kill | C6 | a process past `memory.max` in one cgroup killed, a sibling untouched | 13 |
| M2 | Reclaim: clean page-cache pages, scoped to a job and global, and `memory.high`, which reclaims above it | stage 13's reclaim | a `memory.high` job's file pages evicted and read back identical | 13 |
| F1 | `cgroup.freeze`, over the stopped state processes already have | C4's `frozen` | a frozen job's members stop and resume | 3 |
| S1 | `cpu.weight`: a group entity per job in `libs/sched`'s EEVDF, hierarchical | C6 | host tests of shares; a boot check of two busy cgroups at 1:3 weights within 10% | 13 |
| S2 | `cpu.max`: bandwidth per period, throttling a group's entity | C6 | a `cpu.max 20000 100000` job held near 20% | 8 |
| B1 | `io.weight` in `libs/block`'s scheduler | C6 | host tests of the dispatch shares | 5 |

G1 to G5 are **27 points**, and they are all `docs/INIT.md`'s first boot
waits on. Its landings L1 and L2 are host-only and can run beside them. The
controllers are **58 points** more, and `memory` and `pids` come first. The
roadmap guessed "a month, ≈ 60" for all of stage 13 before points existed.
Its cgroup half alone is 85 by this count, and the roadmap is corrected to
say so.

Order: G1, G2, G3, G4 (init's C1 to C5), then G5 and P1, then M1 and M2 (the
stage exit's memory limit), then F1, S1, S2 and B1. Namespaces and seccomp
follow, as `docs/BACKLOG.md` decided.

## 8. Risks

* **The membership lock on the fork path.** Every fork now takes the
  parent's membership lock and one job lock. Both are short, and neither is
  held across anything that sleeps. G1's gate includes the fork-heavy
  `test-vfs` and `test-rustc` runs, to show no measurable cost.
* **Memory charging touches every user frame.** M1 changes seven allocation
  sites and the free path. The negative control that shows it fired is a
  frame charged to no job, which the charge check must catch by name.
* **`POLLPRI` is new to every poll path.** G3 adds it as a field that is
  false everywhere except `cgroup.events`, so a mistake shows only there.
* **Group scheduling (S1)** is the largest change to `libs/sched` since
  EEVDF. It is last on purpose: init, and the stage's exit, need none of it.

## 9. What the customer decides

1. **The order in §7.** Draft: G1 to G4, then G5 and P1, then memory, then
   the rest.
2. **`drivers.slice`** as the name of `devmgr`'s job in the tree. Draft: as
   written, matching `docs/INIT.md`.
3. **Whether S1 and S2 (cpu) are in stage 13 at all.** Draft: yes, last. The
   stage names the `cpu` controller, but neither init nor the stage's exit
   needs it.
