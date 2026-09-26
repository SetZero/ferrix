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
| Only `ferrousli/` | `cargo xtask check --ferrousli`, then `cargo xtask busybox` and, with the busybox it built, `test-shell` and `test-vfs` on x86_64 with `--init ferrousli`, so the binary the gates run never lags the library; then `cargo xtask uutils`, which links uutils/coreutils against it and is the larger consumer of the two, since it brings Rust's whole `std` with it; a change to `ferrousli/tools/ports/` also runs `cargo xtask ports` and `test-net --arch x86_64 --init ferrousli`, which fetches with the curl it built, and for the Arm ports `cargo xtask ports --arch aarch64` and `--arch armv7a`, then `test-net --arch all` with the static busybox, which fetches and clones with them |
| Only `zinc/` | `cargo xtask check --fast --zinc`: zinc's formatting, clippy, unit tests and the two pty tests (`zinc/tests/pty_completion.py`, `zinc/tests/pty_jobs.py`); a change to what zinc does at boot also runs `test-boot` on x86_64, and a change to how it starts, waits for or signals a process also runs `test-shell --arch all` and `test-jobs`, which is the only gate that types at a console |
| `libs/` only, and no crate the kernel builds | `cargo xtask check`, and one boot: `test-boot --arch armv7a --smp 2` |
| Anything the image contains: `kernel/`, `boot/`, a kernel-side crate in `libs/`, `xtask` | `cargo xtask check`, then `cargo xtask build --arch all --release`, since CI builds and boots the release profile and no other gate does, then `test-boot` on x86_64, aarch64, armv7a at four processors and armv7a at `--smp 2`; a stage 7 or 8 change also runs `test-shell` on x86_64 with no `--init`, which runs zinc, the image's shell, and then with the ferrousli busybox (`--init ferrousli`), the musl busybox *and* the host's glibc busybox (`/usr/bin/busybox`), and `test-vfs` on x86_64 with the ferrousli busybox and the musl one; a stage 7 change also runs `test-threads --arch all`, the Rust `std::thread` program (since 5fd2ab09); a change to the loader (`ferrousli/ld`), to `exec` or to ferrousli also runs `test-shell` with Debian's dynamic busybox twice, on glibc's own `ld.so` and `libc.so.6` on all three architectures and on `--interpreter ferrousli --library ferrousli` on x86_64 (docs/ROADMAP.md, dynamic linking; `scripts/fetch-debian-busybox.sh` fetches it). The whole of that, plus KVM, is what moves `main` |
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

