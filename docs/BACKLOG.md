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
| Anything the image contains: `kernel/`, `boot/`, a kernel-side crate in `libs/`, `xtask` | `cargo xtask check`, then `test-boot` on x86_64, aarch64, armv7a at four processors and armv7a at `--smp 2` |
| User mode, page tables, TLB, SMP or the scheduler | The row above, and x86_64 under `--accel kvm` |

Then fast-forward `main` only if it is still the commit rebased onto. If it
moved: when the commits that moved it touch none of the files the change
touches and nothing in `kernel/`, `boot/` or a kernel-side crate, re-run
`cargo xtask check` and the `armv7a --smp 2` boot and land; otherwise run the
row again. A boot that fails is a result to read, not a reason to retry: only
the stage 5 EEVDF-bound message is a known flake, and its log is kept.

**Worktrees.** One landing, one worktree. `git worktree remove` it once its
branch is on `main`. Check `df -h /` before a landing; after a failed commit
read `git log -1 --stat` before the next step, because a failed commit leaves
its files staged for the next one. On 2026-09-13 the root filesystem filled and
every session's gates failed at once; 70 worktrees held 108 GB of build output.

**Agents.** Gates and boots in the foreground, never `run_in_background`; one
architecture per tool call; the brief says so.

**Milestones.** The customer tests from `main`, so testable progress is tagged
there rather than waiting for a stage to end. The product owner verifies a
candidate from a clean worktree (`test-boot` on all three architectures and
armv7a at `--smp 2`, `test-shell` and `test-vfs` with the static busybox),
then tags it: `stage-N` for a stage's exit, `stage-N.k-<slug>` for a testable
step after it, annotated, with the tag message saying what to test and how.
Owners say in one line what a person can test when such a landing is on
`main`. Tags are local until the customer pushes; the customer is told in a
line or two and nothing stops for it.

**Calling a stage done.** The exit criterion as written, on all three
architectures, and the marker moves in the same commit. A criterion met in a
weaker form is written down as such in the stage's section.

---

## Owners

| Session | Area |
|---|---|
| ferrix-24 | Product owner: priorities, decisions, this file, cross-cutting debt |
| ferrix-91 | Stage 7: processes, signals, threads, tty, futex, time; the user entry path |
| ferrix-c2 | Stage 8: VFS, tmpfs, devfs, procfs, pipes, the fd and path calls; file-backed `mmap` |
| ferrix-2a | Stage 9: native ABI, objects, `vmo_map`, native process creation, process observers |
| ferrix-8b | Stage 10: PCI, IOMMU domains, `devmgr`, the block ring, virtio-blk in ring 3 |
| ferrix-4d | Stage 11: `libs/btrfs`, `libs/block`, the kernel mount; the native user-space runtime |
| ferrix-b9 | Scheduler: stage 5 flakes, lock convoys, wake latency, boot-time performance |
| ferrix-54 | Stage 1–7 review fixes; asked to take the memory-management package below |
| ferrix-b1 | The STM32MP157D-DK1 board, `xtask flash`, `docs/stm32mp157-dk.md`, the Arm UART drivers |
| ferrix-53 | `ferrousli/`, beside the roadmap |

---

## The path to the goal, in order

The goal is `rustc` on Ferrix (stage 16). Everything below is on its path and
is ordered by what blocks what. A row's owner is the session; "open" means
nobody has it yet.

### P0 — blocks the next stage or the goal

| Item | Owner | Why it is P0 |
|---|---|---|
| Threads: `clone(CLONE_VM\|CLONE_THREAD\|CLONE_SETTLS)` and everything a thread implies | ferrix-91 | `rustc` is threaded; the largest missing piece on the roadmap. Exit test: a static musl Rust `std::thread` program under `test-shell` on all three architectures |
| File-backed `mmap`: a file mapping maps the inode's own VMO pages, shared and private, with faults served from them | ferrix-c2 | `mmap` with a descriptor answers `ENODEV` today; `rustc` and the linker map rlibs. Same interface as the btrfs page cache |
| Ring-3 virtio-blk reading sectors: `VMO_PIN`, `devmgr`, the ring, the driver | ferrix-8b, with ferrix-4d and ferrix-2a | Stage 11's mount and exit wait on it; the customer declined a kernel-side disk path |
| `vmo_map` and native process creation | ferrix-2a | `devmgr` cannot start a driver without them |
| Stage 11's kernel mount, through the VFS, with file data in the inode's VMO | ferrix-4d | The read-only sysroot |

### P1 — required before a stage is called done

