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
| Only `zinc/` | `cargo xtask check --fast --zinc`: zinc's formatting, clippy, unit tests and the pty completion test (`zinc/tests/pty_completion.py`); a change to what zinc does at boot also runs `test-boot` on x86_64 |
| `libs/` only, and no crate the kernel builds | `cargo xtask check`, and one boot: `test-boot --arch armv7a --smp 2` |
| Anything the image contains: `kernel/`, `boot/`, a kernel-side crate in `libs/`, `xtask` | `cargo xtask check`, then `test-boot` on x86_64, aarch64, armv7a at four processors and armv7a at `--smp 2`; a stage 7 or 8 change also runs `test-shell` on x86_64 with the ferrousli busybox (`--init ferrousli`), the musl busybox *and* the host's glibc busybox (`/usr/bin/busybox`), and `test-vfs` on x86_64 with the ferrousli busybox and the musl one; a stage 7 change also runs `test-threads --arch all`, the Rust `std::thread` program (since 5fd2ab09). The whole of that, plus KVM, is what moves `main` |
| User mode, page tables, TLB, SMP or the scheduler | The row above, and x86_64 under `--accel kvm` |

Then fast-forward `develop` only if it is still the commit rebased onto.

**The busyboxes.** The busybox built against ferrousli is the primary one: the
userland Ferrix is measured with, and the one every `test-shell` and
`test-vfs` above names first. `cargo xtask busybox` builds and installs it and
`--init ferrousli` runs it; the flag is still given explicitly, since xtask
has no default program. Alpine's static musl busybox and the host's glibc
busybox stay required in every gate that names them, as the compatibility
checks: a failure on any of the three fails the gate, and nothing was dropped
when ferrousli's joined. `--init ferrousli` rebuilds the busybox first when
it is missing or older than anything under `ferrousli/` it is built from, so
the binary a gate runs is the base's.

**After the fast-forward,** the lander boots `develop` itself once on x86_64
under `--accel kvm` and reports the hash together with that result, so a bad
merge is seen by the one who made it.

**Two branches.** Sessions land on `develop`, which may be unstable. `main`
moves only by the product owner, fast-forward, to a `develop` commit that has
passed the whole matrix from a clean worktree on the product owner's own run:
the four boots, x86_64 under KVM, `test-shell` with the ferrousli, the musl
and the glibc busybox, `test-vfs` with the ferrousli and the musl busybox, and
`test-threads --arch all`;
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

**Gates run on the Linux host, not in WSL (customer, 2026-09-16).** Builds,
tests and every gate run on nazuna over `ssh nazuna-wg`, in the session's own
worktree there; WSL on the Windows machine is a convenience for a look, never
the reference, and a row run there does not count.

**A failing gate's log is kept before any re-run (2026-09-16).** Copy it
aside first; a re-gate that overwrites it turns a result into a rumour — the
one full trace of FX-0701 was lost that way.

**One build directory per session on the Linux host (2026-09-16).** Every
session sets `CARGO_TARGET_DIR=~/.local/share/ferrix/target-<session>` for all
its worktrees on nazuna and never lets a worktree grow its own `target/`; a
worktree is removed the moment its landing is in. The root filesystem filled
twice in one day from per-worktree build output (16 GB each) while the host
held 24 GB for every session together. One target directory serves one tree at a time: a second tree built
concurrently by the same session gets `target-<session>-2`, because Cargo
names a workspace member's artifacts without its path and two trees at
different commits clobber each other. After moving `main` here, push it to
the host as well — `git push nazuna-wg:Documents/projects/os/ferrix
main:main`, whose checkout updates in place — so no worktree there is ever
made from a stale `main`.

