# Ferrix — backlog and decisions

`docs/roadmap/` says what each stage is and how it knows it is finished.
This file says who is doing what right now, in what order, and which
decisions were taken along the way. It exists because a dozen sessions work on
the tree at once, and a decision that lives only in a message between two of
them is a decision the third one reverts.

The customer is the product owner and decides scope and priority; the fleet
coordinator keeps the landing order. A session that lands a piece of this
file updates its row in the same landing, the way it updates the stage's
file under `docs/roadmap/` (and `status.md`'s status table when a row
changes). When a row is done it is deleted, not struck through;
the roadmap records what landed, and this file's own history
(`git log -p -- docs/BACKLOG.md`) keeps the investigations and the wind-down
records of earlier days.

---

## Standing rules

These add to `docs/CONVENTIONS.md`, which still governs commits.

**What a landing runs.** Rebase onto `main`, then:

| The change touches | Gate |
|---|---|
| Only `docs/` | `cargo xtask check` |
| Only `userland/ferrousli/` | `cargo xtask check --ferrousli`, then `cargo xtask busybox` and, with the busybox it built, `test-shell` and `test-vfs` on x86_64 with `--init ferrousli`, so the binary the gates run never lags the library; then `cargo xtask uutils`, which links uutils/coreutils against it and is the larger consumer of the two, since it brings Rust's whole `std` with it; a change to `userland/ferrousli/tools/ports/` also runs `cargo xtask ports` and `test-net --arch x86_64 --init ferrousli`, which fetches with the curl it built, and for the Arm ports `cargo xtask ports --arch aarch64` and `--arch armv7a`, then `test-net --arch all` with the static busybox, which fetches and clones with them |
| Only `userland/zinc/` | `cargo xtask check --fast --zinc`: zinc's formatting, clippy, unit tests and the two pty tests (`userland/zinc/tests/pty_completion.py`, `userland/zinc/tests/pty_jobs.py`); a change to what zinc does at boot also runs `test-boot` on x86_64, and a change to how it starts, waits for or signals a process also runs `test-shell --arch all` and `test-jobs`, which is the only gate that types at a console |
| Only `userland/init/`, or `libs/init/svc` | `cargo xtask check`, which runs `userland/init/`'s formatting, clippy and tests by default and `libs/init/svc`'s with the host's, then `cargo xtask test-init --arch all`, which boots `/sbin/init` as pid 1 and types at the shell its getty gives |
| `libs/` only, and no crate the kernel builds | `cargo xtask check`, and one boot: `test-boot --arch armv7a --smp 2` |
| Anything the image contains: `kernel/`, `boot/uefi/`, a kernel-side crate in `libs/`, `xtask` | `cargo xtask check`, then `cargo xtask build --arch all --release`, since CI builds and boots the release profile and no other gate does, then `test-boot` on x86_64, aarch64, armv7a at four processors and armv7a at `--smp 2`; a stage 7 or 8 change also runs `test-shell` on x86_64 with no `--init`, which runs zinc, the image's shell, and then with the ferrousli busybox (`--init ferrousli`), the musl busybox *and* the host's glibc busybox (`/usr/bin/busybox`), and `test-vfs` on x86_64 with the ferrousli busybox and the musl one; a stage 7 change also runs `test-threads --arch all`, the Rust `std::thread` program (since 5fd2ab09); a change to the loader (`userland/ferrousli/ld`), to `exec` or to ferrousli also runs `test-shell` with Debian's dynamic busybox twice, on glibc's own `ld.so` and `libc.so.6` on all three architectures and on `--interpreter ferrousli --library ferrousli` on x86_64 (docs/ROADMAP.md, dynamic linking; `scripts/fetch/fetch-debian-busybox.sh` fetches it). The whole of that, plus KVM, is what moves `main` |
| User mode, page tables, TLB, SMP or the scheduler | The row above, and x86_64 under `--accel kvm` |

**Landing on `main`.** There is one landing branch, `main`; `develop` has
not been used since 2026-09-15 and nothing lands on it. `main` moves only
under the fleet landing lock, `~/.local/share/ferrix/fleet/land.sh`
(`queue`, `take`, `release`; `status` shows the queue), held by the fleet
coordinator's rules (customer, 2026-09-26):

* Gate outside the lock, on a base rebased onto `main`. Then `land.sh take`,
  `git rebase main`, and fast-forward: at once if `main` is still the gated
  base, or if it moved only in files the change does not touch and nothing
  cross-cutting (locks, the scheduler, the trap or system-call entry, memory
  management). Otherwise release, re-verify and queue again.
* Hold the lock only for the rebase and the fast-forward, and `land.sh
  release` at once. Two minutes is the target hold; a hold of fifteen is
  stale.
* Each green slice lands within the hour, one landing deep, about eight
  points. A live branch unlanded for more than four hours is asked for its
  plan.
* Move `main` from a clean root with `git merge --ff-only`, or by
  compare-and-set (`git update-ref refs/heads/main <new> <old>`) followed by
  syncing the root checkout; a root left behind shows the landing as staged
  deletions, and a commit from it would revert the landing.
* After the fast-forward, the lander boots `main` once on x86_64 under
  `--accel kvm` and reports the hash with that result, so a bad merge is
  seen by the one who made it; then pushes `main` to the gate host
  (`git push nazuna-wg:Documents/projects/os/ferrix main:main`), so no
  worktree there is made from a stale `main`. A push to `origin` needs the
  customer's word, given in the session that pushes.

**Re-verifying after `main` moved.** When the commits that moved it touch
none of the files the change touches, the gate still stands: say so in the
report and land (`docs/CONVENTIONS.md`, splitting rule 3). When the files
overlap, or the change is cross-cutting, re-run `cargo xtask check` and two
boots, `armv7a --smp 2` and x86_64 under `--accel kvm`, or the whole row
when the overlap is in code the change depends on. A boot that fails is a
result to read, not a reason to retry: keep its log, rerun once, and give the
failure a row the day it is seen (CONVENTIONS rule 5).

**The busyboxes.** The busybox built against ferrousli is the primary one: the
userland Ferrix is measured with, and the one every `test-shell` and
`test-vfs` above names first. `cargo xtask busybox` builds and installs it and
`--init ferrousli` runs it; the flag is still given explicitly, since xtask
has no default program. Alpine's static musl busybox and the host's glibc
busybox stay required in every gate that names them, as the compatibility
checks: a failure on any of the three fails the gate, and nothing was dropped
when ferrousli's joined. `--init ferrousli` rebuilds the busybox first when
it is missing or older than anything under `userland/ferrousli/` it is built from, so
the binary a gate runs is the base's.

**Gates run on the Linux host, not in WSL (customer, 2026-09-16).** Builds,
tests and every gate run on nazuna over `ssh nazuna-wg`, in the session's own
worktree there; WSL on the Windows machine is a convenience for a look, never
the reference, and a row run there does not count.

**A failing gate's log is kept before any re-run (2026-09-16).** Copy it
aside first; a re-gate that overwrites it turns a result into a rumour — the
one full trace of FX-0701 was lost that way.

**One build directory per worktree on the Linux host (2026-09-16,
corrected 2026-09-26).** Every worktree sets
`CARGO_TARGET_DIR=~/.local/share/ferrix/target-<session>-<branch>` and never
grows its own `target/`; the directory goes when the worktree does. Two
worktrees never share one, not even two of the same session's: xtask finds
the repository through `CARGO_MANIFEST_DIR`, which is fixed when xtask
compiles, so a shared directory runs whichever tree last built xtask, with
that tree's `build/` images (a certification row was voided that way on
2026-09-26), and Cargo names a workspace member's artifacts without its path,
so a stale crate from the other tree passes for current. The root filesystem
filled twice on 2026-09-16 from build output; delete a directory the moment
its worktree is removed.

**Nobody works in the root checkout, and landings are small and often
(customer, 2026-09-16).** The root checkout keeps `main` checked out and
clean; a session that edited there blocked every other session's
fast-forward. Every session works in its own worktree under
`.claude/worktrees/`, and lands each stable, gated step on `main` the day it
is green — a worktree is never more than one landing deep, and a 40-point
milestone is ten landings, not one.

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

**Changes inside the certification item are reviewed first (customer,
2026-09-26).** The certification session (ferrix-55 at the time) stays as a
standing consultant. A change inside the item -- what
`scripts/data/certification-item.json` puts in the `core` or `item` ring and
`scripts/check/check-item-boundary.py` enforces; the `load` ring above it is
outside -- gets its one-line OK before the landing lock is taken: send it the
files, what changes and how it is tested. A structural change gets a design
review before the code. The consultant records found-and-closed findings,
coverage entries and threat updates in `docs/certification/` in small
batches, and flags item changes on `main` that skipped it.

**An item change carries its coverage anchors (2026-09-27).** Since the
coverage evidence landed (41456b68), a change that moves a line of the item
makes `gen-coverage-justification.py --check`, and so `cargo xtask check`,
fail on stale anchors. Before landing it, run `python3
scripts/gen/carry-coverage.py && python3
scripts/gen/gen-coverage-justification.py` on the rebased tree and commit
the result: it renumbers the anchors through the diff, drops and prints the
lines the change edited, and never re-measures.

**Every `unsafe` in the item names its obligation (2026-09-27).** Since
F-26 closed (1be610fe), `scripts/check/check-unsafe-audit.py` fails `cargo
xtask check` on an `unsafe` site in the `core` or `item` ring that does not
name one of the obligations registered in
`scripts/data/safety-requirements.json` and tabled in
`docs/certification/SAFETY-MANUAL.md` §2 (CONTEXT, SYSREG, SHARED, ENTRY,
TRANSLATE, DEVICE, FIRMWARE, PROTECT, KMEM, FRAME, PROBE, DMA, BOOT-DATA,
USER-COPY): `// SAFETY: (ID) prose` on a block, and `/// (ID) ...` as the
first line of an `unsafe fn`'s `# Safety`. A new obligation is added to the
register with the certification consultant, not invented in place.

**Files every landing appends to overlap by function, not by file
(2026-09-27).** `kernel/src/syscall/check.rs`, the panic catalog,
`docs/generated/*` and this file change in nearly every landing, and
counting any change to them as an overlap had a finished landing chase
`main` twice. After a clean rebase, changes on `main` only in other
functions or rows are no overlap for the re-verify rule above -- but
`check.rs` runs at boot, so such a landing still runs `cargo xtask check`
and one x86-64 boot on the rebased tree. A change to the landing's own
function, chain or files is an overlap as before.

**Agents.** Gates and boots in the foreground, never `run_in_background`; one
architecture per tool call; the brief says so.

