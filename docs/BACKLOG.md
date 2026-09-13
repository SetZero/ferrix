# Ferrix — backlog and decisions

`docs/ROADMAP.md` says what each stage is and how it knows it is finished.
This file says who is doing what right now, in what order, and which
decisions were taken along the way. It exists because a dozen sessions work on
the tree at once, and a decision that lives only in a message between two of
them is a decision the third one reverts.

The product owner keeps this file. A session that lands a piece of it updates
its row in the same landing, the way the roadmap's stage sections are updated.
When a row is done it is deleted, not struck through; the roadmap records what
landed.

---

## Standing rules

These add to `docs/CONVENTIONS.md`, which still governs commits.

**What a landing runs.** Rebase onto `main`, then:

| The change touches | Gate |
|---|---|
| Only `docs/`, or only `ferrousli/` | `cargo xtask check`; for ferrousli also `cargo test` in `ferrousli/` |
| `libs/` only, and no crate the kernel builds | `cargo xtask check`, and one boot: `test-boot --arch armv7a --smp 2` |
| Anything the image contains: `kernel/`, `boot/`, a kernel-side crate in `libs/`, `xtask` | `cargo xtask check`, then `test-boot` on x86_64, aarch64, armv7a at four processors and armv7a at `--smp 2`; a stage 7 or 8 change also runs `test-shell` on x86_64 with the musl busybox *and* the host's glibc busybox (`/usr/bin/busybox`), and `test-vfs` on x86_64. The whole of that, plus KVM, is what moves `main` |
| User mode, page tables, TLB, SMP or the scheduler | The row above, and x86_64 under `--accel kvm` |

Then fast-forward `develop` only if it is still the commit rebased onto.

**After the fast-forward,** the lander boots `develop` itself once on x86_64
under `--accel kvm` and reports the hash together with that result, so a bad
merge is seen by the one who made it.

**Two branches.** Sessions land on `develop`, which may be unstable. `main`
moves only by the product owner, fast-forward, to a `develop` commit that has
passed the whole matrix from a clean worktree on the product owner's own run:
the four boots, x86_64 under KVM, `test-shell` with the musl and the glibc
busybox, and `test-vfs`; at least every two hours while `develop` moves. So
`main` is always a working tree by construction, and a failure the customer
finds on it is fixed forward on `develop` and re-verified before `main`
moves again. Tags go on `main`. A landing on `develop` from a worktree is
`git push <root> <branch>:develop`, which refuses anything but a
fast-forward; the root checkout keeps `main` checked out and never lands
`develop` through its index.

**The landing queue.** Image changes on `develop` queue; documentation, ferrousli and
host-only `libs/` landings do not. A session whose image change has passed
its row tells the product owner "ready to land" with the base it verified
on; the product owner answers with its place in the queue and says "go" when
the landing before it has fast-forwarded. Nobody fast-forwards an image
change without a go, and a go lapses after 30 minutes. This exists because
on 2026-09-13 a 25-minute row kept losing to a `main` that moved every ten
minutes, and one branch passed on seven bases without landing.

**Re-verifying after `main` moved.** When the commits that moved it touch
none of the files the change touches, re-run `cargo xtask check` and two
boots, `armv7a --smp 2` and x86_64 under `--accel kvm`, then land. When the
files overlap, or the change is cross-cutting (locks, the scheduler, the
trap or system-call entry, memory management), run the whole row again; the
product owner may ask for the whole row in any case. A boot that fails is a
result to read, not a reason to retry: only the stage 5 EEVDF-bound message
was ever a known flake, and it is fixed.

**Worktrees.** One landing, one worktree. `git worktree remove` it once its
branch is on `main`. Check `df -h /` before a landing; after a failed commit
read `git log -1 --stat` before the next step, because a failed commit leaves
its files staged for the next one. On 2026-09-13 the root filesystem filled and
every session's gates failed at once; 70 worktrees held 108 GB of build output.