**Nobody works in the root checkout, and landings are small and often
(customer, 2026-09-16).** The root checkout keeps `main` checked out and
clean; a session that edited there blocked every other session's
fast-forward. Every session works in its own worktree under
`.claude/worktrees/`, and lands each stable, gated step on `main` the day it
is green — a worktree is never more than one landing deep, and a 40-point
milestone is ten landings, not one. A landing that moves `main` by
`git update-ref` must then sync the root checkout — `git reset --hard main`
there, when it shows nothing of another session's — or land with `git merge
--ff-only` from a clean root, which does both; a root left behind shows the
landing as staged deletions, and a commit from it would revert the landing.

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
| the customer | Product owner since 2026-09-15: priorities, decisions, what is stable enough for `main`. os-f6 (was os-f7, os-23, ferrix-32, ferrix-24) keeps this file, the roster, the points ledger and the verification of `main` when asked |
| os-02 | Stage 7, threads: commits 6 and 7 of the plan — the futex lock kind and the `munmap`/`brk` races, then `/proc` threads and the `std::thread` exit test on three architectures |
| os-05 | zinc: the zsh-compatible shell in Rust toward oh-my-zsh, in the initramfs since a3af247; and `cargo xtask busybox` rebuilding the ferrousli busybox when stale |
| os-a8 (was os-b7, os-50, os-7c, os-fb, ferrix-ce) | `ferrousli/`: `docs/POSIX-2024.md`'s list worked top to bottom (808 present, 435 missing, 124 points on 2026-09-16), each landing rebuilding the busybox and passing both ferrousli gates |
| os-26 | Windows parity for `xtask`: the loopback-UDP network gateway, `run` under WHPX, the ferrousli gate and ARMv7-A U-Boot through WSL, vendored UAPI headers; next `flash` and `watch-serial` on Windows |
| open | Stage 8's list (14), `AF_UNIX` landings 2 and 3 (5, 7), the POSIX interface sweep, stage 10's VT-d record (5) and BAR trust (8), networking's remaining host half (34) and its kernel half, stage 12 |

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
| Threads: `clone(CLONE_VM\|CLONE_THREAD\|CLONE_SETTLS)` and everything a thread implies. In: a `Thread` per task, signal state split between process and thread, `exit` apart from `exit_group` with release on the last thread, and `clone(CLONE_THREAD)` (ferrousli's static pthread test passes on x86-64); and signals, stop, `kill` and `execve` across threads: a signal sent to a process is judged across every live thread's mask and wakes one thread that can take it, `tkill`, `tgkill` and `SIGPIPE` reach one thread's own queue, a stop or continue cancels its opposite in every queue, a stop parks every thread and their blocked calls restart after `SIGCONT`, and `execve` from any thread ends the others and takes the pid. Credentials are kept per process, so `set*id` from any thread changes them for every thread, as POSIX requires -- a written deviation from Linux, whose raw calls change only the calling thread and leave the broadcast to the C library. That broadcast re-applies ids the process already has, and musl and glibc end the process if a later thread's call fails, so it stays safe only while setting an id to a current one is permitted in every `set*id` form; owed: a static musl program of two threads calling `setuid(1000)` as root that survives. The futex table's read of its word never faults: it faults the page in with the table let go and reads it under the table only if still present, retrying if another thread unmapped it, and a user page fault asserts that no preemption-disabling lock is held. `brk` and `fork` take a per-process heap lock that may sleep, so a fork on one thread never copies a heap another thread's `brk` has shrunk but not yet unmapped. A boot check on two processors shows an unmap waiting for a copy that holds its page inside `with_page`, the call every copy to or from a program goes through. `/proc/<pid>/task` lists a directory per thread holding its `status`, `stat` and `comm`, and `Threads:` counts the live threads. Exit met on 2026-09-16: `cargo xtask test-threads` boots `threads-test/`, a static musl Rust program using `std::thread`, `Mutex`, `mpsc` and a `Barrier`, as init on all three architectures; it counts its five threads through `/proc/self/status` and `/proc/self/task` while all are alive, and ends with `threads: all ok` and 0, and the same program built to expect one thread more must fail on that count. Built as rustc's default for musl, which on x86-64 is a static PIE: the loader places an `ET_DYN` image with no interpreter at two thirds of the user half and moves its entry, `AT_PHDR` and heap with it, and musl's start relocates itself (the AArch64 and ARMv7-A musl targets cannot make a static PIE, so they link fixed-address; the boot check loads a synthetic static PIE on all three). Left: the musl two-thread `setuid` program above | os-02 | `rustc` is threaded; the largest missing piece on the roadmap. Exit test: a static musl Rust `std::thread` program under `test-shell` on all three architectures |
| File-backed `mmap`: a file mapping maps the inode's own VMO pages, shared and private, with faults served from them. In order: (1) the VMO reverse map with scoped shootdown, landed; (2) `libs/vfs`'s `PageSource` over tmpfs, with no open file's lock held across an inode call, landed; (3) the kernel VMO filling from a source, landed ahead of the mappings because stage 11's kernel mount needs only it; (4) file mappings on it: `MAP_SHARED` writing through, with `msync`, `SIGBUS` past the end and the boot check both ways, landed; (5) `MAP_PRIVATE` copying into a shadow object of its own on first write, with reads served from the file's VMO until then, a truncation taking the copies past the cut, and checks from the kernel and from user mode, landed | stage 8 (os-58); mm reviewer (os-a0) for the space.rs half | `rustc` and the linker map rlibs. Same interface as the btrfs page cache; stage 11's kernel mount needed only (3), and `rustc` needs (4) and (5) |
| The out-of-domain probe on x86-64 under KVM on a loaded host, once in a few boots: VT-d's single fault record holds a fault other than the probe's, so the probe reports nothing and `test-boot` refuses the boot while the kernel reaches its marker. Read on 2026-09-14 (nazuna `~/.local/share/ferrix/logs/oodprobe/split-86eed16-kvm-1.log`, load about 5): the record held stream 0x0, page 0x1000, a write, where the probe is stream 0x10 at page 0x1000, and QEMU logged "New fault is not recorded due to compression of faults", so a fault attributed to source 0 landed on the probe's IOVA first and the probe's own was dropped. Nothing at 00:00.0 should DMA; next: find what QEMU attributes to source 0 at that IOVA under load (a stale translation of an earlier domain, or a DMA QEMU issues without a requester), then a fix with a negative control. Read against QEMU 9.2's `intel_iommu.c` on 2026-09-14: QEMU compresses a fault only when its single record already holds one from the *same* source id, and its message names the probe's own device, so QEMU believed the record held 00:02.0's fault while the kernel decoded source 0 from it; either `take_fault` read the record's high word torn (QEMU writes the low quad, then the high; the kernel reads the high first) or an earlier fault decoding as source 0 sat in it. Next: print the record's raw high and low words and FSTS in the mismatch line, loop loaded KVM boots, and read them | os-5b | It can fail main's verification, as FX-1001 could |

### P1 — required before a stage is called done

| Item | Owner | Stage |
|---|---|---|
| POSIX.1-2024 interface sweep: from musl's implementation of every mandatory POSIX.1-2024 function, the list of Linux system calls (and flags) they need; the stage 7 sweep tooling runs each on all three architectures and files every `ENOSYS`, `EINVAL` on a mandatory flag, or wrong result with the area that owns it, as rows here. Sockets and threads are known and excluded; the `epoll`, `eventfd`, `timerfd` and `signalfd` families are in scope because stage 17 needs them | os-b6, after its five branches | 7, 8, 17 |
| `AF_UNIX` sockets: `socket`, `socketpair`, `bind`, `listen`, `accept`, `connect`, `send*`/`recv*` with `SCM_RIGHTS`, `shutdown`, `getsockopt` for what busybox, POSIX and Wayland need; no net core, no `AF_INET`. On the compositor's path (stage 17) as well as POSIX's | open — wants a session of its own | 7, 17 |
| `memfd_create` with `F_ADD_SEALS`/`F_GET_SEALS`, on tmpfs, after file-backed `mmap`: `wl_shm` is a sealed memfd both sides map | os-c4, after the chain | 8, 17 |
| Trusting a BAR firmware placed but did not enable | ferrix-d9 | 10 |
| btrfs: CI Miri step for `libs/btrfs` and `libs/block` under 15 minutes, whole-image tests ignored under Miri | ferrix-61 | 11 |
| FX-0701, seen once on 2026-09-16: `test-vfs --init ferrousli` on x86-64 panicked in stage 7's check "a thread of a stopped process kept running instead of stopping" (threads commit 5's stop check), at uptime 13.04 s. The same check passed in `test-shell` in that run and on the re-gate, and the log was overwritten. Reproduce first: loop that row under a second QEMU's load, keeping every log, then find the window (a thread already on its way back to user mode when the stop lands is the first suspect) and fix it with a control. 2 points | os-02 | 7 |
| A two-last-threads exit check that provably races: spin-meet on two processors, with its negative control -- the old last-thread decision put back -- failing by name. Today's check passes that control too, so it shows only that such a process ends with its first thread's status (from stage 9's review of threads commit 4). 1 point | threads (os-9f) | 7 |
| End-to-end user programs for what the `sigpaths` check proves at the kernel's decision: a `SIGSEGV` caught on the alternate stack, and a read interrupted by a handler and restarted under `SA_RESTART` (`SA_RESTART` and the driven signal paths have landed) | ferrix-a5 | 7; `rustc` needs `SIGSEGV` on the alternate stack |
| Stage 6's reverse-map check reported "-4 frames not given back" once in two loaded musl `test-vfs` runs on 2026-09-14: four frames — one kernel stack — came back from outside its window. Cause confirmed by a marker-gated control (the check's own warm-up task, which posts its outcome and exits, reaped inside the measured window because `wait_until_reaper_quiet` counts only tasks already exited). Fix: `run_pinned` waits for its task to be dead before returning, landing with `MAP_PRIVATE`, the control showing zero after it. The threads checks' returning before their programs' spaces are dropped is a separate, real hazard shared with `exec::run`: P2 | os-58 (fix), os-43 (control) | 6 |
| Retire the 20 ms console polling. Receive by interrupt into a 4 KiB ring has landed on all three: the PL011, the STM32 USART (through ST's EXTI on the DK1), and x86-64's 16550 through an I/O APIC input found from the MADT, with `console::input::{waiters, has_input}` for the console thread to wait on. Left: the console thread and readers waiting on it instead of sleeping (in `fs/terminal.rs`) | ferrix-a5 | 7, 15 |
| `AF_UNIX` descriptor passing, after names (bf4eec48). `SCM_RIGHTS` has landed: files travel with the first byte of a message, a receive installs what fits and closes and flags the rest, and a peek installs nothing (a written deviation). Left: the in-flight cycle pass, so a socket passed over its own connection is collected (3 points); `SO_PASSCRED` and `SCM_CREDENTIALS` (2). Carried from landing 1's review: cap a receive's kernel buffer at the receive capacity (`MSG_WAITALL` in chunks of it), drop a refused file outside the `FdTable` lock before descriptors travel, `SO_SNDBUFFORCE`/`SO_RCVBUFFORCE` needing `CAP_NET_ADMIN` and skipping the cap, and Linux's error order in `sendmsg` and `socketpair`. On the compositor's path (stage 17) as well as POSIX's | os-02 | 7, 17 |
| POSIX.1-2024 interface sweep: from musl's implementation of every mandatory POSIX.1-2024 function, the list of Linux system calls (and flags) they need; the stage 7 sweep tooling runs each on all three architectures and files every `ENOSYS`, `EINVAL` on a mandatory flag, or wrong result with the area that owns it, as rows here. Sockets and threads are known and excluded; the `epoll`, `eventfd`, `timerfd` and `signalfd` families are in scope because stage 17 needs them | os-b6, after `AF_UNIX` and the terminal switch | 7, 8, 17 |
| `mprotect` could make a shared mapping writable that its file never allowed: fixed with `memfd_create`'s may-write accounting, `AddressSpace::protect` now refuses `PROT_WRITE` on a shared file mapping that may not write its file with `EACCES`, and the memfd boot check shows it on a file opened read-only, landed | stage 8 (os-58) | 8 |
| `memfd_create` with `F_ADD_SEALS`/`F_GET_SEALS`, on tmpfs, after file-backed `mmap`: `wl_shm` is a sealed memfd both sides map. Landed: seals in tmpfs, the write seal refused while a shared mapping may write the file (a fork child's copy counted), and the FX-0880 boot check; the exec seal (`MFD_NOEXEC_SEAL`, `F_SEAL_EXEC`) is left | stage 8 (os-58) | 8, 17 |
| ferrousli's remaining link-breakers, from `docs/POSIX-2024.md`: `fpclassify` for every type, and `isinf`, `isnan`, `isnormal`, `isfinite`, `signbit` and the six comparison macros (`isgreater` to `isunordered`) for `long double`, fail to link, because `__fpclassify`, `__fpclassifyf`, `__fpclassifyl` and `__signbitl` are missing. They land with the math stubs, whose branch has the `double` and `float` halves. `__assert_fail` and `crypt`'s DES hash landed. 1 point | os-50 | ferrousli |
| Land `ferrousli-threads`, work in progress of 2026-09-13 committed without a build: barriers, spin locks, semaphores with `sem_open` through `/dev/shm`, `sched.h` and C11 `threads.h`, 57 of the POSIX.1-2024 gap's interfaces. Its `src/sched.rs` merges with develop's; its `lib.rs`, `generated/nr.rs` and `gen-abi.py` conflict too. One landing, 8 points | os-50 | ferrousli |
| Land `ferrousli-math`, work in progress of 2026-09-13 not built since its last change: `fenv.h`, rounding, manipulation, remainders and `fma`, with libc-test's math tables in Rust; 43 interfaces; `fmaf` was rewritten and is untested. Rebases without conflicts. One landing, 5 points | os-50 | ferrousli |
| Land `ferrousli-misc`'s finished half: `search.h`, `libgen.h` (`dirname` leaves the stubs) and `glob`, whose ENOENT callback check at `glob.c:153` still fails; 15 interfaces. Its `fnmatch.rs` and `dirent.rs` conflicts resolve to develop's copies, which landed. One landing, 3 points | os-50 | ferrousli |
| `regex.h`, replacing four stubs: the parser and program on `ferrousli-misc` were never compiled, and the exports (`regex.rs`) and the POSIX submatch solver are unwritten. One landing, 5 points | os-50 | ferrousli |
| The seam, gated: a script beside `scripts/check-asm-budget.py`, with an allowlist of the same shape, refuses a volatile MMIO access or port I/O instruction in `kernel/` outside the sites `docs/ARCHITECTURE.md` §1 and §7 permit: `arch/` (MMU, traps, timers, interrupt controllers, the serial writers), `iommu/`, `pci/` enumeration, `mmio.rs`, `block_ring/`, `early.rs` and the panic framebuffer, and the boot checks; fifteen files hold one today. Each entry carries a reason and a line budget, a stale entry fails, and a new site is argued in a diff. The rule it keeps is that the kernel enumerates devices and drives none, which convention keeps for about six weeks (§9). 3 points | open | 10 |
| Isolation per platform, written down: a table in `docs/ARCHITECTURE.md` §7 saying for each machine the gates boot and each board whether driver DMA is translated (x86-64 through VT-d, AArch64 through the SMMUv3, ARMv7-A under QEMU bypassed by U-Boot, the DK1 whose STM32MP157 is checked on the board rather than assumed), with the console line that says so. The seam's safety claim is then read per platform, and a platform in degraded trusted mode is named as one. 1 point | open (the DK1 row with the board's owner) | 10 |
| Per-open windows onto the card VMO: a program that mapped `/dev/dri/card0` and closed it can still read what the next opener draws, since the exclusive open does not end a mapping (`docs/DISPLAY.md` §2.3). Until then `card0` is `0660` and root's, accepted for iteration 1 by os-f6 2026-09-16. Each open gets its own view of the card's pages, which comes with stage 19's render node | GUI session (os-e5), with stage 19 | 17, 19 |

### P2 — quality and performance, on the "fast" half of the goal

| Item | Owner |
|---|---|
| The cost of the 20 µs one-shot armed on every wake onto the caller's processor, measured on pipe and futex paths | ferrix-34 |
| Per-CPU frame and heap caches, deferred since stage 2 | open, once a workload can measure them |
| `Inode::ioctl`: `sys_ioctl` special-cases the console, sockets and `/dev/dri/card<N>` by the open object's type; a hook on the inode replaces the three branches (os-02's review of the display stack, 2026-09-16) | open, kernel VFS owner |
| Checked register offsets in the ring-3 virtio drivers: `Block::read`/`write` in `user/blk` and `user/gpu` assert on a device-controlled `notify_off` × multiplier, and on ARMv7-A `offset + size_of::<T>()` can wrap past the bounds check; one shared checked-offset accessor for both (os-02's review, 2026-09-16) | open, driver owner |
| ASIDs and PCIDs, so a switch stops invalidating every user entry | open, after threads |
| `getrandom` seeded from virtio-rng into a real generator; a real-time clock read from the RTC and `/dev/rtc` | open |
| The debt the roadmap names: fuzz targets for `virtio`, `linux-abi` | open |
| The debt the roadmap names: Miri for `frame`, `heap`, `paging`; fuzz targets for `cpio`, `fdt`, `acpi`, `virtio`, `linux-abi` | ferrix-e5 (the first three crates and `cpio`, `fdt`, `acpi`) |
| Every gate's log names the tree it ran on: `xtask` prints `HEAD`, the branch and whether the tree was clean as the first line of every `check`, `test-boot`, `test-shell` and `test-vfs` log, so a row's evidence pins its commit by itself rather than by the runner's word (asked for by a review of the frame-window evidence, 2026-09-13) | open, cross-cutting |
| The host-test table in the roadmap generated from `cargo test --list` with a gate, instead of counted by hand | ferrix-24 |
| The POSIX measure: musl's libc-test functional and conformance programs built static against musl and against ferrousli, run under `test-shell` on all three architectures, with the pass count in the roadmap's host-test table and every failure filed with its owner | os-7c, with os-9f once threads run | 
| Zero-copy block reads: pin the page-cache pages themselves as the block ring's buffers, removing the data-VMO and scratch copies of stage 11's first read path (ARCHITECTURE §3) | ferrix-61, after stage 11's kernel mount |
| ferrousli's busybox beyond the gates' applets: the 11 stubs in `ferrousli/src/stubs.rs` (regex for `grep` and `sed` patterns busybox does not handle itself, the math functions `awk` calls, and `dirname`), each ending the program when an applet reaches it. Name resolution left the list on 2026-09-16, with the interface and Ethernet lookups; and `crypt`'s `$2*$` blowfish hash, which returns `"*"` (the traditional DES hash, for the two-character salt POSIX requires, is in P1's link-breakers row) | open |
| The rest of ferrousli's POSIX.1-2024 gap, by area in `docs/POSIX-2024.md`: 399 interfaces the P1 rows leave, 125 points. The largest parts: the math library beyond `ferrousli-math` (21: the transcendental functions, the Bessel functions, every `long double` form) with `complex.h` (8); locales and messages (15: `gettext`, `iconv`, `catgets`, `strfmon`); wide-character streams and conversions (14); cancellation and the `clock` waits (11); realtime (11: `aio.h`, `mqueue.h`, timers, `shm_open`); `wordexp` and `nftw` (5); spawning (7); and POSIX.1-2024's declarations in musl 1.2.5's headers (3). Name resolution and the network databases, which were 13, landed on 2026-09-16. Each landing updates the document's tables | open (ferrousli, os-50) |
| `cargo xtask check --ferrousli` cannot pass on Windows: `ferrousli/tools/gen-abi.py` reads `/usr/include/x86_64-linux-gnu/asm/unistd_64.h`, which no Windows host has. Give it a pinned copy of the Linux UAPI numbers in the tree, so the ferrousli gate runs wherever `cargo xtask busybox` now does. 2 points; found landing the native Windows busybox build, 2026-09-14 | open |
| The seam measured, 1: what the hop to ring 3 costs. Under `test-boot`, the kernel times submit-to-complete on the pattern disk at queue depths 1 and 32, and `blk` times its own device round trip for the same requests, so the difference is the ring, the doorbells and the scheduler between them and nothing else. Beside it, the same QEMU disk read at the same depths by a Linux guest (a stock image, `dd` with `iflag=direct`), as the in-kernel reference the 2026-09-13 decision forbids building in Ferrix. Both numbers, on x86-64 under KVM and on AArch64, go into the roadmap's stage 11 section with the boot line that carried them, and the zero-copy row above is re-costed against them. 5 points | open |
| The seam measured, 2: how much of a build-like workload crosses it. Counters kept from boot: Linux system calls answered; page-cache pages served from an inode's VMO against pages filled through the ring; ring submissions and completions; printed as one line at the end of `test-vfs`, and of the `rustc` run when stage 16 has one. The claim under test is that the seam is on a cold path for the goal's workload; the row is done when the ratio is in the roadmap's stage 11 section and the decision of 2026-09-16 cites it. 3 points | open |

### After `rustc` — the compositor's path, unowned until stage 16 is near

| Item | Stage |
|---|---|
| Display core + ring-3 virtio-gpu driver, `/dev/dri/card0` with dumb buffers, atomic page flip and vblank; `xtask` reading QEMU's screendump | 17 |
| Input core + ring-3 virtio-input driver as evdev; QEMU monitor input injection in `xtask` | 17 |
| Iteration 1, the customer's order of 2026-09-16: a blank screen on Ferrix in QEMU. A first cut of stage 17: virtio-gpu 2D in `libs/virtio` (8), a ring-3 virtio-gpu driver started by devmgr (8), a minimal `/dev/dri/card0` with one dumb buffer, legacy `SETCRTC` and `PAGE_FLIP` (13), the compositor binary filling it (2), and `xtask` reading QEMU's screendump (3); 34 points, design in `docs/DISPLAY.md`, approved by os-f6 2026-09-16; Landed: L1, `libs/linux-abi::drm` (3); L2, `libs/virtio::gpu` (5); L3, `libs/displayctl` (3); L4, `libs/virtio-gpu` (5). L7's tooling is in (`compositor/blank`, `xtask test-display` and `--display`). L5 and L6, the display core, `user/gpu` and `/dev/dri/card0`, landed as one stack (16), and with them L7's pixel check passes on x86-64 and AArch64 (5): iteration 1 is done, 37 of 37. AAVMF took the virtio-gpu as the boot framebuffer on AArch64 (`docs/DISPLAY.md` §3): the loader now prefers a framebuffer the allocator will not own and writes `BootInfo.framebuffer.reclaimable`, which the panic screen and the display core both read (2, os-e5, reviewed by os-02). Its negative control, not committed, on x86_64 at 9a1788ef: with a marker in `panic::screen::install` alone the marker printed once; with `finish_boot_info` forcing the flag to 7 as well, the kernel printed `NEGATIVE CONTROL: the loader forced framebuffer.reclaimable to 7` and `display 1280x800, stride 1280, in memory the allocator owns: panics go to serial only`, and the install marker never printed. Kernel reader for L5 and L6: os-02. GUI session (os-e5) | 17 |
| `card0` is opened by one process at a time, standing in for DRM master (`docs/DISPLAY.md` §5, a written deviation from Linux): Linux's many opens with one master, `SET_MASTER`/`DROP_MASTER` arbitrating between them, and the render node beside it come in stage 19 | 19 |
| The compositor workspace: Smithay base (assumed), CPU rendering, dwindle and master, `hyprland.conf`, `hyprctl` IPC, two Rust test clients. `hyprland.conf` landed 2026-09-16 (`compositor/config`, 5); layouts and dispatchers next (8). GUI session | 18 |
| GPU path decision, then `renderD128`, dmabuf, GBM, animations, blur and rounding | 19 |
| The DK1's LTDC display and USB HID as the hardware variant of stage 17 | 17, P3 |

### Networking rows, unowned

| Item | Owner |
|---|---|
| `AF_PACKET` gaps: frames this host sends copied to `ETH_P_ALL` sockets (`PACKET_OUTGOING`), packet sockets on the loopback, and classic BPF (`SO_ATTACH_FILTER`, `SO_DETACH_FILTER`). Raw sockets in both families and `AF_PACKET` landed 2026-09-16. Networking stage | os-26 |

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
* A panic report as a QR code: a port of Linux's `drm_panic_qr` as
  `libs/qr` (ferrix-qr), so a panic screen can carry the whole report. WIP
  on branch `worktree-agent-a33c10946b6721065` (5690f0e), unbuilt into the
  panic path.

---

## `su` failed under the musl busybox, and `AF_UNIX` names fixed it

`cargo xtask test-vfs --arch x86_64` with the Alpine musl busybox failed
applet 17, the permissions script: `su: can't set groups: Not supported`, and
the script never reached the `id` its expectation begins with. The ferrousli
busybox passed the same applet, which is why the landing that added it did not
see this.

