# An opaque kernel: services as supervised userspace servers

**Shelved (customer, 2026-09-27): off the table for now.** S0 measured the
seam, and the customer read the numbers and set the plan aside. S1 and
everything after it are not started. The 2026-09-16 decision stands:
monolithic core, device drivers in ring 3. What would bring the plan back is
a cheaper trip to ring 3 (*The verdict of S0*, below).

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
1. Fix the depth-32 stall.
2. Find where the 250 to 800 us go; the driver's own `device_ticks` already
   splits off the device's share.
3. Add PCIDs.
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
