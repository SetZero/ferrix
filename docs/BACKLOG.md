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
| Only `docs/` | `cargo xtask check` |
| Only `ferrousli/` | `cargo xtask check --ferrousli`, then `cargo xtask busybox` and, with the busybox it built, `test-shell` and `test-vfs` on x86_64 with `--init ferrousli`, so the binary the gates run never lags the library |
| `libs/` only, and no crate the kernel builds | `cargo xtask check`, and one boot: `test-boot --arch armv7a --smp 2` |
| Anything the image contains: `kernel/`, `boot/`, a kernel-side crate in `libs/`, `xtask` | `cargo xtask check`, then `test-boot` on x86_64, aarch64, armv7a at four processors and armv7a at `--smp 2`; a stage 7 or 8 change also runs `test-shell` on x86_64 with the ferrousli busybox (`--init ferrousli`), the musl busybox *and* the host's glibc busybox (`/usr/bin/busybox`), and `test-vfs` on x86_64 with the ferrousli busybox and the musl one. The whole of that, plus KVM, is what moves `main` |
| User mode, page tables, TLB, SMP or the scheduler | The row above, and x86_64 under `--accel kvm` |

Then fast-forward `develop` only if it is still the commit rebased onto.

**The busyboxes.** The busybox built against ferrousli is the primary one: the
userland Ferrix is measured with, and the one every `test-shell` and
`test-vfs` above names first. `cargo xtask busybox` builds and installs it and
`--init ferrousli` runs it; the flag is still given explicitly, since xtask
has no default program. Alpine's static musl busybox and the host's glibc
busybox stay required in every gate that names them, as the compatibility
checks: a failure on any of the three fails the gate, and nothing was dropped
when ferrousli's joined. `--init ferrousli` runs whatever `cargo xtask busybox`
last installed, so on a base whose `ferrousli/` differs from the one the
binary was built from, rebuild it first.

**After the fast-forward,** the lander boots `develop` itself once on x86_64
under `--accel kvm` and reports the hash together with that result, so a bad
merge is seen by the one who made it.

**Two branches.** Sessions land on `develop`, which may be unstable. `main`
moves only by the product owner, fast-forward, to a `develop` commit that has
passed the whole matrix from a clean worktree on the product owner's own run:
the four boots, x86_64 under KVM, `test-shell` with the ferrousli, the musl
and the glibc busybox, and `test-vfs` with the ferrousli and the musl busybox;
at least every two hours while `develop` moves. So
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
armv7a at `--smp 2`, `test-shell` and `test-vfs` with the ferrousli busybox and
the musl one),
then tags it: `stage-N` for a stage's exit, `stage-N.k-<slug>` for a testable
step after it, annotated, with the tag message carrying short release notes as a bullet
list and saying what to test and how; the same notes go into
`docs/RELEASES.md`.
Owners say in one line what a person can test when such a landing is on
`main`. Tags are local until the customer pushes; the customer is told in a
line or two and nothing stops for it.