It was never a regression: the same failure reproduced on `a643475`, the
commit that added the applet. musl's `initgroups` goes through `getgrouplist`,
which tries an `AF_UNIX` connection to nscd before it reads `/etc/group`, and
Ferrix answered `connect` on an `AF_UNIX` socket with `EOPNOTSUPP` because
`AF_UNIX` names had not landed. musl treats that as an error rather than as
"no nscd" and gives up.

It closed itself exactly as predicted: `connect` to `/var/run/nscd/socket` now
answers `ENOENT`, musl falls back to `/etc/group`, and the applet passes. Kept
here because the reasoning -- a refusal with the wrong errno is a library
giving up rather than falling back -- is worth having written down the next
time a C library is surprising. | done | 0

## The debt the net ring took on

`libs/netring` and `libs/blkring` keep the same index discipline -- private
indices, checked reads of the peer's, the want-bell handshake -- and it is
written twice. Extracting it into a crate both depend on is the right shape and
was deliberately not done in the landing that added the second copy: it would
refactor a subsystem that is shipped, fuzzed and on the boot path, in the same
commit as a new one, and a mistake there is a disk that stops reading. The
extraction is 3 points and wants a landing of its own, with both rings' tests
and both fuzz targets as the evidence. | open | 3

## Decisions

Dated, newest first. A decision here is final until the customer says otherwise.

