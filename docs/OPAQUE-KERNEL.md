# An opaque kernel: services as supervised userspace servers

**Proposal, not decided.** The customer's decision of 2026-09-27: measure
first. S0 is next, then S1. The 2026-09-16 decision stands until S0's numbers
are read, and nothing past S1 starts before that.

Drafted by ferrix-55b on 2026-09-27. The certification consultant reviewed
§2 and §5, and the init owner reviewed §3. The decision is recorded in
`docs/BACKLOG.md`, Decisions, 2026-09-27.

## 0. The decision this would change

On 2026-09-16 the customer reaffirmed `docs/ARCHITECTURE.md` §1: *a monolithic
core, capability seams, device drivers in ring 3* (`docs/BACKLOG.md`,
Decisions). The seam sits at devices because `rustc`'s calls are `open`, `stat`,
`read` and `mmap` on files the page cache holds, and those stay function calls.
"What the shape gives up is restarting a kernel subsystem, which the goal does
not need." The decision was to be closed by evidence: the two "seam measured"
rows in P2, which are **still open**. No IPC hop cost has been measured yet.

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

1. **Reopen the 09-16 decision for B, subject to S0's numbers?** If yes, S0 is
   the next landing, and nothing past S1 starts before its numbers are read
   together.
2. **Order after S0:** net stack first (S3, the most self-contained and easiest
   to restart), then device cores, then btrfs. Or btrfs first, because it has
   the largest ring-0 code and the most to gain from restart?
3. **S1 regardless?** The ioctl and socket seams and the generic rebind
   contract pay off even if B is never built.
4. **Option C** stays unplanned unless measurements argue for it.