**Estimates are story points.** Since the evening of 2026-09-13 (customer) a
session says what is left in story points, never in hours or days: 1 is a
change whose pattern and tests already exist, 13 a new subsystem, Fibonacci
between. The product owner measures points into time afterwards, from the
landings, and never the other way round.

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
| os-23 (was ferrix-32, ferrix-24) | Product owner: priorities, decisions, this file, milestones, cross-cutting debt |
| os-b6 (was ferrix-a5, ferrix-91) | Stage 7, its branches: processes, signals, tty, futex, time; the user entry path; pid 1 |
| os-9f (new) | Stage 7, threads: `clone(CLONE_THREAD)` and everything a thread implies; the `fs/terminal.rs` switch to `console::input` |
| os-c4 (was ferrix-e6, ferrix-c2) | Stage 8: VFS, tmpfs, devfs, procfs, pipes, the fd and path calls; file-backed `mmap` and the VMO reverse map |
| os-94 (was ferrix-4b, ferrix-2a) | Stage 9: native ABI, objects, `vmo_map`, native process creation, process observers |
| os-5b (was ferrix-d9, ferrix-8b) | Stage 10: PCI, IOMMU domains, `devmgr`, the block ring, virtio-blk in ring 3; FX-1001 |
| os-96 (was ferrix-61, ferrix-4d) | Stage 11: `libs/btrfs`, `libs/block`, the kernel mount; the virtio-blk library, the native user-space runtime, the QEMU test disk |
| os-a6 (was ferrix-e5, ferrix-54) | The memory-management package below; reviewer of the VMO reverse map and the space.rs halves of `vmo_map` and file-backed mmap; the scheduler rows (was ferrix-34), since no scheduler session was spawned |
| os-87 (was ferrix-3c, and ferrix-4f's area) | The STM32MP157D-DK1 board: the serial link, OpenOCD, every hardware run, filing what the board shows with the area that owns it; `xtask flash` and `deploy`, `docs/stm32mp157-dk.md`, the Arm UART drivers, the x86-64 console interrupt route |
| os-7c (was ferrix-ce, ferrix-53) | `ferrousli/`, on the roadmap by the P0 row below. Done: locales and wide characters; time zones, `strftime` and `strptime`; buffered stdio and `printf`; threads, mutexes, condition variables and keys, with `tests/c/thread/on_ferrix.c` ready as a static threaded program for `CLONE_THREAD`; `dirent.h` and `getopt`; mounts, file system statistics and `mntent.h`; sockets and the address conversions, System V IPC, and Linux's own process and system calls; `termios.h`; `system`, `popen`, temporary files, `realpath`, the `execl` family and `daemon`; the user, group and shadow databases; `syslog.h`, and `utmpx.h` with musl's empty records; the `scanf` family. In progress: termios (area 2); then pwd/grp/shadow, crypt, utmpx and syslog (area 3) and scanf, popen, mkstemp, realpath (area 4), split across sessions when they exist; the wrappers and stubs busybox links against; `scanf`, `glob` and `regex`, thread cancellation and semaphores, the math library |

The fleet restarted on the customer's Windows machine on the evening of
2026-09-13. `cargo` builds there, but every boot, the KVM row, `test-shell`,
`test-vfs`, Miri, fuzz and the board run on the Linux host over `ssh`, in a
worktree of the session's own; a branch reaches that host only through
`origin`, and pushes are the customer's to authorise, asked in the session
that needs one.

**`develop` is `origin/develop` and nothing else.** With two machines there
is one landing branch, the one on `origin`: a landing is
`git push origin <branch>:develop`, which refuses anything but a
fast-forward, made with the customer's word for the push. A local `develop`
on either machine is a mirror to fetch, never a branch to land into; the
Windows root checkout keeps `main` checked out and never lands `develop`
through its index, and nazuna's checkout is the same.

---

## The path to the goal, in order

The goal is `rustc` on Ferrix (stage 16). Everything below is on its path and
is ordered by what blocks what. A row's owner is the session; "open" means
nobody has it yet.

### P0 — blocks the next stage or the goal

| Item | Owner | Why it is P0 |
|---|---|---|
| Threads: `clone(CLONE_VM\|CLONE_THREAD\|CLONE_SETTLS)` and everything a thread implies. In: a `Thread` per task, signal state split between process and thread, `exit` apart from `exit_group` with release on the last thread, and `clone(CLONE_THREAD)` (ferrousli's static pthread test passes on x86-64). Left: signals, stop, `kill` and `execve` across threads (5 points; `execve` answers `EAGAIN` with more than one live thread until then); the futex lock kind, the TLB and `munmap` race and the `brk`-vs-fork race (3); `/proc` threads and the exit test on three architectures (4) | threads (os-9f) | `rustc` is threaded; the largest missing piece on the roadmap. Exit test: a static musl Rust `std::thread` program under `test-shell` on all three architectures |
| File-backed `mmap`: a file mapping maps the inode's own VMO pages, shared and private, with faults served from them. In order: (1) the VMO reverse map with scoped shootdown, landed; (2) `libs/vfs`'s `PageSource` over tmpfs, with no open file's lock held across an inode call, landed; (3) the kernel VMO filling from a source, landed ahead of the mappings because stage 11's kernel mount needs only it; (4) file mappings on it: `MAP_SHARED` writing through, with `msync`, `SIGBUS` past the end and the boot check both ways, landed; (5) `MAP_PRIVATE` copying into a shadow object of its own on first write, with reads served from the file's VMO until then, a truncation taking the copies past the cut, and checks from the kernel and from user mode, landed | stage 8 (os-58); mm reviewer (os-a0) for the space.rs half | `rustc` and the linker map rlibs. Same interface as the btrfs page cache; stage 11's kernel mount needed only (3), and `rustc` needs (4) and (5) |
| The out-of-domain probe on x86-64 under KVM on a loaded host, once in a few boots: VT-d's single fault record holds a fault other than the probe's, so the probe reports nothing and `test-boot` refuses the boot while the kernel reaches its marker. Read on 2026-09-14 (nazuna `~/.local/share/ferrix/logs/oodprobe/split-86eed16-kvm-1.log`, load about 5): the record held stream 0x0, page 0x1000, a write, where the probe is stream 0x10 at page 0x1000, and QEMU logged "New fault is not recorded due to compression of faults", so a fault attributed to source 0 landed on the probe's IOVA first and the probe's own was dropped. Nothing at 00:00.0 should DMA; next: find what QEMU attributes to source 0 at that IOVA under load (a stale translation of an earlier domain, or a DMA QEMU issues without a requester), then a fix with a negative control | os-5b | It can fail main's verification, as FX-1001 could |

### P1 — required before a stage is called done

| Item | Owner | Stage |
|---|---|---|
| POSIX.1-2024 interface sweep: from musl's implementation of every mandatory POSIX.1-2024 function, the list of Linux system calls (and flags) they need; the stage 7 sweep tooling runs each on all three architectures and files every `ENOSYS`, `EINVAL` on a mandatory flag, or wrong result with the area that owns it, as rows here. Sockets and threads are known and excluded; the `epoll`, `eventfd`, `timerfd` and `signalfd` families are in scope because stage 17 needs them | os-b6, after its five branches | 7, 8, 17 |
| `AF_UNIX` sockets: `socket`, `socketpair`, `bind`, `listen`, `accept`, `connect`, `send*`/`recv*` with `SCM_RIGHTS`, `shutdown`, `getsockopt` for what busybox, POSIX and Wayland need; no net core, no `AF_INET`. On the compositor's path (stage 17) as well as POSIX's | open — wants a session of its own | 7, 17 |
| `memfd_create` with `F_ADD_SEALS`/`F_GET_SEALS`, on tmpfs, after file-backed `mmap`: `wl_shm` is a sealed memfd both sides map | os-c4, after the chain | 8, 17 |
| Trusting a BAR firmware placed but did not enable | ferrix-d9 | 10 |
| btrfs: CI Miri step for `libs/btrfs` and `libs/block` under 15 minutes, whole-image tests ignored under Miri | ferrix-61 | 11 |
| A two-last-threads exit check that provably races: spin-meet on two processors, with its negative control -- the old last-thread decision put back -- failing by name. Today's check passes that control too, so it shows only that such a process ends with its first thread's status (from stage 9's review of threads commit 4). 1 point | threads (os-9f) | 7 |
| End-to-end user programs for what the `sigpaths` check proves at the kernel's decision: a `SIGSEGV` caught on the alternate stack, and a read interrupted by a handler and restarted under `SA_RESTART` (`SA_RESTART` and the driven signal paths have landed) | ferrix-a5 | 7; `rustc` needs `SIGSEGV` on the alternate stack |
| Stage 6's reverse-map check reported "-4 frames not given back" once in two loaded musl `test-vfs` runs on 2026-09-14: four frames — one kernel stack — came back from outside its window. Cause confirmed by a marker-gated control (the check's own warm-up task, which posts its outcome and exits, reaped inside the measured window because `wait_until_reaper_quiet` counts only tasks already exited). Fix: `run_pinned` waits for its task to be dead before returning, landing with `MAP_PRIVATE`, the control showing zero after it. The threads checks' returning before their programs' spaces are dropped is a separate, real hazard shared with `exec::run`: P2 | os-58 (fix), os-43 (control) | 6 |
| Retire the 20 ms console polling. Receive by interrupt into a 4 KiB ring has landed on all three: the PL011, the STM32 USART (through ST's EXTI on the DK1), and x86-64's 16550 through an I/O APIC input found from the MADT, with `console::input::{waiters, has_input}` for the console thread to wait on. Left: the console thread and readers waiting on it instead of sleeping (in `fs/terminal.rs`) | ferrix-a5 | 7, 15 |
| `AF_UNIX` names and descriptor passing, the two landings after socket pairs: `bind`, `listen`, `accept`, `connect` over a listener table keyed by inode, abstract names included; then `SCM_RIGHTS` with the in-flight cycle pass, and `SO_PASSCRED` credentials. Carried from landing 1's review: cap a receive's kernel buffer at the receive capacity (`MSG_WAITALL` in chunks of it), drop a refused file outside the `FdTable` lock before descriptors travel, `SO_SNDBUFFORCE`/`SO_RCVBUFFORCE` needing `CAP_NET_ADMIN` and skipping the cap, and Linux's error order in `sendmsg` and `socketpair`. On the compositor's path (stage 17) as well as POSIX's | os-b6, fd/VFS half reviewed by os-c4 | 7, 17 |
| POSIX.1-2024 interface sweep: from musl's implementation of every mandatory POSIX.1-2024 function, the list of Linux system calls (and flags) they need; the stage 7 sweep tooling runs each on all three architectures and files every `ENOSYS`, `EINVAL` on a mandatory flag, or wrong result with the area that owns it, as rows here. Sockets and threads are known and excluded; the `epoll`, `eventfd`, `timerfd` and `signalfd` families are in scope because stage 17 needs them | os-b6, after `AF_UNIX` and the terminal switch | 7, 8, 17 |
| `mprotect` can make a shared mapping writable that its file never allowed: a read-only `MAP_SHARED` mapping of a file opened read-only takes `PROT_WRITE` and writes the file, where Linux answers `EACCES` for a mapping without `VM_MAYWRITE`. Found in the design of `memfd` seals (2026-09-14); fixed by their accounting, which records whether each shared file mapping may write and has `AddressSpace::protect` refuse the rest | stage 8 (os-58), with `memfd_create` | 8 |
| `memfd_create` with `F_ADD_SEALS`/`F_GET_SEALS`, on tmpfs, after file-backed `mmap`: `wl_shm` is a sealed memfd both sides map | stage 8 (os-58), after the release; design agreed with mm, 5 points | 8, 17 |

### P2 — quality and performance, on the "fast" half of the goal

| Item | Owner |
|---|---|
| The cost of the 20 µs one-shot armed on every wake onto the caller's processor, measured on pipe and futex paths | ferrix-34 |
| Per-CPU frame and heap caches, deferred since stage 2 | open, once a workload can measure them |
| ASIDs and PCIDs, so a switch stops invalidating every user entry | open, after threads |
| `getrandom` seeded from virtio-rng into a real generator; a real-time clock read from the RTC and `/dev/rtc` | open |
| The debt the roadmap names: fuzz targets for `virtio`, `linux-abi` | open |
| The debt the roadmap names: Miri for `frame`, `heap`, `paging`; fuzz targets for `cpio`, `fdt`, `acpi`, `virtio`, `linux-abi` | ferrix-e5 (the first three crates and `cpio`, `fdt`, `acpi`) |
| Every gate's log names the tree it ran on: `xtask` prints `HEAD`, the branch and whether the tree was clean as the first line of every `check`, `test-boot`, `test-shell` and `test-vfs` log, so a row's evidence pins its commit by itself rather than by the runner's word (asked for by a review of the frame-window evidence, 2026-09-13) | open, cross-cutting |
| The host-test table in the roadmap generated from `cargo test --list` with a gate, instead of counted by hand | ferrix-24 |
| The POSIX measure: musl's libc-test functional and conformance programs built static against musl and against ferrousli, run under `test-shell` on all three architectures, with the pass count in the roadmap's host-test table and every failure filed with its owner | os-7c, with os-9f once threads run | 
| `futex.rs` holds `TABLE` across `read_word`, a read fault; safe only while a read fault never invalidates and frame allocation has no reclaim. Stage 13's reclaim makes it a bug: read the word before taking the table, or retry after. From the 2026-09-13 spin-lock audit | os-a6, before stage 13's reclaim |
| Zero-copy block reads: pin the page-cache pages themselves as the block ring's buffers, removing the data-VMO and scratch copies of stage 11's first read path (ARCHITECTURE §3) | ferrix-61, after stage 11's kernel mount |
| ferrousli's busybox beyond the gates' applets: the 30 stubs in `ferrousli/src/stubs.rs` (regex for `grep` and `sed` patterns busybox does not handle itself, the math functions `awk` calls, name resolution with interface and Ethernet lookups), each ending the program when an applet reaches it; and `crypt`'s traditional DES and `$2*$` blowfish hashes, which return `"*"` | open |

### After `rustc` — the compositor's path, unowned until stage 16 is near

| Item | Stage |
|---|---|
| Display core + ring-3 virtio-gpu driver, `/dev/dri/card0` with dumb buffers, atomic page flip and vblank; `xtask` reading QEMU's screendump | 17 |
| Input core + ring-3 virtio-input driver as evdev; QEMU monitor input injection in `xtask` | 17 |
| The compositor workspace: Smithay base (assumed), CPU rendering, dwindle and master, `hyprland.conf`, `hyprctl` IPC, two Rust test clients | 18 |
| GPU path decision, then `renderD128`, dmabuf, GBM, animations, blur and rounding | 19 |
| The DK1's LTDC display and USB HID as the hardware variant of stage 17 | 17, P3 |

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

* **2026-09-14** The busybox built against ferrousli is the primary busybox:
  the userland Ferrix is measured with, first in every `test-shell` and
  `test-vfs` the gates run. The musl and glibc busyboxes stay required as
  compatibility checks. A `ferrousli/` landing rebuilds it and runs both with
  it. This carries out the customer's 2026-09-13 order below once the binary
  passed both, at 5e9b0b6 with no stub reached.

* **2026-09-13 (customer)** The goal after `rustc` is a Hyprland-shaped
  Wayland compositor, written in Rust, running on Ferrix. Roadmap stages 17
  (display and input), 18 (the compositor) and 19 (Hyprland fidelity and the
  GPU) carry it; self-hosting moves to stage 20. Pulled onto the path by it:
  `AF_UNIX` with `SCM_RIGHTS` (from networking), `memfd_create` with sealing
  and `MAP_SHARED` file mappings (stage 8), and the `epoll`, `eventfd`,
  `timerfd` and `signalfd` families (the POSIX sweep files them). Three
  choices are the customer's, written into stages 18 and 19 as assumptions
  until made: Smithay as the compositor base rather than from scratch;
  `xkbcommon` as the one C library at stage 18; Mesa on ferrousli versus a
  Rust GPU path at stage 19.
* **2026-09-13 (customer)** POSIX.1-2024 compatibility is a goal, on the
  condition that it never breaks Linux compatibility. Ferrix takes POSIX
  through its libc over the Linux ABI (ARCHITECTURE §2), so the goal costs
  the kernel nothing new in kind: every mandatory POSIX.1-2024 interface is a
  Linux system call the kernel must answer as Linux does, and the libc side is
  ferrousli's. Where POSIX and Linux differ, Linux wins. Rows: threads (P0),
  the POSIX interface sweep and `AF_UNIX` sockets (P1), libc-test as the
  measure (P2). `AF_INET` stays with the networking stage.
* **2026-09-13 (customer)** Ferrousli is to replace the musl and glibc
  busyboxes as the userland Ferrix is measured with, as fast as it can be
  done: once its busybox passes `test-shell` it becomes the primary binary of
  that gate, with musl and glibc kept as the compatibility checks. Static musl
  stays the goal path to `rustc`, whose `std` targets it.
* **2026-09-13 (PO)** `vmo_map` refuses executable mappings in its first
  landing, and the roadmap records that under stage 9's "Left for later
  stages", not as done: an EXECUTE right on the VMO handle comes with
  `process_create`'s native loader, its first consumer.
* **2026-09-13 (PO)** The DK1 reset follow-ups, as specified: the loader reads
  `bootargs` from a `CMDLINE.TXT` on the ESP so `ferrix.onexit=reset` survives
  a reset without U-Boot's `saveenv`; `test-boot --reset` boots with that option
  and requires QEMU to show a reset, not a power-off. Both go into
  `docs/stm32mp157-dk.md` with the landing.
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

* The DK1's link is the ST-LINK on the Windows machine (COM8) since the
  evening of 2026-09-13; the stage-9.1 tag, the paste test and
  `board-reset-3c`'s proof B all passed on it. With `ferrix.onexit=reset`
  the board returns to U-Boot by itself, so a run no longer costs a USB-C
  replug; only a hang still does.
* The root filesystem: 1.7 TB of the 1.9 TB is outside Ferrix. The sessions
  can only keep their own build output down.
* Pushes to `origin`: local `main` is 60 commits ahead, and the last CI runs
  there failed in the Ubuntu test job before today's landings.

---

## Wind-down of 2026-09-14, about 17:00

The fleet ran on the Windows machine from the evening of 2026-09-13 (gates on
the Linux host over ssh) until about 22:30, went quiet, and was renamed again
by a restart; the customer wound it down on 2026-09-14 with the order to
finish, not start. `main` is still ec549f2 (`stage-9.1-console-and-iommu`);
`develop` is 73b17da, 63 commits ahead: 112 story points landed on the
evening of the 13th, the largest of them the VMO reverse map, the kernel VMO
fill, the block registry, the block ring's kernel side, `vmo_map`, native
process creation, threads commits 1–3, the stage 11 stack (native runtime,
test disk, virtio-blk library, SleepLock) and the kernel btrfs mount. Every
one of those updated its rows and booted `develop` under KVM after its push.

**`main` did not move.** `develop` 896ede5 passed the product owner's whole
matrix from a clean worktree (21:37), but a boot check that can fail plain
(FX-0820, the frame-count window, 1 in 16 unloaded boots) was not to be
tagged into `main`'s CI; the frame-window landing below is its gate. The
next product owner verifies the first `develop` that carries it and tags
`stage-9.2`.

**Stage 10's exit criterion is met but not landed.** `/sbin/blk`, a real
user-mode virtio-blk driver started from the boot check, served `vda` through
the block ring with VT-d translating and read 21 sectors back exactly as
`xtask` wrote them, on the same boot that shows the out-of-domain fault
(x86-64 TCG, 2026-09-13 ~22:20). Its landing moves the marker to
`stages 1-10`; the Arm rows were still running at the restart.

**Ferrousli's busybox links** (2026-09-14): 0 undefined symbols after crypt;
in WSL `sh`, `ls`, `grep`, `sed`, `awk` and `ps` run and `mkpasswd` matches
OpenSSL. **Its first boot on Ferrix (2026-09-14, x86-64, `test-shell`):**
it boots as init and passes three of the script's lines (`the sum is 15`,
`hello, ferrix`, `test agrees`), then ash's `case` reaches the `fnmatch`
stub and exits 134. The unlanded `ferrousli-misc` WIP (e95d1e9) carries a
tested `fnmatch`: one point between this busybox and the whole script.

Branches with unlanded work, where they live (W = this Windows checkout's
`.claude/worktrees/`, N = nazuna's repo, O = origin), state as handed off,
and the landing order the product owner had set:

1. `stage11-driver` 5b4b436 (W, N; stage 11): `libs/blkserve`, `user/blk`
   and the boot check that starts it; x86-64 whole row was green at b2abda9,
   the Arm results were pending; `stage10-exit-docs` edb91ea (stage 10's
   exit paragraph and the marker move) merges on top before the push, both
   owners co-sign. Needs a push word. **Lands first.** Driver points left:
   0 of 8 once green; the stage 11 exit test (btrfs image on the second
   disk, mounted in `test-vfs`) is 5.
2. `stage10-quiesce` 705f106 (W, N; stage 10): a quiesce on a served device
   whose driver's end has closed waits, bounded, for the ring to let go; the
   check quiesces the instant the driver dies. Verified on eb3588d. Lands
   right after the driver, whole row.
3. `stage8-framewindow` / `os-c4/framewindow` 02a0da0 (W, N; stage 8): every
   frame-counted window counts free frames plus heap-held pages and prints
   the route on a mismatch; approved by mm and stage 9; evidence on 9706926:
   whole row, 10 plain and 5 loaded KVM boots, negatives in all 9 windows.
   Needs a rebase, a row and a push word. **`main`'s gate.**
4. `stage9/channel-read-topology` 9f1ebe3 (W, O; stage 9): reviewed and
   controlled; re-runs its whole row after 3 (its one red cell was FX-0820).
5. `mm/stack` 0cdd2e6 (W only, on 95b538c; mm): shootdown-locks,
   mprotect checks (waits on 3 as well), the x86-64 IST branch, Miri/fuzz —
   ten commits, gate-green locally, never rowed on nazuna, never pushed.
   **Only this machine holds it.**
6. `land-3c` d30e397 (W; board): the 2 GiB allocation, uart-irq, the
   trampoline pair, board-reset-3c, the DK1 docs; all proven on the board
   (tag run, proof B, the trampoline boot); its go was live at the wind-down
   with a scoped push word.
7. `stage7-threads` 4b8b70a/47bd665 (W, N; threads): commit 4,
   `clone(CLONE_THREAD)`, runs ferrousli's pthread test ("pthread: all ok")
   on x86-64; in review with the check rework; commits 5–7 (signals across
   threads, futex and races, procfs and the exit test) are 12 points.
8. `stage7-unix-l1` b4db5dc/53ec4ac (W, N; sockets): `AF_UNIX` landing 1 of
   3 in progress; L2 names 5, L3 descriptor passing with the in-flight
   cycle pass 7; carries the P1 rows for `AF_UNIX`, the POSIX.1-2024 sweep,
   `memfd_create` and the spawn split.
9. `stage7-spawn-split` 5cfdaa6 (W; stage 7/9): not started; `process_start`
   must not fail after moving the bootstrap; 2 points, after threads 3 (in).
10. `stage8-filemmap` 52fee32 (W; stage 8): MAP_SHARED, 7 of 8 cells green,
    the red one is FX-0601 from growing `AddressSpace`, which 3 makes
    honest; MAP_PRIVATE (8) follows; `memfd_create` after.
11. `stage11-fpos` e6d9c9a (W; stage 11): approved by both reviewers, 3
    points, after the stack (in); the CI Miri step (2) after it.
12. `xtask-busybox` 78013c3 (W, N): `cargo xtask busybox` and
    `--init ferrousli`, row green on 73b17da, waits on a push word and a go
    after 6.
13. `ferrousli-crypt` dcc9adb (W, N): crypt and the `-r` link fix; lands on
    its own gate, no slot.
14. devmgr the program (stage 10, 13 points plus the quiesce race, not
    started; `docs/DEVMGR.md` is the design), BAR trust (8), the VT-d
    stale-record intermittent (P0 row): stage 10's "still to do" after its
    exit.
15. Stage 12 (btrfs write) has no session yet; its host-side half can start
    any time by the continuous rule.

The points ledger, with every estimate and landing timed, is in the product
owner's memory (`ferrix-points-ledger`); the measured pace was about 112
points across nine sessions in the four hours the queue ran.

## Wind-down of 2026-09-13, about 17:00

Every session was asked to stop, commit and hand off. `main` is ec549f2,
verified whole (five boots, KVM, both busyboxes, test-vfs) and tagged
`stage-9.1-console-and-iommu`; `develop` is ahead of it by the stage 4
contended-count rounds (9f6a590, full row plus KVM by its owner), the FX-1001
row and the btrfs node cache. All branches and tags are on origin. Branches
with unlanded work, each committed and pushed, base and state as handed off:

* `stage10-ring` (32d29fb, WIP, never compiled): the block ring's kernel side; rebase onto c9677b7, drop the picked commits, wire the module and the native call, use the registry, write `user/blkring-check` on the runtime, full row.
* `worktree-agent-a16ff91583057388b` (24da536): virtio-blk library; Miri and fuzz passed; needs rebase and the full row.
* `worktree-agent-a102cb3140653d102` (f540994): native user-space runtime and `user/`; needs rebase and the full row.
* `worktree-agent-a317dadb0a679f86d` (5e5037a): the QEMU test disk; full row passed on a39ffe1; needs rebase and a go.
* `stage11-kernel-mount-design` (101dcd2): the approved design, draft 5, parked as a document.
* Stage 7 branches, all on origin: `stage7-startup-argument` (19f9171; b0ae3e2 passed the full row, the WIP StartClaim and `exec::load_native` on top are unverified; reviewer ferrix-4b), `stage7-init-pid1` (43e6b6c; rebase over `BUILT_IN_EXE`, then the row and a `sh -i` showing `$$`=1), `worktree-agent-aa758de17bccf6283` (2de6e28, the leak regression check; rebase and row), `worktree-agent-aa734e4c2fcfdceec` (8824e09, SA_RESTART and the signal-path boot check; three boots, KVM, both busyboxes and test-vfs still to run; SIGSEGV on the alternate stack still wants an end-to-end user program), `stage7-uname-ferrix` (72695f1, unverified: the customer chose `uname -s` = `Ferrix`; ferrousli's identity test and any config.guess-style build must follow). Not started: the terminal switch to `console::input`, threads (design in that session's memory; ferrousli's pthread test is ready).
* A lead from stage 7's sweep tooling (`enosys-sweep`, `worktree-agent-aa3e651b1a5027598`, neither for landing): the single-processor `timeout -s KILL` hang did not reproduce in 7 runs on current main; but a sweep kernel on armv7a `--smp 1` fails stage 8's pipes check 4 of 4 with one frame leaked while plain test-boot passes, logs in that session's scratchpad `hang/`; owner stage 8 or mm.
* Stage 9 (`ferrix-4b`): the interrupt wake (verified on fd4442e), process observers, `vmo_map` on the reverse map.
* mm (`ferrix-e5`): x86-64 IST branch (afb3041, row green but for one FX-1001 hit, since root-caused and fixed in stage 10's probe), mprotect invalidation with the COW and MAP_SHARED checks, Miri and fuzz branch (c660bc7). The frame window has no `expect_heap_quiet`: it returns with its first caller, once stage 9's delivery firers and the console stop moving the heap inside a window; until then every report prints the heap's live bytes as a clue, and small-object teardown is proven with a `Weak` that must fail to upgrade.
* tmpfs's `set_len` holds its state lock, a plain spin lock that raises no preemption count, across `discard_from` and `Vmo::cut_mappings`: across every mapper's space lock and a shootdown's wait (found in mm's review of `MAP_PRIVATE`, 2026-09-14). Not a blocker; it is the first lock to move once the sleeping lock reaches inodes.
* ferrousli (`ferrix-ce`): 154 symbols were undefined at busybox's first link. Area 1, the system-call wrappers, is on develop, and the 31 names areas 5-7 may leave as stubs are in `ferrousli/src/stubs.rs`. termios (area 2), area 4, and area 3 but `crypt` are on develop too. Before busybox links, 1 symbol: `crypt` (area 3). The unlanded `ferrousli-threads` branch has its own `src/sched.rs`, which must merge into develop's when it lands.
* FX-1001 on AArch64, the one known intermittent on `main` at the wind-down, was the out-of-domain probe reading the used ring after the device's completion and the event queue from before it; QEMU completes a refused write through a bounce buffer it then drops. Fixed in the probe (stage 10's roadmap section says how); no row remains.
* Open questions for the customer: none. The board is powered at the U-Boot prompt with 95884cd on the card.
