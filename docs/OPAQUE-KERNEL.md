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