**Milestones.** The customer tests from `main`, so testable progress is
tagged there rather than waiting for a stage to end: `stage-N` for a stage's
exit, `stage-N.k-<slug>` for a testable step after it, annotated, the tag
message carrying short release notes as a bullet list and saying what to test
and how; the same notes go into `docs/RELEASES.md`. A tag is placed only after
the whole matrix has passed on that commit from a clean worktree
(`~/.local/share/ferrix/po-verify.sh <commit>` on nazuna): `check --ferrousli
--zinc`, `busybox`, the four boots, x86_64 under KVM, `test-shell` with the
ferrousli, the musl and the glibc busybox, `test-vfs` with the ferrousli and
the musl busybox, `test-net --arch all`, `test-display --arch all` and
`test-threads --arch all`. Owners say in one line what a person can test when
such a landing is on `main`. A release freeze (the customer's word) means:
each session lands what is gate-green, leaves the rest on its branch with a
row saying where it stands, deletes its target directory and reports; the
release notes land last, that head is verified and tagged, and the push to
`origin` is the customer's.

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

Session names change on every restart. `ListAgents` shows what is alive; this
table is the roster of 2026-09-26, 18:00, from the fleet coordinator. At 19:15 the
customer cut the number of sessions running at once, because the host ran at
load 50 to 80 and every gate took one to two hours: a session marked **winding
down** finishes the task named in its row, lands it, files what is left as open
rows, removes its worktrees and target directories, and takes nothing new. Any
session that has finished everything it owns, winding down or not, reports to
the coordinator and then asks the customer in its own session, with a question
the customer has to answer ("ferrix-xx is done: ... May I be stopped?"), so
that a waiting question shows which session is finished; a question only the
customer can decide is asked the same way. A row
below whose owner is "open" has no live session; take it by putting your
session's name in its owner cell in your first landing.

| Session | Area |
|---|---|
| the customer | Product owner: priorities, decisions, what is stable enough for `main` |
| ferrix-2c | Fleet coordinator: landing order, the landing lock, shared hot files, unblocking |
| ferrix-15 | The init (`docs/INIT.md`), L13 parked; the audit record's design (F-21b) |
| ferrix-55b | T0 of the live kernel update plan (cf265506, debe8998, 742fdaeb) and S0 of the opaque-kernel plan, both seam rows measured (d6974a66, c4c8b186); the plan is shelved by the customer (2026-09-27), so nothing further is owned here |
| ferrix-c7 | Chrome and `rustc` on ferrousli's loader (the customer's ask, 2026-09-26): landed d7b0709a and beffed20; last, the git and foot port reds (weak obstack, `random_r` declared); then **winding down** |
| ferrix-41 | Stage 22's 32-bit x86 ABI (`docs/I386.md`): I1, I2a (the thread pointer), I2b (fork, clone, signal frames) and I3 (Alpine's i386 busybox runs) on main; I4 (8), glibc's i386 busybox, next |
| ferrix-90 | Audio (`docs/AUDIO.md`): L1 to L7 done 2026-09-26, Chrome plays through `/dev/snd`, a dead driver restarted (F-38's quarantine); U1, alsa-lib and aplay on ferrousli, done 2026-09-27; U2, the PulseAudio-protocol server in Rust, 18 points in `docs/AUDIO.md` §5: U2, the PulseAudio-protocol server, done 2026-09-27 in four slices: the protocol, `pulsed` playing one client frame for frame, mixing and resampling, and the desktop with Chrome playing through it. Next U3, SDL and games finding what is missing by running. `userland/media/` is this session's too since 2026-09-27, Bad Apple's included: its `test-badapple` is in the gate of any slice that touches `pcm` or `resample` |
| ferrix-d5 | The Rust desktop clients (`docs/DESKTOP-CLIENTS.md`: the clients-base crates, waybar, fuzzel, hyprlock, hypridle), the EDID override, `docs/AUTH.md` and its phase 1; at most three streams running at once |
| ferrix-55 | Standing certification consultant (reviews item changes before they land; keeps `docs/certification/` current). Its last engineering landing, the combined coverage evidence, is on branch `cov-d-evidence` |
| ferrix-e1 | The repository relayout (`docs/LAYOUT.md`), landed 2026-09-26 as 26303ad5. **Winding down** after its post-landing rows and cleanup |
| ferrix-9c | The Pixel 7's USB CDC-ACM device driver (`docs/PIXEL7-USB-HANDOVER.md`): the phone showed up as `/dev/ttyACM0` on nazuna on 2026-09-26. **Winding down** once `usbdev` with the kernel log channel has landed |
| ferrix-d4 | The Pixel 7's GUI, option A: a desktop in the launcher app's crosvm VM, with Chromium on it; landed 2026-09-27. **Winding down** after that landing |
| ferrix-8e | This file's cleanup (landed 6b942df1); two_clients' GPU-frame flake. **Winding down** after that |
| ferrix-b0 | Bad Apple!! with sound (`docs/MEDIA.md`, the customer's order of 2026-09-26), landed; now Bad Apple on the `--everything` desktop (a toolkit window, `SUPER M`), then stopping; Doom is in the backlog, unowned |
| open | The Pixel 7 bring-up (`boot/pixel7`, statd, `tools/pixel7`; was ferrix-0a); Chrome's extensions bubble and `test-chrome-window`'s context-menu step (was ferrix-a8); every row below owned by an `os-*` session before 2026-09-26 |

---

## Red on `main`

The customer's order of everything puts a working `main` first. These are
gates that fail on `main` itself, not flakes, and come before any row below.

| Item | Owner |
|---|---|
| None known on 2026-09-27: the git and foot port failures were fixed by d567d050 | -- |

## The path to the goal, in order

The first goal, `rustc` on Ferrix (stage 16), was met on 2026-09-22. The
goal since is the Hyprland-shaped desktop (stages 17 to 19, decision of
2026-09-13) and then Steam (stage 22); what is being built toward it now is
the customer's order, and the owners table above lists it. The rows below are
what is left, ordered by what blocks what.

### P1 — required before a stage is called done

| Item | Owner | Stage |
|---|---|---|
| The init's later landings, L11 to L13 of `docs/INIT.md`. **L1 to L10 are done (2026-09-26)**: `/sbin/init` over `libs/init/svc` is pid 1 of `cargo xtask run` and of every desktop image (hyprix is `hyprix.service`, each client in a scope of its own); getty, `svc`, readiness, socket activation, resource limits, bootstrap channels, native services and the directory are in, gated by `cargo xtask test-init` on all three architectures, `test-compositor` and `test-jobs`. **L11 is done (2026-09-26, ferrix-55b)**: `devmgr` restarts by the policy, now its own no-alloc crate `libs/init/restart` with systemd's fixed-window start limit, and reports each death's status through K6. The init is done as far as the customer counts it (L1 to L11, 2026-09-26). `docs/AUTH.md`'s P0b is done too (2026-09-26): a `Type=native` service with `User=` is made by a helper that has become the user, and runs as it. **L12 is done (2026-09-27)**: under `ferrix.devmgr=init`, which every image booting init sets, pid 1 starts `devmgr` through a kernel starter and `/` switches after it. Left: **L13** `PrivateTmp=`, `ProtectSystem=`, `PrivateNetwork=`, `SystemCallFilter=`, `NoNewPrivileges=`, parked until stage 13's namespaces and seccomp (8) | ferrix-15 | 15; 13 for L13 |
| Under `ferrix.devmgr=init` (L12) the kernel's disk checks of stages 10 to 12 do not run, since `devmgr`'s drivers come after pid 1 (`docs/certification/SAFETY-MANUAL.md` AoU-13). Bringing that configuration into the certified one needs those checks to run anyway: the kernel starting check drivers of its own before pid 1, as the stage 10 driver check already can where `devmgr` did not, then handing the devices to the `devmgr` pid 1 starts (the certification review's option (i), 2026-09-27) | unowned | 10, 12 |
| After `svc restart devmgr.service` init cannot remove `drivers.slice/devmgr.service` (`removing its cgroup: Resource busy`) though every process in it has ended: a job beneath it, one a dead driver's was, is still held for a moment. The restart itself works, reusing the cgroup, so this is a wart and not a failure. Where to start: what holds a dead driver's job after its process has gone -- the pin quarantine (a3f23da4), a ring's charge -- and whether init should wait for the subtree to go before removing. Seen in `test-init`'s L12 stage on 2026-09-27 | ferrix-15 | 15 |
| Authentication, phase 1 (`docs/AUTH.md` §7, 27 points): `libs/crypto/argon2` (Argon2id and BLAKE2b against RFC 9106's vectors, timed on each architecture; 5), `libs/proto/auth-proto` (the conversation's records and `Secret`; 3), `authd` (the store on the root volume, policy per service, the throttle, the audit log; 8), `passwd` and `authctl` (3), image seeds and `cargo xtask test-auth --arch all` with a negative control per refusal (5), the Security Target's OE.AUTH and the documents (1). P1.5, hyprlock's backend over it (2), is the hyprlock stream's | auth (ferrix-d5) | 15 |
| Authentication, phase 2 (`docs/AUTH.md` §7, 31 points besides init's L10): procfs honouring `PR_SET_DUMPABLE` (2) and freed socket, pipe and tty buffers zeroed (1); `login` on the getty with the first password on a local console (5); `sessiond`, seat0's owner, handing hyprix its devices and starting it as the user (10); hyprix unlocking only on `authd`'s grant, and a new locker taking over a dead lock (6); `su` with the wheel rule (3); gates that play the compromised client (4) | auth, with init and the compositor | 15 |
| Authentication, phase 3 (`docs/AUTH.md` §7, about 32 points sized): the PAM shim in ferrousli (5), TOTP (3), ssh passwords through `authd` (4), privilege prompts (6), accounts with a generated `/etc/passwd` (4), a graphical greeter (8), `SO_PEERCRED` taken at `connect` (1), `mlock` as a no-op (1); FIDO2 and fingerprint unsized | open | after 15 |
| Threads, the one piece left after the exit of 2026-09-16: credentials are kept per process, a written deviation from Linux, and that is safe under musl's and glibc's `set*id` broadcast only while setting an id to a current one is permitted in every form. Owed: a static musl program of two threads calling `setuid(1000)` as root that survives | open | 7 |
| A two-last-threads exit check that provably races: spin-meet on two processors, with its negative control -- the old last-thread decision put back -- failing by name. Today's check passes that control too, so it shows only that such a process ends with its first thread's status (from stage 9's review of threads commit 4). 1 point | open | 7 |
| End-to-end user programs for what the `sigpaths` check proves at the kernel's decision: a `SIGSEGV` caught on the alternate stack, and a read interrupted by a handler and restarted under `SA_RESTART` (`SA_RESTART` and the driven signal paths have landed) | open | 7; `rustc` needs `SIGSEGV` on the alternate stack |
| Frame-counted checks that return before their task is dead. `sched::wait_until_gone` waits until a task is dead and the reaper quiet, and the rmap, eventfd and exec checks wait on it (FX-0882, FX-0884). Left, 2 points: `exec::run` and every other check site that returns before its task is gone converted, each named in the commit, so no frame-counted check meets this shape again | open | 6, 8 |
| Retire the 20 ms console polling. Receive by interrupt into a 4 KiB ring has landed on all three: the PL011, the STM32 USART (through ST's EXTI on the DK1), and x86-64's 16550 through an I/O APIC input found from the MADT, with `console::input::{waiters, has_input}` for the console thread to wait on. Left: the console thread and readers waiting on it instead of sleeping (in `fs/terminal.rs`) | open | 7, 15 |
| `AF_UNIX` descriptor passing, after names (bf4eec48). `SCM_RIGHTS` has landed: files travel with the first byte of a message, a receive installs what fits and closes and flags the rest, and a peek installs nothing (a written deviation). The in-flight cycle pass has landed too: sockets that only each other's queues refer to are emptied at the next close, process exit or exec. `SO_PASSCRED` and `SCM_CREDENTIALS` landed on 2026-09-26 (909ce1aa). Left, carried from landing 1's review: cap a receive's kernel buffer at the receive capacity (`MSG_WAITALL` in chunks of it), drop a refused file outside the `FdTable` lock before descriptors travel, `SO_SNDBUFFORCE`/`SO_RCVBUFFORCE` needing `CAP_NET_ADMIN` and skipping the cap, and Linux's error order in `sendmsg` and `socketpair`. On the compositor's path (stage 17) as well as POSIX's. | open | 7, 17 |
| POSIX.1-2024 interface sweep: from musl's implementation of every mandatory POSIX.1-2024 function, the list of Linux system calls (and flags) they need; the stage 7 sweep tooling runs each on all three architectures and files every `ENOSYS`, `EINVAL` on a mandatory flag, or wrong result with the area that owns it, as rows here. Sockets and threads are known and excluded; the `epoll`, `eventfd`, `timerfd` and `signalfd` families are in scope because stage 17 needs them | open | 7, 8, 17 |
| `memfd_create`'s exec seal (`MFD_NOEXEC_SEAL`, `F_SEAL_EXEC`); the rest of sealing landed with the FX-0880 boot check | open | 8, 17 |
| Per-open windows onto the card VMO: a program that mapped `/dev/dri/card0` and closed it can still read what the next opener draws, since the exclusive open does not end a mapping (`docs/DISPLAY.md` §2.3). Until then `card0` is `0660` and root's, accepted for iteration 1 by os-f6 2026-09-16. Each open gets its own view of the card's pages, which comes with stage 19's render node | open, with stage 19's render node | 17, 19 |
| Trusting a BAR firmware placed but did not enable. **Started 2026-09-19:** `libs/platform/fdt` now reads a host bridge's `ranges` into `PciWindow`s that keep the bus and CPU addresses apart, with `holds`/`translate` and seven host tests (QEMU `virt`'s own three entries among them). Left, in order: the same windows on ACPI machines, which needs the loader to call `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL.Configuration()` before `ExitBootServices` and carry them in `BootInfo` (a version bump); the vetting itself in `kernel/src/pci.rs` -- whole BAR in one window, window kind, prefetchable one way only, every bridge upstream forwarding and decoding, decoding-on BARs admitted first, unassigned BARs reported as unassigned; and turning decoding on at `IoMapping` creation rather than at enumeration | ferrix-d9 | 10 |
| CI's Miri step for `libs/fs/btrfs`: CI runs Miri over `libs/fs/block`'s request queue and not over btrfs. Under 15 minutes, with the whole-image tests ignored under Miri | open | 11 |

### P1 flakes — seen on a gate, each with its log

A flake that keeps firing is cheaper to fix than to rerun. Each row keeps its
log path and commit; a new sighting is added to its row the day it is seen.

| Item | Owner | Stage |
|---|---|---|
| Stage 7's signal hand-off check flaked on 2026-09-24: "a signal sent to a process of two threads was given to no thread" (`kernel/src/syscall/check.rs`, the check that blocks `SIGUSR1` in the first thread and requires the second to be handed it), at 10.01 s in `test-shell` on x86-64 under TCG with zinc, in cgroup landing G4's first gate (6c553b8c) on a loaded nazuna. The rerun of the row on the same commit passed, as did every other boot of that gate and of G4's final one. G4 changes `clone3` and cgroup moves, not signal delivery; `take_handed_to` reading 0 means `kill::send` picked no thread at all, so start at how the send judges a thread that is between its wake and its return, beside FX-0701's `return_to_user` fix. Kept: `~/ferrix-logs/stage7-handoff/2026-09-24-shell-zinc-6c553b8c.log` on nazuna (the panic at its line 106). **Again 2026-09-26**, at 12.81 s in one of 28 x86-64 TCG `test-boot`s of the FX-1004 channel fix (branch `fix-fx1004-endpoint`, before its commit; it changes only `object/channel.rs`), QEMU pinned to two cores shared with two busy loops. Kept: `~/ferrix-logs/stage7-handoff/2026-09-26-boot-x86_64-fx1004-channel-fix.log` on nazuna. **Once more, 2026-09-24**, in `test-input` on x86-64 under a load of 37, the night of FX-0001 below: `~/ferrix-logs/signal-no-thread/2026-09-24-input-x86_64-454eaab.log` on nazuna **Twice more on 2026-09-26 under the drcov coverage plugin**: at 11.69 s in x86-64 `test-powerfail` (`~/ferrix-logs/fqs5-flakes/sighting-handoff-cov2-x86_64.log`, line 2701) and at 8.85 s in ARMv7-A `test-shell --smp 2` (`sighting-handoff-cov4-armv7a.log`, line 1682). Tried on branch fix-queued-small-5: about 33 boots that run the check, QEMU pinned to two cores beside two pinned busy loops with drcov on -- 27 x86-64 `test-sysfs`, 3 x86-64 `test-boot`, 3 ARMv7-A `test-boot --smp 2` -- and it did not recur. Read: `take_handed_to` answering 0 means `notify_signal` found no thread in `threads()` that does not block `SIGUSR1`, or was not called (`post_into` answering other than `Pending`); the second thread spins in user mode and never changes its mask, and only the check's own `take_handed_to` clears the word, so neither is explained yet. Next: print, in a failing boot, each thread's mask and `is_gone` as `notify_signal` sees them. | open | 7 |
| `test-boot --arch armv7a --smp 2` failed once on 2026-09-26 with "the kernel read no entropy by DMA": the boot reached `FERRIX-BOOT-OK`, but its `pci` line said `0 entropy bytes read by DMA` from the virtio-rng function. The tree was init-l6's gate (47494211), which changes no kernel code; the same row passed on its rerun, and the other three boots of the gate passed. Where to start: the virtio-rng read at boot on ARMv7-A with two processors, whether its completion can be missed or come after the line is printed. Kept: `~/ferrix-logs/init-l6/2026-09-26-boot-armv7a-smp2-47494211-no-entropy.log` on nazuna | unowned | 10 |
| `test-compositor --arch x86_64` is the one image row not yet seen green on the regrouped tree (docs/LAYOUT.md). On main 26303ad5 every functional check passed (the window slid through its three pictures and ended in the blessed one) and the row failed only on its frame budget, the slowest frame 7.9 s against 5 s, with the host at load 44–52 on 24 cores; its rerun on 7322d68a stopped earlier, in FX-0905, since fixed by "Change a task's state and its job's load in one step". Every other row passed on the regrouped tree: check, `build --arch all --release`, the x86_64 (TCG and KVM), aarch64 and armv7a (one and two cores) boots, `test-shell` with ferrousli and with busybox, `test-vfs`, `test-sysfs`, `test-init`. Rerun it on a quiet host; a failure that names a path is the relayout's. Kept: `~/ferrix-logs/relayout/2026-09-26-compositor-x86_64-26303ad5.log` on nazuna **Measured 2026-09-27 (ferrix-9b, branch `frame-budget`, not landed): the host's run-queue wait explains little of it.** That branch reads each virtual processor's `/proc/<tid>/schedstat` through the boot and takes off only the wait inside each frame's own stretch, with a SIGSTOP of the boot's own QEMU as the negative control (which failed as it must, 2.3 to 3.8 s past the bound). At loads of 25 to 40 frames still took 5.4 to 20.6 s on x86-64 and AArch64 while the busiest processor waited 0.7 to 2 s of the whole boot: the 20.6 s frame was 9.6 s of shadow and 5.6 s of surface drawing, against 1.5 s a frame on a quieter host. So the guest ran and did a tenth of the work -- more than a busy hyperthread sibling explains, which is worth its own look inside the guest -- and a wall-clock bound under TCG will keep failing on this host until frames are measured in guest work (`-icount` for this boot, or instructions counted) or the bound is dropped from the gate under load. Kept: `~/ferrix-logs/frame-budget/` on nazuna, the branch's logs among them. | open | 17 |
| FX-1201, seen once on 2026-09-26: stage 12's btrfs write check stopped the boot with "stage 12 self-check failed: a file written could not be read back" at 8.64 s, in one boot of `test-compositor --arch x86_64` under TCG (the one after the screen-lock boot), in the gate of init-l10 at 2f664b4e on main 865c62c8, with the host at a load of 30 to 80. The message is `read_pattern`'s (`kernel/src/fs/btrfs_write_check.rs:147`): `read_at` on a file the check had written, synced, unmounted and mounted again answered an error, so a read of the writable test disk (the third virtio-blk disk, a fresh `blank` fixture) failed through the block ring rather than reading back wrong bytes. It runs before init, and L10 changes no kernel code; the 33 boots of that row before it passed, the panic ended the row, and every other row of the gate passed. Where to start: what the block ring answers a read that times out or comes back short under a stalled host, and whether the btrfs read path turns a retryable ring status into EIO. Kept: `~/ferrix-logs/fx1201/2026-09-26-compositor-x86_64-2f664b4e.log` and `-serial.log` on nazuna | unowned | 12 |
| `test-compositor` on x86-64 under TCG failed once, on 2026-09-26: "the slowest frame took 5715709 us, past the 5000000 us a frame under emulation is allowed", in the window-sliding boot of the gate of feb1d6cf (virtio-gpu `ioeventfd=on`). The frame's time was software drawing -- shadow 2.0 s, surface 1.5 s, border 0.9 s, blur 0.4 s, the flip 0.4 s -- which the change does not touch, with the host at a load of 11 to 26 from other sessions' guests. Two reruns of the whole suite on the same commit passed, with slowest frames of 0.99 s and 1.59 s. Kept: `~/ferrix-logs/compositor-slowframe/2026-09-26-compositor-x86_64-feb1d6cf.log` on nazuna. **Three more on 2026-09-26, 23:10 to 23:30, after the fleet's restart, at a load of 20 to 35**: slowest frames of 7.7 s and 8.1 s in two runs of the badapple-desktop branch (a74b0b42, which touches no hyprix drawing), and 11.8 s on unmodified main e00def08 (init L10). Each was a single frame, spent mostly on shadow, blur and backdrop in software, with every functional check passing. It is a budget the host's load decides, not a regression of either. Kept: `~/.local/share/ferrix/logs/b0/test-compositor-x86_64-slow-frame-*.log` on nazuna. | open | 15 |
| `test-jobs` flaked once, on 2026-09-24: "the session at the console did not answer" after typing `echo jobs-gate: the shell reads the console`, on x86-64 in cgroup landing G4's gate (6551ea89). The transcript shows the typed line echoed only once the next line was typed, as if a console read woke late by one line; every other expectation of the run was met, and the rerun on the same commit passed. G4 does not touch the console or the terminal. Kept: `~/ferrix-logs/jobs-console/2026-09-24-jobs-6551ea89.log` on nazuna. | open | 15 |
| ferrousli's busybox on ARMv7-A fails `test-net --arch armv7a --init ferrousli`: 10 of 13 programs fail, starting with `udhcpc: poll: Invalid argument`, so the guest never gets an address; AArch64 passes all 13. Found on 2026-09-23 when the cgroup gate first ran `--init ferrousli` on all three architectures, and reproduced on main 90b0a1de without the cgroup work. ARMv7-A's `poll`/`ppoll` in ferrousli (the time64 variant?) is where to start. Kept: `~/ferrix-logs/ferrousli-arm/2026-09-23-net-armv7a-90b0a1de.log` on nazuna | open | 15 |
| FX-0001, "processor N never flushed its TLB for a shootdown", stopped `test-selfhost --plan` twice in a row on 2026-09-24 (os-8d), with eight virtual processors in a parallel build on nazuna at a load of 42 to 52 on 24 cores: once in `munmap`, once in `execve`'s `vmap::free`. A waiting lock holder gets four seconds (`TURN_TIMEOUT_NANOS`) because a preempted virtual processor can lose whole seconds; the processors a shootdown waits on got one. **Advanced 2026-09-26:** both waits now also need a count of the waiter's own polls (`smp::patience`), which a slow emulator stretches and a descheduled waiter does not spend: 1.8 s under KVM, 5.1 s under `tcg`, 32 s under the coverage plugin. A host that stops running the processor waited for while it runs the waiter still ends the wait, only later; KVM's steal time is the next thing to try if the plan stops on FX-0001 again. Logs: `~/ferrix-logs/fx0001/2026-09-24-selfhost-plan-smp8-454eaab{,-2}.log` on nazuna. 2 points **Again 2026-09-26, in the evening**, in `test-chrome-window --accel kvm --interpreter ferrousli --library ferrousli` on x86-64, with Chrome running, on branch `chrome-window-ferrousli` (main f670d5ad), at a load of about 40; the rerun passed. Kept: `~/.local/share/ferrix/logs/cwf-gate-98c9740f-on-f670d5ad/chrome-window-ferrousli.log` on nazuna. | open | 20 |
| Stage 5's scheduler self-check panicked once on 2026-09-26, x86-64 under TCG: "a task spawned onto its creator's processor waited for an unrelated interrupt" (FX-0502, `kernel/src/main.rs:1895`), at 2.95 s of test-audio's negative-control boot, gating audio-claims (9e7120e0, which touches no scheduler code), with the host at load 30-32. The same row's rerun passed. Kept: `~/ferrix-logs/fx0502-unrelated-interrupt/2026-09-26-test-audio-x86_64-9e7120e0.log` on nazuna. FX-0502's own causes name the host descheduling virtual processors; whether the check can tell that from a missed wake is the question **Again 2026-09-26**, x86-64 `test-boot` with the drcov plugin at a host load of 42, in the FX-0905 work's loaded loop, before the FX-0905 fix and on a tree whose spawn path never calls `set_state`: `~/ferrix-logs/fx0905/loads/2026-09-26-boot-drcov-load42-stage5-spawn-unrelated-interrupt.log`. **Again 2026-09-27**, a third wording: "a task pulled by balancing waited for an unrelated interrupt", at 9.06 s of `test-vfs --arch x86_64` under TCG, gating the block-ring stall fix (7fdd01ae, no scheduler code), host load about 33; the rerun passed. Kept: `~/.local/share/ferrix/logs/stall/fx0502/` on nazuna. **Again 2026-09-27, a fourth wording** ("a task woken onto its waker's processor waited for an unrelated interrupt"), at 6.14 s of x86-64 TCG `test-shell` with the host's glibc busybox, gating i386 I3 (49132b57, system-call layer only), host load 31; the rerun passed. Kept: `~/ferrix-logs/fx0502-unrelated-interrupt/2026-09-27-shell-glibc-x86_64-49132b57.log` on nazuna. | open | 5 |
| An x86-64 boot hung before stage 3 on 2026-09-26, with no panic: `test-shell --arch x86_64 --init /usr/bin/busybox` (the host's glibc busybox), third boot (`ferrix.init=/etc/k7-read ferrix.onexit=panic`, reading `/data/k7` back), under TCG, printed its `input` line ("the port receives by interrupt 32") at 2.14 s and then nothing until xtask's 120 s timeout. The next line on a good boot is `stage 3` (breakpoints, page faults, 251 ticks at 999 Hz), so the hang is in stage 3's own checks or just before them: no filesystem, no pseudoterminal and no init yet. Gating btop-fix (0778232c on main 6f5090f6, which touches `fs/pty.rs` and the compositor only) at load about 20; the rerun passed at load 36, and so did the other three `test-shell` inits and five boots of the same gate. Not FX-1201, which panics at stage 12. Kept: `~/ferrix-logs/btop-fix-early-stall/2026-09-26-test-shell-glibc-x86_64-0778232c.log` on nazuna | open | 3 |
| `test-compositor` under TCG at load 35 to 40 took input seconds late, twice on 2026-09-27 in the frame-budget branch's runs (15de1447 on main 100c7621, xtask-only): `--boot lock` said "a bind fired while the session was locked" because hyprix said `the session is unlocked` before it started `lswt close one` for the K bind -- the four-second lock, timed in the guest, ran out before the key arrived -- and `--boot animation` kept one picture for `follow`'s 30 s, the slide's frames reported only after it gave up. Both reruns passed. Where to start: judge the lock's bind by where `started /bin/lswt` falls against `the session is unlocked`, or hold the lock until xtask has pressed K; and time a slide from its first changed picture, not from the keypress. Kept: `~/ferrix-logs/frame-budget/2026-09-27-compositor-x86_64-cca78363-lock-bind.log` and `2026-09-27-animation-x86_64-15de1447-slide-late.log` on nazuna | open | 18 |

### P2 — quality and performance, on the "fast" half of the goal

| Item | Owner |
|---|---|
| Chrome drops video frames with the host only moderately loaded. `cargo xtask bench-chrome-video --gl --accel kvm` dropped 13–27% of a 720p30 video's frames at a load of 16–27, glibc and ferrousli alike, after the futex buckets (df446dc3), with the guest 14–18% busy. The compositor drew about 45 frames a second, each taking 6–15 ms, mostly its `flip` to QEMU's GPU. Where to start: whether Chrome's `BeginFrame`s follow hyprix's frame callbacks late, so its video compositor misses deadlines, and whether a flip must finish before the next frame callback goes out. Logs: `~/.local/share/ferrix/logs/yt-ab/after-glibc-lowload.log` and `after-ferrousli-1.log` on nazuna; `docs/CHROME.md` §9 | open |
| A job's processor load can still take in a task after its last leave, found reading the FX-0905 fix (2653b567): a task woken from another processor is joined to its job's load in `Task::set_state`, and if that processor is stalled mid-`join_group` while the task has already exited and left, the join lands after the leave. The quota check's spinners never block, so FX-0905's check cannot see it; the processor share would stay counted for a gone task. Separately, `quota::adjust`'s busy/idle flip can drift across processors, which W-13 (`docs/certification/IMPLEMENTATION.md`) already records. Where to start: make the join and the task's liveness one step, as the FX-0905 fix did for the leave | open |
| `run-compositor --everything` (and so `--chrome`): Chrome's window sometimes never paints, and sometimes goes blank once it is clicked and typed into, seen on 2026-09-26 on x86-64 under KVM, on main 24fb419b plus the host-layout default. Of seven boots on the btrfs root and on tmpfs, four painted the welcome page; one had no window at all after 40 s, its network service ending with "Terminating current process after 15 seconds with no connection"; one showed an empty window before any input; and every one that was clicked into and typed at (two of two, with `us` and with `de`) went blank and stayed blank, though Chrome still asked for cursor shapes and printed no error. Not the keyboard layout: both `us` and `de` did each. Chrome's binary names `/usr/share/X11/xkb`, which neither volume carries; a boot with XKB data under `XKB_CONFIG_ROOT` painted and then went blank on the click like the rest, so that is not it either. `test-chrome-window` does not click. Kept: `~/.local/share/ferrix/logs/b3-chrome-blank-2026-09-26/` on nazuna, the five serial logs and their screenshots. | open |
| Chrome on the desktop, what ferrix-a8 left on 2026-09-26: the toolbar's extensions (puzzle) bubble never appears, and hyprix logs only `window 2 is urgent`; a client that never stops being flooded still overflows the 1 MiB socket queue (971a03f4; the scratch control that put the old drag flood back queued 1,052,588 bytes and then dropped Chrome); and `test-chrome-window` has no context-menu step, so the fixes for Chrome dying after a menu's Copy and after a drag (343409d5, f4ce9a7f) have no gate | open |
| `two_clients`' `a_window_moves_through_the_frames_between_two_layouts` ("only 7 frames") and `a_plugin_adds_a_dispatcher_and_the_compositor_hands_it_over` ("the plugin's dispatcher did not swap the windows", 704220 channels, the frame before the swap) failed together on 2026-09-26 (clients/waybar) in `cargo test --workspace` of branch `waybar` rebased on the relayout (4ae00e73, a new crate hyprix does not link), at a host load of 73 while about twelve sessions rebuilt after the move; `cargo test -p hyprix --test two_clients` passed at once at the same load. Both count frames or take a screenshot after a fixed sleep, as the other load-sensitive tests of that file do. Kept: `~/.local/share/ferrix/logs/waybar/flake-load73-4ae00e73.log`. Seen again on 2026-09-27 at 01:15, alone, 709072 channels, at load 24-28 on `clients-base` (main 83bcd5e7 plus xtask's dotfiles and the caption client, which hyprix does not link); the rerun passed. Kept: `~/.local/share/ferrix/logs/clients-base/g4-check-plugin-flake.log`. | unowned, found by clients/waybar |
| hyprix stops drawing two windows' opening animation when they map within a few milliseconds of each other: it draws three or four frames and no more, so the last frame is caught half-way (both windows at an earlier size: the checkerboard's squares inverted, the gradient 4 off). Found 2026-09-26 while fixing `two_clients`' swapped-window flake: with the second client started the moment the first was drawn, `a_turned_monitor_is_tiled_tall_and_drawn_turned` failed 4 of 12 whole-binary runs and 4 of 6 alone, always 374456 channels, reporting `frames 3` or `4` where a passing run draws 30 to 52; a quarter of a second between the two maps passes every time, which is why `until_drawn` keeps it. A separate observation from the same work: with every two-client test in that file waiting on its first window and then the quarter-second, `a_plugin_adds_a_dispatcher_and_the_compositor_hands_it_over` failed 5 of 10 (709072 channels, the windows not swapped), so its chain of fixed sleeps is the next wait to replace. Logs: `~/ferrix-logs/fix-gpu-frame/` on nazuna (`turn-loop-*`, `final-loop-*`, `turned3-*.ppm`). 3 points | open |
| Chrome on ferrousli, what ferrix-c7 left on 2026-09-26 (`docs/CHROME.md` §8): `test-chrome-audio` on ferrousli, never run, which would put alsa-lib's ioctls through ferrousli (1); nothing but Chrome loads more than 64 objects, so the loader's 256 has no test of its own -- a program with seventy `DT_NEEDED`s in `ld/tests/link.rs` would give it one (1); ferrousli's `ld.so` cannot be run as a command, only reached by `PT_INTERP` (3); `getcontext`, `setcontext`, `swapcontext` and `makecontext` on AArch64 and ARMv7-A, which only x86-64 has (`cargo` imports them) (3); and why the GPU process's fallback stops at the sandbox's `proc_util.cc:115` with `ENOENT` on Ferrix (§8, item 8; unsized). the GPU is the roadmap's Chrome row (`inotify` landed 2026-09-27) | open |
| `test-vfs --arch x86_64 --init ferrousli` failed once on 2026-09-26, in command 19, the `sh -c` script that runs busybox in a cgroup with `pids.max`: after "a fork refused within pids.max" and "pids.events counted it", `rmdir` of the cgroup said "Resource busy" -- a process still counted in it -- and the script exited 9 eleven seconds later instead of printing `removed`. The host was at a load of 51. The tree was branch `rc-ferrousli` (main d1379249 plus ferrousli's loader and library changes, which a static busybox reaches only through `fork` and `exit`, unchanged); the rerun passed. The group's count outliving its last process for a moment is where to start: the script removes it as soon as `wait` returns. Kept: `~/.local/share/ferrix/logs/rcf-gate-ee843753-on-d1379249/vfs-x86-ferrousli-first.log` and `.serial.log` on nazuna | open |
| `cargo xtask check --ferrousli` failed twice in a row on 2026-09-26 in `c_mount`'s `file_system_statistics_and_the_calls_that_mount_swap_and_sync`: "mount/filesystems-O0: still running after 30s". The program calls `sync()`, which flushes every file system on the host, and nazuna had 2 GB of dirty pages from other sessions' builds: a bare `sync` there took 19.7 s at the same moment, at a load of 37, and a third run passed once the host had flushed. Not the library: the tree was branch `chrome-window-ferrousli` (main f670d5ad plus the loader and `posix_fadvise64`, which the program does not call). The test's time limit, or its host-wide `sync()`, needs to allow for a host that is flushing; `syncfs` on the scratch directory proves the same wrapper without waiting on everyone's disks. Kept: `~/.local/share/ferrix/logs/cwf-gate-98c9740f-on-f670d5ad/check-ferrousli.log` and `~/.local/share/ferrix/logs/cwf-gate-98c9740f-on-f670d5ad/rerun/check-ferrousli.log` on nazuna. **Twice more the same evening, around 23:30**, gating the futex buckets (ferrix-41b, 7c12061b, which reaches nothing of ferrousli's), at loads of 28–45; a bare `sync()` took 2.7 s just after. Kept: `~/.local/share/ferrix/logs/yt-check-c_mount-timeout-7c12061b+.log` on nazuna | open |
| hyprix advertises `wp_fifo_manager_v1` and `wp_commit_timing_manager_v1` and ignores their requests (`server/src/client/frames.rs`, `Role::Fifo \| Role::CommitTimer`): Mesa's Vulkan WSI then stops using frame callbacks and a FIFO client runs unthrottled -- vkgears drew 3600 fps against a headless hyprix. Implement the barrier or stop advertising both; also `wp_presentation` never sends `clock_id` after bind, and `now_monotonic()` (`hyprix/src/state.rs`) is the wall clock, so `presented()` carries it. Repro: `~/ferrix-logs/os-ac/hyprix-host.sh` on nazuna | open, found by os-ac |
| zinc can exec a pipeline's last stage in place again: orphans are reaped now -- init is pid 1 since L10, and hyprix reaps what it starts and, when it is pid 1, every orphan (`userland/compositor/hyprix/src/children.rs`, ferrix-e4 2026-09-26) -- so zinc's "Run a subshell's last external command in place" workaround and the fork a prompt it costs agnoster's `$(jobs -l \| wc -l)` can go. 1 point | open |
| The DK1 desktop, what 2026-09-24 left (`docs/ROADMAP.md`, the desktop at the speed of a hand): pointer motion off hyprix's frame thread, since a frame being drawn still holds the pointer (5); the first blur of a translucent window, 0.6 to 1.9 s in f32 (3); the Cortex-A7 at 800 MHz with VDDCORE raised through the STPMIC1 first (5); U-Boot's saved `bootdelay` of 2 s and OP-TEE's 1.4 s finding its device tree, which are firmware | open |
| The cost of the 20 µs one-shot armed on every wake onto the caller's processor, measured on pipe and futex paths | open |
| `EPOLLRDHUP` and `EPOLLPRI` are never reported, a written deviation of the epoll landing (d047480d): `Readiness` carries neither a half-closed peer nor urgent data | open |
| Per-CPU frame and heap caches, deferred since stage 2 | open, once a workload can measure them |
| The board's `HDMI-A-1` has no `EDID` property: `user/ltdc` reads its monitor's EDID and never hands it to the display core, so hyprix has no description there unless `drm.edid_firmware=` names a file. A displayctl message carrying the blob (protocol 7) for the core to serve beside the override, and `GETCONNECTOR`'s `mm_width`/`mm_height` from whichever EDID the connector has (`docs/DISPLAY.md` §7, "Not done"). 3 points | open, display |
| `Inode::ioctl`: `sys_ioctl` special-cases the console, sockets and `/dev/dri/card<N>` by the open object's type; a hook on the inode replaces the three branches (os-02's review of the display stack, 2026-09-16). `/dev/input/eventN` (`docs/INPUT.md` §3.3, L6) would be a fourth special case | open, kernel VFS owner |
| Checked register offsets in the ring-3 virtio drivers: `Block::read`/`write` in `native/drivers/blk` and `native/drivers/gpu` assert on a device-controlled `notify_off` × multiplier, and on ARMv7-A `offset + size_of::<T>()` can wrap past the bounds check; one shared checked-offset accessor for both (os-02's review, 2026-09-16) | open, driver owner |
| devmgr's `await_published` kills a driver that exits without publishing and marks it dead, but never quiesces its device, as it did not before that change either. The IOMMU mappings go with the process, so this is hygiene rather than safety: quiesce the device once its driver is gone. Stage 10 (os-02's review of the display stack, 2026-09-16) | open, devmgr owner |
| ASIDs and PCIDs, so a switch stops invalidating every user entry | open, after threads |
| A gate for btop: a program in `test-net` or `test-vfs` on x86_64 that runs `btop` under `timeout` on the console and requires its panels' titles in the output, with a negative control that shows the check fails when btop cannot start (the 4 MiB `execve` limit it hit is the obvious sabotage). 2 points | open |
| A gate for sshdt: a `test-net` program on x86_64 that starts `sshdt` with a key `xtask` generates, and a `--forward` through which the host's `ssh` runs a command and requires its output, with a negative control (no forward, or a key the server was not given) that shows the check fails. Needs an `ssh` client on the gate host and on CI. 3 points | open |
| `cargo xtask run`'s serial shell has nothing to start sshdt from; `run-compositor --ssh <port>` does, since 2026-09-21 | open |
| No `/etc/os-release` in the image: `cat /etc/os-release` over `ssh` says the file is not there, and it is what a client asks a machine what it is with. A line or five -- `NAME`, `ID`, `VERSION_ID`, `PRETTY_NAME` -- carried like the other `/etc` files. 1 point | open |
| `mlock` and `munlock` are `ENOSYS`; sshdt (through `russh-cryptovec`) warns once per run. A resident-only kernel can accept them as no-ops within `RLIMIT_MEMLOCK`. 1 point | open |
| uutils' `tty` on a PTY prints `/dev/pts/0` with no newline after it; busybox's `tty` prints both, and so does every other program over the same sshdt session. Found 2026-09-21 through `ssh -tt`; not yet narrowed to uutils or the terminal. 1 point | open |
| `test-vfs` on AArch64 and ARMv7-A fails command 17, the permission check, whatever the busybox: it expects uutils' `cat: /tmp/dac-private: Permission denied`, and the Arm images carry no uutils, so `cat` is busybox's, which says `cat: can't open '/tmp/dac-private': Permission denied`. Seen 2026-09-23 with Alpine's musl busybox and with ferrousli's alike; no gate runs `test-vfs` on Arm. Accept either wording, or carry uutils there. 1 point ferrousli's busybox on Arm fails the same command the same way (`~/ferrix-logs/ferrousli-arm/2026-09-23-vfs-aarch64-90b0a1de.log` on nazuna) | open |
| ferrousli's Arm suites in CI: they run by hand under qemu-user (`userland/ferrousli/README.md`), and CI runs x86-64's alone. Needs the cross gcc, QEMU's user mode and two rustup targets on the runner, about ten minutes each. 2 points | open |
| The ports built on Windows: `cargo xtask ports` refuses there, because `userland/ferrousli/tools/ports/` needs gcc and the host's UAPI headers; `build-windows.sh` beside each, as busybox has, with clang and Alpine's pinned headers. libc++ with clang is LLVM's own configuration. 5 points | open |
| `/dev/rtc`, and keeping the clock right after boot: `CLOCK_REALTIME` starts from firmware's `GetTime` and then drifts with the counter, with no NTP and no RTC driver to correct it. The random generator is seeded (firmware, `RDSEED`/`RDRAND`, `RNDR`); a virtio-rng driver would reseed it while running | open |
| The debt the roadmap names: Miri for `frame`, `heap`, `paging`; fuzz targets for `cpio`, `fdt`, `acpi`, `virtio`, `linux-abi` | open |
| Every gate's log names the tree it ran on: `xtask` prints `HEAD`, the branch and whether the tree was clean as the first line of every `check`, `test-boot`, `test-shell` and `test-vfs` log, so a row's evidence pins its commit by itself rather than by the runner's word (asked for by a review of the frame-window evidence, 2026-09-13) | open, cross-cutting |
| The host-test table in the roadmap generated from `cargo test --list` with a gate, instead of counted by hand | open |
| The POSIX measure: musl's libc-test functional and conformance programs built static against musl and against ferrousli, run under `test-shell` on all three architectures, with the pass count in the roadmap's host-test table and every failure filed with its owner | open |
| Zero-copy block reads: pin the page-cache pages themselves as the block ring's buffers, removing the data-VMO and scratch copies of stage 11's first read path (ARCHITECTURE §3) Re-costed against the seam measured, 1 (2026-09-27): the two copies are about a microsecond of a 4 KiB read's 300 us round trip under KVM, so this row saves under 1% until the hop and the depth-32 stall are cut | open |
| **Done 2026-09-27:** the block ring's depth-32 stall. It was not a missed wake-up: instrumentation showed every slow read waiting in `ferrix_block`'s queue while the ring task ran, passed over by the one-way elevator until `mq-deadline`'s 500 ms read expiry, a spinning disk's setting. Ring disks now use `Config::fast_device()`, a 25 ms read expiry. Depth-32 p99 went from 112–228 ms to 28–35 ms under KVM, with the median unchanged at 2–3 ms. A host test is the negative control: a read behind the elevator starves under the default and is bounded under the new config. Logs and the instrumentation patch are in `~/.local/share/ferrix/logs/stall/` on nazuna | ferrix-55b |
| Cut the trip to ring 3: a 4 KiB disk read through the block ring and its driver costs 300 to 844 us on x86-64 under KVM against stock Linux's 27 to 48 us, which is an estimated 17 to 56% of a cold `rustc` run (`docs/OPAQUE-KERNEL.md`, *The verdict of S0*). The depth-32 stall is fixed; next, find where the time goes (the driver's `device_ticks` splits off the device's share), PCIDs so a switch keeps the TLB, then remeasure with `cargo xtask bench-seam` and the `seam` boot line, aiming for within twice Linux's per trip. It pays off whatever is decided about the opaque kernel | open |
| Re-measure the item's coverage to take in `space.rs`'s shared-code test (F-10, 2026-09-27). `alloc_check` now maps a two-page object as shared code, as the vDSO is -- the one-page refusal, both regions' flags, the unmap -- and runs it once per allocation it makes, so `map_shared_code` (`kernel/src/user/space.rs` 2690-2744, unreached on every architecture in 978fead6's run) is exercised on all three; its one unreachable line, 2701's `NotUserRange`, is argued as 2554's is, and `find_free`'s doc now states the window bound both rest on. Landed with the evidence carried, not regenerated, and the floors unchanged: a run of the suite on each architecture under the drcov plugin, then `--json --residual` (VERIFICATION.md §3.3), and a floor raised only from what that run measures | open |
| `crypt`'s `$2*$` blowfish hash, which gives `"*"` so a blowfish entry in `/etc/shadow` matches no password. The stubs this row once listed, regex, `awk`'s math and `dirname`, were all replaced by 2026-09-16 and `src/stubs.rs` is gone | open |
| The rest of ferrousli's POSIX.1-2024 gap, by area in `docs/POSIX-2024.md`: 247 interfaces, 80 points, none written on a branch since the three branches of 2026-09-13 landed. The largest parts: complex arithmetic (66 interfaces, 8 points); every `long double` form of `math.h` (59, 8), the only part of the math library left; realtime (29, 11: `aio.h`, `mqueue.h`, timers, `shm_open`); spawning (25, 7); locales and messages (21, 13: `gettext`, `iconv`, `strfmon`); processes and the system (8, 7); users and databases (`ndbm.h`, 9, 3); the `clock` waits and `pthread_atfork` (6, 3); terminals (7, 3); and POSIX.1-2024's declarations in musl 1.2.5's headers (3). Each landing updates the document's tables | open |
| **Done 2026-09-27:** the seam measured, 1. The `seam` boot line times a 4 KiB read through the ring at depths 1 and 32, with the driver's own submit-to-drain in each completion's `device_ticks` (x86-64). `cargo xtask bench-seam` reads the same disk from a stock Linux kernel on the same QEMU machine. On x86-64 under KVM, depth 1, over four pairs: Linux 27 to 48 us, Ferrix 300 to 844 us. On AArch64 under TCG: Linux 145 us, Ferrix 453 us. The table is in stage 11's roadmap section, and the 2026-09-16 decision cites it | ferrix-55b |
| **Done 2026-09-27:** the seam measured, 2. The kernel counts syscalls, page-cache pages served against pages filled from disk, and block-ring crossings (`/proc/ferrix-seam`). `test-vfs` prints the counters, and `test-rustc` prints them cold and warm. A warm compile crossed once in 4,345 syscalls; the numbers are in stage 11's roadmap section, and the 2026-09-16 decision cites them | ferrix-55b |
| FX-1151 under WHPX at one processor: "net ring self-check failed: the kernel closed the control channel", with no program faulting, in `test-boot --arch x86_64 --init ferrousli --net --zinc --accel whpx --smp 1` on main 1a55a1d3 on the Windows PC (QEMU 11.1.0), 2026-09-16; two boots of nine at one processor, among them one of three with the workaround below in place, the others clean. Logs on the Windows PC: `~/.local/share/ferrix/logs/os-26-whpx/whpx-s1-b.log` and `whpx-fix-1.log`, beside the WHPX row's failing boot (`whpx-smp4-2.log`) and diagnostic boot (`whpx-diag-1.log`). **Where it stands, 2026-09-17 (os-26, stopped at the wind-down, about 1 of 2 points spent):** a diagnostic build printing why the kernel ends each net ring is the side ref `os-26/fx1151-diag` on nazuna (61bb2204, on a86115cb; not for main). 18 WHPX one-processor boots with it on the Windows PC did not fail, each ending its rings as expected (the check's bad HELLO refused for its handles, then the check's own close); logs on the Windows PC `~/.local/share/ferrix/logs/os-26-whpx/fx1151-1.log` to `fx1151-18.log`. Lead: both failing boots (23:47 and 23:51 on 2026-09-16) ran while the customer's own WHPX guest was running on the same PC, and none of the 18 clean ones did, so host contention looks like the trigger of a timing window in the check or the ring. Reproducing under deliberate host load on the customer's PC was refused by the session's permission check and is the customer's call; 20 KVM and 20 TCG one-processor boots of the diagnostic build on nazuna under the fleet's own load were started and stopped at the wind-down with none finished. Next: those boots, or the customer's load, and read which exit path the diagnostic prints in a failing boot **Seen again 2026-09-26 under the drcov coverage plugin**, x86-64 `test-sysfs` (`~/ferrix-logs/fqs5-flakes/sighting-netring-cov-x86-snap2.log`, line 7797), and reproduced once on branch fix-queued-small-5 in 12 `test-sysfs` boots with drcov and QEMU pinned to two cores beside two pinned busy loops (`2026-09-26-sysfs-drcov-load-netring-closed.log`), 10 ms after the `net` line, so not the 10 s HELLO patience by wall time. A diagnostic (scratch, `netring-diag-scratch.patch` beside it) printing why each ring's task ends and the check's read error then ran 9 more such boots without a recurrence. Read: the kernel's end closes with nothing queued only when `receive_hello` gives up -- a read error other than `Empty`, or its wait's deadline -- or when a refusal's or READY's write fails, since every other exit writes a message first. | open |
| `net_ring::run()` unclaims its device twice when a ring is refused or never begins: `unclaim(&start.device)` inside `if outcome.is_none()` and again after it (`kernel/src/net_ring/mod.rs`). `unclaim` removes the node from `CLAIMED` by pointer, so the second is harmless alone; but a ring created for the same device between the two, by another task on another processor, has its claim removed by the second call, and a third ring could then be created beside it for one device. Found reading FX-1151's path on 2026-09-17; not shown to cause it. Fix: one unclaim on every path, with a check that creates a ring in that window. 1 point | open |
| `test-boot --net` on Windows answers "the x86_64 kernel reported success but QEMU exited 1" after a clean boot: the xtask gateway's teardown, seen on 2026-09-16 with QEMU 11.1.0 under both WHPX and TCG. 1 point | open |
| The WHPX panic's QEMU half (root-caused 2026-09-16: QEMU 11.1's own MMIO emulator under WHPX walks the guest's page tables and answers "not mapped" for a mapping that exists, with more than one vCPU). The workaround landed: `xtask` gives the guest one processor under `whpx` unless `--smp` is given. Whether to report it upstream is the customer's (below); `git log -S whpx_handle_mmio -- docs/BACKLOG.md` finds the full analysis | open |
| The shared ring index discipline: `libs/proto/netring` and `libs/proto/blkring` keep the same private indices, checked reads of the peer's and want-bell handshake, written twice. Extract it into one crate both depend on, in a landing of its own, with both rings' tests and both fuzz targets as the evidence; it was deliberately not done in the landing that added the second copy, since a mistake there is a disk that stops reading. 3 points | open |
| uutils' procps and util-linux, which hold `sysctl`, `ps` and `top`: their only release, 0.0.1, does not compile on this toolchain, and their main branches do not compile for this target (procps' `top` wants libsystemd through pkg-config, util-linux's `blockdev` and `fsfreeze` pass `ioctl` glibc's request type). 5 points, better spent when either project releases again (`docs/UUTILS.md` §6a) | open |
| The ports built only when stale, not on every `cargo xtask ports` (5). Work is on branch `os-12/ports-autobuild` (1dd0e503: staleness per port, a build lock, the Linux path, Windows through WSL), state unreported; read its commits before trusting them | open |
| zinc-next's remaining 21 points, as sized on 2026-09-17: the builtins B1 to B4 (B1 starts with the `BIN_FG` numbering fix), the history ring and file, ZLE, completion and modules, and the swap to `zinc` | open |
| The `AF_PACKET` gaps: frames this host sends copied to `ETH_P_ALL` sockets (`PACKET_OUTGOING`), packet sockets on the loopback, and classic BPF (`SO_ATTACH_FILTER`, `SO_DETACH_FILTER`) | open |
| **Done 2026-09-26:** the Pixel 7's USB serial port, live. During a native boot Ferrix presents a CDC-ACM port (`1209:0001`, "Ferrix console") on the phone's USB-C port, and `tools/pixel7/monitor` streams the kernel log from `/dev/ttyACM*`: the boot's stages and `ferrix-statd`'s samples, live. Proven on the phone in run `usblog2`; `docs/PIXEL7-USB-HANDOVER.md` §8 has the three writing runs, the PO's standing write list and what is left. The survey, the `TREE_GS201_DWC3` binding, `libs/drivers/dwc3`, `libs/drivers/usb-device`, `native/drivers/usbdev`, the kernel log (`console/log.rs`, `syslog(2)`, `logctl`) and the monitor's watcher | ferrix-9c |
| The Pixel's USB port at SuperSpeed: `libs/drivers/dwc3` holds `DCFG` at high speed and leaves the combo PHY (`0x110F_0000`) alone. SuperSpeed means writing that PHY's window, a new block the PO has to approve | open |
| Input over the Pixel's USB port: `usbdev` reads what the host sends and drops it. A shell or tty over the port needs the console's input side to take bytes from a ring-3 driver | open |
| **Done 2026-09-27:** every driver kind with a core restarted by devmgr (T0 of the live kernel update plan, customer 2026-09-26): display, sound, network, input and disk. A net interface is parked with its addresses and a disk with its requests queued for the next driver, so a btrfs root survives its disk drivers being killed; the quarantine's pins are given back at HELLO; `cargo xtask test-restart --boot all`. Left: `Port`, `Host`, `Engine` and `Gadget`, which have no core that waits for its claim | open |
| devmgr does not restart `usbdev` (its `Gadget` kind, like `Port` and `Engine`): a driver that dies leaves the Pixel with no port until the next boot. The log core ends the claim when the channel closes, so a restart could reclaim it | open |
| The Pixel's DWC3 runs with the PHY's suspend (`SUSPHY`, `ENBLSLPM`) and USB 2 LPM off, costing the power they save; turning them on needs Linux's save and restore around endpoint commands | open |
| Read the Pixel's DWC3 release (`VER_NUMBER`, `0xC1A0`, `DWC_usb31`) in the loader's survey: it decides the soft-reset timing quirks `libs/drivers/dwc3` now covers by always waiting 50 ms more | open |
| The log core's REFUSED can go missing: once, on an aarch64 `test-boot` under a host at load ~50, `kernel/src/logctl`'s boot check sent DATA as a driver and found the channel closed with no REFUSED queued, though the core's task had ended the claim (`FERRIX-PANIC log control self-check failed: a driver that sent DATA was not refused`, tree 566ca8be). The rerun passed. The check now requires the claim to end, with or without the REFUSED, and prints `logctl   SIGHTING: ...` on every boot where the REFUSED is missing, so the boot logs count it; why it was not there is open. Log: `~/ferrix-logs/pixel7-usb-log/boot-aarch64-logctl-refused-566ca8be.log` | open |
| adb for Ferrix: `adbd` over TCP first (gated in QEMU with the host's `adb`), then over USB as a function beside the Pixel's serial port, for `adb shell`, `push`/`pull`, `reboot` and `forward`. Chosen with the owner on 2026-09-26 over fastboot (a bootloader's protocol) and SSH over USB networking. `docs/ADB.md` is the handover; the USB half needs the PO's OK for new DWC3 endpoint registers. **Done 2026-09-27:** adbd over TCP (`cargo xtask test-adb` green on x86_64, aarch64 and armv7a) and over the Pixel's USB port (run `adbusb4`: devices, shell, 1 MB push and pull byte for byte, reboot). Open: authentication, `shell,v2`, starting adbd from init (`docs/ADB.md` §6) | ferrix-9c |
| `reboot bootloader` from Ferrix on the Pixel: Android's `pixel-reboot` writes `0xfc` to the PMU's reboot word (`0x1806_0810`) through an EL3 SMC (`set_priv_reg`) and also stores the mode with `gbms_storage_write` in the battery-management chip's persistent storage, which Ferrix must never write. Without it ABL may ignore the mode. Not planned; the lap through Android works (`docs/ADB.md` §4) | open |
| **Done 2026-09-27:** the Pixel 7's GUI, option A. The launcher app (`tools/pixel7/android`) runs a Ferrix desktop in the phone's crosvm VM: the compositor on crosvm's display (a root `app_process` bridge asks virtualizationservice for it by cid and hands it to the app), a touchscreen, keyboard and mouse over sockets, `--scale 2`, and the soft keyboard reserving its height at the bottom (`addreserved` through the serial console's shell), so that the desktop shrinks rather than pans. Chromium 154 (Debian's arm64 build, `scripts/fetch/fetch-chromium-arm64.sh`) runs on it from `chromium.img` at `/data`; typing into its address bar with the keyboard open was seen on the phone on 2026-09-27, on Android CP3A.260905.009. For it: CAM PCI with INTx, crosvm's doubled `TRANSFER_TO_HOST_2D` offset told apart by its PCI subsystem, getty for `console=uart8250,...`, and four AArch64 kernel fixes (SCTLR UCT/UCI, CNTKCTL EL0VCTEN, a vDSO with `__kernel_rt_sigreturn`, and F-41: `mprotect` marking a private region copy-on-write) | ferrix-d4 |
| The Pixel 7 launcher's VM leans on two undocumented Android internals that the CP3A.260905.009 update already changed once: `waitDisplayService(int cid)` as `IVirtualizationServiceInternal` transaction 17, and crosvm's `--android-display-service cid:N`. After an Android update, a guest with no screen ("FERRIX-VM-BRIDGE failed") means these moved again; the Terminal app's inlined call in `VmTerminalAppGoogle.apk` (dexdump, `DisplayProvider`) shows the current code | open |
| Memory exhaustion without a cgroup limit ends the wrong processes the wrong way (ferrix-ea's black-box pass, 2026-09-26, `~/ferrix-logs/break-ea/`, `hog.c`): a process that faults a page in with the machine out of frames dies of `SIGSEGV`, with no `oom` line, where Linux's OOM killer picks a victim (Chrome's tab shows "Aw, Snap! 11", not V8's OOM page); meanwhile `read` returns `ENOMEM` in bystanders -- the terminal and the wallpaper client exit on it and nothing restarts the wallpaper, and Chrome's zygote logged 870 of them in 1.5 s. And tmpfs ignores `size=` (`-o size=4m` took 10 MB; `/tmp` took 2.5 GB until fork failed everywhere), with `statfs` always 0 used | open |
| Resource limits are accepted and read back but not enforced (ferrix-ea, 2026-09-26): `RLIMIT_CPU` sends no `SIGXCPU`, `RLIMIT_FSIZE` no `SIGXFSZ`/`EFBIG` (`ulimit -f 10` then a 100 KB `dd` wrote it all), `RLIMIT_AS` stops no allocation. cgroup `memory.max` and `pids.max` do work | open |
| Mounts, from userspace (ferrix-ea, 2026-09-26): `umount /data` succeeds while Chrome runs from it (Linux: `EBUSY`), and `/data` leaves the table with Chrome still running on it; a btrfs device mounted twice gives a second mount that is silently read-only (`EROFS`) while `mount` lists it `rw`; `mount -o remount,ro` of `/` and `/data` is `EINVAL`, which also breaks init's shutdown remounts ("remounting / read-only: Invalid argument"); `mount --bind` and `pivot_root` `EINVAL`, `swapon` `ENOSYS`, no loop devices, `blkid` finds nothing | open |
| Linux ABI gaps seen from userspace (ferrix-ea, 2026-09-26): `mincore` and `mlock` `ENOSYS` (Chrome logs "CountResidentBytes: Function not implemented" continuously; sshdt warns); `/proc` lacks `loadavg`, `interrupts`, `self/mountinfo`, `self/limits`, `self/stack`, `<pid>/environ`, `<pid>/ns/*`, `modules`, `sys/fs/inotify/*`, and `/proc/sys/vm/drop_caches` is `ENOTDIR`, not `ENOENT`; `readlink` of a directory's `/proc/self/fd` entry is `ENOENT`; `pmap` fails; no `/dev/kmsg`, no `/dev/rtc`; `unshare(CLONE_NEWNET)` `EINVAL`; `FS_IOC_GETFLAGS` `ENOTTY`; `/proc/stat`'s system column always 0; a zombie still reports its RSS | open |
| The desktop's own programs, from ferrix-ea's pass (2026-09-26): term leaks its Wayland descriptors (two memfds and a pipe) into the shells it starts after the first -- the kernel honours `CLOEXEC` in every form tried (`cloexec.c`), so term does not set it; the tiling layout gives 0x0 tiles after about eight splits of the newest tile (20 terms, 8 at 0x0, `u3.png`), which xdg-shell reads as "choose your own size"; `hyprctl workspaces` lists only the active workspace; zinc's `ulimit` does nothing, `exec 10</dev/null` runs `10`, `kill -SEGV` is unknown, `trap … 40` misses real-time signals, and errors print Rust's `io::Error` text. And once the whole guest wedged with no panic during `timeout 1 sleep 99999999999` after signal tests, not reproduced (`hang4-sleep-huge.log`) | open |
| btrfs, from ferrix-ea's second pass (2026-09-26, `~/ferrix-logs/break-ea/` `btrfs.sh`, `bis.sh`, `seq.sh`, on the writable test disk): no single file grows past 32 MiB -- `dd bs=1M count=33` stops at 33550336 bytes with an I/O error and the file reads back 33554432; and eight concurrent writers (`dd … bs=64k count=100 &` ×8) each report success, leave no file, and from then on every create, truncate, link or mkfifo on that mount is `EIO` until a remount, with nothing in the serial log. A write-protected block device mounts and lists as `rw`, then every write is `EROFS` (Linux mounts it read-only). (`umount` losing what was written since the last commit was fixed on 2026-09-27.) | open |
| Processes and signals, from ferrix-ea's second pass (2026-09-26, `dig.c`, `spawn.c`, `posix2.c`; the host as the reference): `vfork` does not share the parent's memory, so glibc's `posix_spawn` of a missing program reports success and the child exits 127 where Linux returns `ENOENT` (Rust's `Command` on glibc spawns through it); `rt_sigqueueinfo`/`sigqueue` and `timer_create` `ENOSYS`; `RLIMIT_NPROC` not enforced (201 forks after `setrlimit(NPROC, 20)` and `setuid`); `PR_SET_NAME` not shown in `/proc/self/comm`; `getrusage` reports zero user time and `ru_maxrss`; `F_GETPIPE_SZ`/`F_SETPIPE_SZ` `EINVAL`; `sendto` an abstract `AF_UNIX` datagram address `EOPNOTSUPP`; `setitimer` at 10 ms and `sched_setaffinity`+`sched_getcpu` each failed once. (Real-time futex deadlines never expiring, and zinc taking `sh -c --`'s `--` for the command, were fixed on 2026-09-27.) | open |
| Terminals and sessions, from ferrix-ea's second pass (2026-09-26, `ptyt.c`, `ctty.c`): closing a pty master sends no `SIGHUP` to the session it controls; sshdt never closes a dropped session's master, so with the first every lost interactive ssh session leaks a shell; the input side of a pty has no bound (4 MiB taken with no newline, Linux stops near 4 KiB with `EAGAIN`); `/proc/<pid>/stat`'s `tty_nr` is always 0; zinc's `$?` is 0 after `^C` kills the foreground job (130 on Linux); and zinc ignores `errexit` in every form (`-e`, `set -e`, `setopt errexit`) | open |
| The rest of ferrix-ea's second pass (2026-09-26): busybox `poweroff` does nothing (it signals pid 1 with `SIGUSR2`, which init does not take; only `kill -TERM 1` powers off), and at shutdown init remounts only `/` and `/data` read-only, both `EINVAL`; an ELF whose `e_type` is `ET_REL` runs (Linux: `ENOEXEC`) -- 89 other malformed ELF and `#!` files were all refused; sshdt moves bulk data at about 400 KB/s; Chrome's Ctrl+O opens no file chooser | open |
| What btop showed on the desktop after its fixes (3e0a94b8, 2026-09-27) that is still wrong, none of which it depends on: `/proc/self/mounts` lists the initramfs's mounts from before the switch to the btrfs root (`tmpfs /`, `/dev`, `/proc`, `/tmp`, `/sys` twice) beside the new ones, where Linux shows only mounts reachable from the reader's root, so `mount` and `df` name `tmpfs` for `/`; `statfs` answers `f_type` 0 for btrfs and tmpfs alike (`stat -f` prints `UNKNOWN`), where Linux gives `BTRFS_SUPER_MAGIC` and `TMPFS_MAGIC`; and the terminal's Hack face has no U+2074, so btop's panel number `⁴proc` is the replacement box. Seen in `run-compositor --release --no-gl --ssh` on x86-64 | open |

### The desktop and the GPU, what is left

| Item | Stage |
|---|---|
| LEDs on virtio-input: its status queue takes STATUS and drops it, so QEMU's keyboard stays dark; `write` of types other than `EV_LED` and `EV_SYN`, which Linux injects and Ferrix answers `EINVAL`; the LED events passed to the node's readers. Done for USB keyboards on 2026-09-23 (`docs/INPUT.md` §3.3, §7.4) | 17 |
| Multi-touch axes (`ABS_MT_*`), force feedback (`EV_FF`, `EVIOCSFF`) and sound (`EV_SND`) on `/dev/input/eventN`. The input core publishes a device without them and its boot line says what was left out; QEMU's keyboard and tablet declare none (`docs/INPUT.md` §3.2, §6, os-f6 2026-09-16) | 17 |
| Input hotplug: devices exist from boot in the input iteration. A device that arrives or leaves later, and how a compositor learns of it without udev (`inotify` on `/dev/input`, in the kernel since 2026-09-27 but not for devfs's own nodes, which it makes without a call and so without `IN_CREATE`; or a rescan) (`docs/INPUT.md` §3.4, §6, os-f6 2026-09-16) | 17 |
| `card0` is opened by one process at a time, standing in for DRM master (`docs/DISPLAY.md` §5, a written deviation from Linux): Linux's many opens with one master, `SET_MASTER`/`DROP_MASTER` arbitrating between them, and the render node beside it come in stage 19 | 19 |
| The GPU for clients: A4 `zwp_linux_dmabuf` and a GBM-shaped allocator, and Mesa's virgl on ferrousli (`docs/GPU.md` §3, 3a), for clients that render on the GPU themselves, 8 points and 40 or more; zero-copy presentation for Venus through the same dmabuf (§3 step 4); and the Khronos Vulkan loader, which waits on `dlopen` of a library with thread-local storage | 19 |
| Gears on the DK1's GC400 (`docs/GPU.md` §6.2, §6.3): G1 and G2 were done on the board on 2026-09-24. Left: G3 a clear resolved to the LTDC's buffer (8), G4 draws with host-compiled shaders (8), G5 gears on the board (5) | 19, P3 |
| The DK1's LTDC display and USB HID, done 2026-09-23 (`docs/DISPLAY.md` §6, `docs/INPUT.md` §7). Left: other modes than 720p60, display hotplug, and HID's absolute axes and vendor reports (§7.5) | 17, P3 |
| The DK1's USB host after U-Boot's `ums`: ending mass-storage mode switches the PMIC's `vdd_usb` (STPMIC1 LDO4, on I2C4) off, and the kernel never turns it on, so the USB PHY is unpowered and nothing enumerates; U-Boot's `regulator dev vdd_usb; regulator enable` before `bootefi` is the workaround (`docs/stm32mp157-dk.md`, 2026-09-23). The kernel's USB preparation should turn LDO4 on itself -- a write to the PMIC every rail of the board hangs off, so with the care the RCC gets | 17, P3 |
| The desktop clients' foundation (`docs/DESKTOP-CLIENTS.md` §2): `userland/compositor/toolkit`, `userland/compositor/text`, `userland/compositor/hyprlang`, `userland/compositor/image`, and `run-compositor --config` carrying the user's dotfiles and the fonts they name. Owner: clients-base. Estimated 21 points | 19 |
| waybar in Rust for the user's own `~/.config/waybar` (`userland/compositor/waybar`, the desktop clients' design). **The part that needs no screen landed 2026-09-26:** the config (JSONC, search, `include`, `output`), libfmt formats, a GTK3 stylesheet and its cascade, GTK's box model and layout, the painter, the modules' logic, and `waybar-probe` over the real files (no parse error; `#tray menu` matches no node). Left: drawing on `userland/compositor/toolkit`, `text` and `image` once they are on main; tooltips, which need hyprix to accept an `xdg_popup` made through `zwlr_layer_surface_v1.get_popup` (refused today); the boot gate; a PulseAudio-protocol client. On Ferrix the user's `output` matches only with the EDID override, and their three Python scripts are not there, so the desktop chips and the clock are hidden, as upstream hides them. Owner: clients/waybar. Estimated 34 points, 13 spent | 19 |
| hypridle (`docs/DESKTOP-CLIENTS.md` §6): `userland/compositor/hypridle`, which builds `/bin/hypridle` and `/bin/loginctl` over the foundation's hyprlang and toolkit, and the `idle` and `idle-user` boots. Green on x86_64 and aarch64 on 2026-09-26. Left for Ferrix: a suspend for the sleep hooks, and D-Bus inhibitors. Owner: clients-hypridle. Estimated 5 points | 19 |

### P3 — hardware variants and later stages

* GICv3 and its redistributors, with a second AArch64 boot configuration
  (`gic-version=3`); x2APIC; TSC-deadline. Real AArch64 hardware is GICv3.
* Stage 13's namespaces and seccomp (its cgroups are in), stage 14
  (real-time domains), and global page-cache reclaim, which `rustc` on a
  small machine needs and no stage names today.
* `vfork` sharing memory rather than copying it.
* Stage 21, bare metal with an NVIDIA card driven by Ferrix itself: Path B
  of the GPU decision of 2026-09-18, `docs/GPU.md` §4. Opened when the
  customer wants Ferrix on real hardware; unsized, over 100 points.
* Stage 22, Steam (decided 2026-09-18): the 32-bit x86 ABI, being built
  (ferrix-41, `docs/I386.md`), glibc's place taken by ferrousli under the
  Steam runtime (13 priced, the rest unsized), bubblewrap's needs on top of
  stage 13 (13), XWayland (40 as a first guess), sound (current work,
  `docs/AUDIO.md`), and Vulkan through Venus on a KVM host (landed for
  vkgears). Over 300 points; the roadmap's stage 22 is the list.
* Bad Apple!! and Doom, beside stage 22 (games with sound; the customer's
  order of 2026-09-26, ferrix-b0, `docs/MEDIA.md`). Bad Apple!! with sound
  and `test-badapple`: not estimated before it started, ≈ 8 spent, landed.
  Bad Apple!! on the `--everything` desktop, as a window, with the
  toolkit's `Client::toplevel` it needed: estimated 8, ≈ 8 spent.
  **Doom in Rust is in the backlog** (the customer, 2026-09-26: stop after
  Bad Apple): D1 to D4, estimated 21, not started and unowned.
  `docs/MEDIA.md` §3 has the plan and what reading room4doom found.
* Huge pages; frame share and release are order 0 by design.
* A panic report as a QR code: a port of Linux's `drm_panic_qr` as
  `libs/kernel/qr` (ferrix-qr), so a panic screen can carry the whole report. WIP
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

### Update, 2026-09-26

Counted from the 2026-09-24 count (88cd2740) to 02afa6c8, 356 commits, by
the same rule: a landing counts at the estimate written before its work
started, and what had none is sized afterwards from `git log`, in
clusters, on the same scale, and said to be so.

| day | estimated before | sized afterwards | what |
|---|---|---|---|
| 2026-09-24, after the count | 0 | ≈ 27 | the DK1's desktop at the speed of a hand, its cursor plane, its HDMI modes, the console sending by interrupt, zinc's start |
| 2026-09-25 | 0 | ≈ 26 | the certification audit: the item's boundary, coverage, the Security Target and the rest of its set, SMEP, SMAP and PAN, the trap return and `StatLayout` inverted; one or two sessions |
| 2026-09-26 | 103 | ≈ 175 | estimated: init L4 to L9 45, audio L1 to L7 24, cgroups P1, M1 and S1 28, `SCM_CREDENTIALS` 2, the vDSO ≈ 4; afterwards: the certification's F-23, F-31 with KASLR, F-34, F-36, F-37, the item's split and its coverage suite ≈ 70, the Pixel 7 ≈ 39, Chrome on ferrousli, its speed and its desktop ≈ 33, and the terminal, fuzzel and Linux-compat fixes ≈ 33; about twelve sessions |

So **≈ 26 on 2026-09-25 and ≈ 278 on 2026-09-26**, 103 of the latter
estimated before it started. S1 counts its 13 although it was built
differently from its plan, and the vDSO's 4 is its share of a joint
estimate; both are soft. The running total is **≈ 1,200 points in 13
calendar days, ≈ 92 a day**, or ≈ 973 (≈ 75 a day) counting after
2026-09-24 only what had an estimate. What was estimated landed at ≈ 67 a
day over 2026-09-24 to -26, and that is the rate the roadmap's sized scope,
≈ 442 points, burns at: the unsized work beside it takes nothing off it.
The roadmap's *Burndown* lists that scope.

---

## Decisions

Dated, newest first. A decision here is final until the customer says
otherwise; one a later decision replaced is deleted, and the history keeps it.

* **2026-09-27 (customer)** The opaque-kernel plan (`docs/OPAQUE-KERNEL.md`,
  option B: the net stack, btrfs and the device cores as supervised servers
  behind the page cache) is measured before it is decided. S0 is the two
  "seam measured" rows in P2, then S1, the in-kernel refactors that pay off
  either way. The decision of 2026-09-16 stands until S0's numbers are read,
  and nothing past S1 starts before that. Owner: ferrix-55b. **Later the
  same day, S0 measured, the customer shelved the plan: off the table for
  now.** S1 is not started, and the 2026-09-16 decision stands. A trip to
  ring 3 costs 10 to 20 times Linux's in QEMU, which is what would have to
  change first (`docs/OPAQUE-KERNEL.md`, *The verdict of S0*).
* **2026-09-27 (customer, ferrix-55)** `kernel/src` is grouped by what each
  file is (`docs/LAYOUT.md`): `arch/<isa>/`, `arch/arm_common/`,
  `platform/<vendor>/<soc>/`. Every file kept its ring in the move.
  `#[cfg(target_arch)]` stays under `arch/` alone -- the certification
  consultant declined an exception for driver selector files -- so
  architecture-bound drivers stay under `arch/`.
* **2026-09-26 (customer)** **Authentication is a ring-3 service,
  `authd`, as `docs/AUTH.md` proposes, with all eleven of its decisions as
  recommended.** Among them: Argon2id written in the tree, root locked and
  `wheel` members becoming root with their own password, a throttle and no
  permanent lockout, programs that read `/etc/shadow` refused and PAM
  programs given a shim later, and a lock screen that will not lock an
  account with no password. Phase 1 locks the desktop with root's
  password until phase 2 moves the desktop off root.

* **2026-09-26 (customer)** **Doom is room4doom, fetched at build time,**
  never committed. It is labelled MIT but calls itself a transliteration of
  id's GPL-2.0 C source, so Ferrix treats it as GPL: cargo fetches it as a
  git dependency pinned to one revision, the way Chrome is fetched, and the
  repository holds only Ferrix's MIT backend for it (`docs/MEDIA.md` §1).
* **2026-09-26 (customer)** The fleet has a coordinator, ferrix-2c, which
  keeps the landing order under the landing lock (standing rules above); the
  customer stays product owner for scope. No new scope starts without the
  customer.
* **2026-09-26 (customer)** Audio moved forward to current work:
  `/dev/snd` over a ring-3 virtio-snd driver, then a sound server
  (`docs/AUDIO.md`).
* **2026-09-26 (customer)** The desktop's clients are Rust programs that
  read the real dotfiles unchanged: waybar, fuzzel, hyprlock and hypridle
  over shared clients-base crates (`docs/DESKTOP-CLIENTS.md`).
* **2026-09-26 (customer)** The 32-bit x86 ABI is Steam's next step
  (`docs/I386.md`).
* **2026-09-26 (customer)** The kernel takes an EDID override,
  `drm.edid_firmware=`, so `run-compositor`'s screen can be the host's
  monitor (55aca9d1).
* **2026-09-26 (customer)** Certification engineering: F-23 at every site,
  F-31 behind an on/off switch, F-35's quotas built into Jobs, and coverage at
  100 % or justified line by line (`docs/certification/`).
* **2026-09-26 (customer)** The repository is laid out again
  (`docs/LAYOUT.md`), in one short freeze.
* **2026-09-26 (customer)** A USB CDC-ACM device driver for the Pixel 7
  (`docs/PIXEL7-USB-HANDOVER.md`).

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
* **2026-09-17 (customer, delegated to the GUI session) The compositor
  server is written from scratch, not on Smithay.** The customer asked for
  the work to go on without stopping for questions, which settles the open
  choice of 2026-09-13 below. Smithay's value is its backends -- udev,
  libinput, libseat, GBM, EGL and its DRM session handling -- and every one
  of those is C, which `userland/compositor/README.md`'s no-C-device-stack rule
  already forbids and which this tree has already replaced: `blank` drives
  `/dev/dri/card0` itself, `libs/drivers/virtio-input` is the input driver, and
  `userland/compositor/render` is the renderer Smithay's own pixman would have been.
  What would be left of Smithay is `wayland-server`'s marshalling and its
  protocol handlers, and taking those means every Ferrix system call they
  make is a dependency's choice rather than this tree's -- on a kernel whose
  Linux surface is still being filled in, that turns a compositor bug into a
  hunt through someone else's crate. Writing the wire protocol is about 20
  points more before the first client, and buys a `no_std`-shaped,
  host-tested, fuzzed crate in the same shape as `libs/network/netwire`, `libs/fs/cpio`
  and `libs/proto/inputctl`. The protocol XML this is written from is on this
  machine (`/usr/share/wayland/wayland.xml`,
  `/usr/share/wayland-protocols/`), and so is Hyprland 0.56.2's own source,
  the behaviour reference. `xkbcommon` stays the one C library allowed at
  stage 18 and is unaffected; the stage 19 GPU choice was settled on 2026-09-18.
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
  the per-platform isolation table in P1. The second row was measured on
  2026-09-27: a warm `rustc` compile crossed to ring 3 once in 4,345 system
  calls, and a cold one about once per three, while the page cache filled
  (stage 11's roadmap section). The first row was measured the same day: a
  crossing, one 4 KiB read, costs ten to twenty times stock Linux's on
  x86-64 under KVM (300 to 844 us against 27 to 48) and three times on
  AArch64 under TCG, paid on cold reads only. Drivers stay in ring 3 and no
  kernel disk path is built for the measurement (2026-09-13 below).

* **2026-09-15 (customer)** The customer holds the product owner seat:
  there is no product-owner session. A session lands when the gate its
  change's row names has passed on the commit being landed, and commits to
  `main` directly rather than through `develop`, which is retired. What does
  not change: the gate table, judging a gate by its output, and that a
  landing carries its own documentation.

* **2026-09-14** The busybox built against ferrousli is the primary busybox:
  the userland Ferrix is measured with, first in every `test-shell` and
  `test-vfs` the gates run. The musl and glibc busyboxes stay required as
  compatibility checks. A `userland/ferrousli/` landing rebuilds it and runs both with
  it. This carries out the customer's 2026-09-13 order below once the binary
  passed both, at 5e9b0b6 with no stub reached.

* **2026-09-13 (customer)** The goal after `rustc` is a Hyprland-shaped
  Wayland compositor, written in Rust, running on Ferrix. Roadmap stages 17
  (display and input), 18 (the compositor) and 19 (Hyprland fidelity and the
  GPU) carry it; self-hosting moves to stage 20. Pulled onto the path by it:
  `AF_UNIX` with `SCM_RIGHTS` (from networking), `memfd_create` with sealing
  and `MAP_SHARED` file mappings (stage 8), and the `epoll`, `eventfd`,
  `timerfd` and `signalfd` families. `xkbcommon` is the one C library
  allowed at stage 18; the other two choices named that day were settled on
  2026-09-17 (from scratch, not Smithay) and 2026-09-18 (the GPU's Path A).
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
* **2026-09-13 (customer)** Order of everything: first a working `main`, second
  ferrousli on the build with busybox rebuilt against it, third being surer
  that `main` is stable before it moves. Ferrousli is on the roadmap, and
  its busybox is a `test-shell` target.

* **2026-09-13** Stage 10's exit criterion requires the out-of-domain DMA fault
  on x86-64 and AArch64 only; ARMv7-A's virtio-pci runs in degraded trusted
  mode because U-Boot forces the SMMU bypass. A ring-3 driver reading sectors
  through an untranslated domain may land so stage 11 can proceed, but stage
  10 is not done until domains translate and the fault is shown.
* **2026-09-13** Stage 11's read stage refuses a volume with a log tree, with a
  clear message; log replay is stage 12's. The default subvolume, data
  checksums and a node cache are required before stage 11 is done.
* **2026-09-13** The page-cache interface, agreed between stages 8 and 11: a
  `PageSource` in `libs/fs/vfs` with `fill_range(first, pages)` filling at least
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
  the names. Each has a draft answer the design assumes meanwhile; L11 to
  L13 wait on them.
* The sound server: U1 (alsa-lib) and U2 (the Pulse server), `docs/AUDIO.md`.
* The Pixel 7's GUI: options A to D from ferrix-d4's survey.
* Whether deleting busybox (S8, `docs/UUTILS.md` §8.3) is wanted at all.
* The WHPX panic: whether to report QEMU's MMIO emulator upstream, and
  permission to load the Windows machine to reproduce FX-1151.
* The Pixel 7 launcher helper on nazuna (`tools/pixel7/helper.py`, port
  47707) was started before the relayout and still holds
  `bootloaders/pixel7/mkbootimg.py`, which is now `boot/pixel7/`: a boot it
  builds fails until it is restarted. Restarting it was refused to an agent,
  as interfering with a running workload; it needs the customer's hand.
* Pushes to `origin`: local `main` is about a dozen commits ahead; a push
  needs the customer's word, given in the session that pushes.

---

## Branches that still hold unlanded work

The wind-downs of 2026-09-13, -14 and -17 surveyed every branch; what is
left of them, each with its row above: `os-12/ports-autobuild` (the ports
built when stale) and
`worktree-agent-a33c10946b6721065` (the panic QR code). Those wind-down
records are in this file's history.