**Landing on `main`, and milestones.** Since 2026-09-15 there is no
`develop`: a session lands on `main` directly at a stable point, from its
own worktree, after its row from the gate table has passed on the Linux
host, by compare-and-set (`git update-ref refs/heads/main <new> <old>`) or a
fast-forward from a clean root, and pushes `main` to nazuna afterwards. A
milestone is an annotated tag on `main`, placed only after the product
owner has run the whole matrix on that commit from a clean worktree
(`~/.local/share/ferrix/po-verify.sh <commit>` on nazuna): `check
--ferrousli --zinc`, `busybox`, the four boots, x86_64 under KVM,
`test-shell` with the ferrousli, the musl and the glibc busybox, `test-vfs`
with the ferrousli and the musl busybox, `test-net --arch all`,
`test-display --arch all` and `test-threads --arch all`. A release freeze
(the customer's word) means: each session lands what is gate-green, leaves
the rest on its branch with a row saying where it stands, deletes its
target directory and reports; the product owner lands the release notes
last, verifies that head, tags it, and the push to `origin` is the
customer's.

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
| os-02 | Stage 7's remainder: `AF_UNIX` 3b, the in-flight cycle pass (3), and 3c, credentials (2); the kernel-side reviews of stage 17's landings (E4 card0 planes, input L5) |
| os-05 | zinc: `zinc-next`, the port of zsh's C runtime (36k lines; it compiles and passes `check --zinc`), landing in five slices that keep the oh-my-zsh gate green: all five slices are on main (core state, expansion, signals and jobs, execution, prompt with builtins and start-up) (19 of the 40 points); the other 21 are the remaining builtins, the history ring, ZLE, completion and modules, and the swap to `zinc`; `cargo xtask busybox` and its staleness rule |
| os-a8 (was os-b7, os-50, os-7c, os-fb, ferrix-ce) | `ferrousli/`: first the wrappers for epoll, eventfd and `FIONBIO` with a C test (3), then `docs/POSIX-2024.md`'s list from the top of what is left (996 present, 247 missing, 80 points on 2026-09-16), each landing rebuilding the busybox and passing both ferrousli gates |
| os-26 | The event loop's kernel rows are done (epoll, eventfd, `FIONBIO`, `FIOCLEX`/`FIONCLEX`); next the wake-on-event row (5) so `poll`/`epoll` waits stop rechecking every 5 ms, then the `AF_PACKET` gaps; Windows parity for `xtask` is held |
| os-12 | Ports onto ferrousli: curl with HTTPS through mbedTLS and btop with libc++ landed for the release; the firmware clock, ChaCha20 `getrandom` (BootInfo v5), the hermetic HTTPS `test-net` program and git all landed and gated on 2026-09-17. What is left of `ports-autobuild`: building the ports when stale rather than every time (5) |
| open | Stage 8's list (14), `AF_UNIX` 3b the in-flight cycle pass (3) and 3c credentials (2), stage 10's BAR trust (8, first piece landed), stage 12 |

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
| **Done 2026-09-19 (FX-1001).** The out-of-domain probe on x86-64 under KVM on a loaded host reported the unit's record holding the probe's own page at stream 0x0 instead of 0x10, once in a few boots and never under TCG. It was the torn read, and QEMU's source decides between the two candidates: a fault recording register is 128 bits read 32 at a time, the source id is in the low half of the high quad and F in the high half, and `read64` read low half first -- so `take_fault` took the source id from a still-empty record and then saw the F of a fault recorded in between. `vtd_record_frcd` writes the low quad and then the high quad with F clear ("Must not update F field now, should be done later"), and sets F in a second write. The other candidate, a stale fault decoding as source 0, is ruled out: it would have carried its own page, and the page matched the probe's. Fixed by reading F by itself, first, which is correct by construction against that write order. x86-64 `test-boot` passes and six loaded KVM boots were clean -- not offered as proof of a rare race; the argument is the write order | os-5b | It can fail main's verification, as FX-1001 could |
| WHPX: `/sbin/blk` died at its first common-config read on Windows (QEMU 11.1.0 under WHPX). In `ferrix_virtio::pci::negotiate` (`libs/virtio/src/pci.rs:185`, `movl 0x4(%rax),%esi` reading `device_feature` right after writing `device_feature_select`, `rax` its aperture 0x7fffff6fe000) the kernel took vector 14 with error code 12 (user, reserved bit) at address 0; one or both blk drivers died and stage 10's driver check panicked FX-1005. **Root cause, 2026-09-16 (os-26): QEMU, not Ferrix.** A diagnostic print at the fault showed, in 2 of 2 failing boots, the vCPU's live `CR3` equal to blk's own root and blk's tables mapping `rax+4` (to GPA 0xC000008004; the other blk process maps the same VA to 0xC000004004). QEMU 11.1's WHPX backend no longer uses Hyper-V's instruction emulator: `whpx_handle_mmio` decodes the exit's instruction bytes with QEMU's own x86 emulator, which walks the guest page tables itself (`target/i386/emulate/x86_mmu.c` `walk_gpt`). That walk answered `MMU_TRANSLATE_PAGE_NOT_MAPPED` (1) for a mapping that is there. `translate_res_to_error_code` tests the result enum as bit flags, so 1 becomes U|RSVD with P clear, error code 12; and for a read `x86_read_mem_ex` puts the address only in `env->cr[2]`, which `whpx_set_registers(WHPX_LEVEL_FAST_RUNTIME_STATE)` never writes back, so the guest sees `CR2` 0. It depends on the processor count: on main 1a55a1d3, `test-boot --init ferrousli --net --zinc --accel whpx` lost a blk driver in 4 of 5 boots at `--smp 4`, 1 of 3 at `--smp 2`, 0 of 6 at `--smp 1`. blk is hit because its two identical processes negotiate at once; gpu and net have one each. `CR0`/`CR3`/`CR4` are read live, long mode and CPL come from the exit context, the general registers are fetched per exit and DS's base is 0, so the race inside QEMU is not isolated further; a debug QEMU would. Whether to report it upstream is the customer's. Workaround, 1 point: `xtask` gives the guest one processor under `whpx` unless `--smp` is given, printing why, and warns when more than one is asked for | os-26 | The customer's own `run` on Windows panicked at boot |

### P1 — required before a stage is called done

| Item | Owner | Stage |
|---|---|---|
| POSIX.1-2024 interface sweep: from musl's implementation of every mandatory POSIX.1-2024 function, the list of Linux system calls (and flags) they need; the stage 7 sweep tooling runs each on all three architectures and files every `ENOSYS`, `EINVAL` on a mandatory flag, or wrong result with the area that owns it, as rows here. Sockets and threads are known and excluded; the `epoll`, `eventfd`, `timerfd` and `signalfd` families are in scope because stage 17 needs them | os-b6, after its five branches | 7, 8, 17 |
| `AF_UNIX` sockets: `socket`, `socketpair`, `bind`, `listen`, `accept`, `connect`, `send*`/`recv*` with `SCM_RIGHTS`, `shutdown`, `getsockopt` for what busybox, POSIX and Wayland need; no net core, no `AF_INET`. On the compositor's path (stage 17) as well as POSIX's | open — wants a session of its own | 7, 17 |
| `memfd_create` with `F_ADD_SEALS`/`F_GET_SEALS`, on tmpfs, after file-backed `mmap`: `wl_shm` is a sealed memfd both sides map | os-c4, after the chain | 8, 17 |
| Trusting a BAR firmware placed but did not enable. **Started 2026-09-19:** `libs/fdt` now reads a host bridge's `ranges` into `PciWindow`s that keep the bus and CPU addresses apart, with `holds`/`translate` and seven host tests (QEMU `virt`'s own three entries among them). Left, in order: the same windows on ACPI machines, which needs the loader to call `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL.Configuration()` before `ExitBootServices` and carry them in `BootInfo` (a version bump); the vetting itself in `kernel/src/pci.rs` -- whole BAR in one window, window kind, prefetchable one way only, every bridge upstream forwarding and decoding, decoding-on BARs admitted first, unassigned BARs reported as unassigned; and turning decoding on at `IoMapping` creation rather than at enumeration | ferrix-d9 | 10 |
| btrfs: CI Miri step for `libs/btrfs` and `libs/block` under 15 minutes, whole-image tests ignored under Miri | ferrix-61 | 11 |
| **Fixed 2026-09-24 by "Look again with interrupts masked before going back to a program".** The gap was in `deliver::return_to_user`, not in the kick: it acted on what it found with interrupts open and returned without looking again once it masked them. A `SIGSTOP` pending for the process draws both counting threads in at once, because the kick is a broadcast; one takes the stop, and the other, having seen no stop and then no signal, is still on its way out with interrupts open when the stop's own kick lands. Taken in the kernel, that interrupt is spent without a look, and the thread went back to user mode with its process stopped and nothing left to stop it -- alone on its processor, with no tick. It now masks, asks `needs_attention` again and goes round once more, as Linux's `exit_to_user_mode_loop` does. Control (x86-64, KVM, not landed; `~/ferrix-logs/fx0701/fx0701_control.py` on nazuna): the stop's taker waits 5 ms before entering the stop, and a thread of the three-thread program on its way out unstopped holds its window open 10 ms, printing a `NEGATIVE CONTROL fx0701` line at each entry, take and window. On main caa76fc3, 2 boots of 2 showed both counters entering with `SIGSTOP` deliverable, one taking it, the other's window closing with the process stopped -- and then the check's own "kept running instead of stopping" 10 s later; with the fix, 3 of 3 went through the same sequence and passed. Logs: `~/ferrix-logs/fx0701/ctl-0924-1826/` and `ctl-0924-1827/`. The futex stop check's flake below has another message and is not shown to share this cause. The history as it was investigated: FX-0701, seen once on 2026-09-16: `test-vfs --init ferrousli` on x86-64 panicked in stage 7's check "a thread of a stopped process kept running instead of stopping" (threads commit 5's stop check), at uptime 13.04 s. The same check passed in `test-shell` in that run and on the re-gate, and the log was overwritten. Not reproduced: 23 valid runs of that row on 2e877aa under an aarch64 boot loop for load, all passing, the last 7 with a diagnostic build that prints, when the check gives up, whether the process took the stop, each task's state, queue, switch and preemption counts and processors, and whether the counting threads still count (not landed; rebuild it from this description). Lead, from reading only: the check waits 10 s for every task to be blocked, so a 13.04 s uptime fits a stop that never reached one spinning thread rather than a slow host. The stop reaches a spinning thread by `sched::interrupt`'s broadcast IPI and the trap exit's `needs_attention`; no gap was found in that path by reading, so the next step is the loop again with the diagnostics, longer, and on armv7a --smp 2. **Seen twice more on 2026-09-18, and both logs are kept:** the fourth and then the third boot of two `test-compositor` runs on nazuna under KVM, in the kernel's own boot check before the compositor had started, each the check's 10 s again (`spinning task 2` at 3.77 s and the panic at 14.03 s; 3.51 s and 13.64 s). Three other guests were running on the host for the first and none for the second, so load is not required. Both were on main 0888a9f0, which resolves a program's page fault with interrupts open and is the one change near this path; but 20 boots of the same image with that change and 20 with it taken back all passed, and the night's count is 2 of about 65 boots with it against 0 of about 60 before it, which decides nothing. `~/ferrix-logs/os-bd-gate/test-compositor-cores-bootpanic.log` (the panic at its line 3673) and `-bootpanic2.log` (line 2223) on nazuna; `~/ferrix-logs/os-bd-rate.sh <runs>` is the with-and-without loop. **A fourth sighting, 2026-09-19 (ferrix-a7):** boot 2 of an x86-64 `test-compositor` on nazuna under KVM, on main c3336c60, same shape again -- `spinning task 2` at 3.54 s and the panic at 13.68 s, the check's 10 s. Nothing else was running on the host but that one QEMU, which is a second run with no load behind it. Kept, not overwritten: `~/ferrix-logs/fx0701/2026-09-19-test-compositor-boot2.log` and `-boot2-xtask.log`. The immediate re-gate of the same image passed all 18 boots. That makes three sightings in `test-compositor`'s own boot check against one in `test-vfs`, all four at the 10 s. **A fifth, 2026-09-21 (stage 12's session):** boot 6 of an x86-64 `test-powerfail --seeds 12` under TCG on nazuna, on 89f9dc7d (main 21f47390 plus the power-fail harness), in the boot check long before stage 12's code runs -- `spinning task 2` and `1` at 3.58 s and the panic at 13.71 s, the 10 s once more, with the host at a load of about 3.7 from other sessions' guests. Kept: `~/ferrix-logs/fx0701/2026-09-21-test-powerfail-seed6.log` on nazuna. `test-powerfail` now retries a seed once when its boot dies before the churn starts, and says so, so this no longer ends a long run. **A sixth, the first on `AArch64`, the same evening:** the replay boot of seed 25 of an aarch64 `test-powerfail --seeds 25` under TCG on nazuna, on 32eed6a6 (main 21f47390 plus the power-fail harness) -- the panic at 18.07 s, the check's 10 s again after a slower boot. Kept: `~/ferrix-logs/fx0701/2026-09-21-test-powerfail-aarch64-seed25-replay.log` on nazuna. Both boots of a seed are retried now. **A seventh, 2026-09-21 (os-ef):** `test-threads` on x86_64 under TCG in the full-matrix gate of 5bdc7567, in the boot check before init -- `spinning task 1` at 3.53 s and the panic at 13.66 s. The re-gate of the same tree with one formatting-only commit on top passed every row. Kept: `~/.local/share/ferrix/logs/os-ef-gate-5bdc756-kept/threads.log` on nazuna. **An eighth, 2026-09-22 (stage 16's session):** `test-rustc --release` under TCG in the full-matrix gate of d969cdca, in the boot check before init and so before rustc ran -- `spinning task 1` and `2` at 3.42 s and the panic at 13.54 s, the 10 s again. The same compile under KVM passed in the same run. Kept: `~/ferrix-logs/fx0701/2026-09-22-test-rustc-tcg.log` on nazuna. **Ninth, 2026-09-22 (stage 16's session):** plain `test-boot --arch x86_64` under TCG in the full-matrix gate of 4091725a, `spinning task 1` at 3.60 s and the panic at 13.74 s, the 10 s once more; three reruns of the row on the same commit passed. Kept: `~/ferrix-logs/fx0701/2026-09-22-boot-x86_64-4091725.log` on nazuna. **Tenth, 2026-09-22 (stage 16's session):** `test-compositor --arch x86_64` under TCG, gating the terminal's cell damage (0a0780f8, the same code as main's 394f0fa2), in the last boot's stage 7 check before the compositor ran -- `spinning task 1` and `2` at 3.71 s and the panic at 13.88 s, the 10 s once more; the rerun of the row passed. Kept: `~/ferrix-logs/fx0701/2026-09-22-compositor-x86_64-0a0780f.log` on nazuna. **Eleventh, 2026-09-23 (stage 13's session):** `test-threads` on x86-64 under TCG, gating cgroup landing G1 (423166b), in the boot check before the threads test ran -- `spinning task 1` at 4.31 s and the panic at 14.47 s, the 10 s once more. The same check passed in that commit's other boots, and in 30 boots of G1 alternated with 30 of main's f8647fe7 FX-0701 did not recur on either. G1 changes how processes are counted into jobs, not how they stop. Kept: `~/ferrix-logs/fx0701/2026-09-23-threads-x86_64-423166b.log` on nazuna. **Twelfth, 2026-09-23 (stage 13's session):** `test-net --arch all` on AArch64 under TCG, in G1's final gate (a6916326), in the second boot's stage 7 check before the network test -- `spinning task 1` and `2` at 10.06 s and the panic at 20.21 s, the 10 s once more; every other row of that gate passed. Kept: `~/ferrix-logs/fx0701/2026-09-23-net-aarch64-a691632.log` on nazuna. 2 points | done | 7 |
| A two-last-threads exit check that provably races: spin-meet on two processors, with its negative control -- the old last-thread decision put back -- failing by name. Today's check passes that control too, so it shows only that such a process ends with its first thread's status (from stage 9's review of threads commit 4). 1 point | threads (os-9f) | 7 |
| End-to-end user programs for what the `sigpaths` check proves at the kernel's decision: a `SIGSEGV` caught on the alternate stack, and a read interrupted by a handler and restarted under `SA_RESTART` (`SA_RESTART` and the driven signal paths have landed) | ferrix-a5 | 7; `rustc` needs `SIGSEGV` on the alternate stack |
| Stage 6's reverse-map check reported "-4 frames not given back" once in two loaded musl `test-vfs` runs on 2026-09-14: four frames — one kernel stack — came back from outside its window. Cause confirmed by a marker-gated control (the check's own warm-up task, which posts its outcome and exits, reaped inside the measured window because `wait_until_reaper_quiet` counts only tasks already exited). Fix: `run_pinned` waits for its task to be dead before returning, landing with `MAP_PRIVATE`, the control showing zero after it. The threads checks' returning before their programs' spaces are dropped is a separate, real hazard shared with `exec::run`: P2 | os-58 (fix), os-43 (control) | 6 |
| Stage 8's eventfd check reported "-4 frames across the second run" once in 13 x86-64 `test-net` boots with the musl busybox on 2026-09-16 (os-12's matrix): the same shape, its own warm-up reader task dying inside the measured window. `sched::wait_until_gone` now waits until a task is dead and the reaper quiet, and the eventfd check's reader and `poll`/`epoll_wait` waiter wait on it before their handles drop (1 point, landed 2026-09-16). Its control: a reader lingering 2 s after answering gives "4 frames across the second run" without the wait and passes with it. Next, 2 points: `exec::run` and the other check sites that return before their task is dead converted to `wait_until_gone`, each named in the commit, so no frame-counted check meets this shape again | os-26 | 6, 8 |
| Retire the 20 ms console polling. Receive by interrupt into a 4 KiB ring has landed on all three: the PL011, the STM32 USART (through ST's EXTI on the DK1), and x86-64's 16550 through an I/O APIC input found from the MADT, with `console::input::{waiters, has_input}` for the console thread to wait on. Left: the console thread and readers waiting on it instead of sleeping (in `fs/terminal.rs`) | ferrix-a5 | 7, 15 |
| A real init, which is what is left of stage 15. The kernel starts the shell itself as pid 1 (`kernel/src/init.rs`), or since L3 (2026-09-24) the file `ferrix.init=` names, with a `reboot(2)` that commits the disks first; so nothing in user space mounts `/proc`, `/dev` and `/tmp`, reaps what a session orphans, gives a shell a session and a controlling terminal of its own, respawns one that dies, or brings the machine down on `SIGTERM`. Job control landed on 2026-09-19 without it, because a shell that is pid 1 is a session leader and is given the console by asking (`kernel/src/syscall/tty.rs`); an init is what makes a *second* terminal, and a getty per terminal, possible at all. Wants the gate to grow with it: `cargo xtask test-jobs` types at one console today. **Designed 2026-09-23 in `docs/INIT.md`**: 67 points to L10, and 18 more later (L11 to L13); needed stage 13's cgroups, C1 to C5 of its §0.1, all met with C7 on 2026-09-24 (G1 to G4, `docs/CGROUPS.md` §7.1), and C8, for native services, the same day (G5). 51 of the 67 points are left (`docs/INIT.md` §16): L3, L1 and L2 landed 2026-09-24, L1 and L2 being `libs/svc`, the manager's pure core (unit files, the dependency graph, operations, the slice tree, restart policy, boot and shutdown, as `Manager::step`); L4, the init program, is next | open | 15; stage 13's cgroups for its first boot are in |
| `AF_UNIX` descriptor passing, after names (bf4eec48). `SCM_RIGHTS` has landed: files travel with the first byte of a message, a receive installs what fits and closes and flags the rest, and a peek installs nothing (a written deviation). The in-flight cycle pass has landed too: sockets that only each other's queues refer to are emptied at the next close, process exit or exec. Left: `SO_PASSCRED` and `SCM_CREDENTIALS` (2). Carried from landing 1's review: cap a receive's kernel buffer at the receive capacity (`MSG_WAITALL` in chunks of it), drop a refused file outside the `FdTable` lock before descriptors travel, `SO_SNDBUFFORCE`/`SO_RCVBUFFORCE` needing `CAP_NET_ADMIN` and skipping the cap, and Linux's error order in `sendmsg` and `socketpair`. On the compositor's path (stage 17) as well as POSIX's | os-02 | 7, 17 |
| POSIX.1-2024 interface sweep: from musl's implementation of every mandatory POSIX.1-2024 function, the list of Linux system calls (and flags) they need; the stage 7 sweep tooling runs each on all three architectures and files every `ENOSYS`, `EINVAL` on a mandatory flag, or wrong result with the area that owns it, as rows here. Sockets and threads are known and excluded; the `epoll`, `eventfd`, `timerfd` and `signalfd` families are in scope because stage 17 needs them | os-b6, after `AF_UNIX` and the terminal switch | 7, 8, 17 |
| `mprotect` could make a shared mapping writable that its file never allowed: fixed with `memfd_create`'s may-write accounting, `AddressSpace::protect` now refuses `PROT_WRITE` on a shared file mapping that may not write its file with `EACCES`, and the memfd boot check shows it on a file opened read-only, landed | stage 8 (os-58) | 8 |
| `memfd_create` with `F_ADD_SEALS`/`F_GET_SEALS`, on tmpfs, after file-backed `mmap`: `wl_shm` is a sealed memfd both sides map. Landed: seals in tmpfs, the write seal refused while a shared mapping may write the file (a fork child's copy counted), and the FX-0880 boot check; the exec seal (`MFD_NOEXEC_SEAL`, `F_SEAL_EXEC`) is left | stage 8 (os-58) | 8, 17 |
| **Done 2026-09-19.** The seam, gated: `scripts/check-device-access.py` and `scripts/device-access-allowlist.json` refuse a volatile access or port instruction under `kernel/` outside an argued entry. Three kinds are kept apart, because conflating them would make the gate noise: `register` (MMIO or an x86 port, confined to the paths §1 and §7 permit), `shared` (RAM a device also walks -- IOMMU tables, the block and net rings), `memory` (volatile only so a boot check cannot be optimised into proving itself). 34 register sites, 11 shared, 35 memory. In `cargo xtask check` and CI, with five negative controls. Two detector bugs were found by checking the count against grep: path-qualified calls were invisible (hiding `cpu::outb`'s users) and the comment skip ate `*byte = ... read_volatile(..)` | os-f6 | 10 |
| **Done 2026-09-19.** Isolation per platform, written down: `docs/ARCHITECTURE.md` §7 now holds the table -- x86-64 translated through VT-d, AArch64 through the `SMMUv3`, ARMv7-A and the DK1 in degraded trusted mode -- each row naming the console line that decides it, every row read from a boot log rather than the roadmap's prose. The two untranslated rows are kept apart: ARMv7-A has an `SMMUv3` the kernel declines to program because U-Boot resets on `VIRTIO_F_ACCESS_PLATFORM`, the DK1 has no unit at all (read on the board). Function counts are deliberately not quoted: they vary with the devices a gate configures | os-f6 | 10 |
| Per-open windows onto the card VMO: a program that mapped `/dev/dri/card0` and closed it can still read what the next opener draws, since the exclusive open does not end a mapping (`docs/DISPLAY.md` §2.3). Until then `card0` is `0660` and root's, accepted for iteration 1 by os-f6 2026-09-16. Each open gets its own view of the card's pages, which comes with stage 19's render node | GUI session (os-e5), with stage 19 | 17, 19 |
| **Done 2026-09-24.** ferrousli's wrapper for `signalfd`, which stage 17's Rust event loop (calloop over the `polling` crate) can use, once the kernel has it; with a C test on the host and the same program booted on Ferrix, as the epoll and eventfd wrappers had. The epoll and eventfd half landed on 2026-09-17: all six epoll calls, `eventfd`, `eventfd_read` and `eventfd_write`, and `epoll_pwait2` declared in `sys/epoll.h`. The `timerfd` half landed on 2026-09-24 with foot (`docs/CHROME.md` §6): `timerfd_create`, `timerfd_settime` and `timerfd_gettime`, a C test on the host, and foot on Ferrix as the program that uses them. The `signalfd` half landed the same day with the kernel's `signalfd4` (`docs/CHROME.md` §3): `signalfd` over `signalfd4` on every architecture, its number through `tools/gen-abi.py`, and `tests/c/linux/signalfd.c` on the host -- a signal read through epoll with its sender, one outside the mask left pending until the mask changes, two in one read, a blocked read woken by a child's `kill`, and the errors. On Ferrix the kernel's boot check (FX-0884) drives the same calls by number; no ported program uses it yet. 1 point | os-a8 (ferrousli), after os-26's kernel rows | 17 |
| **Fixed 2026-09-24 by "Close a channel end when its handle closes, not when a drain reaches it".** A second path to the same refusal -- a write holding the driver's end past its wake -- was fixed on 2026-09-26; see the P2 row fixed by "Reach a channel's peer through the channel, so no write holds it open". `object::dispose` queued a closed channel end whole, and when another processor was draining that queue a close returned at once and left the end for the drain; until the drain reached it the end was still referenced, so the kernel's end of the ring's control channel was not `PEER_CLOSED`, and the check's `device_quiesce`, made the instant it closed its driver's end as devmgr does on `TERMINATED`, was refused as `BAD_STATE` -- a driver still serving. A channel end now closes in `dispose` itself: the last reference is taken with `Arc::into_inner`, what its unread messages carry is queued, and the emptied end is dropped at once, as an interrupt's line has been freed since 1e67240f. c2129a68 was not it. Both checks the race reached now close under `object::as_if_draining_elsewhere`, and the ring check names the status the quiesce returned. Control (x86-64, KVM; `~/ferrix-logs/fx1004/fx1004-ctl.sh` on nazuna): main with only the ring check's change failed 2 boots of 2 with "quiescing the instant a driver died was refused: its channel was still open"; main with only the object check's change failed 2 of 2 in stage 9 with "a write to a closed channel was accepted"; the fix passed 2 of 2, and one AArch64 TCG boot. Logs: `~/ferrix-logs/fx1004/ctl-0924-2121/` (object) and `ctl-0924-2125/` (ring, fix) on nazuna. The history as it was investigated: Stage 10's block ring self-check flakes on x86-64: once in 11 `test-boot --arch x86_64` runs on nazuna (TCG) it panicked FX-1004 "stage 10 block ring self-check failed: quiescing the instant a driver died failed" (`kernel/src/block_ring/check.rs:343`, `kernel/src/main.rs:844`), at 669c2303 in E4's row; the other 10 boots, 5 with E4 and 5 at its base 1a55a1d3, and both `test-display` boots of that row passed the same check. E4 does not touch the block ring. The check waits on a driver's death, and c2129a68 changed how waits wake (on the event, not a 5 ms recheck), so start there. Log: nazuna `~/.local/share/ferrix/logs/gui/row-669c2303/boot-x86_64.log`. **Second, 2026-09-23 (stage 13's session):** once in 30 x86-64 TCG boots of cgroup landing G1 (a6916326), alternated with 30 of main's f8647fe7 that all passed; G1 does not touch the block ring or its check, which ends its driver by closing a handle, not by a process death. Kept: `~/ferrix-logs/flakes/2026-09-23-fx1004-quiesce-a691632.log` on nazuna. **Third, 2026-09-23 (os-8d), the first on AArch64:** the replay boot of seed 5 of an aarch64 `test-powerfail --seeds 8` under TCG on nazuna, in the full-matrix gate of 50ae697 (stage 20's landing, which does not touch the block ring either), at 10.08 s, before stage 12 began; the seed's retry passed, as did every other row. Kept: `~/ferrix-logs/fx1004/2026-09-23-powerfail-aarch64-50ae697.log` on nazuna (the panic at its line 3700). **Fourth, 2026-09-23 (the desktop-performance session):** one of the 22 boots of `test-compositor --arch x86_64` at bee16ec7, the cursor plane, which does not touch the block ring -- the boot after `groups`; the same row passed whole before and after. Log: nazuna `~/.local/share/ferrix/logs/os-ba-fx1004/compositor-bee16ec7.log`. **Fifth, 2026-09-23 (the same session):** the third boot of the same row at 50658ca9, the idle-loop landing, which touches only a socket's readiness, its boot check and the clipboard driver, at 7.27 s; every other row of that gate passed. Log: nazuna `~/.local/share/ferrix/logs/os-ba-fx1004/compositor-50658ca.log`. **Sixth, 2026-09-24 (FX-0701's futex session):** x86-64 `test-boot` under KVM, one of four boots of the futex fix with its negative control applied, at 7.23 s, stage 7 having passed; the other seven boots of that run passed stage 10. Log: nazuna `~/ferrix-logs/fx0701/futex-ctl-x86_64-0924-2049/b-4.log`. **Seventh and eighth, 2026-09-24 (sysfs), and one on main's own tree:** twice in about 15 x86-64 TCG boots of the sysfs landing (a `test-restart` boot at 1e5e7108, one of three `test-boot` reruns), whose only block-ring change is looking up the device node when a HELLO is accepted. Then a control of eight boots each, alternated: main at d5eb9798 panicked once, the sysfs commit on top of it none. Logs: nazuna `~/ferrix-logs/fx1004/2026-09-24-restart-x86_64-1e5e7108.log` and `2026-09-24-control-main-x86_64-d5eb9798.log`. The check closes the driver's end and quiesces at once, and `block_ring::wait_until_unserved` refuses with `ByADriver` whenever the kernel's end does not show `PEER_CLOSED` yet, so start there. 2 points | done | 10 |
| **Fixed 2026-09-24 by "Go round a futex wait whose stop was gone before it answered".** `futex::wait` ended on a pending `SIGSTOP` or a stopped process and then asked `signal_pending` again to choose its answer. Between the two looks another thread could take the stop -- dequeued, the process not yet marked stopped -- or the stop could be over, continued while the waiter had not yet run; neither woken nor signalled, a wait with no timeout answered `ETIMEDOUT`, which the program recorded. It now goes round again when it ends for none of its reasons, as Linux's `futex_wait` does after a spurious wakeup, and answers `ETIMEDOUT` only past its deadline. The check had a gap of its own, closed in the same commit: after `SIGCONT` it passed as soon as the counts moved and a waiter was listed, and a waiter the stop woke stays listed until it answers; it now waits the 50 ms still window and reads the recorded answer again. Control (x86-64, KVM, not landed; `~/ferrix-logs/fx0701/fx0701f_control.py` and `fx0701f-ctl.sh` on nazuna): a futex waiter the stop woke is held asleep until the stop is over before it answers -- the host stall the TCG sightings needed, made certain -- with a `NEGATIVE CONTROL fx0701f` line as it is held and as it answers. With the stronger check and without the fix (e0ed964c), 4 boots of 4 printed both lines and then failed with the check's own message; with the fix, 4 of 4 printed both and passed stage 7 (one then hit stage 10's block ring flake, the row above). Logs: `~/ferrix-logs/fx0701/futex-ctl-x86_64-0924-2049/` on nazuna. The history as it was investigated: Stage 7's futex stop check flakes on AArch64: "a FUTEX_WAIT stopped and continued returned instead of being restarted" (FX-0701, the stage 7 code, with a message of its own). Seen twice on 2026-09-23, both under TCG on a loaded nazuna: in `test-display` on another session's 9d44b791, which does not contain the cgroup work, and in one of three `test-net --arch all` reruns of cgroup landing G1 (a6916326), where main's f8647fe7 passed three. Like the thread-stop check above it is a stop and a continue timed against the machine, so it probably shares that check's cause. Kept: `~/ferrix-logs/fx0701/2026-09-23-display-aarch64-futex-9d44b79.log` and `~/ferrix-logs/fx0701/2026-09-23-net-aarch64-futex-a691632.log` on nazuna. **Third, 2026-09-23 (os-8d, stage 20):** on ARMv7-A, in `test-powerfail`'s seed 4 churn boot at 6.76 s, in the full-matrix gate of 27ed9f30; the retry passed. Kept: `~/ferrix-logs/fx1004/2026-09-23-powerfail-27ed9f3.log` on nazuna, the panic at line 5626. 1 point | done | 7 |
| Stage 7's signal hand-off check flaked on 2026-09-24: "a signal sent to a process of two threads was given to no thread" (`kernel/src/syscall/check.rs`, the check that blocks `SIGUSR1` in the first thread and requires the second to be handed it), at 10.01 s in `test-shell` on x86-64 under TCG with zinc, in cgroup landing G4's first gate (6c553b8c) on a loaded nazuna. The rerun of the row on the same commit passed, as did every other boot of that gate and of G4's final one. G4 changes `clone3` and cgroup moves, not signal delivery; `take_handed_to` reading 0 means `kill::send` picked no thread at all, so start at how the send judges a thread that is between its wake and its return, beside FX-0701's `return_to_user` fix. Kept: `~/ferrix-logs/stage7-handoff/2026-09-24-shell-zinc-6c553b8c.log` on nazuna (the panic at its line 106). **Again 2026-09-26**, at 12.81 s in one of 28 x86-64 TCG `test-boot`s of the FX-1004 channel fix (branch `fix-fx1004-endpoint`, before its commit; it changes only `object/channel.rs`), QEMU pinned to two cores shared with two busy loops. Kept: `~/ferrix-logs/stage7-handoff/2026-09-26-boot-x86_64-fx1004-channel-fix.log` on nazuna. | open | 7 |
| `test-compositor` on x86-64 under TCG failed once, on 2026-09-26: "the slowest frame took 5715709 us, past the 5000000 us a frame under emulation is allowed", in the window-sliding boot of the gate of feb1d6cf (virtio-gpu `ioeventfd=on`). The frame's time was software drawing -- shadow 2.0 s, surface 1.5 s, border 0.9 s, blur 0.4 s, the flip 0.4 s -- which the change does not touch, with the host at a load of 11 to 26 from other sessions' guests. Two reruns of the whole suite on the same commit passed, with slowest frames of 0.99 s and 1.59 s. Kept: `~/ferrix-logs/compositor-slowframe/2026-09-26-compositor-x86_64-feb1d6cf.log` on nazuna. | open | 15 |
| `test-jobs` flaked once, on 2026-09-24: "the session at the console did not answer" after typing `echo jobs-gate: the shell reads the console`, on x86-64 in cgroup landing G4's gate (6551ea89). The transcript shows the typed line echoed only once the next line was typed, as if a console read woke late by one line; every other expectation of the run was met, and the rerun on the same commit passed. G4 does not touch the console or the terminal. Kept: `~/ferrix-logs/jobs-console/2026-09-24-jobs-6551ea89.log` on nazuna. | open | 15 |
| ferrousli's busybox on Arm fails two gate rows that pass on x86-64, found on 2026-09-23 when the cgroup gate first ran `--init ferrousli` on all three architectures after 093bfe1e, and reproduced on main 90b0a1de without the cgroup work. (1) `test-vfs --arch aarch64|armv7a --init ferrousli`, command 17: the uid-1000 `cat` of a private file prints `cat: can'...` where `cat: /tmp/dac-private: Permission denied` is expected, so on Arm the errno or its `strerror` text differs. (2) `test-net --arch armv7a --init ferrousli`: 10 of 13 programs fail, starting with `udhcpc: poll: Invalid argument`, so the guest never gets an address; AArch64 passes all 13. ARMv7-A's `poll`/`ppoll` in ferrousli (the time64 variant?) is where to start. Kept: `~/ferrix-logs/ferrousli-arm/2026-09-23-vfs-aarch64-90b0a1de.log` and `~/ferrix-logs/ferrousli-arm/2026-09-23-net-armv7a-90b0a1de.log` on nazuna. os-b0, whose Arm port this is, had ended when it was found. | open | 15 |
| ~~Stage 8's eventfd check flaked once, on 2026-09-23: "a waiting eventfd reader was ended by its recheck, not by the write's wake", in one of ten x86-64 TCG boots of cgroup landing G1 (7ee833b); none of 30 more boots of G1, nor 40 of main, repeated it. It is the message the check's own negative control prints with the write's wake removed, so a wake that arrives after the reader's recheck under load is where to start. G1 adds a tree-wide spin lock on process creation and release and no eventfd path. Kept: `~/ferrix-logs/flakes/2026-09-23-eventfd-recheck-7ee833b.log` on nazuna.~~ **Fixed 2026-09-23 by "Let a blocked eventfd wait trust its queue, and check it once it waits" (os-d1).** Seen twice more the same day: on ARMv7-A in `test-compositor` under host load (os-d1's armv7 work, logs in nazuna `~/ferrix-logs/fx0882/`), and on ARMv7-A in `test-threads` in G2's gate 960fffe (`~/.local/share/ferrix/logs/cg-gate-960fffe/threads.log`). The cause was the wait, not the lock: a blocked eventfd read waited in 5 ms slices and left the queue between them, so a write landing in the gap woke nobody and the reader found the value on its next slice -- and the check's 20 ms sleep is exactly four slices, which load lines up. Blocked reads and writes now wait on the queue with the 1 s recheck `poll` and epoll use (c2129a68), and the check waits until the reader is listed on the queue (`WaitQueue::listed`) rather than assuming it after 20 ms | done | 8 |
| **Fixed 2026-09-26 by "Count a wake that lands before a waiter's last look as the wake it is" and "Wait for the exec check's programs to be gone, not only ended" (branch fix-queued-small).** Stage 8's signalfd check, FX-0884, failed twice in the loaded 20+20 `test-shell` control of 2026-09-26 (`docs/certification/SPECULATION.md`), once on main 74e1ea46 and once on cert-f31-mitigations e64b2013, with two different sentences and two causes. (1) "a waiter on a signalfd was ended by its recheck, not by the signal's wake": the check sends once it sees the waiter listed on `signal_arrived`, and a listed waiter has not yet taken its last look; a signal landing in between ended the wait at the last look, which `wait_until_deadline` and `wait_on_any` did not count as a wake. They count it now. The eventfd and timerfd checks read the same count, and row 230's fix -- wait until the reader is listed -- is what opened this window. (2) "-148 frames across the second run", released 137 and user tables 9: the exec check just before it dropped its three program tasks at `wait_for_exit`, which returns at the release while the task still holds the process and its address space until reaped, so the space went inside signalfd's window. The exec check now waits `wait_until_gone`. Negative controls (scratch edits, quoted in the two commits): holding the waiter 5 ms between listing and last look reproduces (1) every boot before the fix and passes after; holding an exec program's exiting task with its process until signalfd's window opens reproduces (2) ("-307 frames") before and passes after, the exec check waiting the stalls out. Loaded loop, x86-64 TCG, QEMU and 4 then 8 bounded busy loops pinned to CPUs 16-19: before the fixes 0 FX-0884 in 8 + 16 boots, after 0 in 16 -- the natural rate, about one in eighty loaded boots, is too low for a loop of this size to show, which is why the controls widen each window. Logs: `~/ferrix-logs/fqs-fx0884/` (e1-e4 the controls, loop-* the loops); the two sightings in `~/ferrix-logs/f31/fx0884-signalfd-{main-74e1ea46,branch-e64b2013}.log`. | done | 8 |
| **Fixed 2026-09-26 by "Give a timerfd waiter that a host stall made late another attempt" (branch fix-queued-small-4).** Stage 8's timerfd check, FX-0883, failed once under the drcov coverage plugin, ARMv7-A `test-threads` at `--smp 2`: "a waiter came back 384 ms after its deadline", the wake count having moved -- so not the window aeb9a377 closed (a wake before the waiter's last look, read as a recheck), which that message would have been. Scratch instrumentation of when the `timerfds` thread fired split the lateness: in 12 drcov boots of main every waiter was 0.1-1.9 ms late; with one 400 ms SIGSTOP of QEMU injected after the eventfd line (a `FERRIX_QEMU` wrapper that stops its own QEMU by PID), 12 of 12 boots failed FX-0883 at 352-400 ms, every millisecond of it between the deadline and the thread firing, the wake to the waiter's return under half a millisecond. The guest's clock runs on while the emulator is stopped, so a timer due inside the stop fires as late as the stop is long -- the host descheduling QEMU, or QEMU busy outside the guest, which a plugin on every block makes likelier -- and nothing inside the guest can tell that from a kernel that overslept. The sighting is that signature; it was not caught in the act. The check now times each waiter up to three times and passes on the first attempt that is on time and woken by the deadline, printing every miss; a kernel that oversleeps or never wakes the queue fails all three. After: the stall loop 12 of 12 pass (8 print "attempt 1 of 3 ... 369-398 ms"), 12 plain drcov boots pass; negative control, a scratch edit making the thread sleep 300 ms past every deadline, stops the boot at FX-0883 after three attempts of 302 ms. The signalfd (FX-0884) and eventfd checks bound their waiters the same way and have the same exposure to a stall; not changed. Logs: `~/ferrix-logs/fqs4-fx0883/`. | done | 8 |

### P2 — quality and performance, on the "fast" half of the goal

| Item | Owner |
|---|---|
| `run-compositor --everything` (and so `--chrome`): Chrome's window sometimes never paints, and sometimes goes blank once it is clicked and typed into, seen on 2026-09-26 on x86-64 under KVM, on main 24fb419b plus the host-layout default. Of seven boots on the btrfs root and on tmpfs, four painted the welcome page; one had no window at all after 40 s, its network service ending with "Terminating current process after 15 seconds with no connection"; one showed an empty window before any input; and every one that was clicked into and typed at (two of two, with `us` and with `de`) went blank and stayed blank, though Chrome still asked for cursor shapes and printed no error. Not the keyboard layout: both `us` and `de` did each. Chrome's binary names `/usr/share/X11/xkb`, which neither volume carries; a boot with XKB data under `XKB_CONFIG_ROOT` painted and then went blank on the click like the rest, so that is not it either. `test-chrome-window` does not click. Kept: `~/.local/share/ferrix/logs/b3-chrome-blank-2026-09-26/` on nazuna, the five serial logs and their screenshots. | open |
| `test-compositor`'s driver-restart check failed once on 2026-09-26, x86-64 under KVM, gating the cursor-shape fix (9b1339a0, which touches only `wl_cursor_shape_device_v1`): "the display driver was not restarted under the compositor: the compositor saw its card go: `the card went away` 0 of 1 times". The restart itself happened -- devmgr started the driver again twice (restart 1 at 5.50 s, restart 2 at 5.76 s) and hyprix said "the card is back; drawing on it again" at 6.02 s -- but hyprix never printed `the card went away`, so either it missed the first loss between two restarts 260 ms apart or the line is not printed on that path. The rerun of the same tree passed every boot. Kept: `~/.local/share/ferrix/logs/cursor-shape-compositor-fail1.log` on nazuna, lines 350-388. **No longer occasional, 2026-09-26 late:** 4 of 4 runs failed the same way, 2 of them on main 94dc95bc with nothing added (`baseline-compositor-1.log`, `-2.log`) and 2 with the window-geometry crop on top (`crop-compositor-fail1.log`, `crop-compositor.log`), all in `~/.local/share/ferrix/logs/` on nazuna. Each time devmgr's first restart comes at 5.35-5.39 s, within 20 ms of hyprix printing its monitor and its socket, and hyprix never says the card went away, only that it is back after the second restart. The lead is that the first loss lands before hyprix is watching for it; the check runs in the first boot, so while it fails every other boot of test-compositor goes unrun unless named with `--boot`. 1 point | open |
| **Fixed 2026-09-26 by "Reach a channel's peer through the channel, so no write holds it open"** (branch `fix-fx1004-endpoint`): a channel's state -- each side's queue, waiters, port registrations and a closed mark -- lives in one `Channel` both `Endpoint`s share, and an end reaches its peer's side through it, never through the peer's `Endpoint`, so no write, `signals()`, `peer_has_room()` or read that makes room can hold the other end open; an end is open exactly while its `Endpoint` is alive (a handle, a message in flight, its own holder's call), and its close marks its side closed under the queue lock a write checks. The window is gone at its cause, not waited out. **6afa79fc on `dk1-console/tx-fix` is superseded -- do not land it:** its `Endpoint::peer_gone` waited up to 20 ms for a dead driver's end, a timing assumption nothing bounds, and its check that holds the end deliberately tests a hold that is now an owner, whose refusal is right. Measured on nazuna, x86_64 `test-boot` under TCG, QEMU pinned with `taskset` to cores shared with one busy loop each, alternated with main (94c07969): 4 cores, main 1 FX-1004 in 18 (plus one stage 3 FX-0302), the fix 0 in 9; 2 cores, main 4 in 18, the fix 0 in 28 (one stopped before stage 10 at stage 7's signal hand-off, the open P1 row). Negative control (not committed, `negctl-full.patch`): the fix with the old hold put back -- `write` upgrading a weak peer before the push and dropping it 5 ms after the wake -- stopped with "quiescing the instant a driver died was refused: its channel was still open" in 4 of 4 loaded 2-core boots and 1 of 2 unloaded. Logs and the loop script: `~/ferrix-logs/fx1004/loaded/` on nazuna (`main-94c07969*/`, `fix*/`, `negctl*/`). The history: found the day main's fix landed, by the DK1 session: `Endpoint::write` held its reference to the driver's end until after it had woken the reader, so a driver that read READY, closed its handle and was quiesced at once could find its end still alive for a moment (4 of 30 loaded boots on a branch without main's fix); recurred twice in about four x86_64 `test-shell` runs on 2026-09-26 on a branch adding speculation barriers on context switch (`~/ferrix-logs/f31/fx1004-test-shell-x86_64-34482ecf.log`) | done |
| `test-compositor`'s `dispatchers` boot is red on main on x86-64 and armv7a since at least 45425ee5 (2026-09-24): "the focus moved twice by keybind and the socket said so 2 times in all" -- the event socket carried two `activewindowv2` of the three the check wants, in 3 of 3 runs on main and on every branch built on it | unowned, found by the DK1 session |
| hyprix advertises `wp_fifo_manager_v1` and `wp_commit_timing_manager_v1` and ignores their requests (`server/src/client/frames.rs`, `Role::Fifo \| Role::CommitTimer`): Mesa's Vulkan WSI then stops using frame callbacks and a FIFO client runs unthrottled -- vkgears drew 3600 fps against a headless hyprix. Implement the barrier or stop advertising both; also `wp_presentation` never sends `clock_id` after bind, and `now_monotonic()` (`hyprix/src/state.rs`) is the wall clock, so `presented()` carries it. Repro: `~/ferrix-logs/os-ac/hyprix-host.sh` on nazuna | unowned, found by os-ac |
| hyprix as pid 1 never reaps orphans: a pipeline's earlier stages outlive their parent and stay zombies, which is why zinc does not exec a pipeline's last stage in place (zinc's "Run a subshell's last external command in place") and pays a fork a prompt for agnoster's `$(jobs -l \| wc -l)`. 2 points | unowned |
| The DK1 desktop, what 2026-09-24 left (`docs/ROADMAP.md`, the desktop at the speed of a hand): pointer motion off hyprix's frame thread, since a frame being drawn still holds the pointer (5); the first blur of a translucent window, 0.6 to 1.9 s in f32 (3); the Cortex-A7 at 800 MHz with VDDCORE raised through the STPMIC1 first (5); U-Boot's saved `bootdelay` of 2 s and OP-TEE's 1.4 s finding its device tree, which are firmware | unowned |
| The cost of the 20 µs one-shot armed on every wake onto the caller's processor, measured on pipe and futex paths | ferrix-34 |
| CI's host steps from one rule: GitHub CI's test job went red on 06336c60 with E0152, a second `panic_impl`, building `ferrix-devmgr` as a host test. CI's host clippy, test, doc test, documentation and native clippy steps carried their own list of freestanding crates, older than `xtask/src/check.rs`'s. Now `xtask/src/workspace.rs` sorts the workspace by a rule (members at `boot`, `kernel` and under `user/` are freestanding; a `#![no_main]` program anywhere else fails the gate), CI calls `cargo xtask host-clippy`, `host-test`, `host-doctest`, `host-doc` and `native-clippy`, and `cargo xtask check` runs the same functions, doc tests and documentation included. 2 points, landed 2026-09-16 | os-26 |
| Waits that wake on the event: `poll`, `ppoll`, `select`, `pselect6` and the epoll waits recheck every 5 ms rather than sleeping on the files' wait queues, so a compositor's frame loop and every idle client pay a wake-up per slice and up to 5 ms of latency. The same wake-from-the-source pattern as d22f9d3: a pollable inode names the wait queues its readiness reads, and a wait joins all of them before its last look, so a `wake_all` on any of them ends it. 5 points, landed 2026-09-16. Written deviations of the epoll landing (d047480d) until then: `EPOLLRDHUP` and `EPOLLPRI` are never reported, because `Readiness` carries neither a half-closed peer nor urgent data | os-26 |
| Per-CPU frame and heap caches, deferred since stage 2 | open, once a workload can measure them |
| `Inode::ioctl`: `sys_ioctl` special-cases the console, sockets and `/dev/dri/card<N>` by the open object's type; a hook on the inode replaces the three branches (os-02's review of the display stack, 2026-09-16). `/dev/input/eventN` (`docs/INPUT.md` §3.3, L6) would be a fourth special case | open, kernel VFS owner |
| Checked register offsets in the ring-3 virtio drivers: `Block::read`/`write` in `user/blk` and `user/gpu` assert on a device-controlled `notify_off` × multiplier, and on ARMv7-A `offset + size_of::<T>()` can wrap past the bounds check; one shared checked-offset accessor for both (os-02's review, 2026-09-16) | open, driver owner |
| devmgr's `await_published` kills a driver that exits without publishing and marks it dead, but never quiesces its device, as it did not before that change either. The IOMMU mappings go with the process, so this is hygiene rather than safety: quiesce the device once its driver is gone. Stage 10 (os-02's review of the display stack, 2026-09-16) | open, devmgr owner |
| ASIDs and PCIDs, so a switch stops invalidating every user entry | open, after threads |
| A gate for btop: a program in `test-net` or `test-vfs` on x86_64 that runs `btop` under `timeout` on the console and requires its panels' titles in the output, with a negative control that shows the check fails when btop cannot start (the 4 MiB `execve` limit it hit is the obvious sabotage). 2 points | open |
| A gate for sshdt: a `test-net` program on x86_64 that starts `sshdt` with a key `xtask` generates, and a `--forward` through which the host's `ssh` runs a command and requires its output, with a negative control (no forward, or a key the server was not given) that shows the check fails. Needs an `ssh` client on the gate host and on CI. 3 points | open |
| sshdt at boot: `run-compositor --ssh <port>` starts `sshdt` from the compositor's `exec-once` with a key kept on that machine, the host user's `authorized_keys` and `*.pub`, and whatever `--ssh-key` names, and with a host key kept beside the first; `remote-desktop` asks for it (`[ssh] port`, 22022) and tunnels it. A busy port costs the SSH, not the boot. Proven 2026-09-21 on nazuna: this PC logged in through an `ssh -L` tunnel, and a key never carried was refused. 2 points, landed 2026-09-21. Left: `run`'s serial shell has nothing to start it from | done |
| A key of the machine's own, so a client with an empty `~/.ssh` can log in: `~/.local/share/ferrix/ssh/id_ed25519`, made once, authorized by every `--ssh` boot and printed as the `ssh -i` line to paste; `--ssh-key <FILE\|KEY>` for a key in neither place. Before it, a sandbox with no private key was refused with `Permission denied (publickey)` and read it as a server that refuses non-interactive sessions. Proven 2026-09-21 on nazuna from a client with an empty `~/.ssh`: a command, its exit status and a piped stdin, with no key and an unnamed key both refused. 1 point, landed 2026-09-21 | done |
| No `/etc/os-release` in the image: `cat /etc/os-release` over `ssh` says the file is not there, and it is what a client asks a machine what it is with. A line or five -- `NAME`, `ID`, `VERSION_ID`, `PRETTY_NAME` -- carried like the other `/etc` files. 1 point | open |
| `mlock` and `munlock` are `ENOSYS`; sshdt (through `russh-cryptovec`) warns once per run. A resident-only kernel can accept them as no-ops within `RLIMIT_MEMLOCK`. 1 point | open |
| uutils' `tty` on a PTY prints `/dev/pts/0` with no newline after it; busybox's `tty` prints both, and so does every other program over the same sshdt session. Found 2026-09-21 through `ssh -tt`; not yet narrowed to uutils or the terminal. 1 point | open |
| `test-vfs` on AArch64 and ARMv7-A fails command 17, the permission check, whatever the busybox: it expects uutils' `cat: /tmp/dac-private: Permission denied`, and the Arm images carry no uutils, so `cat` is busybox's, which says `cat: can't open '/tmp/dac-private': Permission denied`. Seen 2026-09-23 with Alpine's musl busybox and with ferrousli's alike; no gate runs `test-vfs` on Arm. Accept either wording, or carry uutils there. 1 point | open |
| ferrousli's Arm suites in CI: they run by hand under qemu-user (`ferrousli/README.md`), and CI runs x86-64's alone. Needs the cross gcc, QEMU's user mode and two rustup targets on the runner, about ten minutes each. 2 points | open |
| The ports built on Windows: `cargo xtask ports` refuses there, because `ferrousli/tools/ports/` needs gcc and the host's UAPI headers; `build-windows.sh` beside each, as busybox has, with clang and Alpine's pinned headers. libc++ with clang is LLVM's own configuration. 5 points | open |
| `/dev/rtc`, and keeping the clock right after boot: `CLOCK_REALTIME` starts from firmware's `GetTime` and then drifts with the counter, with no NTP and no RTC driver to correct it. The random generator is seeded (firmware, `RDSEED`/`RDRAND`, `RNDR`); a virtio-rng driver would reseed it while running | open |
| The debt the roadmap names: fuzz targets for `virtio`, `linux-abi` | open |
| The debt the roadmap names: Miri for `frame`, `heap`, `paging`; fuzz targets for `cpio`, `fdt`, `acpi`, `virtio`, `linux-abi` | ferrix-e5 (the first three crates and `cpio`, `fdt`, `acpi`) |
| Every gate's log names the tree it ran on: `xtask` prints `HEAD`, the branch and whether the tree was clean as the first line of every `check`, `test-boot`, `test-shell` and `test-vfs` log, so a row's evidence pins its commit by itself rather than by the runner's word (asked for by a review of the frame-window evidence, 2026-09-13) | open, cross-cutting |
| The host-test table in the roadmap generated from `cargo test --list` with a gate, instead of counted by hand | ferrix-24 |
| The POSIX measure: musl's libc-test functional and conformance programs built static against musl and against ferrousli, run under `test-shell` on all three architectures, with the pass count in the roadmap's host-test table and every failure filed with its owner | os-7c, with os-9f once threads run | 
| Zero-copy block reads: pin the page-cache pages themselves as the block ring's buffers, removing the data-VMO and scratch copies of stage 11's first read path (ARCHITECTURE §3) | ferrix-61, after stage 11's kernel mount |
| `crypt`'s `$2*$` blowfish hash, which gives `"*"` so a blowfish entry in `/etc/shadow` matches no password. The stubs this row once listed, regex, `awk`'s math and `dirname`, were all replaced by 2026-09-16 and `src/stubs.rs` is gone | open |
| The rest of ferrousli's POSIX.1-2024 gap, by area in `docs/POSIX-2024.md`: 247 interfaces, 80 points, none written on a branch since the three branches of 2026-09-13 landed. The largest parts: complex arithmetic (66 interfaces, 8 points); every `long double` form of `math.h` (59, 8), the only part of the math library left; realtime (29, 11: `aio.h`, `mqueue.h`, timers, `shm_open`); spawning (25, 7); locales and messages (21, 13: `gettext`, `iconv`, `strfmon`); processes and the system (8, 7); users and databases (`ndbm.h`, 9, 3); the `clock` waits and `pthread_atfork` (6, 3); terminals (7, 3); and POSIX.1-2024's declarations in musl 1.2.5's headers (3). Each landing updates the document's tables | os-a8 (ferrousli; was os-50) |
| The seam measured, 1: what the hop to ring 3 costs. Under `test-boot`, the kernel times submit-to-complete on the pattern disk at queue depths 1 and 32, and `blk` times its own device round trip for the same requests, so the difference is the ring, the doorbells and the scheduler between them and nothing else. Beside it, the same QEMU disk read at the same depths by a Linux guest (a stock image, `dd` with `iflag=direct`), as the in-kernel reference the 2026-09-13 decision forbids building in Ferrix. Both numbers, on x86-64 under KVM and on AArch64, go into the roadmap's stage 11 section with the boot line that carried them, and the zero-copy row above is re-costed against them. 5 points | open |
| The seam measured, 2: how much of a build-like workload crosses it. Counters kept from boot: Linux system calls answered; page-cache pages served from an inode's VMO against pages filled through the ring; ring submissions and completions; printed as one line at the end of `test-vfs`, and of the `rustc` run when stage 16 has one. The claim under test is that the seam is on a cold path for the goal's workload; the row is done when the ratio is in the roadmap's stage 11 section and the decision of 2026-09-16 cites it. 3 points | open |
| FX-1151 under WHPX at one processor: "net ring self-check failed: the kernel closed the control channel", with no program faulting, in `test-boot --arch x86_64 --init ferrousli --net --zinc --accel whpx --smp 1` on main 1a55a1d3 on the Windows PC (QEMU 11.1.0), 2026-09-16; two boots of nine at one processor, among them one of three with the workaround below in place, the others clean. Logs on the Windows PC: `~/.local/share/ferrix/logs/os-26-whpx/whpx-s1-b.log` and `whpx-fix-1.log`, beside the WHPX row's failing boot (`whpx-smp4-2.log`) and diagnostic boot (`whpx-diag-1.log`). **Where it stands, 2026-09-17 (os-26, stopped at the wind-down, about 1 of 2 points spent):** a diagnostic build printing why the kernel ends each net ring is the side ref `os-26/fx1151-diag` on nazuna (61bb2204, on a86115cb; not for main). 18 WHPX one-processor boots with it on the Windows PC did not fail, each ending its rings as expected (the check's bad HELLO refused for its handles, then the check's own close); logs on the Windows PC `~/.local/share/ferrix/logs/os-26-whpx/fx1151-1.log` to `fx1151-18.log`. Lead: both failing boots (23:47 and 23:51 on 2026-09-16) ran while the customer's own WHPX guest was running on the same PC, and none of the 18 clean ones did, so host contention looks like the trigger of a timing window in the check or the ring. Reproducing under deliberate host load on the customer's PC was refused by the session's permission check and is the customer's call; 20 KVM and 20 TCG one-processor boots of the diagnostic build on nazuna under the fleet's own load were started and stopped at the wind-down with none finished. Next: those boots, or the customer's load, and read which exit path the diagnostic prints in a failing boot | unowned |
| `net_ring::run()` unclaims its device twice when a ring is refused or never begins: `unclaim(&start.device)` inside `if outcome.is_none()` and again after it (`kernel/src/net_ring/mod.rs`). `unclaim` removes the node from `CLAIMED` by pointer, so the second is harmless alone; but a ring created for the same device between the two, by another task on another processor, has its claim removed by the second call, and a third ring could then be created beside it for one device. Found reading FX-1151's path on 2026-09-17; not shown to cause it. Fix: one unclaim on every path, with a check that creates a ring in that window. 1 point | os-02 |
| `test-boot --net` on Windows answers "the x86_64 kernel reported success but QEMU exited 1" after a clean boot: the xtask gateway's teardown, seen on 2026-09-16 with QEMU 11.1.0 under both WHPX and TCG. 1 point | os-26, later |

### After `rustc` — the compositor's path, unowned until stage 16 is near

| Item | Stage |
|---|---|
| Display core + ring-3 virtio-gpu driver, `/dev/dri/card0` with dumb buffers, atomic page flip and vblank; `xtask` reading QEMU's screendump | 17 |
| Input core + ring-3 virtio-input driver as evdev; QEMU monitor input injection in `xtask` | 17 |
| Iteration 1, the customer's order of 2026-09-16: a blank screen on Ferrix in QEMU. A first cut of stage 17: virtio-gpu 2D in `libs/virtio` (8), a ring-3 virtio-gpu driver started by devmgr (8), a minimal `/dev/dri/card0` with one dumb buffer, legacy `SETCRTC` and `PAGE_FLIP` (13), the compositor binary filling it (2), and `xtask` reading QEMU's screendump (3); 34 points, design in `docs/DISPLAY.md`, approved by os-f6 2026-09-16; Landed: L1, `libs/linux-abi::drm` (3); L2, `libs/virtio::gpu` (5); L3, `libs/displayctl` (3); L4, `libs/virtio-gpu` (5). L7's tooling is in (`compositor/blank`, `xtask test-display` and `--display`). L5 and L6, the display core, `user/gpu` and `/dev/dri/card0`, landed as one stack (16), and with them L7's pixel check passes on x86-64 and AArch64 (5): iteration 1 is done, 37 of 37. AAVMF took the virtio-gpu as the boot framebuffer on AArch64 (`docs/DISPLAY.md` §3): the loader now prefers a framebuffer the allocator will not own and writes `BootInfo.framebuffer.reclaimable`, which the panic screen and the display core both read (2, os-e5, reviewed by os-02). Its negative control, not committed, on x86_64 at 9a1788ef: with a marker in `panic::screen::install` alone the marker printed once; with `finish_boot_info` forcing the flag to 7 as well, the kernel printed `NEGATIVE CONTROL: the loader forced framebuffer.reclaimable to 7` and `display 1280x800, stride 1280, in memory the allocator owns: panics go to serial only`, and the install marker never printed. Kernel reader for L5 and L6: os-02. E4, `card0`'s primary plane and its `type` property, is on the branch `e4-planes` for os-02's review, and `test-display` now requires the marker line to end in `plane <id> Primary`. Its negative control, not committed, at 9b01812a: with `obj_get_properties` giving the plane `DRM_PLANE_TYPE_OVERLAY` instead of `PRIMARY` (one line), the program printed `compositor: scanout 1024x768 1024x768 colour 0x1e1e2e plane 4 Overlay` and `test-display` failed with `the program found no primary plane on the card` on x86_64 and on aarch64 (nazuna `~/.local/share/ferrix/logs/gui/e4-9b01812a-plane-control.log`). GUI session (os-e5) | 17 |
| Iteration 2, the Wayland server's kernel calls, verified from source by the GUI session: E1 `epoll_create1`/`epoll_ctl`/`epoll_wait`/`epoll_pwait`, a set waitable inside another (8, landed 2026-09-16); E2 `eventfd2` (2, landed 2026-09-16); E3 `ioctl(FIONBIO)` on sockets and pipes (1, landed 2026-09-16), and `FIOCLEX`/`FIONCLEX` on every file (1); the kernel side is done. `timerfd` landed 2026-09-24 (E5, next row), and `signalfd` the same day, for Chrome (`docs/CHROME.md` §3). Kernel session os-26 | 17 |
| The input iteration, and iteration 2's prerequisites, from `docs/INPUT.md`, approved by os-f6 2026-09-16. Input, 23 points: L1 `libs/linux-abi::input` from a committed probe (2), L2 `libs/virtio::input` (2), L3 `libs/inputctl`, the messages and the per-open queues (3), L4 `libs/virtio-input`, the driver logic (3), L5 `user/input`, devmgr's entry and the input core's task (5), L6 `/dev/input/eventN` and its evdev subset (5), L7 `compositor/evecho` and `xtask test-input` over QMP with its negative control (3); GUI session (os-e5), kernel reader for L5 and L6 open. Iteration 2's prerequisites, 14 points: E1 `epoll` with nesting (8, landed 2026-09-16), E2 `eventfd2` (2, landed 2026-09-16) and E3 `ioctl(FIONBIO)` (1, landed 2026-09-16), os-26's row above; E4 `card0`'s primary plane and properties (3, `docs/DISPLAY.md` §2.3), GUI session (os-e5), landed after os-02's review. E5 `timerfd` (3), wanted and not required, is done (2026-09-24). Landed: L1 (2) and L2 (2), whose fuzz target ran ten minutes without a failure and whose evdev numbers are now L1's. L3 (3), `libs/inputctl`, host-tested and fuzzed; E1, E2 and E3 (11, os-26), and E4, so iteration 2's prerequisites are all in | 17 |
| Input L4, `libs/virtio-input`, the driver logic (3): **landed 2026-09-17**. `Driver` over `Transport`/`DevicePages`/`EventArea` as `libs/virtio-gpu` has it (bring-up without `DRIVER_OK` until READY, HELLO from every event type the device declares, 64 event buffers, batching at the last `SYN_REPORT` within 64 events, a report reaching one event short of the core's limit cut by the driver, `Teardown::Wedged` when the reset does not finish), a fake device copying QEMU 9.2.4's four tables, 28 tests including `src/tests/batch.rs`, and the `virtio_input_driver` fuzz target with five seeds, which ran 10,523,605 inputs in ten minutes without a failure. The two departures the WIP left for review are `docs/INPUT.md` §6 decisions 9 and 10. Three expectations in the WIP's own tests were written without running and were wrong, not the code: the batch fills to one short of its capacity, not two; the gradient's cell step is 8, not 16; and a `Damage` past its rectangle limit collapses and then fills up again. For L5: how the core learns a driver broke is still undefined, and the glue must call `on_interrupt` again while `Drained.more` is set. GUI session (os-e5) | 17 |
| The compositor's tiny-skia renderer, `compositor/render` (5, stage 18's list): **landed 2026-09-17**. A Smithay-free `Canvas` over a tiny-skia pixmap (`tiny-skia =0.12.0`, default features off, `std` and `simd`: no C, no build script) with `clear`, `fill`, `border` and `composite`, `present`, `render` of one monitor and `damage_between`; the two pattern clients; the run-length expected-image format; 20 tests including the byte-exact golden image of two tiled clients at 1024x768 and its one-pixel negative control. The image was looked at through the PPM dump before it was blessed and its translucent half checked against the arithmetic by hand (green 176 over background 17 at alpha 0xC0 is 137). Green on the host and for `x86_64-unknown-linux-musl`. GUI session (os-e5) | 18 |
| LEDs: `write` to `/dev/input/eventN` of `EV_LED` events, carried to the driver by an additive `inputctl` message (`STATUS`, core → driver). **Done for USB keyboards, 2026-09-23** (`docs/INPUT.md` §3.3, §7.4): the core keeps the state and a USB keyboard's output report lights it. Left: virtio-input's status queue (it takes STATUS and drops it, so QEMU's keyboard stays dark); `write` of types other than `EV_LED` and `EV_SYN`, which Linux injects and Ferrix answers `EINVAL`; the LED events passed to the node's readers. hyprix writes Caps Lock and Num Lock as the locks change | 17 |
| Multi-touch axes (`ABS_MT_*`), force feedback (`EV_FF`, `EVIOCSFF`) and sound (`EV_SND`) on `/dev/input/eventN`. The input core publishes a device without them and its boot line says what was left out; QEMU's keyboard and tablet declare none (`docs/INPUT.md` §3.2, §6, os-f6 2026-09-16) | 17 |
| Input hotplug: devices exist from boot in the input iteration. A device that arrives or leaves later, and how a compositor learns of it without udev (`inotify` on `/dev/input`, not in Ferrix today, or a rescan) (`docs/INPUT.md` §3.4, §6, os-f6 2026-09-16) | 17 |
| `card0` is opened by one process at a time, standing in for DRM master (`docs/DISPLAY.md` §5, a written deviation from Linux): Linux's many opens with one master, `SET_MASTER`/`DROP_MASTER` arbitrating between them, and the render node beside it come in stage 19 | 19 |
| The compositor workspace: the server written from scratch (decided 2026-09-17), CPU rendering, dwindle and master, `hyprland.conf`, `hyprctl` IPC, two Rust test clients. `hyprland.conf` landed 2026-09-16 (`compositor/config`, 5); the layouts and dispatchers landed (8); `compositor/render` landed 2026-09-17 (5). `compositor/wire`, the Wayland wire protocol with no libwayland, and `compositor/protocol`, the interface tables generated from the XML, landed 2026-09-17 (8). `compositor/server`'s connection, `wl_display` and `wl_registry` landed 2026-09-17, with a real libwayland client reading its globals over a replayed transcript (5). `wl_shm` over sealed memfds and `wl_compositor`/`wl_surface` landed 2026-09-17, with a real libwayland client's window-setup requests replayed into the server (8). `xdg_shell` and the `AF_UNIX` socket landed 2026-09-17, and with them a real libwayland client's whole startup handshake runs against the server over a real socket (11). `compositor/hyprix`, the compositor itself, and `compositor/pattern`, the test client, landed 2026-09-17 with the headless half of stage 18's exit passing: two clients tiled, 0 differing pixels of 786,432 against `compositor/render`'s expected image, and a negative control (13). `compositor/ipc` and the `hyprctl` socket landed 2026-09-17, driven by Hyprland's own `hyprctl` binary: `clients`, `monitors`, `workspaces`, `activewindow`, `dispatch` and `runtime keyword` all answer (5). The DRM backend and `xtask test-compositor` landed 2026-09-17: `compositor/drm` is the card, lifted out of `compositor/blank`, and the compositor boots as init on Ferrix and shows its background on every one of QEMU's 786,432 pixels on x86-64 and AArch64 (10). Two clients on the card landed the same day: the initramfs carries `compositor/pattern` at `/bin/pattern`, the compositor's `exec-once` starts two, and `cargo xtask test-compositor` requires QEMU's screendump to match the renderer's expected image pixel for pixel on x86-64 and AArch64 (5). Next: `wl_seat` with keyboard and pointer, which waits on stage 17's input L5-L7 (8); the event socket `.socket2.sock` a bar subscribes to (3). GUI session | 18 |
| ~~The GPU, Path A (`docs/GPU.md` §3, decided 2026-09-18): A1 virtio-gpu 3D in the ring-3 driver; A2 `/dev/dri/renderD128`, GEM handles, the `virtgpu` ioctls from a committed probe, and scanout of a 3D resource; A5 the host half in xtask; A3b `compositor/virgl` behind a renderer trait with the software renderer as the fallback. 52 points~~ **Done 2026-09-19** (`docs/GPU.md` §3.7 and §3.8): the desktop composites on the GPU and the screen is shown the texture it drew into, 39 ms to 12 for a 1080p video-and-blur frame. `cargo xtask test-compositor --gl` judges it from inside the guest | 19 |
| The GPU, after Path A: A4 `zwp_linux_dmabuf` and a GBM-shaped allocator, and Mesa's virgl on ferrousli (`docs/GPU.md` §3, 3a), for clients that render on the GPU themselves. 8 points and 40 or more. Unowned | 19 |
| ~~vkgears through Venus (`docs/GPU.md` §6.1, decided 2026-09-24): V1 blob resources and Venus in `libs/virtio::gpu` (5), V2 `user/gpu` with the `hostmem` BAR and fences per ring (8), V3 the render node's blob, context and fence-descriptor ioctls (8), V4 libdrm, Mesa's Venus driver static, a static loader, libdecor and vkgears on ferrousli (13), V5 `cargo xtask test-vkgears` on the Linux host (5). 39 points. os-ac~~ **Done 2026-09-24** (`docs/GPU.md` §6.1): vkgears draws on the host's RADV through Venus, 212 frames in 5 seconds, judged by `cargo xtask test-vkgears`; `test-display --venus` maps a Venus blob. Left: zero-copy presentation through `zwp_linux_dmabuf` (§3 step 4), and the Khronos loader, which waits on `dlopen` of a library with thread-local storage | 19 |
| Gears on the DK1's GC400 (`docs/GPU.md` §6.2, decided 2026-09-24): G1 its clocks, reset and device node (3), G2 `user/gc400` identifying the core and running a command buffer (8), G3 a clear resolved to the LTDC's buffer (8), G4 draws with host-compiled shaders (8), G5 gears on the board (5). 32 points, judged on the board. **G1 and G2 done 2026-09-24 on the board** (`docs/GPU.md` §6.3): `kernel/src/stm32mp1_gpu.rs` checks PLL2's Q output, turns on `GPUEN` and pulses `GPURST`, and publishes binding `TREE_STM32_GPU`; `libs/gc400` (28 host tests) holds the registers, the command encoders, etnaviv's identify, reset and init sequence and a `WAIT`/`LINK` ring; `user/gc400` prints the identity, resets the core, runs two event blocks through the ring on one coherent page and takes each by interrupt. `xtask check` and the three QEMU boots pass unchanged. On the board the core identified as etnaviv's database entry, reset on the first attempt, and ran both blocks, their events by interrupt 133 and 105 us after the splice. os-ac | 19, P3 |
| The DK1's LTDC display and USB HID as the hardware variant of stage 17. **The display is done, 2026-09-23** (`docs/DISPLAY.md` §6): the kernel clocks and muxes the LTDC and I2C1 and publishes a device-tree node, `user/ltdc` drives the LTDC and the SiI9022 bridge, buffers are contiguous and cache-cleaned for it, and `hyprix` ran on the board at 1280x720 over HDMI. **USB HID is done, 2026-09-23** (`docs/INPUT.md` §7): the kernel clocks and powers the USB host and its PHY, `vmo_pin`'s `PIN_COHERENT` gives the driver memory the non-snooping controller sees as the CPU does, and `user/usbhid` found the board's USB2514B hub, a G502 mouse through its transaction translator at full speed and a keyboard at low speed, whose keys, buttons, motion and wheel reached `/dev/input/event0` and `event1`. Left: other modes than 720p60, display hotplug. Report-protocol HID -- the mouse's sixteen buttons and wheel tilt, every keyboard's media and system keys -- and keyboard LEDs followed the same day (`docs/INPUT.md` §7.4); absolute axes and vendor reports are not read (§7.5) | 17, P3 |
| The DK1's USB host after U-Boot's `ums`: ending mass-storage mode switches the PMIC's `vdd_usb` (STPMIC1 LDO4, on I2C4) off, and the kernel never turns it on, so the USB PHY is unpowered and nothing enumerates; U-Boot's `regulator dev vdd_usb; regulator enable` before `bootefi` is the workaround (`docs/stm32mp157-dk.md`, 2026-09-23). The kernel's USB preparation should turn LDO4 on itself -- a write to the PMIC every rail of the board hangs off, so with the care the RCC gets | unowned |
| **Fixed 2026-09-24 by "Send the console's output by interrupt, not by polling with interrupts masked":** a task's write is queued and sent by the port's transmit interrupt, a writer finding the ring full sleeps. A process writing the serial console flat out starves others: on the DK1 at two processors, two `hexdump`s of `/dev/input/event*` to the 115200-baud console kept `usbhid` from running for seconds, and the clicks made meanwhile were lost (2026-09-23, `docs/INPUT.md` §7.5). Find where a console write waits on the UART, and whether it holds a processor while it does | unowned |

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
* Stage 21, bare metal with an NVIDIA card driven by Ferrix itself: Path B
  of the GPU decision of 2026-09-18, `docs/GPU.md` §4. Opened when the
  customer wants Ferrix on real hardware; unsized, over 100 points.
* Stage 22, Steam (decided 2026-09-18): the 32-bit x86 ABI (unsized),
  glibc's place taken by ferrousli under the Steam runtime (13 priced, the
  rest unsized), bubblewrap's needs on top of stage 13 (13), a root on btrfs
  (stage 12), XWayland (40 as a first guess), sound -- `virtio-snd`, an
  audio core, a PulseAudio or PipeWire server (30; the customer moved audio
  forward to current work on 2026-09-26, and `docs/AUDIO.md` is its design)
  -- and Vulkan through Venus on a KVM host (8 over Path A, plus the driver question). Over 300
  points; the roadmap's stage 22 is the list.
* Huge pages; frame share and release are order 0 by design.
* A panic report as a QR code: a port of Linux's `drm_panic_qr` as
  `libs/qr` (ferrix-qr), so a panic screen can carry the whole report. WIP
  on branch `worktree-agent-a33c10946b6721065` (5690f0e), unbuilt into the
  panic path.

---

## Fourteen programs stand between the userland and deleting busybox

The uutils family, zinc and the ports own `/bin` now. Fourteen names are
still busybox's and still gated: `sysctl`, `fdisk`, `top`, `mpstat`,
`iostat`, `pwdx` and `su` in `test-vfs`, and `ip`, `route`, `netstat`,
`nslookup`, `ping`, `ping6` and `udhcpc` in `test-net`.

None of it is blocked on the kernel. Netlink, raw sockets, `/proc`,
set-user-id and ferrousli's resolver are all there and gated; what is missing
is user-space programs over them. `docs/UUTILS.md` §8.1 breaks it down: 28
points, the largest single piece being `ip` over netlink at 8.

And read §8.3 before starting. Deleting busybox also takes `grep`, `sed`,
`awk`, `tar`, `mount` and every editor with it, none of which anything
replaces. Keeping busybox in the image as one program among others costs
1.2 MiB beside uutils' 14 MiB. Whether S8 is wanted at all is the customer's
call, and nothing after S6 depends on it.

## procps and util-linux are the two of the family that do not build

D2 named four uutils projects. coreutils, findutils and diffutils are in the
image; procps and util-linux are not, and it is not a pinning choice. Their
only release, 0.0.1, does not compile on this toolchain -- dependency rot --
and their main branches do not compile for this target either: procps' `top`
wants libsystemd through pkg-config and refuses to cross-compile, and
util-linux's `blockdev` and `fsfreeze` pass `ioctl` the request type glibc
declares, which is not musl's.

They hold what `test-vfs` would most like to move off busybox: `sysctl`, `ps`
and `top`. Getting them would mean pinning main commits and excluding by
feature exactly the utilities that make them worth having. 5 points, and
better spent when either project releases again. `docs/UUTILS.md` §6a has the
detail.

## A shootdown gives the processors it waits on one second, and a busy host takes more

FX-0001, "processor N never flushed its TLB for a shootdown", stopped
`test-selfhost --plan` twice in a row on 2026-09-24 (os-8d). Ferrix had eight
virtual processors in a parallel build: once in `munmap`, once in `execve`'s
`vmap::free`. Both times nazuna was at a load of 42 to 52 on 24 cores. Last
night the same eight-processor run went 56 minutes without it. `smp.rs`
already gives a waiting holder four seconds (`TURN_TIMEOUT_NANOS`), "because
a holder preempted on a host with more virtual processors than real ones
can lose whole seconds without being stuck". The processors a shootdown
waits on get only `SHOOTDOWN_TIMEOUT_NANOS`, one second, and a preempted
virtual processor loses that time just the same. The fix to try first is the
same room for both, or Linux's unbounded wait with a warning; then run
stage 20's plan again. **Advanced 2026-09-26:** both waits now also need a
count of the waiter's own polls (`smp::patience`), which a slow emulator
stretches and a descheduled waiter does not spend: 1.8 s under KVM, 5.1 s
under `tcg`, 32 s under the coverage plugin for a processor that really is
stuck. A host that stops running the processor waited for while it runs the
waiter still ends the wait, only later; KVM's steal time is what would see
that, and is the next thing to try if the plan stops on FX-0001 again. Logs:
`~/ferrix-logs/fx0001/2026-09-24-selfhost-plan-smp8-454eaab{,-2}.log` on
nazuna.

The same night's matrix recorded one self-check failure no row had shown
before: stage 7's "a signal sent to a process of two threads was given to no
thread", in `test-input` on x86-64 under a load of 37. Log:
`~/ferrix-logs/signal-no-thread/2026-09-24-input-x86_64-454eaab.log`.
2 points | os-8d | 20

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

## Velocity, measured on 2026-09-18

Points are what a session says before it starts (the rule of 2026-09-13,
`docs/ROADMAP.md`'s opening) and velocity is what landed, counted
afterwards. This is the first count over the whole points era, from the
evening of 2026-09-13, when the first estimates were written, to the morning
of 2026-09-18. Sources: the product-owner ledger kept at the time (which
measured 2026-09-14 at the time), the landings recorded in this file, and
the roadmap's stage totals -- stage totals for stages 17, 18 and 19 rather
than their rows, so that nothing is counted twice.

| day | points landed | what |
|---|---|---|
| 2026-09-13 | 0 | the first estimates written that evening; the day's landings were sized before the rule and are not counted |
| 2026-09-14 | 131 | measured by the product owner at 23:20: 19 landings across ten sessions -- stages 7, 8, 9, 10, 11, the mm stack, the board -- ≈ 21 points a queue-hour |
| 2026-09-15 | 34 | 00:20–03:00: memfd 5, threads 5 (7), the POSIX gap document 3, the native Windows busybox 5, the branch cleanup 3, netwire 8, inet ABI 3; then the fleet stopped until the evening of the 16th |
| 2026-09-16 | 66 | from 19:40: threads 6 (3), the zinc gate 2, the compositor's parser 5, display L1 3, ferrousli's threads 8, a busybox fix 1, and the rest of networking (44 of its 50 estimated), whose exit was met at 22:50 |
| 2026-09-17 | 214 | stage 17 (74) and stage 18 (96) less the 8 above, both exits met; stage 19's first ≈ 44 (rules, dispatchers, layouts, groups, monitors, plugins, blur and shadows); static PIE 3, the threads exit 2, ferrousli-misc 3 |
| 2026-09-18 | unpointed | the compositor's frame time, pacing, the kernel's fault fix, the cores, 1080p and wallpapers, the GPU and Steam decisions: none was sized before it started, so none counts |

**About 445 points in four calendar days, 2026-09-14 to -17: ≈ 111 a
calendar day, and ≈ 150 a day the fleet was actually running** (the 15th was
three hours). The finer number the ledger measured on the 14th, 21 points a
queue-hour, held on the 17th too by this count. Ten sessions ran on the 14th
and about eight on the 17th, so a session-day is 15–20 points, and every
estimate under 8 held both days.

What the number is good for and what it is not:

* It sizes the pointed remainder: stage 19's ≈ 100 and dynamic linking's 39
  are a fleet-day each at the 17th's pace *if the work is of the kind that
  was measured* -- protocol tables, syscalls, a renderer -- which the GPU
  path (unknowns in every step) and XWayland (a server) are not. Stages 21
  and 22 are unsized and the number says nothing about them.
* Three calibrations are mixed in it: session estimates against each other's
  yardsticks, the product owner's "unmeasured" sizing of stages 17–19 on
  the 13th, and the rows that carry their own points. The stage totals for
  17 and 18 came in where they were sized (74 and 96), which is the one
  check the mix has passed.
* The 18th's landings are counted at zero, not because they were small
  (sized afterwards they would be about 35: the backdrop 8, pacing and the
  card's damage 5, the fault fix 3, the cores and the shadow 8, 1080p and
  wallpapers 8, the two decisions 3) but because a size given after the fact
  is not an estimate. The next count should not have such a row.

### Update, 2026-09-24

**The code base** (`git ls-files` on `main` at 046ea79a): 702,675 lines of
Rust in 2,080 tracked files of all kinds, plus 47,769 lines of C, headers and
assembly (ferrousli's and the ports' glue). By tree: libs 188,154, compositor
169,567, kernel 122,989, ferrousli 114,688, zinc 52,011, xtask 34,449, user
9,152, fuzz 7,970, boot 3,474. The docs are 26,714 lines of Markdown, 25,061
of them under `docs/`. Nothing is excluded or generated-filtered, so read the
Rust total as an upper bound.

**Today.** The 66 commits that landed on 2026-09-24 add 54,477 lines and
remove 4,309, of which 45,560 added and 2,054 removed are Rust: a net
+50,168 lines in the day, ≈ 7 % of everything above.

| landing | points | of |
|---|---|---|
| sysfs (`b65b5e41`) | 26 | 26 |
| the init push: G3, G4, L1, L2, L3, G5 | ≈ 23 | 22 |
| vkgears through Venus (V1–V5) | 39 | 39 |
| the GC400's first two steps (G1, G2) | 11 | 11 |

That is **≈ 99 points landed on 2026-09-24**, against the ≈ 54 a day the
backfill below gives for 09-18 to 09-23. The landings ran from about 18:15
to 22:45, so about 4.5 hours: **≈ 22 points an hour**, level with the 21 an
hour measured on the 14th and the 17th. It is a few sessions, not a
fleet: four agents ran in parallel on the init push and two other sessions
landed the rest.

Not counted, because nothing was sized before it started (the rule above):
Chrome's headless run, foot on the compositor, timerfd and signalfd, the
control queue and the debug FPS overlay, ferrousli's glibc names for Chrome,
stage 20's build recording, splice and copy_file_range. Sized afterwards they
would be perhaps 60–80 more, which is why 99 is a floor. The day's cost was
not in the code: the sysfs landing was gated three times and the gears
landing four times because `main` moved under every gate, and FX-1004 flaked
in two of the rows.

**The days between, 09-18 to 09-23**, are not in the table above because no
session reported them. They were sized afterwards from `git log`, in clusters
against the same scale, and are lower in confidence: ≈ 50, 50, 55, 96 and 20
for 09-18 to 09-22, about 324 in six days (btrfs write's 60 included), ≈ 54 a
calendar day. With the first count's 445 and today's 99 the running
total is ≈ 870 points in 11 calendar days, ≈ 79 a day. The drop from the
first week's 111 a day reads as a change in the kind of work (the zsh
compatibility tail, and GPU, dynamic linking and btrfs write, whose steps
each have unknowns) and not as a stall.

---

## Decisions

Dated, newest first. A decision here is final until the customer says otherwise.

* **2026-09-24 (customer)** **Gears, as the 3D demo: real vkgears through
  Venus, and GLES2 gears on the DK1's own GPU.** vkgears runs in QEMU on the
  Linux host, where Venus carries Vulkan to the host's GPU. Mesa's Venus
  driver is built as a static archive against ferrousli and presents over
  `wl_shm` until dmabuf lands. The DK1's Vivante GC400T is an OpenGL ES 2.0
  core with no Vulkan in any driver, so the board gets gears drawn by that
  core through a ring-3 driver of Ferrix's own, rather than vkgears on a CPU
  Vulkan that would test no GPU. `docs/GPU.md` §6 has the reasons and the
  steps: 39 points for Venus, 32 for the GC400.

* **2026-09-23 (customer)** **Stage 13's cgroups come before init.** The
  real init that is the rest of stage 15 is planned as if cgroup v2 exists,
  and stage 13's cgroup half is built first to make that true. Its namespaces
  and seccomp follow and are not init's prerequisites. `docs/INIT.md` is the
  design, §0.1 what it needs from stage 13. **And stage 13 builds every
  cgroup over a `Job`** (C8): one container object, seen as a cgroup by the
  Linux ABI and as a job by the native one, so that a microkernel can keep
  the jobs if it drops cgroupfs. The rest of `docs/INIT.md` §14 is still
  open.
* **2026-09-18 (customer)** **Steam is on the roadmap, as stage 22**, the
  step after the GPU decision below: the client starts and logs in, a
  native game installs to btrfs and plays with sound through the GPU path,
  and a Windows game runs through Proton. It is a guest's stage first and
  does not wait for bare metal. What it stands on that the roadmap did not
  stage until now -- the 32-bit x86 ABI, XWayland, sound, Vulkan through
  Venus -- is written into the stage as a list of what has to be true, with
  first guesses that sum past 300 points; the rest is stages 12, 13 and
  dynamic linking, which were already there.
* **2026-09-18 (customer)** The GPU: **Path A first, Path B in a later
  stage.** Path A is GPU acceleration for Ferrix as a guest through
  virtio-gpu's 3D commands, which uses the host's NVIDIA driver without
  porting it: the ring-3 driver's 3D commands, a render node with the
  `virtgpu` ioctls and 3D scanout, the host half in xtask, and a Rust virgl
  encoder as the compositor's renderer behind a renderer trait -- 52 points,
  in that order, `docs/GPU.md` §3. Path B is a driver for an NVIDIA card
  under Ferrix itself, for the day Ferrix runs on bare metal: stage 21,
  unsized, `docs/GPU.md` §4. This settles the third of the three choices
  left open on 2026-09-13 below: neither Mesa on ferrousli nor Vulkan for
  the compositor, but a Rust encoder of virgl's command stream, with Mesa
  kept for the day clients render on the GPU themselves. The Windows QEMU
  already offers `virtio-gpu-gl`; the gate host's has to be rebuilt for it.
* **2026-09-16 (customer)** A release with every current feature finished:
  the freeze went to os-02, os-05, os-a8, os-26, os-e5 and os-12 at about
  22:35; each landed what was gate-green and stopped, the product owner
  verified the head (a89aeb25 with these notes) whole on nazuna and tagged it `stage-11.1-network-display-and-threads`
  (notes in `docs/RELEASES.md`). After the tag the fleet continues, and
  every session is to have work — the owners table says what.
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
* **2026-09-17 (customer, delegated to the GUI session) The compositor
  server is written from scratch, not on Smithay.** The customer asked for
  the work to go on without stopping for questions, which settles the open
  choice of 2026-09-13 above. Smithay's value is its backends -- udev,
  libinput, libseat, GBM, EGL and its DRM session handling -- and every one
  of those is C, which `compositor/README.md`'s no-C-device-stack rule
  already forbids and which this tree has already replaced: `blank` drives
  `/dev/dri/card0` itself, `libs/virtio-input` is the input driver, and
  `compositor/render` is the renderer Smithay's own pixman would have been.
  What would be left of Smithay is `wayland-server`'s marshalling and its
  protocol handlers, and taking those means every Ferrix system call they
  make is a dependency's choice rather than this tree's -- on a kernel whose
  Linux surface is still being filled in, that turns a compositor bug into a
  hunt through someone else's crate. Writing the wire protocol is about 20
  points more before the first client, and buys a `no_std`-shaped,
  host-tested, fuzzed crate in the same shape as `libs/netwire`, `libs/cpio`
  and `libs/inputctl`. The protocol XML this is written from is on this
  machine (`/usr/share/wayland/wayland.xml`,
  `/usr/share/wayland-protocols/`), and so is Hyprland 0.56.2's own source,
  the behaviour reference. `xkbcommon` stays the one C library allowed at
  stage 18 and is unaffected; the stage 19 GPU choice is still open.
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

* The init design's open decisions, `docs/INIT.md` §14, 2 to 8: the unit
  syntax, hyprix leaving pid 1, `devmgr` under init, what init's death does,
  the names. Each has a draft answer the design assumes meanwhile.
* The DK1's link is the ST-LINK on the Windows machine (COM8) since the
  evening of 2026-09-13; the stage-9.1 tag, the paste test and
  `board-reset-3c`'s proof B all passed on it. With `ferrix.onexit=reset`
  the board returns to U-Boot by itself, so a run no longer costs a USB-C
  replug; only a hang still does.
* The root filesystem: 1.7 TB of the 1.9 TB is outside Ferrix. The sessions
  can only keep their own build output down.
* Pushes to `origin`: local `main` is 152 commits ahead at the `stage-11.1-network-display-and-threads` tag;
  pushing it is the customer's word, given in a session of their own.

---

## The compositor of 2026-09-17: stage 18's exit passes on the card

Twelve landings on one branch, each gated whole. The compositor runs: `cargo
xtask test-compositor` boots `hyprix` as init on Ferrix and two Wayland
clients tile on `/dev/dri/card0`, every one of 786,432 pixels the image
`compositor/render`'s own tests bless, on x86-64 and AArch64.

**What landed, in order.** Input L4 and the renderer, both from the
wind-down's side refs. Then `compositor/wire`, the Wayland wire protocol with
no libwayland; `compositor/protocol`, the interface tables generated from the
XML; `compositor/server`, the protocol itself, in four landings --
connection, surfaces and shared memory, `xdg_shell` and the socket, and the
interfaces a real toolkit asks for; `compositor/hyprix` and
`compositor/pattern`, the compositor and its test client;
`compositor/ipc`, `hyprctl`'s shape; `compositor/drm`, the card lifted out of
`compositor/blank`; and the two-client picture on the screen.

**How each was checked.** Every layer is pinned to an implementation this
tree did not write, because a test written against its own crates only shows
the two halves agree:

* `compositor/wire/probe/wire.c` drives a real libwayland client and server
  and records their bytes; this crate writes the same ones.
* `compositor/protocol/probe/interfaces.c` links libwayland's own compiled
  `wl_*_interface` tables; the generated ones agree, 31 interfaces and 194
  messages.
* `compositor/server/probe/live.c` is a libwayland client that completes the
  whole startup handshake against `examples/serve.rs` over a real socket.
* `compositor/hyprix/probe/real-client.sh` runs **foot**, a real Wayland
  terminal. It found four gaps nothing else could:
  `wl_data_device_manager` not offered, `wl_subcompositor` advertised
  without `get_subsurface`, `wl_output` describing nothing, and `wl_seat`
  announcing what it had no path for. **Run it after any protocol change.**
* `compositor/hyprix/probe/hyprctl.sh` drives the compositor with
  Hyprland's own `hyprctl`: `clients`, `monitors`, `workspaces`,
  `activewindow`, `dispatch movefocus l` and `keyword general:gaps_in 40`
  all work, the last re-tiling both windows while it runs.

**The Smithay decision is settled**, delegated by the customer's order to
work without stopping for questions: the server is written from scratch. The
reasoning is in the decisions above.

**What the compositor still owes, in the order it needs them:**

* **Input.** `wl_seat` announces no capabilities, because there is none to
  announce: the input iteration's L5 (`user/input` and the kernel's input
  core), L6 (`/dev/input/eventN`) and L7 are not started. Until they are, a
  keybind parses and nothing can fire it, and a keymap needs `xkbcommon` or
  a committed XKB file. This is the largest single thing between here and a
  compositor a person can use.
* **The event socket** `.socket2.sock`, which a bar subscribes to (3).
* **`zwlr_layer_shell_v1`**, which has tables and is not offered, so no bar
  can place itself (5).
* **Stage 19 entire** (144): animations with Hyprland's bezier curves,
  rounded corners, blur, shadows, opacity rules, special workspaces, groups,
  multi-monitor, the plugin-shaped extension points, and the GPU. This is
  what makes it Hyprland rather than a tiling compositor. *(2026-09-18:
  everything but the GPU and XWayland is landed and the exit met for it;
  about 100 of the 144 are left, and the roadmap's stage 19 says which.)*

## Wind-down of 2026-09-17, about 00:10: the fleet moves to another machine

The customer wound the fleet down after the release to move it to another
PC. `main` is b4665bfd, on nazuna and on `origin`; the tag
`stage-11.1-network-display-and-threads` is on 06336c60 (notes in
`docs/RELEASES.md`), verified whole on nazuna (15 gates, five boots at
stages 1-11, no panic). Everything below the tag landed after it, on the
same day, each on its own gate:

* **CI, red for two days, fixed forward in four landings:** the release
  kernel links again (one declaration of the system-call entry; `cargo
  xtask build --arch all --release` joined the gate table's kernel row);
  `cargo xtask host-test`/`host-clippy`/`host-doctest`/`host-doc` are the
  one source of the host commands and `ci.yml` calls them (17 broken doc
  links fixed on the way); the ferrousli job fetches musl's libc-test at
  the pinned commit; xtask passes `-global arm-smmuv3.stage=2` so QEMU
  before 9.2 faults the SMMU check, with a skip line otherwise. The run on
  f577541e is the first with all four; read its result before anything
  else on the next machine (`gh run list --branch main`).
* **The customer's panic under WHPX** (`run --init ferrousli --net --zinc
  --display` on Windows): `/sbin/blk` died at its first common-config read
  with a reserved-bit fault at address 0. Root cause is QEMU 11.1's own
  MMIO emulator under WHPX (`whpx_handle_mmio` walks the guest page tables
  itself; with more than one vCPU its walk returns "not mapped" for a
  mapping that exists, and `MMU_TRANSLATE_PAGE_NOT_MAPPED` is turned into
  error code 12; CR2 is never synced). Ferrix's tables and CR3 were
  verified correct at the fault. Workaround landed: WHPX without `--smp`
  runs one processor and says so. The upstream report is written from the
  row; filing it is the customer's. `driver_check` now names a driver that
  died instead of "not in the registry".
* **The compositor's kernel side is complete:** epoll with nesting,
  eventfd, `FIONBIO`/`FIOCLEX`/`FIONCLEX`, waits that wake on the event
  (no 5 ms recheck any more: `docs/RELEASES.md`'s known limit is out of
  date since c2129a68), card0's primary plane and properties with
  `UNIVERSAL_PLANES` accepted; ferrousli's epoll/eventfd wrappers; input
  L1-L3. Iteration 2 (two tiled pattern clients) waits on one decision.
* **ferrousli:** complex.h double and float, the musl NaN/flags fixes;
  `docs/POSIX-2024.md` at 1040 present, 203 missing, 74 points.
* **zinc-next:** the zsh runtime port, 35.7k lines, on `main` in five
  slices (19 of 40 points), `check --zinc` building and testing it with
  `--features next`. Sized for what is left (21): builtins B1 jobs and
  flow control 2, B2 parameters and options 3, B3 input and output 3, B4
  commands and functions 3; then the history ring and file 3, ZLE 3,
  completion and modules 3, the swap to zinc 1. B1 starts with the
  `BIN_FG` numbering fix.
* **Ports (os-12, session ended):** the firmware clock and ChaCha20
  `getrandom` (BootInfo v5) landed; the hermetic HTTPS `test-net` program
  (3) and git (12) landed on 2026-09-17, both gated by the whole matrix.
  The ports auto-build in xtask (5) is still on `ports-autobuild`.
* **Flakes met and closed:** FX-0882 (the eventfd check's own reader
  reaped inside its window; `sched::wait_until_gone`, the mm task-gone row
  is now os-26's for the other sites). **Open:** FX-1004 (quiesce at a
  driver's death, 1 in 11 x86_64 boots, os-02, 2); FX-1151 (the net ring's
  control channel closed, 2 of 9 one-processor WHPX boots, both while the
  customer's own guest ran on the same PC: a contention lead, 0 of 18
  clean boots with reason prints; reproducing it under load on the
  customer's PC is theirs to allow); `net_ring::run()` double unclaim
  (os-02, 1); the VT-d stale record (P0, unowned).

**Side refs on nazuna** (`refs/heads/<session>/<name>`, fetch with
`git fetch nazuna-wg:Documents/projects/os/ferrix 'refs/heads/*:refs/remotes/nazuna/*'`),
each with a row above saying where it stands:

* `os-02/unix-creds` a19de2d3: `AF_UNIX` 3c, `SCM_CREDENTIALS`/`SO_PASSCRED`
  (2), written but never built: the parse and its checks, the reply before
  `SCM_RIGHTS`, credentials per piece of a split send and on peek, `accept`
  inheriting the option; missing the build, a host test, the boot check
  with its control, the doc and the rows. The commit message lists the
  steps. `os-02/fx0701-diag` 24ec0d30 is diagnostics only, not for landing.
  `os-02/smmu`, `os-02/dead`, `os-02/unix-gc` are pre-rebase copies of
  landed commits: delete.
* os-05: nothing on a branch; the builtins slice B1 had no code yet.
* `os-12/ports-autobuild` 1dd0e503: the xtask ports auto-build (staleness
  per port, a build lock, the Linux path, Windows through WSL), state
  unreported; read the commits before trusting them. `ports-curl-btop` is
  landed and can be deleted.
* `os-a8/long-double` is landed and can be deleted: the x87 long double
  foundation is compiled, and with it the 31 `long double` functions the
  x87 computes directly (2026-09-17). What is left of that row: the 28
  with no single instruction behind them -- the transcendentals, `cbrtl`,
  `hypotl`, `fmal`, and the gamma and error functions -- each a port of
  musl's, 5 points, and the 20 `long double complex` forms after them, 2.
  The 200,000-argument comparison against the musl build on nazuna that
  the original plan named was not written: the landed slice is checked
  instead by its Rust unit tests and by `tests/c/math/longdouble.c`,
  which is what exercises the naked shims' calling convention. Write the
  comparison with the transcendentals, where rounding is the question.
  `os-a8/kept-os02-stub-462f7714` pins a superseded os-02 commit: delete.
* `os-26/fx1151-diag` 61bb2204: the reason-print build for FX-1151, not
  for `main`; its logs are on the Windows PC under
  `~/.local/share/ferrix/logs/os-26-whpx/`. The 15-site task-gone
  conversion (2) and the `AF_PACKET` gaps have no branch.
* `os-e5/input-l4` e42623ea (input L4, `libs/virtio-input` driver logic,
  17 tests written) and `os-e5/compositor-render` 5e2a9d12 (the tiny-skia
  renderer, 21 tests, golden image not yet blessed): both never compiled.
  **Both landed on 2026-09-17**, with the fixes their rows in the
  after-`rustc` table record; the branches can be deleted.

**The branches, surveyed against `main` and cleaned up on 2026-09-17.** Each
was compared file by file and item by item, not by its subject line, because a
branch whose base is months old diffs against `main` as though it held
everything that landed since. Eighteen were deleted, none of them holding
anything `main` lacks:

* Nine were already in `main` and went with `git branch -d`, which checks:
  `codex/posix-assert`, `codex/posix-endian`, `codex/posix-stdatomic`,
  `ferrousli-posix`, `linux-abi-inet`, `netwire`, `ports-curl-btop`,
  `zinc-next`, `zinc2`.
* Nine more were superseded -- `main` defines every item they define, and
  their patches differ from what landed only by their base: `ci-smmu-stage2`,
  `codex/posix-2024`, `compositor-render`, `dac5`, `dead-driver`,
  `display-core`, `input-l4`, `stage8-filemmap`, `unix-gc`. Two looked as
  though they held something of their own and did not: `codex/posix-2024`'s
  extra names are the internals and tests of an older netdb, whose exported
  functions are all in `main` and which would bring back
  `ferrousli/src/stubs.rs`, the placeholder `main` emptied one implementation
  at a time; and `stage8-filemmap`'s `copy_out` is at the merge base, so it is
  code `main` refactored away rather than code the branch adds.

*Kept, as the only copy of what they hold:* `fx0701` (the FX-0701 diagnostics
the row above asks the next session to rebuild from prose -- they exist here,
and nothing of them is on `main`); `stage9/log-header` (`xtask/src/tree.rs`,
for the open row that every gate log names its tree); `unix-creds`;
`ports-autobuild`. `stage8-diag` is a diagnostic its own message says is not
for landing, and `pre-pull-backup-2026-09-13` has no merge base with `main` at
all.

*Kept because a worktree under them holds uncommitted work*, whatever their
merge status: `codex/posix-stdlib`, `display-design`, `ferrousli-netdb` and
`ferrousli-netcore`. Check `git -C <worktree> status` before removing any of
them; `ferrousli-netcore` is superseded but its worktree is not empty.

*Left alone as live work:* `compositor-backdrop`, `gui/window-head`,
`gui/xkb-ipc`, `gui/xkb-merge`, `gui/xkb-probe` and `uutils-shell`, which
gained commits while this survey was being written.

**Waiting on the customer:** ~~Smithay's core crates or from scratch for the
compositor server~~ — settled on 2026-09-17, from scratch, in the decisions
above; permission to load this
PC for FX-1151; the QEMU upstream report; the VT-d record's owner.

**For the next machine:** clone from nazuna or `origin` (both at `main`
b4665bfd), keep `nazuna-wg` as the gate host, one `CARGO_TARGET_DIR` per
worktree being built (two trees in one dir leave an xtask binary that
carries the other tree's path: that invalidated one release verify and one
gate today), `~/.local/share/ferrix/po-verify.sh <commit>` for a whole
matrix, and the points ledger is in the product owner's memory
(`ferrix-points-ledger`), where today's ~120 points after the tag are
timed.

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