**The host is shared.** One cargo build, clippy run or QEMU boot at a time
per session, agents included: a session with agents serialises them. Miri and
fuzz runs one at a time machine-wide, and never while a boot matrix runs
anywhere. No worktree under `/tmp`: it is a 30 GB tmpfs, so a build tree
there lives in RAM; the scratchpad is for logs and patches only. Run `free -g`
before a boot matrix and wait while available memory is under 12 GB. On
2026-09-13 the host reached 47 of 59 GB used with swap full, and the customer
reported it close to stalling: three worktrees on the tmpfs held 9 GB of build
output, and ten sessions were building and booting at once.

**Agents.** Gates and boots in the foreground, never `run_in_background`; one
architecture per tool call; the brief says so.

**Milestones.** The customer tests from `main`, so testable progress is tagged
there rather than waiting for a stage to end. The product owner verifies a
candidate from a clean worktree (`test-boot` on all three architectures and
armv7a at `--smp 2`, `test-shell` and `test-vfs` with the static busybox),
then tags it: `stage-N` for a stage's exit, `stage-N.k-<slug>` for a testable
step after it, annotated, with the tag message carrying short release notes as a bullet
list and saying what to test and how; the same notes go into
`docs/RELEASES.md`.
Owners say in one line what a person can test when such a landing is on
`main`. Tags are local until the customer pushes; the customer is told in a
line or two and nothing stops for it.

**Calling a stage done.** The exit criterion as written, on all three
architectures, and the marker moves in the same commit. A criterion met in a
weaker form is written down as such in the stage's section.

---

## Owners

Session names change on every restart; the table carries the current name
and the one the rows below were written under. `ListAgents` shows what is
alive.

| Session | Area |
|---|---|
| ferrix-32 (was ferrix-24) | Product owner: priorities, decisions, this file, milestones, cross-cutting debt |
| ferrix-a5 (was ferrix-91) | Stage 7: processes, signals, threads, tty, futex, time; the user entry path |
| ferrix-e6 (was ferrix-c2) | Stage 8: VFS, tmpfs, devfs, procfs, pipes, the fd and path calls; file-backed `mmap` |
| ferrix-4b (was ferrix-2a) | Stage 9: native ABI, objects, `vmo_map`, native process creation, process observers |
| ferrix-d9 (was ferrix-8b) | Stage 10: PCI, IOMMU domains, `devmgr`, the block ring, virtio-blk in ring 3 |
| ferrix-61 (was ferrix-4d) | Stage 11: `libs/btrfs`, `libs/block`, the kernel mount; the native user-space runtime |
| ferrix-34 (was ferrix-b9) | Scheduler: stage 5 flakes, lock convoys, wake latency, boot-time performance |
| ferrix-e5 (was ferrix-54) | The memory-management package below; reviewer of the VMO reverse map and the space.rs half of file-backed mmap |
| ferrix-4f (was ferrix-b1) | `xtask flash` and `deploy`, `docs/stm32mp157-dk.md`, the Arm UART drivers and console receive |
| ferrix-3c | The STM32MP157D-DK1 board itself: the serial link, OpenOCD, every hardware run, and filing what the board shows with the area that owns it |
| ferrix-ce (was ferrix-53) | `ferrousli/`, beside the roadmap. Done: locales and wide characters; time zones, `strftime` and `strptime`; buffered stdio and `printf`; threads, mutexes, condition variables and keys, with `tests/c/thread/on_ferrix.c` ready as a static threaded program for `CLONE_THREAD`; `dirent.h` and `getopt`; mounts, file system statistics and `mntent.h`. In progress: `scanf`, `glob` and `regex`, thread cancellation and semaphores, the math library |

---

## The path to the goal, in order

The goal is `rustc` on Ferrix (stage 16). Everything below is on its path and
is ordered by what blocks what. A row's owner is the session; "open" means
nobody has it yet.