| Item | Owner | Stage |
|---|---|---|
| Translated `SMMUv3` domains, and the out-of-domain fault on x86-64 and AArch64 (VT-d translates since this landing; ARMv7-A is stated as degraded trusted mode in the exit criterion) | ferrix-8b | 10 |
| Reset on driver death, designed in the ring spec before the driver lands | ferrix-8b | 10 |
| Trusting a BAR firmware placed but did not enable | ferrix-8b | 10 |
| btrfs: honour the default subvolume; verify data checksums from the csum tree; parse `INODE_EXTREF`; a bounded metadata node cache | ferrix-4d | 11 |
| btrfs: CI Miri step for `libs/btrfs` and `libs/block` under 15 minutes, whole-image tests ignored under Miri | ferrix-4d | 11 |
| Process observers, so a port can watch a process end | ferrix-2a, reviewed by ferrix-91 | 9, for `devmgr` |
| An interrupt wakes its port waiter at once, not at the 5 ms recheck | ferrix-b9 | 9, on the virtio-blk latency path |
| `mprotect` leaves stale translations: `AddressSpace::protect` must invalidate, shown by a user-mode check | asked of ferrix-54 | 6 |
| A user-mode copy-on-write write check, and a `MAP_SHARED` write check | asked of ferrix-54 | 6 |
| `SA_RESTART`; boot checks that drive `SIGCHLD`, stop and continue, `alarm`, `sigaltstack` and fault-to-signal | ferrix-91 | 7; `rustc` needs `SIGSEGV` on the alternate stack |
| Pid 1 for init and orphans reparented to it | ferrix-91 | 7, 15 |
| `TCGETS2`, `fcntl` record locks, `flock` | ferrix-91 | 7 |
| `mount -t proc` and `devtmpfs` | ferrix-c2 | 8 |
| An "applets" group in `test-vfs`, separate from the exit criterion | ferrix-c2 | 8 |
| The EEVDF fairness flake, root-caused; then a preemption count so no plain spin lock taken with interrupts on can convoy | ferrix-b9 | 5 |
| FX-0601 "reserving a thousand pages cost a frame", about one x86-64 boot in three under KVM: a reap landing inside stage 6's frame-count window; the check must settle first, as the path check does | ferrix-b9 | 6 |
| A regression check that exited programs give every frame back once reaped (the leak 0510a8a fixed) | ferrix-91 | 7 |
| `xtask flash --init`; `docs/stm32mp157-dk.md` brought up to what the card actually runs; the first hardware run of stages 6–9 | ferrix-b1 | ARMv7-A |
| Console receive by interrupt on the PL011 and the STM32 USART, retiring the 20 ms polling thread | ferrix-b1, with ferrix-91 | 7, 15 |
| ARMv7-A with 2 GiB does not boot: the loader must allocate below the direct map's ceiling, and RAM beyond it is reported unused rather than fatal | ferrix-b1 | ARMv7-A |

### P2 — quality and performance, on the "fast" half of the goal

| Item | Owner |
|---|---|
| Scoped user TLB shootdown: one page, only the processors running the space | asked of ferrix-54 |
| The cost of the 20 µs one-shot armed on every wake onto the caller's processor, measured on pipe and futex paths | ferrix-b9 |
| Boot time: stage 3's tick count at 250 ms, a per-check cost line, an idle mask so a spawn stops broadcasting, one shootdown per reap batch | ferrix-b9 |
| Timestamps in xtask's serial logs | ferrix-b9 |
| Per-CPU frame and heap caches, deferred since stage 2 | open, once a workload can measure them |
| ASIDs and PCIDs, so a switch stops invalidating every user entry | open, after threads |
| x86-64 kernel-mode NMI, #DB and #MC on IST stacks with a paranoid entry | asked of ferrix-54 |
| `mremap` below 64 KiB answering `EPERM`; `AT_HWCAP` rechecked against real headers | asked of ferrix-54 |
| `getrandom` seeded from virtio-rng into a real generator; a real-time clock read from the RTC and `/dev/rtc` | open |
| The debt the roadmap names: Miri for `frame`, `heap`, `paging`; fuzz targets for `cpio`, `fdt`, `acpi`, `virtio`, `linux-abi` | asked of ferrix-54 (the first three crates and `cpio`, `fdt`, `acpi`) |
| The host-test table in the roadmap generated from `cargo test --list` with a gate, instead of counted by hand | ferrix-24 |

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

* The DK1 board has no power (ST-LINK reads 0 V); stages 6–9 have never run on
  hardware.
* The root filesystem: 1.7 TB of the 1.9 TB is outside Ferrix. The sessions
  can only keep their own build output down.
* Pushes to `origin`: local `main` is 60 commits ahead, and the last CI runs
  there failed in the Ubuntu test job before today's landings.