* **2026-09-16 (customer)** Everyone has work and the record says who: the
  owners table above is the roster of this day, with the product-owner
  session back as os-f6 keeping the file while the customer holds the seat.
  `main` is landed on directly at a stable point, as decided on 2026-09-15,
  each landing gated on nazuna by its own row; os-f6 verified 5d2b4db whole
  on 2026-09-16 (eleven gates, no panic, every boot at stages 1-11). `main`
  on this machine is 64 commits past `origin/main`; pushing it is the
  customer's word. A session that shares the root checkout commits through a
  private index holding only its own paths and moves `main` by compare-and-
  set (`git update-ref refs/heads/main <new> <old>`), so no landing carries
  another session's half-work; a session with a branch of its own lands from
  its worktree as before.
* **2026-09-16 (customer)** The architecture stays what `docs/ARCHITECTURE.md`
  §1 says: a monolithic core, capability seams, device drivers in ring 3.
  Asked whether Ferrix should be a monolith, a microkernel or a hybrid, the
  answer is that the seam sits at devices because the goal puts it there:
  `rustc`'s system calls are `open`, `stat`, `read` and `mmap` on files the
  page cache already holds, and those stay function calls; only disk traffic
  crosses to ring 3, batched through a ring behind the page cache, where a
  hop is amortised over a queue. The hybrid that is "the worst of both
  worlds" keeps message passing between subsystems and compiles them into
  one address space; Ferrix passes messages only across the privilege
  boundary, and in-kernel subsystems call each other. Rust confines the
  core's memory bugs to its audited `unsafe`, and the IOMMU confines what
  Rust cannot, a device's DMA. What the shape gives up is restarting a
  kernel subsystem, which the goal does not need. A full microkernel would
  make the Linux ABI an emulation layer over servers, against §2; a full
  monolith would delete stages 9 and 10 and gain nothing on the compiler's
  path, which crosses no seam. The decision is closed by evidence rather
  than by argument: the two "seam measured" rows in P2, the seam gate and
  the per-platform isolation table in P1. Drivers stay in ring 3 and no
  kernel disk path is built for the measurement (2026-09-13 below).