### P0 — blocks the next stage or the goal

| Item | Owner | Why it is P0 |
|---|---|---|
| Threads: `clone(CLONE_VM\|CLONE_THREAD\|CLONE_SETTLS)` and everything a thread implies | ferrix-a5 | `rustc` is threaded; the largest missing piece on the roadmap. Exit test: a static musl Rust `std::thread` program under `test-shell` on all three architectures |
| File-backed `mmap`: a file mapping maps the inode's own VMO pages, shared and private, with faults served from them | ferrix-e6 | `mmap` with a descriptor answers `ENODEV` today; `rustc` and the linker map rlibs. Same interface as the btrfs page cache |
| Ring-3 virtio-blk reading sectors: `devmgr` (waiting on stage 9's `process_create`/`process_start`, which follow ferrix-a5's start argument, and process observers, ferrix-4b); the block ring's kernel side (the devfs node from HELLO, and reset before release on driver death); the virtio-blk driver process on `ferrix-rt` reading sectors through a translated domain | ferrix-d9, with ferrix-61 | Stage 11's mount and exit wait on it; the customer declined a kernel-side disk path |
| FX-1001 on aarch64, once: "virtio-rng: the device wrote into a page its domain does not map". `probe_out_of_domain` (`kernel/src/pci/virtio.rs`) took a used entry with `written > 0` before the SMMUv3 recorded any fault for the probe page. Seen at 7.95 s on aarch64 TCG at 4 processors, on develop 04bb0a1 plus ferrix-e5's IST branch afb3041, which changes nothing in iommu, pci or virtio; a rerun and develop alone both passed. Log: `~/.local/share/ferrix/logs/fx1001-smmu-out-of-domain-aarch64-afb3041.log`, which holds no event-queue record because the check prints none. Suspects, none verified: a used entry left by the entropy request itself; a stale translation at IOVA 0x1000, or an invalidation not complete before the probe; QEMU's SMMUv3 completing the request before recording the event. Next: print the used entry and the event queue on this failure, loop aarch64 boots one at a time, and fix with a negative control | ferrix-d9 | The one known intermittent on main: it can fail main's verification. ferrix-32 put it ahead of the ring's kernel side |
| `vmo_map` and native process creation | ferrix-4b, space.rs half reviewed by ferrix-e5 | `devmgr` cannot start a driver without them |
| Stage 11's kernel mount, through the VFS, with file data in the inode's VMO | ferrix-61 | The read-only sysroot |

| ferrousli on the build path: `cargo xtask check` builds and tests `ferrousli/`; a script under `ferrousli/tools` builds busybox from a pinned source tarball against ferrousli as a static x86-64 program (musl headers, `crt1.o`, `libferrousli.a`, no other libc); `test-shell` runs it beside the musl and glibc busyboxes, and it becomes the third binary every stage 7 landing must pass | ferrix-ce, with ferrix-a5 for what the kernel owes it | The customer's call on 2026-09-13: ferrousli stops being beside the roadmap; a busybox built on it is the userland Ferrix is measured with |

### P1 — required before a stage is called done

