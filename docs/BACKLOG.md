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

Then fast-forward `main` only if it is still the commit rebased onto.

**The landing queue.** Image changes queue; documentation, ferrousli and
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
| ferrix-ce (was ferrix-53) | `ferrousli/`, beside the roadmap. Done: locales and wide characters; time zones, `strftime` and `strptime`; buffered stdio and `printf`. In progress: `scanf`, `dirent` and `getopt`, threads, the math library |

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
| Ring-3 virtio-blk reading sectors: `devmgr`, the ring, the driver (`VMO_PIN` is on main) | ferrix-d9, with ferrix-61 | Stage 11's mount and exit wait on it; the customer declined a kernel-side disk path |
| `vmo_map` and native process creation | ferrix-4b, space.rs half reviewed by ferrix-e5 | `devmgr` cannot start a driver without them |
| Stage 11's kernel mount, through the VFS, with file data in the inode's VMO | ferrix-61 | The read-only sysroot |

### P1 — required before a stage is called done

| Item | Owner | Stage |
|---|---|---|
| Translated `SMMUv3` domains, and the out-of-domain fault on x86-64 and AArch64 (VT-d translates since this landing; ARMv7-A is stated as degraded trusted mode in the exit criterion) | ferrix-d9 | 10 |
| Reset on driver death, designed in the ring spec before the driver lands | ferrix-d9 | 10 |
| Trusting a BAR firmware placed but did not enable | ferrix-d9 | 10 |
| btrfs: verify data checksums from the csum tree; parse `INODE_EXTREF`; a bounded metadata node cache | ferrix-61 | 11 |
| btrfs: CI Miri step for `libs/btrfs` and `libs/block` under 15 minutes, whole-image tests ignored under Miri | ferrix-61 | 11 |
| Process observers, so a port can watch a process end | ferrix-4b, reviewed by ferrix-a5 | 9, for `devmgr` |
| An interrupt wakes its port waiter at once, not at the 5 ms recheck | ferrix-34 | 9, on the virtio-blk latency path |
| `mprotect` leaves stale translations: `AddressSpace::protect` must invalidate, shown by a user-mode check | ferrix-e5 | 6 |
| A user-mode copy-on-write write check, and a `MAP_SHARED` write check | ferrix-e5 | 6 |
| `SA_RESTART`; boot checks that drive `SIGCHLD`, stop and continue, `alarm`, `sigaltstack` and fault-to-signal | ferrix-a5 | 7; `rustc` needs `SIGSEGV` on the alternate stack |
| Pid 1 for init and orphans reparented to it | ferrix-a5 | 7, 15 |
| FX-0601 "reserving a thousand pages cost a frame", about one x86-64 boot in three under KVM: a reap landing inside stage 6's frame-count window; the check must settle first, as the path check does | ferrix-34 | 6 |
| A regression check that exited programs give every frame back once reaped (the leak 0510a8a fixed) | ferrix-a5 | 7 |
| The last console line before power-off is lost on the DK1: `stm32_usart` waits for TXE, never TC, so `arch::shutdown`'s PSCI `SYSTEM_OFF` cut "the shell exited with 7" mid-byte on 2026-09-13. Drain the transmitter (TC; the PL011's `FR.BUSY`) on the shutdown path of every UART; then the board reruns `test-shell`'s script and expects that line byte for byte | ferrix-4f | ARMv7-A |
| Console receive by interrupt on the PL011 and the STM32 USART, retiring the 20 ms polling thread | ferrix-4f, with ferrix-a5 | 7, 15 |
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

* A reset button press per DK1 run: the ST-LINK's voltage sense and SWD are
  blind under this firmware, so the host cannot reset the board. Stages 1–9
  and `test-shell`'s script ran on it at `fd4442e`; the rerun after the UART
  drain fix, and the interactive shell once console receive lands, each need
  one more press.
* The root filesystem: 1.7 TB of the 1.9 TB is outside Ferrix. The sessions
  can only keep their own build output down.
* Pushes to `origin`: local `main` is 60 commits ahead, and the last CI runs
  there failed in the Ubuntu test job before today's landings.
