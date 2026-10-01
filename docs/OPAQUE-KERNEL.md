# An opaque kernel: services as supervised userspace servers

**Shelved (customer, 2026-09-27): off the table for now.** S0 measured the
seam, and the customer read the numbers and set the plan aside. S1 and
everything after it are not started. The 2026-09-16 decision stands:
monolithic core, device drivers in ring 3. What would bring the plan back is
a cheaper trip to ring 3 (*The verdict of S0*, below).

**2026-09-30: the trip is being cut** (§ 8). The customer chose a direction
for Ferrix, Linux software beside safety functions on a kernel that can be
assured. Getting the btrfs parser out of ring 0 is one step of that, and it
needs the trip cheap first. The trace now says where a trip's time goes,
and the fixes are landing one at a time.

Drafted by ferrix-55b on 2026-09-27. The certification consultant reviewed
§2 and §5, and the init owner reviewed §3. Both decisions are recorded in
`docs/BACKLOG.md`, Decisions, 2026-09-27.

## The verdict of S0

Worth it for filesystems on paper, not yet for the network stack, and not
before the trip to ring 3 is made cheap. That trip is what today's Ferrix
already pays for every cold disk read.

**How often a build crosses to ring 3.** A warm `rustc` compile made 4,345
system calls and crossed once. A cold run crossed 2,986 times while the page
cache filled (§ *S0's first result*).

**What one crossing costs.** A 4 KiB read through the block ring and the
ring-3 driver took 300 to 844 us on x86-64 under KVM, over four back-to-back
pairs at host loads of 20 to 28. Stock Linux, on the same QEMU machine with
its driver in ring 0, took 27 to 48 us: 10 to 20 times less. On AArch64 under
TCG the figures were 453 us and 145 us. With 32 reads in flight the 99th
percentile reaches 150 to 230 ms (§ *S0's second result*).

**What that costs a whole workload.** No Ferrix with in-kernel drivers exists
(the 2026-09-13 decision forbids building one), so stock Linux stands in for
one. Each trip costs Ferrix 252 to 817 us more. Multiplied by the trips each
run made (x86-64, KVM; the `test-rustc` gate on 2026-09-27):

| Workload | Time on today's Ferrix | Trips to ring 3 | Lost to the seam vs in-kernel drivers | Option B would add |
| --- | --- | --- | --- | --- |
| Cold: `rustc -vV`, `cargo -V`, rustc and gcc compile and run | 4.35 s | 2,982 | 0.75 to 2.4 s (17 to 56%) | about 0.9 to 2.5 s more (a server trip per fill) |
| Warm: the second `rustc hello.rs` | 0.14 s | 1 | under 1 ms | under 1 ms |

These are estimates. The per-trip cost was measured on 4 KiB reads one at a
time, while the cold run's trips carry about 15 pages each, and some overlap.
A direct measure would sum the time every request waits in the ring during
the run.

**Why the cost looks like the implementation, not the design.** Kernels that
run drivers in user mode, such as seL4 and Fuchsia, pass a request between
processes in about a microsecond; Ferrix's trip costs hundreds. A median of 2
to 4 ms with a 99th percentile of 150 to 230 ms at depth 32 is the shape of
missed wake-ups rescued by timers, not of crossing into ring 3. Known costs
sit on the path:
- the block ring task's 50 ms recheck timer;
- no PCIDs, so every switch between the kernel's work and the driver
  flushes the TLB;
- the data copied between the kernel and the driver.

QEMU makes each doorbell, interrupt and wake-up an exit to the host, which
Linux's in-kernel path takes fewer of. Real hardware would narrow the gap,
but that is not measured.

**What would reopen the plan.** Cut the trip, then measure again:
1. Fix the depth-32 stall. Done 2026-09-27: the elevator starved reads
   behind its head for up to 500 ms; ring disks now use a 25 ms read
   expiry, and the p99 fell to 28–35 ms.
2. Find where the 250 to 800 us go; the driver's own `device_ticks` already
   splits off the device's share. Done 2026-09-30: the `seam-trip` and
   `seam-count` lines, and what they found in § 8.
3. Add PCIDs. Re-costed 2026-09-30 (§ 8): tagging saves microseconds per
   trip under KVM and nothing under TCG. Lazy TLB for kernel threads comes
   first, and wake placement matters more.
4. Remeasure with `cargo xtask bench-seam` and the `seam` boot line, with a
   target such as within twice Linux's per trip.
5. Add the direct measure of a cold `rustc` run.

That work pays off whatever is decided: every cold read on today's Ferrix
pays the trip. If the trip cannot be brought near the target, today's ring-3
disk driver deserves a harder look too. BACKLOG has a row for each: the stall,
and cutting the trip.

## 0. The decision this would change

On 2026-09-16 the customer reaffirmed `docs/ARCHITECTURE.md` §1: *a monolithic
core, capability seams, device drivers in ring 3* (`docs/BACKLOG.md`,
Decisions). The seam sits at devices because `rustc`'s calls are `open`, `stat`,
`read` and `mmap` on files the page cache holds, and those stay function calls.
"What the shape gives up is restarting a kernel subsystem, which the goal does
not need." The decision was to be closed by evidence: the two "seam measured"
rows in P2, which were measured on 2026-09-27 (*The verdict of S0*).

The door was kept open. C8 (2026-09-23) builds every cgroup over a `Job` "so a
future microkernel keeps working". init treats the kernel's subsystems as
`vfs.builtin`, `net.builtin`, `block.builtin`, which become `*.service`
aliases the day they are servers (INIT.md §7). L12, which ferrix-15 is building
now, makes init start devmgr, and is the first step of that on-ramp.

**What has changed since 09-16:**
- The customer now wants live kernel update and restartable parts of the
  system: the hot-patch plan (kept outside the repository, in `~/.local/share/ferrix/live-update/PLAN.md` on the build host), and
  T0, landed 2026-09-27. That is exactly the "restarting a subsystem" the
  decision gave up.
- T0 showed the cost of keeping services in ring 0. Every driver kind needed
  its own in-kernel park and take-up code (net, input, block), because the
  state that must survive a driver's death is kernel state.
- ~93,700 lines of uncertified code (the `load` ring: fs, net, block, the device
  cores, the Linux personality) run in ring 0. A bug there is a kernel
  compromise, and `ITEM.md` has to argue the load ring around the item.

### S0's first result (2026-09-27): the seam measured, 2

A warm `rustc` compile made 4,345 system calls and crossed to ring 3 once,
for a single page, while the page cache served 16,566 pages. A cold run
crossed about once per three calls while the cache filled (stage 11's
roadmap section has the table). For this plan that means the page cache, and
the VFS caches in front of it, carry the compiler. Option B must keep them in
the kernel, as it does. A filesystem server in B sees cold fills and
metadata writes, never the warm path. Row 1, the cost of one crossing, is
next.

### S0's second result (2026-09-27): the seam measured, 1

One crossing, a 4 KiB disk read through the ring and its ring-3 driver, took
300 to 844 us on x86-64 under KVM over four runs. Stock Linux, on the same
QEMU machine with its driver in ring 0, took 27 to 48 us: ten to twenty times
less. On AArch64 under TCG the figures were 453 us and 145 us. At depth 32 a stall reaches 150 to 230 ms at the 99th percentile
(a BACKLOG row). What that means for B:

- For files, B is sound as drawn. The page cache takes the warm path, and a
  server pays the crossing only on cold fills, as the drivers pay it today.
- For the network stack (S3), B's cost is different in kind. A socket's
  send and receive would cross to a server on every call, not once per
  cache fill. At today's hop cost, each is hundreds of microseconds. S3
  would need the hop itself cut first: batching, the stall fixed, and
  PCIDs so that a switch stops flushing the TLB. It should be re-measured
  before it is planned.
- The stall is a bug whatever is decided, and it costs the drivers already
  in ring 3 today.

### Where a trip's time goes: the `seam-trip` and `seam-count` lines

Since 2026-09-30 the boot check traces its 1,024 depth-1 reads
(`sched::trip`) and prints two lines under `seam`. `seam-trip` gives each
hop's p50/p99 in microseconds, in the order a read passes them: `issue`,
`queued` (before the ring task is nudged), `ring` (the ring task running),
`on-ring`, `bell` (the driver rung), `drv-bell` (its `port_wait` took the
bell), `irq` (the device's interrupt queued), `drv-irq`, `posted` (the driver
rang the ring's port), `ring-again`, `answered` (the bytes copied out),
`reader` (the reader running) and `done`. Then the whole trip's p50/p99 and
what the hops' medians add up to: near 100% means the hops account for the
trip. Then, for each of the five wakes, how many trips woke the task on
another processor than the waker's. Last, how often a hop was not taken: a
ring task or driver that was already awake is not woken, and that hop reads
as zero. `seam-count` divides what the run did by 1,024: switches, switch
barriers (IBPB), user roots written and taken off, IPIs sent, device
interrupts, and the sleeps on the reader's queue and the two ports, ended by
a wake or by the recheck timer. A trip spent in recheck sleeps, rather than
in wakes, is a missed wake-up. On AArch64 the driver now reads `CNTVCT_EL0`,
so the `seam` line's device share is measured there too.

## 1. What "opaque" means here

The kernel keeps what only a kernel can do:
- the scheduler, VM, objects and handles, channels, ports, VMOs, jobs;
- interrupts, the IOMMU, SMP;
- the Linux personality's process, signal, futex, time and memory calls;
- a forwarding layer.

Services move out as ordinary processes, supervised and restartable, and the
kernel stops knowing what kind of service a server is.

There are three shapes, from least to most change:

| | Stays in the kernel | Moves to servers | `rustc`'s cached `read` | Restartable |
|---|---|---|---|---|
| **A. Today + a generic rebind contract** | everything | nothing | function call | drivers only |
| **B. Servers behind the page cache** (recommended) | VFS, page cache, fd table, pipes, epoll, eventfd, procfs/sysfs/cgroupfs | the net stack, filesystem implementations (btrfs, later others), the device cores (input, display, render, audio), the terminal | **function call** (a cache hit never leaves the kernel) | net, fs, device services, drivers |
| **C. Fully opaque (starnix-like)** | the fd table and forwarding only | VFS, page cache, every filesystem, sockets, everything above | IPC round trip | everything but the kernel |

**Recommendation: B, staged, gated by measurement.** B keeps the 09-16
decision's reason (the compiler's hot path stays function calls) and drops its
cost (nothing but the page cache and VFS glue stays in ring 0). C inverts §2,
where Linux is the native ABI, into an emulation layer. It is worth doing only
if the seam measurements come out far better than a TLB-flushing switch
suggests, and it can be decided after B.

Why procfs, sysfs and cgroupfs stay in B: they are views of kernel state, and
`docs/SYSFS.md` already argues that a served `/sys` puts an IPC round trip and
a hung server under every `stat`.

## 2. Kernel mechanisms B needs

Each is useful on its own, and each is a landing.

1. **Measure the seam first (P2 "seam measured" rows 1 and 2).** Measure the
   ring-3 hop cost at queue depths 1 and 32, and the ratio of a syscall to a
   ring crossing, on all three arches and under KVM. Today's only figures are
   open+close ≈ 4.1 µs and a fault 848 ns. Without PCIDs/ASIDs (a P2 row), every
   switch flushes user TLB entries, so this row is also a candidate
   prerequisite. **This is the decision gate for everything after it.**
2. **A served inode.** A kernel `Inode`/`FileSystem` whose operations go to a
   server over the one proven pattern: a request ring in a VMO, a doorbell
   port, and the kernel as the client, as the block ring does. A page-cache
   miss fills from the server as it fills from a disk today (`fs/pages.rs`).
   The fd layer needs no change, because every fd is already a `dyn Inode`.
3. **The three seams that bypass `Inode`:**
   - `Inode::ioctl` (already a BACKLOG row) replaces the type ladder in
     `sys_ioctl`;
   - a socket trait replaces `sockets.rs`'s closed `enum Any`;
   - `mmap` of a served object takes a VMO the server hands over, which
     `memory.rs` can already map.
4. **Kernel-attested caller identity (a new item interface).** A forwarded
   request carries the caller's job, pid and credentials written by the kernel,
   never taken from the message body. Validated and unforgeable, with its own
   checks, like the handover record in the live-update plan. The servers' own
   protocols stay outside the item, as devmgr's do. (Certification's one new
   item interface.)
5. **Charging that follows the client (T.EXHAUST, F-35/F-37).** Memory a server
   holds for a client must be charged to the client's job. Otherwise one client
   exhausts a shared server, and F-37 reopens by another route. Two ways:
   client-supplied VMOs for request memory and buffers, and a kernel
   "charge-to-requester" on the attested identity of point 4. The second is
   **bounded**: a server charges only through a token tied to one outstanding
   forwarded request. The kernel issues the token with the attested identity,
   it is valid until the reply, and the charge is undone when the object it
   paid for goes. A server can never name a job to charge, or a compromised
   server could exhaust any victim by the other door. **This is in the design
   from the start, not retrofitted** (certification's main condition).
6. **One generic rebind contract.** T0's park/take-up, generalised: a service
   whose server dies has its kernel-side state parked (the page cache keeps its
   pages; the socket layer keeps sockets without a stack), its requests
   requeued, and the next server instance takes them up. That is T0's pattern
   with one implementation instead of one per kind, and it is what makes a
   server upgrade a restart rather than a hot patch. Parked state stays
   charged to the jobs of the clients it was held for, as F-37 charges kernel
   heap today, so parking never becomes an uncharged pool.
7. **Item reads through the fs become registered hooks.** Today the item reads
   through the fs in three places: `root_disk::process_context`,
   `fs::read_file_beneath` for firmware, and devmgr's image reader. The
   boundary gate refuses an item-to-load call unless it is a registered hook
   (the F-04/F-07/F-08 pattern).

## 3. Supervision and boot (agreed with ferrix-15)

- **L12 lands first and unchanged:** the kernel starts only init, and init
  starts devmgr. This plan adds successor landings to INIT.md §7.3 rather than
  changing L12. L12's parts stay: the opt-in `ferrix.devmgr=init`, init
  starting nothing but `devmgr.service` before ROOT, and pid 1 re-rooted once.
- **Servers are `Type=native` units** in a system slice, and `net.builtin`
  becomes `net.service` with `Alias=net.builtin` (INIT.md's own scheme).
  devmgr keeps the drivers.
- **Authority never passes through init.** A server that needs a device or a
  block ring gets it from the kernel or devmgr, as L12's starter-capability
  rule already requires for devmgr.
- **The fs server starts from initramfs memory** before any mount it depends
  on (DEVMGR.md §5's rule, applied to the fs). Init needs nothing from it to
  start: its binary and units come from the initramfs, `/run` is tmpfs, and
  cgroupfs and `/proc/cmdline` are kernel-made.
- **The root switch** stays the kernel's under L12. Moving btrfs out makes
  "switch" the fs server's job, and the kernel's re-root of pid 1 must be
  re-argued then (ferrix-15).

## 4. Stages

The points are rough, in the backlog's units, and each stage gates the next.

| Stage | What | Points | Gate |
|---|---|---|---|
| S0 | Measure the seam (P2 rows 1 and 2), and PCIDs/ASIDs if the numbers need them | 5 (+8) | **the customer decides B or not on these numbers** |
| S1 | In-kernel refactors, useful whatever S0 says: `Inode::ioctl`, a socket trait, the generic rebind contract, and item fs reads as hooks | 12 | the current row; T0's `test-restart --boot all` on the generic contract |
| S2 | Kernel-attested identity and client-following charging (item interface; certification review) | 10 | new item checks; F-37's charging tests rerun through a server |
| S3 | **The net stack as a server** (TCP/IP, netlink and packet sockets). The kernel keeps socket inodes that forward over a per-socket ring. Restartable: sockets park | 30 | `test-net --arch all`, Chrome's network bench, a kill-the-stack row |
| S4 | **The device cores as servers**: input, display, render, audio, and the terminal/pty. `/dev` nodes become served inodes | 25 | the compositor, audio, jobs and pty gates, kill rows |
| S5 | **btrfs as a server behind the page cache**, and the root switch through it | 35 | `test-btrfs`, `test-powerfail`, the self-host build time, a kill-the-fs row with `btrfs check` |
| (S6) | Option C: VFS and page cache out | not planned | only if S0 and S3 to S5's numbers argue for it |

That is about 117 points for B after S0, about as large as stages 17 to 19 were.

## 5. What it costs

- **Performance.**
  - Unknown until S0. Cached file I/O is unaffected in B.
  - Every socket operation and every page-cache miss gains a server hop. So
    do device-node ioctls, which matter for the compositor (a GPU submit is
    ≈ 0.95 ms today, dominated by the host).
  - Chrome makes ≈ 8,000 syscalls/s after the vDSO; its network path would be
    the one to watch.
- **Boot checks.** The stage 8 to 12 in-kernel checks that exercise fs, net and
  block in ring 0 are how the reference boot proves those paths today. They
  need server-side equivalents in the gates (certification). Several dozen
  checks move or get rewritten.
- **Certification.**
  - The item hardly shrinks, because these are `load` code already. The gain
    is that they leave ring 0, so isolation holds against them by
    construction.
  - New: the identity interface (item); an A./OE. pair naming the servers as
    environment (the OE.AUTH precedent), because file permissions
    (FDP_ACC) are enforced by a server; T.EXHAUST re-argued.
  - **A build switch, `--servers off|on`.** `off`, the in-kernel build, stays
    the certified reference configuration until the servers and forwarding
    have their own gates. The item's code is identical under both, so its
    evidence covers both, and ITEM.md §5 says so.
- **Effort and focus.** ≈ 117 points after S0, competing with the desktop,
  Steam and the certification work.

## 6. What it gives

- **Restart instead of hot patch.** The net stack, filesystems and device
  services restart and upgrade like drivers do after T0. The live-update plan's
  T1 (kexec handover) shrinks to the small kernel, and its state record
  shrinks with it: servers keep their own state across their restart.
- **Ring 0 shrinks** from ≈ 115,000 lines without checks toward the item plus
  the forwarding and page-cache glue. A bug in TCP or btrfs stops being a
  kernel compromise.
- **One mechanism.** One rebind contract, one forwarding path and one charging
  rule, instead of per-kind kernel code.

## 7. Decisions for the customer

**Answered 2026-09-27: the plan is shelved.** None of the four below is
taken up; they stand as they were asked, for whoever reopens the plan.

1. **Reopen the 09-16 decision for B, subject to S0's numbers?** If yes, S0 is
   the next landing, and nothing past S1 starts before its numbers are read
   together.
2. **Order after S0:** net stack first (S3, the most self-contained and easiest
   to restart), then device cores, then btrfs. Or btrfs first, because it has
   the largest ring-0 code and the most to gain from restart?
3. **S1 regardless?** The ioctl and socket seams and the generic rebind
   contract pay off even if B is never built.
4. **Option C** stays unplanned unless measurements argue for it.

## 8. Cutting the trip (2026-09-30, os-35)

**Why now.** The customer chose a direction for Ferrix: unmodified Linux
software running as a non-safety partition beside safety functions, on a
kernel item that can be assured. The certification consultant (os-9f)
proposed the route, with IEC 61508 SIL 2 as the first target; the claim's
exact wording and that standard are still the customer's to confirm
(`docs/certification/CLAIM.md`, marked PROPOSED). Its
freedom-from-interference argument is easier the less of the Linux layer
runs in ring 0, and the cheapest large piece to move out is btrfs's parser
for untrusted disk images (about 25,000 lines). A filesystem server pays
the trip on every cold fill, so the trip has to be cheap first. The
consultant's recommendation for SIL 2 is software compartments in ring 0
first, then this trip, then btrfs as a server. For SIL 3 it is either the
full opaque kernel, which this makes affordable, or a separation kernel
with a real Linux guest; that choice is the customer's.

**What the trace found.** Four read-only studies of the path, then the
`seam-trip` and `seam-count` lines (§ *Where a trip's time goes*). The
baseline on x86-64 under KVM at two processors, host load 31 to 42:

- A depth-1 trip is p50 230 us and p99 417 us. The hops' medians add up to
  98% of it, so the trace accounts for the trip.
- Per read: 13 switches, 2.4 user roots written and 2.4 taken off, 3.2
  IPIs, 0.2 switch barriers (IBPB) and one device interrupt.
- No sleep was ended by the recheck timer. The missed-wake-up theory of
  *The verdict of S0* is wrong at depth 1: the port packets persist and the
  want-bell handshake is correct. The recheck is 5 ms in practice, not
  50 ms, because `wait_until_deadline` sleeps in 5 ms slices.
- 60 to 85% of the wakes put the woken task on another processor than its
  waker's. `sched::wake` never moves a task, so each such wake is an IPI to
  a virtual processor that is probably halted, and on a loaded host each
  costs tens of microseconds.
- The largest single hop is the bell to the device's interrupt (the device
  and QEMU): 56 us at p50.

A trip makes four or five hops (reader, ring task, driver, ring task,
reader). `dispatch()` wakes every reader on every pass, so a reader wakes
about three times per read and twice for nothing. Every interrupt masks
and unmasks its MSI-X entry, two exits to QEMU. The data is copied four
times, the first byte by byte under the disk's lock.

**The plan, in order.** Points are guesses.

| Step | What | Points | State |
|---|---|---|---|
| 1 | Trace a trip: `sched::trip`, `seam-trip`, `seam-count`; AArch64's driver reads `CNTVCT_EL0` | 8 | **landed** 5cc5ed38 |
| 2 | The ring: wake a reader only when its answer is there, no nudge when the ring task is awake, one word-wise copy, the shared indices read whole (F-45), a driver that rewrites a posted completion checked | 3 (6 spent) | **landed** b7cab053 |
| 3 | Lazy TLB: a kernel thread keeps the last program's space loaded, where the processor has SMAP or PAN | 3–5 (7 spent) | gated; WIP until renumbered and re-gated |
| 4 | Take the ring task off the data path: the reader publishes, the driver's `port_queue` completes inline | 5–8 (7 spent) | written, WIP on `os-35/ipc-ring` (7c52861e) |
| 5 | Interrupts: no MSI-X mask per delivery, with a stated storm bound (L.object.41 rewritten) | 2 (2.5 spent) | **landed** 1dcc433f |
| 6 | A sync wake onto the waker's processor, within its affinity and quota | 3–5 (4 spent) | written, WIP |
| 7 | A bounded poll before the idle halt; targeted IPIs on the GIC | 2 (3 spent) | written, WIP |
| 8 | Read into page-cache frames, one kernel copy | 3–5 | not started |
| 9 | A direct hand-off call (`port_queue_wait`) | 8–13 | not started; high risk |
| 10 | PCIDs and ASIDs | ~12 | not started; small gain |
| 11 | DMA into the page cache | 13–21 | blocked: domains are untranslated |

Steps 2 to 7 are the ones expected to bring a trip near twice Linux's. Each
branch measures itself against step 1's lines, back to back on the same
host load, and goes to the certification consultant before it lands.

**What the consultant requires of these branches.**
- Traceability entries for new functions, a changed requirement for changed
  behaviour, carry-coverage after the last rebase, and a negative control
  shown firing for every new check.
- The ring: every value read from memory the driver can write is read once,
  validated, then used; inline completion is bounded per call and charged
  to the driver's job.
- Lazy TLB: tables are freed only once no processor has the space loaded,
  eagerly or lazily (FX-0009), and the switch barrier keys on the last
  program's space, so user A, a kernel thread, then user B still gets it.
  On a processor without SMAP or PAN (ARMv7-A, a Cortex-A72, an x86-64
  without SMAP) a stray kernel pointer would read the last program's
  memory where it used to fault, so those processors stay eager.
- The IBPB policy stays as it is. `--mitigations off` moved p50 by 0 to
  25 us, an upper bound for every defence together; IBPB fires on 0.2 to
  1 read in 1. Linux's conditional mode would take that to zero, but it
  would narrow SPECULATION.md §3's claim, and it is the customer's and the
  consultant's decision, not proposed.

**Where the branches stand (2026-10-01 wind-down).**

- **The measurement** (step 1), landed as 5cc5ed38 without the consultant's
  review. **After-the-fact review (os-ad, 2026-10-01): OK with conditions.**
  It adds no `unsafe`, no `cfg` or feature, no native call and no upward
  reference. Unarmed, a stamp point costs one relaxed load; armed, it takes no
  lock, allocates nothing and fills at most 12 slots, and its numbers reach
  only the console, so V-06 does not move. Owed, and accepted by os-35 for a
  commit of its own on top of ring part B: (C1) `sched/trip.rs`'s 20 functions
  are classed as check code, yet the hop check arms them on every default
  boot, so an L.* requirement that the instrumentation is inert outside the
  hop check, a check that `TRACING` is false after it and at the boot marker,
  and a negative control (no `disarm`) stopping the boot on the check's own
  message; (C2) `sched/trip.rs` was never measured
  (`docs/certification/TODO.md` §0.2 now lists it); (C3) the traceability
  text of `sched::trip::count` says it counts switches, which no `Count`
  does.
- **The ring, part A** (step 2), landed as b7cab053. The consultant's
  review asked that the ring task's 50 ms recheck stay as the liveness
  backstop, and it does. The landing's KVM boot counted 0.17 sleeps per
  read ended on the 5 ms wait slice rather than a wake. No nudge was lost
  (FX-1005 did not fire), so these are reads slower than 5 ms on a loaded
  host; watch the count. Against 5cc5ed38, back to back at nazuna loads of
  41 to 51:
  - KVM at four processors: 12.7 switches per read became 10.1; depth 32
    went from a mean of 10.3 ms to 2.1 ms; the depth-1 p99 went from 3.3 ms
    to 1.0 ms.
  - AArch64 under TCG: the depth-1 p50 went from 954 us to 314 us.
  - A reader now sleeps once per read, not about three times.
- **The ring, part B** (step 4), WIP 7c52861e on part A. The ring belongs
  to the disk: a caller dispatches its own request and rings the driver.
  The completion port has a server (`Port::new_served`, L.object.106), so
  the driver's own `port_queue` takes the completions and wakes the
  readers. A trip is reader, driver, reader.
  - What it holds to: at most 64 completions per call, work charged to
    the driver's thread, nothing allocated under the lock, and corruption
    handed to the task.
  - Both negative controls fired, and it passed the full row on 5cc5ed38.
  - Measured against 5cc5ed38:
    - KVM at four processors: 4.95 switches and 0 IPIs per read.
    - AArch64 under TCG: the p50 went from 954 us to 211 us.
  - Left: re-gate on current main, the consultant's review, land.
- **Lazy TLB** (step 3), reworked to the consultant's three conditions.
  - The kernel is lazy only where the processor refuses ring 0 a user page:
    SMAP on x86-64, PAN on AArch64. It stays eager on ARMv7-A and wherever
    the backstop is missing, and the `lazy` boot line names the mode.
  - FX-0010 stops a drop that cannot wait, in every build.
  - Gated green at 065e3cb3. User roots written per read went from about
    2.5 installs and 2.5 uninstalls to 0.2 and 0 on lazy processors. The
    p50 moved by no more than the host's noise.
  - QEMU's default AArch64 processor has no PAN, so AArch64 ran eager.
  - Left, on `os-35/ipc-lazytlb-on-ef206bb2`: renumbered past F-55; the
    generated evidence, the full row and the consultant's second look.
- **Interrupts** (step 5), landed as 1dcc433f.
  - An edge MSI-X vector is no longer masked per delivery. The bound is 64
    deliveries per acknowledgement, and L.object.41 is rewritten.
  - At the consultant's word, MEMORY-AND-TIMING.md §2.2 now states who pays
    for a storm: at most 64 handler runs per scheduling of the holder, each
    charged to the task the interrupt cut.
  - On x86-64 under KVM at two processors, eight alternating boots each:
    the depth-1 p50 went from 291 us to 206 us, and the driver's
    submit-to-drain from 204 us to 83 us (medians).
- **Sync wake and idle poll** (steps 6 and 7), WIP on `os-35/ipc-wake`.
  - When reader, ring task and driver meet on one processor, cross-processor
    wakes fall from 60–85% to 1–4%, and IPIs to about 0.1 per read. Until
    the ring's spurious `wake_all` is gone, other boots miss that.
  - The work found a lost wake on AArch64 with a GICv3; it has a BACKLOG
    row.
  - What is left is in `docs/BACKLOG.md`, *Branches that still hold
    unlanded work*.

Hazards the ring work found:
- The new `ferrix-driver` ring (`src/user/system/native/driver/src/block.rs`)
  still reads and writes the shared indices a byte at a time. That is
  F-45 on the driver's side, and it has a BACKLOG row.
- Only four commands fit in flight: four 128 KiB regions of a 512 KiB data
  VMO.
- With the copy moved to the reader, `reader>done` is now 11 to 15 us, a
  cache miss on freshly DMA'd data. `answered>reader` is 20 to 26 us, a
  same-processor wake, which step 6 addresses.

**Measure on hardware for Arm.** Under TCG, QEMU flushes its whole TLB on
the register writes this work saves, so AArch64 numbers from TCG say
nothing about steps 3 and 10. Use the Pixel 7 under KVM or the DK1.

**Still owed to the trace.** `bench-seam`'s Linux side prints a mean only,
not p50/p99 per read; the ftrace segments on Linux and the host-side
count of VM exits (`trace-cmd` on nazuna) are not written.

## 9. The channel round trip, and speculation domains (2026-10-01, os-c7)

**The speculation domain is on `main` since bf9efba95 (2026-10-01).** Its
design and its code were reviewed by the certification consultant (os-ad,
§9.3a and §9.3b). It landed ahead of its full gate on the customer's word;
evidence: `fleet/gate.sh` INDEX tags `osc7-sd4-c1` to `-c8` (the controls,
each `FIRED (panic)`), `osc7-sd4-check`, `-build`, `-boot-*`, `-shell`,
`-threads`, `-vfs-*` on 96f0d28c8, the same tree before its rebase, and
`osc7-land-check`, `-boot-kvm`, `-boot-tcg`, `-boot-a64` on bf9efba95 itself.
What of that was still running at the wind-down is a row in
`docs/BACKLOG.md`, *Verification audit*. The customer's decision is in
`docs/BACKLOG.md`, Decisions, 2026-10-01 (2c0e37214). The rest of the round
trip work (§9.1's figures, §9.4) is not on `main`: it is WIP on branch
`os-ipc/zircon-trip`.

### 9.1 Where the round trip stands

`cargo xtask bench-ipc`, on branch `os-ipc/zircon-trip` and not yet on
`main`, boots a native client and a native echo server
(`/sbin/ipc-bench`) and times 20,000 round trips of eight bytes. That is the
figure an IPC design is quoted by, and it has no device in it.

The reference points:
- Zircon's own report gives about 6 us for a cross-process `channel_call`
  on bare metal (`zircon/docs/benchmarks/microbenchmarks.md`, 2018).
- The SkyBridge and UnderBridge papers measured Zircon at 8,000 to 20,000
  cycles.
- seL4's direct-switch fast path is the one design under a microsecond.

Branch `os-ipc/zircon-trip`, x86-64 under KVM on nazuna, one processor, p50:

| | mitigations on | off |
|---|---|---|
| `origin/main`: write, wait and read on each side | 37 us | |
| branch, the same calls | 12 us | 5.1 us |
| branch, `channel_write_read` (0x1013) | 6.5 us | 2.6 us |

§9.4 lists what the branch changes. The gap between the two columns is the
switch barrier: `IBPB` and the return-stack refill at the two switches between
programs that a round trip makes, about 2 us each on this processor. No round
trip under a microsecond is possible while every switch between two programs
pays it. The decision changes that.

### 9.2 The speculation domain

**What a domain is.** A job created marked. The domain's identity is a
non-zero `u64` taken from a counter when the job is made, and it is never
reused; zero means "no domain". An unmarked job, the default, is no domain.
Neither is any job inside a marked one, because a child job is made unmarked.

**Marking.** `job_create` gains an options argument in its second register,
which today's callers already pass as zero.
- `JOB_SPECULATION_DOMAIN` asks for the mark, and any other bit is
  `INVALID_ARGS`.
- The call already needs MANAGE on the parent, so the mark needs nothing more.
- A job is marked only as it is made, when it has no process. Nothing else
  sets or clears the mark.
- The kernel writes an audit record of the marking, with the parent's and the
  child's ids. It is a new event, `DOMAIN`, in `docs/certification/AUDIT.md`
  §1.

**Membership is stricter than "in the job".** A process is in a domain only
if it was made in the marked job and has never left it.
- It was made by `process_create` into the job, or by a fork of such a
  process, whose child starts in its parent's job.
- `Process` keeps the domain it was born with.
- Any move between jobs sets that domain to zero for good, whichever job it
  moves to. A move is `Process::move_to`: a `cgroup.procs` write,
  `CLONE_INTO_CGROUP`, or a delegation.
- A process moved *into* a marked job does not join the domain.

So nothing joins a domain except by being started in it by a holder of the
job's MANAGE right, and nothing that leaves keeps it. The integrator who marks
a job decides what may be started in it. That is what the assumption of use
means by "places in one domain only programs that may read each other's
memory". MANAGE on a marked job is that authority, so handing it to another
program hands that authority over too, and the AoU says so.

**Where the switch reads it.** On the `AddressSpace`, which is what the barrier
is keyed on today (`speculation::entered_space(root)`). The space gets a
`domain: AtomicU64`, set from its process's domain when the space is made for
that process. It is set to zero for good in two cases:
- a process of another domain, or one that left its domain, comes to share
  the space (`CLONE_VM` without `CLONE_THREAD`);
- the owning process leaves its domain.

Threads share their process's space, so they share its domain.

**The decision is one rule on every architecture.** `entered_space` already
runs on all three, called from each architecture's `install_user_root`.
- Each processor keeps `LAST_DOMAIN` beside `LAST_ROOT`.
- The outgoing space's domain is read *as it leaves*, not as it came: by
  `AddressSpace::install` from the space it replaces, and by
  `AddressSpace::uninstall` on the way to a kernel thread. A process that left
  its domain while it ran is no longer in it at the switch that ends its turn.
- The barrier is skipped exactly when the root differs and
  `LAST_DOMAIN == domain != 0`. Otherwise it is issued as now.
- `forget_root` also clears `LAST_DOMAIN` wherever it clears a root, so a
  reused root never inherits a domain.

The compare is two loads and a branch, at the place the barrier is decided.
Arm's predictor invalidation (`entered_space` on AArch64 and ARMv7-A) follows
the same rule.

### 9.3 Requirements, checks and controls

The rows, as numbered (os-ad, against main and the unlanded branches):
- **H.TRAP.17:** a switch between the address spaces of two programs not
  in one speculation domain issues the predictor barrier. A switch between two
  address spaces of one domain does not.
- **L.object.113, with L.x86_64.126, L.aarch64.52 and L.armv7a.3 for each
  architecture's in-domain barrier:** `entered_space` skips the barrier only when
  the root differs, and the outgoing and incoming domains are the same and
  non-zero, the outgoing one read as it leaves.
- **L.object.114:** `job_create` marks a job only with `JOB_SPECULATION_DOMAIN` and
  only under the parent's MANAGE. It refuses any other option bit and writes
  the `DOMAIN` audit record. A child of a marked job is unmarked.
- **L.object.115:** a process is in a domain only if it was made in the marked job
  and never moved. A move sets its domain, and its space's, to zero.
- **L.object.116:** a member leaves for good, its space with it, on a move
  between jobs and on losing dumpability, and every processor whose last
  space was in the domain issues the barrier before the leave returns
  (§9.3a A1, §9.3b F1).

The check runs in stage 9, on every architecture, at two processors or more.
It counts `switch_barriers_on` around pinned switches:
1. Two processes of one marked job handed one processor back and forth: no new
   barrier.
2. One of them switched with a process of an unmarked job, and with a process
   of another marked job: one barrier each way.
3. A process moved out of the marked job, switched with one left in it: one
   barrier each way.
4. `job_create` with the option but without MANAGE on the parent, and with an
   unknown option bit: refused, nothing marked, no audit record.

Each negative control must be shown firing and stopping the boot on the
check's own message:
- the domain compare answering always true, which fails case 2;
- the MANAGE test removed, which fails case 4;
- the move not clearing the domain, which fails case 3.

### 9.3a The consultant's amendments (os-ad, 2026-10-01, design review of 54509856e)

The verdict was **OK to build**, with these three amendments.

**(A1) A rise in privilege leaves the domain.** A member that ran a set-id
program would otherwise keep its domain, and its peers could then attack the
privileged program by branch-target injection. Linux's conditional `IBPB`
keys on exactly this. So a process leaves its domain for good, its own and
its space's, when it stops being dumpable, by any route:
- an `execve` that changes its effective or filesystem ids (`exec_dumpable`);
- a later change of those ids (`credentials_changed`);
- `prctl(PR_SET_DUMPABLE, 0)`.

Every one of these routes passes through `syscall::attributes::update`.
Dumpability and credentials are the personality's, so the core offers a
one-way `Process::leave_speculation_domain`. The personality calls it from
there whenever a process ends up not dumpable. That is a downward call.

Owed with it:
- **Check case 5:** a member that execs a set-id file, and one that clears
  dumpable, each switched with a member, issue one barrier each way.
- **Control 4:** `leave_speculation_domain` made a no-op.

**(A2) What the skip leaves out, and what it keeps.** The skip removes only the
*predictor invalidation*:
- x86-64: `IBPB`;
- AArch64: `SMCCC_ARCH_WORKAROUND_1`;
- ARMv7-A: `BPIALL`, or `ICIALLU` with `ACTLR.IBE`.

x86-64's 32-entry return-stack refill stays at every switch of address space,
in a domain or not. It is a few hundred cycles where `IBPB` costs about
2 us, and keeping it removes any question of whether a defence of the kernel
leans on it.

Why the invalidation can go without weakening the kernel, on every family in
the reference configuration: a program enters the kernel through its own
system calls, faults and interrupts with no switch of address space in
between. Anything it can train into a predictor against the kernel, it can
therefore use against the kernel from its own entries, where no switch
barrier has ever stood. The kernel is defended at entry instead, and that is
unchanged:
- x86-64: enhanced IBRS, AutoIBRS or IBRS-always-on, `STIBP`, and the entry
  hardening of SPECULATION.md §3;
- AArch64: the Spectre-BHB loop on every vector entry from EL0, and the
  `CSV2` that SPECULATION.md §4 relies on;
- ARMv7-A: SPECULATION.md §5.

A barrier at a switch separates the program that ran from the program that
runs next, and nothing else. SPECULATION.md §3's table changes in one row,
*Spectre v2, program → program*, whose `IBPB` becomes "when the processor
switches to another program's address space outside its speculation domain".
`STIBP`, the refill and every program → kernel row are unchanged, and the row
says so.

**(A3) Two more check cases.**
- **Case 6:** two processes in different domains sharing one space
  (`CLONE_VM` without `CLONE_THREAD`). The space's domain is zero, so its
  switch with a member issues the barrier.
- **Case 7:** a job created marked inside a marked job is a domain of its own,
  not its parent's. A switch between the two jobs' processes issues the
  barrier.

Items 3 and 4 of §9.4: the arguments are accepted, and the checks and
controls listed there are owed with those landings. For 4, that includes a
failing 0x1013 leaving registers 2 to 4 exactly as sent.

When this lands, §9 carries this review as its "where it stands" verdict.
The code goes back to the consultant with the check's logs and each of the
four controls' logs, each showing its marker and the check's own
`FERRIX-PANIC`.

### 9.3b The consultant's code review (os-ad, 2026-10-01) and what changed

The verdict on 5658e7c0c and 0ce13778f (rebased as 8527c9d12) was **changes
required**. What each finding changed:

- **F1, blocker: leaving must take effect at once.** A member that rose in
  privilege kept its domain's predictor state until its next switch, because
  an `execve` reuses the space in place and its other threads run on. Now
  `Process::leave_speculation_domain` calls `arch::leaving_domain`, which asks
  every processor whose last space was in the domain for the barrier. This
  processor serves the request at once. Every other one serves it at the IPI
  of the grace period the leave then waits for (`smp::synchronize`, and
  `arch::serve_wanted_barrier` in `on_ipi`). A fork's child, which has never
  run, leaves without the wait. **Case 8** checks both halves: a task parked
  in the leaver's space on another processor, and this processor, each
  decide the barrier before the leave returns.
- **F2: "left" is a state, not zero.** `Process`'s domain becomes `LEFT`,
  which a birth by `move_new_to` respects. A child that `inherit` made not
  dumpable in the root job therefore stays out when `process_create` moves
  it. **Case 9.**
- **F3: the check covers what the rows claim.**
  - **Case 10** checks: a child job of a marked job; a member's fork; a
    process moved into a marked job and its fork; and a forgotten root,
    after which the next member installed decides the barrier.
  - Case 1 now also counts invalidations issued, which must be none, and
    x86-64's in-domain refills, which must be one per switch (`REFILL_IN_DOMAIN`).
  - Case 4 reads the `DOMAIN` record back: the job, its parent and its domain.
  - *Read as it leaves* is case 8's.
  - The set-id `execve` is not driven by the check. The decision point is
    `syscall::attributes::update`, which is how N4 argued it, because every
    route to "not dumpable" goes through it: `exec_dumpable` writes
    `dumpable` there for a set-id `execve`, as do `credentials_changed` for
    a change of ids, `PR_SET_DUMPABLE`, and `inherit`. Case 5 drives two of
    them through `update` itself.
- **F4:** `AddressSpace::new` and `fork` build their spaces through one
  `assemble`, so `fork` is back under the complexity floor.
- **F5:** the Security Target's FDP_IFC.1/FDP_IFF.1 SFP names the in-domain
  exception. FMT_MSA.1 and FMT_MSA.3 cover the mark: set only by MANAGE on
  the parent at creation, membership only ever lost, unmarked by default. Both
  map to O.ISOLATE, and FAU_GEN.1 lists the `DOMAIN` event.
- **F6: ordering with os-35's lazy TLB.** `os-35/ipc-lazytlb-land` replaces
  `install` and `uninstall` with `switch_here`. Whichever of the two lands
  second moves `left_space` and `entering_space` across. "Read as it leaves"
  must then come from the space actually loaded on the processor, since a
  kernel thread will no longer uninstall it. os-35 (os-49) was not running
  when this was written; this paragraph is the note to it.
- **F7: the window of a move.** A move between jobs made by another process
  (a `cgroup.procs` write by the holder of MANAGE) is ordered with the moved
  process's own running only by that holder's action. Since F1, though, the
  move's leave makes every processor that last ran the domain issue the
  barrier before the write returns. The moved process can still run in the
  space between the write's start and its return, and so can the domain's
  other members: the window is the length of one grace period, and it is the
  MANAGE holder who opened it.
- **C1, on F1: a leave waits for a grace period, so it may not run under a
  spin lock or with interrupts masked.** Where each caller is when it
  reaches `attributes::update` or a move:
  - `exec_dumpable` runs in `execve` after the new image is populated,
    holding no spin lock, with interrupts on.
  - `credentials_changed` runs in the setuid family after `with_credentials`
    has let its lock go.
  - `PR_SET_DUMPABLE` calls `update` straight from `prctl`.
  - A move by `cgroup.procs` (`write_to`, `Job::adopt`) runs on a file
    write's path, which holds only sleeping locks.
  - `inherit` (fork, `process_create`) and `setrlimit`'s `update` never
    wait, because the process they reach is not a member. A fork's child
    whose parent is not dumpable has already left. A child of
    `process_create` is in the root job's no-domain. And a member that
    is still dumpable does not leave.

  The rule is checked as well as argued. The waiting branch of
  `leave_speculation_domain` stops the machine with FX-0907 unless
  `sched::may_block()` holds, that is unless the preemption count (FX-0503's)
  is zero and interrupts are on. A control calls a leave from inside a
  `SpinLock` in the check.
- **F8:** `Job::new_child_in` is `bare_child` with a domain argument.
- **F9:** MEMORY-AND-TIMING §2.2c gives the commit measured and the
  benchmark's resolution: its percentiles are an eighth of a power of two,
  so a p50 can read below the minimum.

### 9.4 The rest of the branch, for its own review

These are reviewed separately, once rebased and gated, as the consultant asked.

1. **The timer** (`timer.rs`). A one-shot already armed no later than the
   deadline asked for is not rewritten, and `stop` leaves a one-shot to fire.
   The LAPIC's registers are emulated, so each write is an exit, about 7 us on
   nazuna.
2. **Wakes inside a call.** `trap::system_call` marks the task as inside a
   call. A wake the call makes then leaves its decision to the call's way out
   (`sched::call_left`) rather than to a 20 us timer.
3. **The wake and the wait.** os-35's `Wake::Sync` is used for channel writes,
   and defers the decision to the waker's block. The wait queue keeps its
   buffer, and `wait_trusting` is new.
   - Why `wait_trusting` needs no recheck: every condition its one caller
     waits for, a message or the peer's close, wakes the queue in the same
     lock order as `wait_until_deadline`'s wakers do. A signal or a kill wakes
     the task itself (`syscall::process`, `sched::wake` then
     `sched::interrupt`). A task there was no memory to list rechecks, as F-23
     has it.
   - Its check, owed: a waiter woken by each of the three. The control makes
     the channel's close not wake the queue, which must leave the waiter
     blocked past a bound the check states.
4. **The new call.** `channel_write_read` (0x1013), with
   `trap::Outcome::ReturnWords`.
   - The words handed back are the message's bytes, then zeros. They come from
     an inbox slot, or from a queued message copied into a zeroed array
     (`Small::of`). A call that fails answers `Outcome::Return`, which leaves
     the argument registers as the caller set them. So no kernel value reaches
     the registers.
   - Its check, owed: a message shorter than three words comes back with the
     rest zero, and a failing call's registers come back as sent. The control
     fills the slot's tail with a pattern, which the check must catch.
5. **Segment state.** x86-64's switch skips segment, descriptor and base
   writes that equal what the same switch's save read. It made no measurable
   difference, and is a candidate to drop.
6. **The clock.** `now_nanos` uses two exact 64-bit divisions in place of one
   128-bit division.

With the domain, a round trip between two members of one domain is 2.8 to
3.0 us p50 with every mitigation on (`bench-ipc`'s `domain-call`, one
processor under KVM), against 2.6 us with mitigations off. What is left before
it is under a microsecond:
- PCIDs, so a switch does not flush the user half;
- a direct switch from caller to callee;
- the system call's own path, at 464 ns for a native call that does not sleep.

**Where the branch stands (wind-down, 2026-10-01, os-86, which was os-c7).**
`os-ipc/zircon-trip` (a1379b25b, on GitHub) is based on a `main` from before
the domain landed. Its domain commits (54509856e to 09afb2ddf) are an early
version and are superseded by `main`'s. The work still to land is its other
eight commits:
- 937b75972, `bench-ipc`;
- 5b300da82, item 1 and 2;
- 54ff92c2a, the sync wake;
- 169c39374, item 4;
- 82ee2fa44, item 6;
- dfedadeae, item 3;
- 15ae15cc3, item 5, to drop;
- a1379b25b, XSAVEOPT, which made no difference, to drop.

To resume, cherry-pick the six that stay onto `main`, then write the two owed
checks with their controls (items 3 and 4), gate, and send it to the
certification consultant. `os-ipc/prof` and `os-ipc/prof2` are timing builds
(spans printed at the shell's exit). They exist to find costs and must never
land. The per-span figures they gave are the ones in §9.1 and the list above.

### 9.5 Within 1.5 times seL4: the plan (2026-10-01, os-86)

The customer asked for a plan to bring §9.1's round trip within 1.5 times
seL4's. Nothing in it is built yet. Points are estimates, one point being 20 to
30 minutes of one session. A figure marked *guess* stands until step 0
measures it.

**Start here** (for a session pointed at this section).
- *Read first:* §9.1 to §9.4, then this section.
- *The code to start from is on GitHub*, branch `os-ipc/zircon-trip` at
  a1379b25b. It forks from a `main` older than the speculation domain, so do
  not merge or rebase it whole.
  - Make a branch from current `main`.
  - Cherry-pick these six commits in this order, the branch's own:
    1. 937b75972, `bench-ipc`;
    2. 5b300da82, the timer and the decision at a call's end;
    3. 54ff92c2a, the sync wake;
    4. 169c39374, `channel_write_read` (0x1013);
    5. 82ee2fa44, the clock;
    6. dfedadeae, the deferred decision and `wait_trusting`.
  - Leave out the rest of the branch:
    - 15ae15cc3, the segment skip;
    - 54509856e to 09afb2ddf, an early speculation domain that `main`'s
      supersedes;
    - a1379b25b, `XSAVEOPT`.
  - That is step 1.
- *The timing builds* are on GitHub too: `os-ipc/prof` (41bdef2ca) and
  `os-ipc/prof2` (4b5be585c), both on `os-ipc/zircon-trip`. They print each
  span's ns at the shell's exit. Step 0 refreshes `os-ipc/prof2` onto `main`.
  Neither ever lands.
- *Branches that meet this work:*
  - os-35's lazy TLB, backed up on GitHub as
    `backup/2026-10-01/os-35/ipc-lazytlb-land` (bdc227dbd). Step 3's PCIDs
    must be built with it (§9.3b, F6; FX-0009).
  - os-35's sync wake and idle poll, `os-35/ipc-wake` (dbd392808, §8).
- *How the work is run:*
  - Builds and gates run on nazuna. The gate pool is
    `~/.local/share/ferrix/fleet/gate.sh`, with verdicts in its INDEX.
  - Landings take the lock with `fleet/land.sh` and follow
    `docs/CONVENTIONS.md`.
  - Requirement ids are reserved before they are written.
  - Every landing here touches the item, so it goes to the certification
    consultant first. The customer names that seat.
- *The measurement*, once step 1 is in: `cargo xtask bench-ipc --release
  --accel kvm --smp 1`. Its `domain-call` line is the figure. Run it with and
  without `--mitigations off`, back to back on the same host load.

**The target, as a number that can be checked.**
- *What is compared.* §9.1's round trip: a client's `channel_write_read`
  answered by the echo server's own. The seL4 equivalent is `seL4_Call` plus
  `seL4_ReplyRecv` between two address spaces on one core at one priority.
  sel4bench reports those two as "IPC call" and "IPC reply", each one way, so
  a round trip is their sum.
- *seL4's published figure*, from sel4.systems/performance.html. On an i7-6700
  (Skylake, 3.4 GHz), in the default configuration without its Meltdown
  defence, a call is 741 cycles and a reply 598. That is 1,339 cycles a round
  trip, or 0.39 us.
  - Both run seL4's fast path: the message fits in registers, no capability
    moves, and the server is waiting.
  - In that setup the server's FPU is off, so no vector state is switched.
- *On nazuna* (Ryzen 9 9900X, Zen 5, up to 5.66 GHz), the same cycle count
  would be about 0.25 us, and 1.5 times that about 0.37 us (*guess*). Today's
  2.8 to 3.0 us inside a domain is eight times that.
- *How it is compared.*
  - Both kernels boot under the same QEMU and KVM, with the same `-cpu` model
    and one virtual processor pinned to one host core.
  - They run alternately: seL4, Ferrix, seL4, Ferrix.
  - The figure is the ratio of their p50s within one run. nazuna's load moves
    absolute figures from hour to hour; a ratio taken in one run holds.
  - Both are kept in TSC ticks, which count at a fixed rate rather than at the
    core's clock.
- *Configuration: the gate figure has the protections matched.*
  - **Ferrix**: every mitigation on, and the two programs in one speculation
    domain.
    - On Zen 5 that means AutoIBRS and `STIBP`, both set once at boot, and the
      return-stack refill at every switch of address space (§9.3a, A2).
    - Ferrix builds no page-table isolation, and Zen 5 needs none.
  - **seL4**, built for the same processor:
    - `KernelSkimWindow` off. This is its Meltdown defence, which Zen 5 does
      not need.
    - `KernelX86RSBOnContextSwitch` on, to match Ferrix's refill.
    - `KernelX86IBPBOnContextSwitch` off, as by default.
    - PCIDs on (`KernelSupportPCID`, also its default).
  - **Reported beside the gate figure**, not gated:
    - Ferrix with `--mitigations off`, against seL4's defaults.
    - Both kernels with `IBPB` at every switch. `IBPB` alone costs about 2 us
      a switch on this processor, so no design reaches the target between
      programs that are not in one domain. The report says so.

**Where the time goes today.** Read from the code on `os-ipc/zircon-trip` and
from the timing builds.
- *Locks.* A round trip takes about 25 spin locks on each side:
  - about eight of the run-queue lock;
  - about 14 `PreemptSpinLock`s: the handle table, the inboxes and
    observers, the task slots, the switch's `LEFT`, and the process's and
    thread's signal state;
  - three wait-queue locks.

  A `PreemptSpinLock`, taken and released, costs about six locked
  read-modify-writes and four interrupt saves and restores. They keep the
  preemption count and FX-0503's bookkeeping.
- *`sched::current()`* takes the run-queue lock and clones an `Arc<Task>`. A
  side calls it about eight times: from `native_call`, from `must_leave` three
  times in one wait, from the wait itself, and from `needs_attention`.
- *The way out of every call.* On every return to ring 3, `needs_attention`
  takes `current()`, two `Arc` clones, the process's `state` lock and the
  thread's `signals` lock. With it, `decode_syscall` matches twice and the call
  is dispatched indirectly through the Linux personality. A native call that
  does not sleep costs 464 ns.
- *The scheduler.*
  - The writer's wake files the reader in the EEVDF queue. That costs
    `effective_weight`'s 128-bit divisions at each job level, `now_nanos` and
    the quota atomics.
  - The writer's block then runs `choose_next`, which picks the same reader
    back out. That costs four `Arc` clones, a SeqCst read-modify-write of the
    global `IDLE` word on every switch, and `arm_timer`.
- *The switch's state.*
  - Two `rdmsr`s read the FS and GS bases at every save, though the kernel
    already knows both: a program has no `FSGSBASE` and can change them only
    by a call.
  - An `XSAVEOPT` and an `XRSTOR` of 832 bytes, because the program's
    `syscall` stub promises it every vector register back.
  - A `mov cr3` without PCID, which drops every user translation. QEMU's CPU
    model does not offer `pcid` at all.
- *What the timing builds measured*, per switch:

  | Span | Time |
  |---|---|
  | the scheduler's choice | 0.8 to 1.2 us |
  | the wake | 0.3 to 0.6 us |
  | user state | 0.37 us (vector 0.12, segment bases 0.14) |
  | the address space | 0.32 us |

  The stamps inflate every span and the spans overlap. So the figures rank the
  costs; they do not add up to the trip.

seL4's fast path has none of this. It is one function. The caller's message
stays in registers, one capability lookup finds the endpoint, and the checks
are a handful of compares. The processor goes straight from the caller to the
waiting server, and neither ever enters a run queue (Heiser and Elphinstone,
*L4 Microkernels: The Lessons from 20 Years of Research and Deployment*,
2016).

**The budget.** At 1.5 times seL4, one direction may take about 185 ns, or
about 1,000 cycles. Here is an allowance for each piece, to be checked against
step 0's profile:

| Piece | Allowance (*guess*) |
|---|---|
| `syscall`, `sysret`, and the frame pushed and popped | 40 ns |
| the fast path's checks and its one handle lookup | 25 ns |
| the words written into the waiting peer's frame | 5 ns |
| the direct switch: stack, current task, and one `rdtsc` of accounting | 25 ns |
| the FS base write and the vector scrub | 25 ns |
| `CR3` with a PCID, no flush | 20 ns |
| the return-stack refill | 40 ns |
| **total** | **180 ns** |

The refill and the hardware's own entry take almost half of it. The software
between them has about 100 ns.

**The steps.**

| Step | What | Points | Round trip after it (*guess*) |
|---|---|---|---|
| 0 | Measure the target | 10–16 | — |
| 1 | Land the written work (§9.4) | 5–8 | 2.8–3.0 us |
| 2 | A cheap common path | 20–33 | 1.3–1.8 us |
| 3 | A cheap switch | 17–24 | 0.9–1.3 us |
| 4 | The direct switch and the fast path | 23–37 | 0.3–0.4 us |
| 5 | Squeeze by the profile, and hold it | 5–11 | within 1.5 times seL4 |
| | **Total** | **80–129** | about 27 to 65 hours of one session |

0. **Measure the target** (10–16; runs beside step 1).
   - *seL4 on nazuna* (3–5). Build sel4bench for x86-64 in the two
     configurations above, and boot it with the gate's QEMU and `-cpu`.
     nazuna has `cmake`, `ninja`, `gcc` and `python3` but not `repo`, so either
     clone the manifest's repositories by hand or install `repo` for the user.
   - *A seL4 root task that times like `ipc-bench`* (2–3). It runs 20,000
     `Call` and `ReplyRecv` round trips, timed and counted exactly as
     `ipc-bench` does, so the two figures differ in nothing but the kernel.
   - *`bench-ipc` made exact* (3–5):
     - an `lfence` around the counter read, or `rdtscp`;
     - the p50 taken from sorted samples, since today's histogram reads up to
       an eighth low;
     - one processor by default, the virtual processor pinned;
     - `+pcid,+invpcid` in the CPU model, for both kernels;
     - `--against-sel4`, which alternates the two images and prints the ratio
       and its spread.
   - *The timing build, refreshed onto `main`* (2–3). The stamp's own cost is
     measured and subtracted. Ablation switches skip one piece at a time and
     measure the change, which gives each piece's cost without the stamps'
     distortion. This build never lands.
   - Done when the target is a number measured on nazuna, written here and in
     the BACKLOG row.
1. **Land the written work** (5–8). Cherry-pick §9.4's six commits onto
   `main`, write the two owed checks with their controls, gate, and send it
   to the certification consultant.
2. **A cheap common path** (20–33). Every program gains from these, Linux
   programs included, and none of them changes what a call does.
   - `current()` read from the processor's own record, with no run-queue lock
     and no `Arc` clone. It returns a borrow that lives while the task runs
     (3–5).
   - A lighter `PreemptSpinLock` (5–8). The preemption count and FX-0503's site
     words become plain fields of the processor's record, which need no locked
     operation and no masked interrupts. A lock and its release then cost one
     locked operation. FX-0503's checks stay.
   - The way out as one word of pending work per task (5–8):
     - its bits are a signal, a stop, a kill, a resched and a regroup;
     - it is read with interrupts masked, just before `sysret`;
     - `needs_attention`'s locks are taken only when a bit is set;
     - whoever posts the work sets the bit under the lock it already holds.
   - A native call decoded once, and dispatched without the Linux
     personality's table (1–2).
   - The channel's `ready()` and `must_leave` read from one atomic state word,
     instead of the inbox lock and `current()` (3–5).
   - No global or locked writes in the switch (3–5):
     - `IDLE` is written only when a processor goes idle or wakes from it;
     - `entered_space`'s three swaps become plain per-processor stores, since
       interrupts are already masked there;
     - `effective_weight` is kept per job and recomputed only when a weight
       changes.
3. **A cheap switch** (17–24).
   - *PCIDs on x86-64* (§8, step 10; 10–13):
     - an allocator per processor, with generations;
     - `CR3` written with the no-flush bit;
     - a PCID that is reused gets flushed;
     - a shootdown reaches every PCID a space holds, including one a processor
       keeps lazily (os-35's lazy TLB, FX-0009).

     Kernel pages are already global.
   - *A vector-state contract for native calls that block* (5–8; a decision
     for the customer, below):
     - `channel_write_read` and the native waits are declared to destroy the
       vector registers, as a function call does in the C ABI, and the
       runtime's stub tells the compiler so.
     - A task blocked in one of these calls has no vector state to save.
     - Before the task runs again, the kernel resets its vector registers to
       their initial state (`XRSTOR` of an empty header), keeping `MXCSR` and
       the x87 control word. Nothing of the other program reaches it.
     - A task preempted anywhere else is saved and restored as today.
   - *The FS and GS bases kept in the task* rather than read back with
     `rdmsr`, since only a call changes them. The switch's `LEFT` lock is
     replaced by per-processor fields (2–3).
4. **The direct switch and the fast path** (23–37). The design goes to the
   certification consultant before any code (2–3).
   - *The direct switch* (8–13).
     - When a call wakes the task it then blocks waiting for, on this
       processor, and nothing runnable here is more urgent, the processor
       goes straight to that task. It is not filed in the run queue and
       picked back out.
     - The run queue's policy is kept by three conditions: the woken task's
       affinity includes this processor, its job is within its quota, and
       the caller's slice has not ended.
     - Every channel call gains from this, including one that carries handles.
   - *The fast path* (8–13). It is one function, reached from the entry stub
     for 0x1013 before the general dispatch. It runs with interrupts masked
     from entry to `sysret` and takes one lock, the channel's.
     - It applies only when all of these hold:
       - the message is at most 24 bytes and carries no handles;
       - the handle names a channel with the write and read rights;
       - the peer is the one task blocked in 0x1013 reading that channel;
       - the peer's inbox is empty, so nothing queued is overtaken;
       - the direct switch's conditions hold;
       - neither task has a pending-work bit set.
     - Then the words go straight into the peer's saved frame as its return
       values, the caller is marked blocked reading, and the processor
       switches.
     - If any test fails, the general path runs, unchanged.
   - *The evidence* (5–8). The fast path is a second implementation of
     0x1013, so the argument is that its results equal the general path's.
     - A boot argument turns the fast path off, and the boot says which path
       it ran. The channel checks and `ipc-bench` run both ways, and every
       observable result must agree: return words, return codes, order, a
       peer's close, and a signal or a kill during the wait.
     - One negative control per condition: each condition's test replaced by
       "true", one at a time, must make a check fire.
     - The check covers every branch of the fast path, and the coverage is
       carried as for the rest of the item.

     seL4 proved its fast path equal to its slow path; this is the tested
     version of that argument. Whether that is enough for DAL C and EAL5+ is
     the consultant's call, before step 4 starts.
5. **Squeeze, and hold it** (5–11).
   - Work through whatever step 0's profile still shows (3–8): the layout of
     the task and channel records, `Arc` traffic on the fast path, the handle
     lookup.
   - Then add a perf row to the gate pool that runs `bench-ipc
     --against-sel4` and fails above 1.65 times, so the figure stays (2–3).

**Order and wall time.**
- Steps 0 and 1 run side by side.
- After step 1, step 2 splits across two sessions, and step 3's PCIDs take a
  third.
- Step 4 starts once steps 2 and 3 are in. Its conditions read the
  pending-work word, and its switch assumes the cheap one.

That critical path is about 50 to 80 points, 17 to 40 hours with three
sessions. Every landing touches the item, so each needs the certification
consultant, and at this writing that seat is empty.

**Where it can fail, and what then.**
- *seL4 on nazuna is far from 0.25 us.* The target moves with it, and step 0
  restates it.
- *After steps 2 and 3, the profile puts the cost somewhere other than the
  scheduler and the wake.* Step 4 is re-planned before it is built.
- *The consultant does not accept a tested second implementation.* Then the
  work stops after the direct switch, at about 0.5 to 0.8 us (*guess*): under
  the customer's microsecond, but two to three times seL4.
- *The refill and the hardware entry leave too little room.* A2 keeps the
  refill, and only the consultant can change that. The matched seL4 pays it
  too.

**Not in this plan.**
- Calls between processors. seL4's fast path is one core only, too.
- Linux programs. They gain from step 2 only.
- Messages that carry handles or are longer than 24 bytes. They gain from
  steps 2 and 3 and from the direct switch, but not from the fast path.
- AArch64 and ARMv7-A. They gain from step 2, and get ASIDs and a fast path of
  their own after x86-64. Those are measured on hardware, because under TCG
  the TLB costs say nothing (§8).

**Decisions for the customer.**
1. Which figure is the promise: protections matched inside a domain
   (recommended), or mitigations off.
2. The vector-state contract for native calls that block. It changes the
   native ABI for native programs only; the Linux ABI is untouched.
3. Whether a fast path, which is a second implementation inside the
   certified item, is acceptable at all, with the consultant's view. Without
   it the plan stops at the direct switch.
4. Who takes the certification consultant's seat.