| Item | Owner | Stage |
|---|---|---|
| Reset on driver death, designed in the ring spec before the driver lands | ferrix-d9 | 10 |
| Trusting a BAR firmware placed but did not enable | ferrix-d9 | 10 |
| btrfs: CI Miri step for `libs/btrfs` and `libs/block` under 15 minutes, whole-image tests ignored under Miri | ferrix-61 | 11 |
| Process observers, so a port can watch a process end | ferrix-4b, reviewed by ferrix-a5 | 9, for `devmgr` |
| `mprotect` leaves stale translations: `AddressSpace::protect` must invalidate, shown by a user-mode check | ferrix-e5 | 6 |
| A user-mode copy-on-write write check, and a `MAP_SHARED` write check | ferrix-e5 | 6 |
| `SA_RESTART`; boot checks that drive `SIGCHLD`, stop and continue, `alarm`, `sigaltstack` and fault-to-signal | ferrix-a5 | 7; `rustc` needs `SIGSEGV` on the alternate stack |
| Pid 1 for init and orphans reparented to it | ferrix-a5 | 7, 15 |
| A regression check that exited programs give every frame back once reaped (the leak 0510a8a fixed) | ferrix-a5 | 7 |
| Retire the 20 ms console polling. Receive by interrupt into a 4 KiB ring has landed on the PL011 and the STM32 USART (through ST's EXTI on the DK1), with `console::input::{waiters, has_input}` for the console thread to wait on. Left: the console thread and readers waiting on it instead of sleeping (ferrix-a5, in `fs/terminal.rs`), and an I/O APIC route for the 16550's GSI so x86-64 stops polling too | ferrix-4f, with ferrix-a5 | 7, 15 |
| ARMv7-A with 2 GiB does not boot: the loader must allocate below the direct map's ceiling, and RAM beyond it is reported unused rather than fatal | ferrix-4f | ARMv7-A |

### P2 — quality and performance, on the "fast" half of the goal

| Item | Owner |
|---|---|
| Scoped user TLB shootdown: one page, only the processors running the space; part of the VMO reverse map | ferrix-e6, reviewed by ferrix-e5 |
| The cost of the 20 µs one-shot armed on every wake onto the caller's processor, measured on pipe and futex paths | ferrix-34 |
| Per-CPU frame and heap caches, deferred since stage 2 | open, once a workload can measure them |
| ASIDs and PCIDs, so a switch stops invalidating every user entry | open, after threads |
| x86-64 kernel-mode NMI, #DB and #MC on IST stacks with a paranoid entry | ferrix-e5 |
| `getrandom` seeded from virtio-rng into a real generator; a real-time clock read from the RTC and `/dev/rtc` | open |
| The debt the roadmap names: Miri for `frame`, `heap`, `paging`; fuzz targets for `cpio`, `fdt`, `acpi`, `virtio`, `linux-abi` | ferrix-e5 (the first three crates and `cpio`, `fdt`, `acpi`) |
| The host-test table in the roadmap generated from `cargo test --list` with a gate, instead of counted by hand | ferrix-24 |
| Zero-copy block reads: pin the page-cache pages themselves as the block ring's buffers, removing the data-VMO and scratch copies of stage 11's first read path (ARCHITECTURE §3) | ferrix-61, after stage 11's kernel mount |

### P3 — hardware variants and later stages, unowned

* GICv3 and its redistributors, with a second AArch64 boot configuration
  (`gic-version=3`); x2APIC; TSC-deadline. Real AArch64 hardware is GICv3.
* Networking, placed after stage 11: sockets, the net core, virtio-net.
* Stage 12 (btrfs write), 13 (namespaces, cgroups, seccomp), 14 (real-time
  domains). Stage 13 also carries global page-cache reclaim and an OOM kill,
  which `rustc` on a small machine needs and no stage names today.
* `/sys`, `/dev/rtc`, tmpfs `FS_IOC_GETFLAGS`: with stage 13's cgroupfs, the
  RTC driver, and never, respectively.
* `vfork` sharing memory rather than copying it.
* Huge pages; frame share and release are order 0 by design.

---

## Decisions

Dated, newest first. A decision here is final until the customer says otherwise.

* **2026-09-13 (customer)** `develop` is the landing branch and may be unstable;
  `main` moves only to a verified `develop` commit. Set up at 8342362.
* **2026-09-13 (customer)** Order of everything: first a working `main`, second
  ferrousli on the build with busybox rebuilt against it, third being surer
  that `main` is stable before it moves. This supersedes the earlier
  decision that ferrousli sits beside the roadmap: it now has a P0 row and
  its busybox is a `test-shell` target.

* **2026-09-13** Ferrousli is beside the roadmap, not on it: the goal's path is
  static musl, and ferrousli counts toward no stage. A landing touching only
  `ferrousli/` needs no boots. Its pthread tests become the first foreign
  threaded program once `CLONE_THREAD` exists.
* **2026-09-13** Stage 10's exit criterion requires the out-of-domain DMA fault
  on x86-64 and AArch64 only; ARMv7-A's virtio-pci runs in degraded trusted
  mode because U-Boot forces the SMMU bypass. A ring-3 driver reading sectors
  through an untranslated domain may land so stage 11 can proceed, but stage
  10 is not done until domains translate and the fault is shown.
* **2026-09-13** Stage 11's read stage refuses a volume with a log tree, with a
  clear message; log replay is stage 12's. The default subvolume, data
  checksums and a node cache are required before stage 11 is done.
* **2026-09-13** The page-cache interface, agreed between stages 8 and 11: a
  `PageSource` in `libs/vfs` with `fill_range(first, pages)` filling at least
  one page, stopping at an extent boundary, zeros past the file's size, called
  under no lock and allowed to block; a checksum failure is `EIO`, never a
  zeroed page; the kernel VMO allocates first, fills with no lock held and
  inserts only if the page is still absent; `read` reports `EIO` and a fault
  `SIGBUS`. Eviction and writeback are stage 12's. Order: the VMO reverse
  map, then `PageSource` with tmpfs over it, then file-backed `mmap`.
* **2026-09-13** A ring-3 driver serving a disk must never fault on a file
  mapping of that disk, or it waits on its own completion: its image and data
  come from initramfs, tmpfs, anonymous or ring VMOs, or are committed before
  any pivot onto btrfs. Stage 10's `devmgr` enforces it.
* **2026-09-13** The page cache is the inode's VMO, on tmpfs and on btrfs
  alike, and file-backed `mmap` and `read` share its pages. One interface,
  agreed between the stage 8 and stage 11 owners before either writes it.
* **2026-09-13** No doc gate compares quoted boot lines with a live log; they
  are illustrations and the roadmap says the numbers move. The host-test table
  becomes a generated document with a gate instead.
* **2026-09-13** CI's 120 s boot timeout stays. A quiet boot takes about 10 s;
  local runs on a loaded host use `--timeout 600`.
* **2026-09-13** (customer, relayed) Drivers stay in ring 3; no interim
  kernel-side disk path, even as a test harness.

---

## Waiting on the customer

* A USB-C unplug and replug per DK1 run: every run ends in PSCI `SYSTEM_OFF`,
  after which the reset button does nothing, and the ST-LINK's voltage sense
  and SWD are blind under this firmware, so the host cannot restart the board
  either. Stages 1–9 and `test-shell`'s script ran on it at `fd4442e`; typing
  and the UART drain were proven on it with `uart-rx`. The tag rerun on main,
  and the paste test of interrupt-driven receive, each need one more power
  cycle.
* The root filesystem: 1.7 TB of the 1.9 TB is outside Ferrix. The sessions
  can only keep their own build output down.
* Pushes to `origin`: local `main` is 60 commits ahead, and the last CI runs
  there failed in the Ubuntu test job before today's landings.

---

## Wind-down of 2026-09-13, about 17:00

Every session was asked to stop, commit and hand off. `main` is ec549f2,
verified whole (five boots, KVM, both busyboxes, test-vfs) and tagged
`stage-9.1-console-and-iommu`; `develop` is ahead of it by the stage 4
contended-count rounds (9f6a590, full row plus KVM by its owner), the FX-1001
row and the btrfs node cache. All branches and tags are on origin. Branches
with unlanded work, each committed and pushed, base and state as handed off:

* `board-reset-3c` (95884cd): `arch::reset`, `reboot` resets, `ferrix.onexit=reset`; both reviews in, board proof A passed; needs a rebase over ec549f2's `arch/mod.rs` export lists, the row, proof B (`exit 7` under the option; no press needed). Then the CMDLINE.TXT follow-up and `test-boot --reset`.
* `worktree-agent-a658b7811c6e2577d`: VMO reverse map with scoped shootdown; d5b515c passed the full row and review, two unbuilt WIP commits on top (review fixes, `protect` shootdown); needs squash, full row with KVM, re-review by the mm owner, then the stage 9 bullet.
* `worktree-agent-a74d4fa11607fe5ad`: `libs/vfs` `PageSource` and the offset-lock fix; passed on c979d03; needs rebase, light row, go. Follow-up: a sleeping lock for `Namespace::rename` before stage 11's kernel mount.
* `worktree-agent-a2d52ca1305875553` (a67f795): devfs block-device registry; full row passed on ec549f2; needs the stage 11 owner's trait review and a go.
* `stage10-ring` (32d29fb, WIP, never compiled): the block ring's kernel side; rebase onto c9677b7, drop the picked commits, wire the module and the native call, use the registry, write `user/blkring-check` on the runtime, full row.
* `worktree-agent-a16ff91583057388b` (24da536): virtio-blk library; Miri and fuzz passed; needs rebase and the full row.
* `worktree-agent-a102cb3140653d102` (f540994): native user-space runtime and `user/`; needs rebase and the full row.
* `worktree-agent-a317dadb0a679f86d` (5e5037a): the QEMU test disk; full row passed on a39ffe1; needs rebase and a go.
* `stage11-kernel-mount-design` (101dcd2): the approved design, draft 5, parked as a document.
* Stage 7 branches, all on origin: `stage7-startup-argument` (19f9171; b0ae3e2 passed the full row, the WIP StartClaim and `exec::load_native` on top are unverified; reviewer ferrix-4b), `stage7-init-pid1` (43e6b6c; rebase over `BUILT_IN_EXE`, then the row and a `sh -i` showing `$$`=1), `worktree-agent-aa758de17bccf6283` (2de6e28, the leak regression check; rebase and row), `worktree-agent-aa734e4c2fcfdceec` (8824e09, SA_RESTART and the signal-path boot check; three boots, KVM, both busyboxes and test-vfs still to run; SIGSEGV on the alternate stack still wants an end-to-end user program), `stage7-uname-ferrix` (72695f1, unverified: the customer chose `uname -s` = `Ferrix`; ferrousli's identity test and any config.guess-style build must follow). Not started: the terminal switch to `console::input`, threads (design in that session's memory; ferrousli's pthread test is ready).
* A lead from stage 7's sweep tooling (`enosys-sweep`, `worktree-agent-aa3e651b1a5027598`, neither for landing): the single-processor `timeout -s KILL` hang did not reproduce in 7 runs on current main; but a sweep kernel on armv7a `--smp 1` fails stage 8's pipes check 4 of 4 with one frame leaked while plain test-boot passes, logs in that session's scratchpad `hang/`; owner stage 8 or mm.
* Stage 9 (`ferrix-4b`): the interrupt wake (verified on fd4442e), process observers, `vmo_map` on the reverse map.
* mm (`ferrix-e5`): x86-64 IST branch (afb3041, row green but for one FX-1001 hit that is stage 10's), mprotect invalidation with the COW and MAP_SHARED checks, Miri and fuzz branch (c660bc7).
* ferrousli (`ferrix-ce`): 154 symbols were undefined at busybox's first link. Area 1's mounts, file system statistics and `mntent.h` are on develop; its other 57 wrappers (sockets and `inet_*`, SysV IPC, process and system calls) and the stub file are in progress; busybox links after them.
* Known intermittent on `main`: FX-1001 on AArch64 (a device write reported outside its domain, once in three boots), owner stage 10, row above.
* Open questions for the customer: none. The board is powered at the U-Boot prompt with 95884cd on the card.