* **2026-09-15 (customer)** The customer holds the product owner seat: there is
  no product-owner session, and the names in this file from before the restart
  (os-23, os-f7 and the rest) are gone. Two sessions are left, so the queue and
  the per-landing "go" have nobody to ask and are suspended: a session lands
  when it judges the tree stable, and commits to `main` directly rather than
  through `develop`. This supersedes "The landing queue" and "Two branches"
  above for as long as the fleet is this small. What does not change: the gate
  table, judging a gate by its output, and that a landing carries its own
  documentation. `main` is still what the customer tests, so "stable" means the
  gate a change's own row names has passed on the commit being landed.

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

## The day of 2026-09-14: everything unfinished landed, and `main` moved

The fleet went quiet on 2026-09-13 at about 22:30 and was renamed by a
restart; on 2026-09-14 the customer's order was that every unfinished task
lands on `main`, so `main` is the ground truth. It did. `develop` went from
73b17da to c955482 by 19 landings, each on a whole row and each booted under
KVM after its push; the product owner then verified the head whole from a
clean worktree, fast-forwarded `main` to it and tagged
`stage-11-ring-3-disk-and-btrfs` (notes in `docs/RELEASES.md`). What
landed, with the story points the sessions gave before starting:

* **Stages 10 and 11 done, markers `stages 1-10` and `1-11`:** the ring-3
  virtio-blk driver stack (8), stage 11's exit — a `mkfs.btrfs` image read
  back byte for byte through that driver on all three architectures (5),
  the quiesce-after-death fix with `TIMED_OUT` (2), the driver start order
  (1), `f_pos` as a sleeping lock (3), and `devmgr` the program, starting
  drivers from `/lib/drivers` one at a time (13). Left for stage 10: the
  VT-d stale-record intermittent (P0, reproduced once under load with its
  log) and BAR trust (P1, 8).
