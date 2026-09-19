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
| Only `ferrousli/` | `cargo xtask check --ferrousli`, then `cargo xtask busybox` and, with the busybox it built, `test-shell` and `test-vfs` on x86_64 with `--init ferrousli`, so the binary the gates run never lags the library; then `cargo xtask uutils`, which links uutils/coreutils against it and is the larger consumer of the two, since it brings Rust's whole `std` with it; a change to `ferrousli/tools/ports/` also runs `cargo xtask ports` and `test-net --arch x86_64 --init ferrousli`, which fetches with the curl it built |
| Only `zinc/` | `cargo xtask check --fast --zinc`: zinc's formatting, clippy, unit tests and the pty completion test (`zinc/tests/pty_completion.py`); a change to what zinc does at boot also runs `test-boot` on x86_64 |
| `libs/` only, and no crate the kernel builds | `cargo xtask check`, and one boot: `test-boot --arch armv7a --smp 2` |
| Anything the image contains: `kernel/`, `boot/`, a kernel-side crate in `libs/`, `xtask` | `cargo xtask check`, then `cargo xtask build --arch all --release`, since CI builds and boots the release profile and no other gate does, then `test-boot` on x86_64, aarch64, armv7a at four processors and armv7a at `--smp 2`; a stage 7 or 8 change also runs `test-shell` on x86_64 with no `--init`, which runs zinc, the image's shell, and then with the ferrousli busybox (`--init ferrousli`), the musl busybox *and* the host's glibc busybox (`/usr/bin/busybox`), and `test-vfs` on x86_64 with the ferrousli busybox and the musl one; a stage 7 change also runs `test-threads --arch all`, the Rust `std::thread` program (since 5fd2ab09). The whole of that, plus KVM, is what moves `main` |
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
| os-02 | Stage 7's remainder: `AF_UNIX` 3b, the in-flight cycle pass (3), and 3c, credentials (2); FX-0701's lead; the kernel-side reviews of stage 17's landings (E4 card0 planes, input L5) |
| os-05 | zinc: `zinc-next`, the port of zsh's C runtime (36k lines; it compiles and passes `check --zinc`), landing in five slices that keep the oh-my-zsh gate green: all five slices are on main (core state, expansion, signals and jobs, execution, prompt with builtins and start-up) (19 of the 40 points); the other 21 are the remaining builtins, the history ring, ZLE, completion and modules, and the swap to `zinc`; `cargo xtask busybox` and its staleness rule |
| os-a8 (was os-b7, os-50, os-7c, os-fb, ferrix-ce) | `ferrousli/`: first the wrappers for epoll, eventfd and `FIONBIO` with a C test (3), then `docs/POSIX-2024.md`'s list from the top of what is left (996 present, 247 missing, 80 points on 2026-09-16), each landing rebuilding the busybox and passing both ferrousli gates |
| os-26 | The event loop's kernel rows are done (epoll, eventfd, `FIONBIO`, `FIOCLEX`/`FIONCLEX`); next the wake-on-event row (5) so `poll`/`epoll` waits stop rechecking every 5 ms, then the `AF_PACKET` gaps; Windows parity for `xtask` is held |
| os-12 | Ports onto ferrousli: curl with HTTPS through mbedTLS and btop with libc++ landed for the release; the firmware clock, ChaCha20 `getrandom` (BootInfo v5), the hermetic HTTPS `test-net` program and git all landed and gated on 2026-09-17. What is left of `ports-autobuild`: building the ports when stale rather than every time (5) |
| open | Stage 8's list (14), `AF_UNIX` 3b the in-flight cycle pass (3) and 3c credentials (2), stage 10's BAR trust (8, first piece landed), `timerfd` (3), dynamic linking (39), stage 12 |

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
| FX-0701, seen once on 2026-09-16: `test-vfs --init ferrousli` on x86-64 panicked in stage 7's check "a thread of a stopped process kept running instead of stopping" (threads commit 5's stop check), at uptime 13.04 s. The same check passed in `test-shell` in that run and on the re-gate, and the log was overwritten. Not reproduced: 23 valid runs of that row on 2e877aa under an aarch64 boot loop for load, all passing, the last 7 with a diagnostic build that prints, when the check gives up, whether the process took the stop, each task's state, queue, switch and preemption counts and processors, and whether the counting threads still count (not landed; rebuild it from this description). Lead, from reading only: the check waits 10 s for every task to be blocked, so a 13.04 s uptime fits a stop that never reached one spinning thread rather than a slow host. The stop reaches a spinning thread by `sched::interrupt`'s broadcast IPI and the trap exit's `needs_attention`; no gap was found in that path by reading, so the next step is the loop again with the diagnostics, longer, and on armv7a --smp 2. **Seen twice more on 2026-09-18, and both logs are kept:** the fourth and then the third boot of two `test-compositor` runs on nazuna under KVM, in the kernel's own boot check before the compositor had started, each the check's 10 s again (`spinning task 2` at 3.77 s and the panic at 14.03 s; 3.51 s and 13.64 s). Three other guests were running on the host for the first and none for the second, so load is not required. Both were on main 0888a9f0, which resolves a program's page fault with interrupts open and is the one change near this path; but 20 boots of the same image with that change and 20 with it taken back all passed, and the night's count is 2 of about 65 boots with it against 0 of about 60 before it, which decides nothing. `~/ferrix-logs/os-bd-gate/test-compositor-cores-bootpanic.log` (the panic at its line 3673) and `-bootpanic2.log` (line 2223) on nazuna; `~/ferrix-logs/os-bd-rate.sh <runs>` is the with-and-without loop. 2 points | os-02 | 7 |
| A two-last-threads exit check that provably races: spin-meet on two processors, with its negative control -- the old last-thread decision put back -- failing by name. Today's check passes that control too, so it shows only that such a process ends with its first thread's status (from stage 9's review of threads commit 4). 1 point | threads (os-9f) | 7 |
| End-to-end user programs for what the `sigpaths` check proves at the kernel's decision: a `SIGSEGV` caught on the alternate stack, and a read interrupted by a handler and restarted under `SA_RESTART` (`SA_RESTART` and the driven signal paths have landed) | ferrix-a5 | 7; `rustc` needs `SIGSEGV` on the alternate stack |
| Stage 6's reverse-map check reported "-4 frames not given back" once in two loaded musl `test-vfs` runs on 2026-09-14: four frames — one kernel stack — came back from outside its window. Cause confirmed by a marker-gated control (the check's own warm-up task, which posts its outcome and exits, reaped inside the measured window because `wait_until_reaper_quiet` counts only tasks already exited). Fix: `run_pinned` waits for its task to be dead before returning, landing with `MAP_PRIVATE`, the control showing zero after it. The threads checks' returning before their programs' spaces are dropped is a separate, real hazard shared with `exec::run`: P2 | os-58 (fix), os-43 (control) | 6 |
| Stage 8's eventfd check reported "-4 frames across the second run" once in 13 x86-64 `test-net` boots with the musl busybox on 2026-09-16 (os-12's matrix): the same shape, its own warm-up reader task dying inside the measured window. `sched::wait_until_gone` now waits until a task is dead and the reaper quiet, and the eventfd check's reader and `poll`/`epoll_wait` waiter wait on it before their handles drop (1 point, landed 2026-09-16). Its control: a reader lingering 2 s after answering gives "4 frames across the second run" without the wait and passes with it. Next, 2 points: `exec::run` and the other check sites that return before their task is dead converted to `wait_until_gone`, each named in the commit, so no frame-counted check meets this shape again | os-26 | 6, 8 |
| Retire the 20 ms console polling. Receive by interrupt into a 4 KiB ring has landed on all three: the PL011, the STM32 USART (through ST's EXTI on the DK1), and x86-64's 16550 through an I/O APIC input found from the MADT, with `console::input::{waiters, has_input}` for the console thread to wait on. Left: the console thread and readers waiting on it instead of sleeping (in `fs/terminal.rs`) | ferrix-a5 | 7, 15 |
| `AF_UNIX` descriptor passing, after names (bf4eec48). `SCM_RIGHTS` has landed: files travel with the first byte of a message, a receive installs what fits and closes and flags the rest, and a peek installs nothing (a written deviation). The in-flight cycle pass has landed too: sockets that only each other's queues refer to are emptied at the next close, process exit or exec. Left: `SO_PASSCRED` and `SCM_CREDENTIALS` (2). Carried from landing 1's review: cap a receive's kernel buffer at the receive capacity (`MSG_WAITALL` in chunks of it), drop a refused file outside the `FdTable` lock before descriptors travel, `SO_SNDBUFFORCE`/`SO_RCVBUFFORCE` needing `CAP_NET_ADMIN` and skipping the cap, and Linux's error order in `sendmsg` and `socketpair`. On the compositor's path (stage 17) as well as POSIX's | os-02 | 7, 17 |
| POSIX.1-2024 interface sweep: from musl's implementation of every mandatory POSIX.1-2024 function, the list of Linux system calls (and flags) they need; the stage 7 sweep tooling runs each on all three architectures and files every `ENOSYS`, `EINVAL` on a mandatory flag, or wrong result with the area that owns it, as rows here. Sockets and threads are known and excluded; the `epoll`, `eventfd`, `timerfd` and `signalfd` families are in scope because stage 17 needs them | os-b6, after `AF_UNIX` and the terminal switch | 7, 8, 17 |
| `mprotect` could make a shared mapping writable that its file never allowed: fixed with `memfd_create`'s may-write accounting, `AddressSpace::protect` now refuses `PROT_WRITE` on a shared file mapping that may not write its file with `EACCES`, and the memfd boot check shows it on a file opened read-only, landed | stage 8 (os-58) | 8 |
| `memfd_create` with `F_ADD_SEALS`/`F_GET_SEALS`, on tmpfs, after file-backed `mmap`: `wl_shm` is a sealed memfd both sides map. Landed: seals in tmpfs, the write seal refused while a shared mapping may write the file (a fork child's copy counted), and the FX-0880 boot check; the exec seal (`MFD_NOEXEC_SEAL`, `F_SEAL_EXEC`) is left | stage 8 (os-58) | 8, 17 |
| **Done 2026-09-19.** The seam, gated: `scripts/check-device-access.py` and `scripts/device-access-allowlist.json` refuse a volatile access or port instruction under `kernel/` outside an argued entry. Three kinds are kept apart, because conflating them would make the gate noise: `register` (MMIO or an x86 port, confined to the paths §1 and §7 permit), `shared` (RAM a device also walks -- IOMMU tables, the block and net rings), `memory` (volatile only so a boot check cannot be optimised into proving itself). 34 register sites, 11 shared, 35 memory. In `cargo xtask check` and CI, with five negative controls. Two detector bugs were found by checking the count against grep: path-qualified calls were invisible (hiding `cpu::outb`'s users) and the comment skip ate `*byte = ... read_volatile(..)` | os-f6 | 10 |
| **Done 2026-09-19.** Isolation per platform, written down: `docs/ARCHITECTURE.md` §7 now holds the table -- x86-64 translated through VT-d, AArch64 through the `SMMUv3`, ARMv7-A and the DK1 in degraded trusted mode -- each row naming the console line that decides it, every row read from a boot log rather than the roadmap's prose. The two untranslated rows are kept apart: ARMv7-A has an `SMMUv3` the kernel declines to program because U-Boot resets on `VIRTIO_F_ACCESS_PLATFORM`, the DK1 has no unit at all (read on the board). Function counts are deliberately not quoted: they vary with the devices a gate configures | os-f6 | 10 |
| Per-open windows onto the card VMO: a program that mapped `/dev/dri/card0` and closed it can still read what the next opener draws, since the exclusive open does not end a mapping (`docs/DISPLAY.md` §2.3). Until then `card0` is `0660` and root's, accepted for iteration 1 by os-f6 2026-09-16. Each open gets its own view of the card's pages, which comes with stage 19's render node | GUI session (os-e5), with stage 19 | 17, 19 |
| ferrousli's wrappers for `timerfd_create`, `timerfd_settime`, `timerfd_gettime` and `signalfd`, which stage 17's Rust event loop (calloop over the `polling` crate) can use, once the kernel has them; with a C test on the host and the same program booted on Ferrix, as the epoll and eventfd wrappers had. The epoll and eventfd half landed on 2026-09-17: all six epoll calls, `eventfd`, `eventfd_read` and `eventfd_write`, and `epoll_pwait2` declared in `sys/epoll.h`. 1 point | os-a8 (ferrousli), after os-26's kernel rows | 17 |
| Stage 10's block ring self-check flakes on x86-64: once in 11 `test-boot --arch x86_64` runs on nazuna (TCG) it panicked FX-1004 "stage 10 block ring self-check failed: quiescing the instant a driver died failed" (`kernel/src/block_ring/check.rs:343`, `kernel/src/main.rs:844`), at 669c2303 in E4's row; the other 10 boots, 5 with E4 and 5 at its base 1a55a1d3, and both `test-display` boots of that row passed the same check. E4 does not touch the block ring. The check waits on a driver's death, and c2129a68 changed how waits wake (on the event, not a 5 ms recheck), so start there. Log: nazuna `~/.local/share/ferrix/logs/gui/row-669c2303/boot-x86_64.log` | os-02, after the driver check row | 10 |

### P2 — quality and performance, on the "fast" half of the goal

| Item | Owner |
|---|---|
| The cost of the 20 µs one-shot armed on every wake onto the caller's processor, measured on pipe and futex paths | ferrix-34 |
| CI's host steps from one rule: GitHub CI's test job went red on 06336c60 with E0152, a second `panic_impl`, building `ferrix-devmgr` as a host test. CI's host clippy, test, doc test, documentation and native clippy steps carried their own list of freestanding crates, older than `xtask/src/check.rs`'s. Now `xtask/src/workspace.rs` sorts the workspace by a rule (members at `boot`, `kernel` and under `user/` are freestanding; a `#![no_main]` program anywhere else fails the gate), CI calls `cargo xtask host-clippy`, `host-test`, `host-doctest`, `host-doc` and `native-clippy`, and `cargo xtask check` runs the same functions, doc tests and documentation included. 2 points, landed 2026-09-16 | os-26 |
| Waits that wake on the event: `poll`, `ppoll`, `select`, `pselect6` and the epoll waits recheck every 5 ms rather than sleeping on the files' wait queues, so a compositor's frame loop and every idle client pay a wake-up per slice and up to 5 ms of latency. The same wake-from-the-source pattern as d22f9d3: a pollable inode names the wait queues its readiness reads, and a wait joins all of them before its last look, so a `wake_all` on any of them ends it. 5 points, landed 2026-09-16. Written deviations of the epoll landing (d047480d) until then: `EPOLLRDHUP` and `EPOLLPRI` are never reported, because `Readiness` carries neither a half-closed peer nor urgent data | os-26 |
| Per-CPU frame and heap caches, deferred since stage 2 | open, once a workload can measure them |
| `Inode::ioctl`: `sys_ioctl` special-cases the console, sockets and `/dev/dri/card<N>` by the open object's type; a hook on the inode replaces the three branches (os-02's review of the display stack, 2026-09-16). `/dev/input/eventN` (`docs/INPUT.md` §3.3, L6) would be a fourth special case | open, kernel VFS owner |
| Checked register offsets in the ring-3 virtio drivers: `Block::read`/`write` in `user/blk` and `user/gpu` assert on a device-controlled `notify_off` × multiplier, and on ARMv7-A `offset + size_of::<T>()` can wrap past the bounds check; one shared checked-offset accessor for both (os-02's review, 2026-09-16) | open, driver owner |
| devmgr's `await_published` kills a driver that exits without publishing and marks it dead, but never quiesces its device, as it did not before that change either. The IOMMU mappings go with the process, so this is hygiene rather than safety: quiesce the device once its driver is gone. Stage 10 (os-02's review of the display stack, 2026-09-16) | open, devmgr owner |
| ASIDs and PCIDs, so a switch stops invalidating every user entry | open, after threads |
| A gate for btop: a program in `test-net` or `test-vfs` on x86_64 that runs `btop` under `timeout` on the console and requires its panels' titles in the output, with a negative control that shows the check fails when btop cannot start (the 4 MiB `execve` limit it hit is the obvious sabotage). 2 points | open |
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
| Iteration 2, the Wayland server's kernel calls, verified from source by the GUI session: E1 `epoll_create1`/`epoll_ctl`/`epoll_wait`/`epoll_pwait`, a set waitable inside another (8, landed 2026-09-16); E2 `eventfd2` (2, landed 2026-09-16); E3 `ioctl(FIONBIO)` on sockets and pipes (1, landed 2026-09-16), and `FIOCLEX`/`FIONCLEX` on every file (1); the kernel side is done. No `timerfd`, no `signalfd`. Kernel session os-26 | 17 |
| The input iteration, and iteration 2's prerequisites, from `docs/INPUT.md`, approved by os-f6 2026-09-16. Input, 23 points: L1 `libs/linux-abi::input` from a committed probe (2), L2 `libs/virtio::input` (2), L3 `libs/inputctl`, the messages and the per-open queues (3), L4 `libs/virtio-input`, the driver logic (3), L5 `user/input`, devmgr's entry and the input core's task (5), L6 `/dev/input/eventN` and its evdev subset (5), L7 `compositor/evecho` and `xtask test-input` over QMP with its negative control (3); GUI session (os-e5), kernel reader for L5 and L6 open. Iteration 2's prerequisites, 14 points: E1 `epoll` with nesting (8, landed 2026-09-16), E2 `eventfd2` (2, landed 2026-09-16) and E3 `ioctl(FIONBIO)` (1, landed 2026-09-16), os-26's row above; E4 `card0`'s primary plane and properties (3, `docs/DISPLAY.md` §2.3), GUI session (os-e5), landed after os-02's review. E5 `timerfd` (3) is wanted, not required, and unassigned. Landed: L1 (2) and L2 (2), whose fuzz target ran ten minutes without a failure and whose evdev numbers are now L1's. L3 (3), `libs/inputctl`, host-tested and fuzzed; E1, E2 and E3 (11, os-26), and E4, so iteration 2's prerequisites are all in | 17 |
| Input L4, `libs/virtio-input`, the driver logic (3): **landed 2026-09-17**. `Driver` over `Transport`/`DevicePages`/`EventArea` as `libs/virtio-gpu` has it (bring-up without `DRIVER_OK` until READY, HELLO from every event type the device declares, 64 event buffers, batching at the last `SYN_REPORT` within 64 events, a report reaching one event short of the core's limit cut by the driver, `Teardown::Wedged` when the reset does not finish), a fake device copying QEMU 9.2.4's four tables, 28 tests including `src/tests/batch.rs`, and the `virtio_input_driver` fuzz target with five seeds, which ran 10,523,605 inputs in ten minutes without a failure. The two departures the WIP left for review are `docs/INPUT.md` §6 decisions 9 and 10. Three expectations in the WIP's own tests were written without running and were wrong, not the code: the batch fills to one short of its capacity, not two; the gradient's cell step is 8, not 16; and a `Damage` past its rectangle limit collapses and then fills up again. For L5: how the core learns a driver broke is still undefined, and the glue must call `on_interrupt` again while `Drained.more` is set. GUI session (os-e5) | 17 |
| The compositor's tiny-skia renderer, `compositor/render` (5, stage 18's list): **landed 2026-09-17**. A Smithay-free `Canvas` over a tiny-skia pixmap (`tiny-skia =0.12.0`, default features off, `std` and `simd`: no C, no build script) with `clear`, `fill`, `border` and `composite`, `present`, `render` of one monitor and `damage_between`; the two pattern clients; the run-length expected-image format; 20 tests including the byte-exact golden image of two tiled clients at 1024x768 and its one-pixel negative control. The image was looked at through the PPM dump before it was blessed and its translucent half checked against the arithmetic by hand (green 176 over background 17 at alpha 0xC0 is 137). Green on the host and for `x86_64-unknown-linux-musl`. GUI session (os-e5) | 18 |
| LEDs: `write` to `/dev/input/eventN` injecting `EV_LED` events, as Linux does, carried to the device through virtio-input's status queue by an additive `inputctl` message (`STATUS`, core → driver). Until then `write` answers `EINVAL` and the caps-lock LED does not light (`docs/INPUT.md` §6, os-f6 2026-09-16) | 17 |
| Multi-touch axes (`ABS_MT_*`), force feedback (`EV_FF`, `EVIOCSFF`) and sound (`EV_SND`) on `/dev/input/eventN`. The input core publishes a device without them and its boot line says what was left out; QEMU's keyboard and tablet declare none (`docs/INPUT.md` §3.2, §6, os-f6 2026-09-16) | 17 |
| Input hotplug: devices exist from boot in the input iteration. A device that arrives or leaves later, and how a compositor learns of it without udev (`inotify` on `/dev/input`, not in Ferrix today, or a rescan) (`docs/INPUT.md` §3.4, §6, os-f6 2026-09-16) | 17 |
| `card0` is opened by one process at a time, standing in for DRM master (`docs/DISPLAY.md` §5, a written deviation from Linux): Linux's many opens with one master, `SET_MASTER`/`DROP_MASTER` arbitrating between them, and the render node beside it come in stage 19 | 19 |
| The compositor workspace: the server written from scratch (decided 2026-09-17), CPU rendering, dwindle and master, `hyprland.conf`, `hyprctl` IPC, two Rust test clients. `hyprland.conf` landed 2026-09-16 (`compositor/config`, 5); the layouts and dispatchers landed (8); `compositor/render` landed 2026-09-17 (5). `compositor/wire`, the Wayland wire protocol with no libwayland, and `compositor/protocol`, the interface tables generated from the XML, landed 2026-09-17 (8). `compositor/server`'s connection, `wl_display` and `wl_registry` landed 2026-09-17, with a real libwayland client reading its globals over a replayed transcript (5). `wl_shm` over sealed memfds and `wl_compositor`/`wl_surface` landed 2026-09-17, with a real libwayland client's window-setup requests replayed into the server (8). `xdg_shell` and the `AF_UNIX` socket landed 2026-09-17, and with them a real libwayland client's whole startup handshake runs against the server over a real socket (11). `compositor/hyprix`, the compositor itself, and `compositor/pattern`, the test client, landed 2026-09-17 with the headless half of stage 18's exit passing: two clients tiled, 0 differing pixels of 786,432 against `compositor/render`'s expected image, and a negative control (13). `compositor/ipc` and the `hyprctl` socket landed 2026-09-17, driven by Hyprland's own `hyprctl` binary: `clients`, `monitors`, `workspaces`, `activewindow`, `dispatch` and `runtime keyword` all answer (5). The DRM backend and `xtask test-compositor` landed 2026-09-17: `compositor/drm` is the card, lifted out of `compositor/blank`, and the compositor boots as init on Ferrix and shows its background on every one of QEMU's 786,432 pixels on x86-64 and AArch64 (10). Two clients on the card landed the same day: the initramfs carries `compositor/pattern` at `/bin/pattern`, the compositor's `exec-once` starts two, and `cargo xtask test-compositor` requires QEMU's screendump to match the renderer's expected image pixel for pixel on x86-64 and AArch64 (5). Next: `wl_seat` with keyboard and pointer, which waits on stage 17's input L5-L7 (8); the event socket `.socket2.sock` a bar subscribes to (3). GUI session | 18 |
| ~~The GPU, Path A (`docs/GPU.md` §3, decided 2026-09-18): A1 virtio-gpu 3D in the ring-3 driver; A2 `/dev/dri/renderD128`, GEM handles, the `virtgpu` ioctls from a committed probe, and scanout of a 3D resource; A5 the host half in xtask; A3b `compositor/virgl` behind a renderer trait with the software renderer as the fallback. 52 points~~ **Done 2026-09-19** (`docs/GPU.md` §3.7 and §3.8): the desktop composites on the GPU and the screen is shown the texture it drew into, 39 ms to 12 for a 1080p video-and-blur frame. `cargo xtask test-compositor --gl` judges it from inside the guest | 19 |
| The GPU, after Path A: A4 `zwp_linux_dmabuf` and a GBM-shaped allocator, and Mesa's virgl on ferrousli (`docs/GPU.md` §3, 3a), for clients that render on the GPU themselves. 8 points and 40 or more. Unowned | 19 |
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
* Stage 21, bare metal with an NVIDIA card driven by Ferrix itself: Path B
  of the GPU decision of 2026-09-18, `docs/GPU.md` §4. Opened when the
  customer wants Ferrix on real hardware; unsized, over 100 points.
* Stage 22, Steam (decided 2026-09-18): the 32-bit x86 ABI (unsized),
  glibc's place taken by ferrousli under the Steam runtime (13 priced, the
  rest unsized), bubblewrap's needs on top of stage 13 (13), a root on btrfs
  (stage 12), XWayland (40 as a first guess), sound -- `virtio-snd`, an
  audio core, a PulseAudio or PipeWire server (30) -- and Vulkan through
  Venus on a KVM host (8 over Path A, plus the driver question). Over 300
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

## `splice` and `copy_file_range` are answered `ENOSYS`, and uutils asks

uutils/coreutils reaches for both before falling back: `cat` and `cp` take a
kernel-side copy where there is one. Every `test-vfs` boot since uutils owned
`/bin` reports them, four to six times a run, as `syscall number 275` and
`number 326` in no table. Nothing fails — the fallbacks are correct — but the
lines are noise in every log, and a copy through user space is the slow path
for exactly the programs a userland uses most.

busybox never asked for either, which is the point: `std` and the crates
above it are a much larger consumer of the kernel than busybox was, and what
they need shows up at run time. 3 points, and the control is the absence of
those two lines from a `test-vfs` log.

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

---

## Decisions

Dated, newest first. A decision here is final until the customer says otherwise.

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