* **Stage 8 and file mmap:** the frame window that nets heap pages in every
  frame-counted check and names the route of a mismatch (3), `MAP_SHARED`
  (8) and `MAP_PRIVATE` with the truncation decommit (10 + 1). Left:
  `memfd_create` with seals (5, started after the tag), the `mprotect`
  read-only-file gap it closes (P1).
* **Stage 7:** `AF_UNIX` landing 1 of 3 (5), the `process_start` spawn split
  (2), threads commit 4 — `clone(CLONE_THREAD)` on all three architectures,
  ferrousli's pthread test passing (3). Left: `AF_UNIX` landings 2 and 3
  (5, 7), threads commits 5–7 (5, 3, 4) with commit 5 in review, the
  terminal switch, the POSIX sweep, the provably-racing exit check (1).
* **Stage 9:** `channel_read` under the topology lock without faulting, with
  a fork check that builds its hazard (2). Nothing left queued.
* **mm:** the shootdown-lock discipline with its assertions, the mprotect
  and copy-on-write user checks, x86-64 IST stacks, Miri and fuzz (14, ten
  commits) — its own assertion found and the landing fixed two real bugs
  (a masked copy-on-write flush, a secondary's first reap with interrupts
  masked); the lock-site naming fix (1).
* **Board:** 2 GiB ARMv7-A by allocation and a trampoline, the x86-64 COM1
  interrupt, `ferrix.onexit=reset` and `CMDLINE.TXT`, `test-boot --reset`,
  the DK1 flashed and driven from Windows, all proven on the board (11 + 6).
  Nothing left.
* **Ferrousli:** crypt (5), `cargo xtask busybox` and `--init ferrousli` (3),
  fnmatch (1), the gate change making its busybox the primary one every
  gate runs with musl and glibc kept (2), the README's shell section. Its
  busybox passes `test-shell` and `test-vfs` on Ferrix with no stub reached.
  Left, P2: regex (5), the math stubs (3), name resolution (3), crypt's DES
  and blowfish (3).

Flakes met and closed on the way, each with a control: the driver-order
race in `/proc/partitions`, the rmap check's own warm-up task reaped inside
its window, a `-4`-frame report that named it. Open: the VT-d record (P0)
and the threads checks returning before their spaces drop (P2).

Measured pace (the ledger is in the product owner's memory): about 130
points landed on 2026-09-14 across ten sessions in six hours of queue time,
after 112 on the evening of the 13th; estimates held on every item under 8
and the two 13s came in at 13.

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
