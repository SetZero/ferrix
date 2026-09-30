# Test and gate run time

The customer's priority one from 2026-09-27: *"we seriously need to bring down
test run time."* This is phase 1's record -- where a landing gate's time goes --
and the cuts it points to. Cut since: the Arm firmware waits (item 4),
the waits on finished guests (item 3, "Cut 2") and the architectures one
after another (item 2, "Cut 3"). Owner: os-98 (was ferrix-90).

Targets to argue with (the coordinator's): a standard item gate within 5 min
on a quiet host, each desktop boot within 60 s, CI green within an hour. The
rule: no gate checks less. Where a switch from TCG to KVM loses what TCG
catches, a TCG variant stays in CI.

## Measured, 2026-09-27

On main 11532464, one step at a time in a worktree of its own
(`~/ferrix-logs/gate-time/`: `measure.sh`, `summary.txt`, and a log per step
whose lines `stamp.py` stamped with seconds since the step began). The host
was **not** quiet: the load average at each step's start is given, and the
fleet's other gates ran beside it. So the figures are upper bounds. Their
proportions are what they are for.

**A: a standard item gate after a one-line kernel change** (`touch
src/kernel/src/main.rs`):

| Step | Wall | Load | Where it goes |
|---|---:|---:|---|
| `cargo xtask check` | 193 s | 12 | Clippy, of which 12 cross-target passes (6 whole-kernel), all sequential |
| `build --arch all --release` | 293 s | 14 | Three release kernels, one after another |
| `test-boot --arch x86_64` (TCG) | 37 s | 22 | ~20 s building the debug image, then the guest |
| `test-boot --arch aarch64` | 94 s | 21 | 74 s building, 20 s the guest, 5.3 s of it EDK2's wait |
| `test-boot --arch armv7a` | 58 s | 31 | 40 s building, 17 s the guest, 2.0 s of it U-Boot's countdown |
| `test-boot --arch armv7a --smp 2` | 24 s | 29 | Built already |
| `test-init --arch all` | 129 s | 26 | Three arches, one after another |
| **Total** | **828 s** | | about 14 min |

**B: the same, nothing changed** -- the floor each step pays:

| Step | Wall | Load |
|---|---:|---:|
| `test-boot --arch x86_64`, TCG | 20.5 s | 16 |
| `test-boot --arch x86_64 --accel kvm` | 16.4 s | 16 |
| `test-boot --arch aarch64` | 24.7 s | 17 |
| `test-boot --arch armv7a` | 19.3 s | 16 |
| `build --arch all --release` | 12.5 s | 14 |
| `cargo xtask check` | 154 s | 16 |

A warm x86-64 boot is 4 s faster on KVM than on TCG: the guest is not most of
a boot. `check` with nothing to rebuild still takes two and a half minutes.

**C: gates past the item gate:**

| Step | Wall | Load |
|---|---:|---:|
| `test-shell --arch x86_64` | 31 s | 12 |
| `test-boot --arch x86_64` after it | 12 s | 10 |
| `test-restart --arch x86_64` | 22 s | 9 |
| `test-compositor --arch x86_64` | **> 1,107 s**, stopped for the wind-down after 33 of its boots | 9-15 |
| `cargo xtask coverage --arch x86_64` (TCG, drcov) | about 22 min, 13:59 to 14:21 | 3-24 |

(`test-net` was not measured: it needs `--init`, which the script left out.)

A compositor boot averaged **31.9 s**: 12.6 s from the guest's first serial
line to its last, and **17.9 s** from that last line to the next boot's image.
That gap is the 5 s power-off grace, a 2 s trailing read, the stop and the
next build's cargo invocations. So 56% of test-compositor is not the guest.

## Measured, 2026-09-28: switching the built-in init

On main a0772269, in a worktree and target dir of their own, at load 1.5 to
9 (`~/ferrix-logs/gate-time/tt-cut1/`: `before-summary.txt`,
`switch-builds.txt` and a log per step). The kernel step is cargo's own
"Finished in" for `ferrix-kernel`:

| Build | Kernel step | Load |
|---|---:|---:|
| debug, cold (the crate and every dependency), each arch | 16-18 s | 2-4 |
| debug, init switched, x86_64 (inside test-boot, test-shell, test-vfs) | 1.5-3.3 s | 1.5-5.5 |
| debug, init switched, aarch64 / armv7a (`build`, with and without `--init`) | 1.5-1.9 s / 1.6-2.3 s | 4-9 |
| release, cold, x86_64 | 72 s | 7.5 |
| release, init switched, x86_64 | 69-71 s | 4.5-6 |

A flavour's debug kernel target is 1.2-1.4 GB per arch, 730 MB of it the
kernel's incremental cache.

x86_64, one after another, on a target dir every flavour had been built in:

| Step | Wall | Load |
|---|---:|---:|
| `test-boot` | 12.3 s | 2.0 |
| `test-shell --init` (Alpine's musl busybox) | 45.1 s | 2.0 |
| `test-boot` | 12.4 s | 1.5 |
| `test-vfs --init` (the same) | 15.6 s | 3.3 |

So the gates do not pay what cost 1 below supposed. The image row's gate
(`check`, `build --release`, the four test-boots, test-init) builds only the
kernel with no init and never switches; in the stage 7 and 8 row each
test-shell program and test-vfs is a flavour's first use, which a target
dir per flavour would make a cold build (16-18 s, not 2 s). Keying the dir
by the init's digest had problems of its own: test-net's command list holds
the ports of the host's stub servers, so its digest changes every run and
each run would make a new dir that is never reused; a rebuilt busybox would
start cold; and a digest in `--target-dir` enters `builds.rs`'s build keys,
so test-selfhost's replay would miss whenever the init is an earlier build's
output, whose bytes Ferrix makes differently. The two variants left, dirs
per flavour for `--release` only and the init carried in the image rather
than built into the kernel (a `src/kernel/` change, for the certification
consultant), are sound but not worth it today: neither takes time off a
gate as the gates are.

test-shell's 45 s is not the kernel build (1.5 s). With `--init` it boots
four times: the built-in script, the same shell started by `ferrix.init=`,
`poweroff -f -n` after writing `/data/k7`, and reading it back under
`ferrix.onexit=panic`. Each guest runs about 9.2 s, and the last one's panic
is followed by the 5 s power-off grace before QEMU is stopped (cost 3). The
31 s in C above was test-shell with zinc, which boots twice.

## Where the time goes: the five likely costs

From a read of `tools/common/xtask/src` and `src/kernel/src` (file and line in the survey,
kept in the owner's handover), checked against the timings above:

1. **The kernel crate rebuilds whenever a gate embeds a different init.**
   `FERRIX_INIT`, `FERRIX_INIT_SCRIPT`, `FERRIX_INIT_COMMANDS` and their
   digests are `rerun-if-env-changed` (`src/kernel/build.rs`), and every flavour
   builds into one target dir. **Measured and dropped** (above, *Measured,
   2026-09-28*): in the debug profile, which every test gate boots, the
   rebuild is incremental and costs about 2 s; only a release build pays the whole
   kernel crate again, about 70 s, and no gate switches flavours in release.
   A target dir per flavour would turn each flavour's first build into a
   cold one and win back 2 s only when the same worktree used it again.
   Cut 1 was dropped by the product owner on 2026-09-28.
2. **Every `--arch all` loop is sequential**: `build`, `test-boot`,
   `test-init`, `test-compositor`, `test-audio` and `coverage`. The host has
   24 threads and a boot uses 4. **Cut:** run the three arches' boots at
   once, with output buffered per arch and failures reported per arch.
3. **The power-off grace, 5 s, on every boot whose guest is still running
   when its check returns** (`qemu.rs` `POWER_OFF_GRACE`): every compositor,
   restart, sysfs, audio, display and chrome boot, about 30 per arch in
   test-compositor alone. Add 2 s trailing reads in about 22 compositor
   boots, and settle sleeps of 1.5 to 9 s in jobs, init, auth, sysfs and
   restart. **Cut:** a guest the check is done with is stopped, not waited
   for, and fixed settles become waits on the line they wait for. **Cut**
   by the PO session on 2026-09-28, for the gates below ("Cut 2").
4. **Firmware waits on every Arm boot**: EDK2 5.3 s on aarch64, U-Boot's 2 s
   autoboot on armv7a. **Cut** by ferrix-79 (was ferrix-e4) on 2026-09-27:
   to 0.37 s and 0.18 s (the BACKLOG's done row).
5. **TCG by default everywhere** (`qemu::accelerator`), for reproducibility
   and because CI has no hypervisor. It costs less than expected on a warm
   test-boot (4 s of 20), but more in long guests: chrome's timeouts are
   written for emulation. **Cut:** KVM by default on x86-64 where the host
   has it and nothing checked needs TCG, with CI's TCG boots kept.

Also found, smaller or later:
- **`check` is sequential**: 18 Python gates, five workspaces with target
  dirs of their own, and 12 cross clippies that could be one cargo call with
  several `--target`s.
- **CI**: no job has `timeout-minutes`. Miri is one job of 16 crates one
  after another. The fuzz job spends 30 s on each of 37 targets.
- **The guest's own fixed waits are small**: the boot self-checks sleep
  about 1.5 s in all. The ~6 s of checks on an Arm boot is emulated work.
  test-compositor's boots wait for `hyprix:`, not `FERRIX-BOOT-OK`, so they
  could boot with `ferrix.checks=skip`: the checks are proved by test-boot.

## Cut 2: a finished guest stopped, not waited for (2026-09-28)

xtask only; owner the PO session. Each wait was read for what it protects
(`git log -S` on it) before it was cut; one that protects a check stayed.

- **The power-off grace.** `qemu::finish` gave every guest 5 s to power
  itself off once its hook returned, then asked QEMU to stop. A guest that
  powers off -- `test-boot`, `test-shell`, a hook that types `poweroff` and
  waits for `reboot: Power down` -- is gone in well under a second, so the
  grace cost those nothing; a guest that never powers off paid all of it,
  and every one of `test-compositor`'s 33 boots was ended by SIGTERM after
  the full 5 s. Nothing looked at those 5 s: the transcript ends when the
  hook returns, and `Ended::powered_off` is read only by
  `watch_to_power_off`, whose hook waits for the port to close. A hook now
  says `Watching::stop_when_done()` and QEMU is asked to stop at once --
  still SIGTERM, so a coverage run's drcov table is written as before. Said
  by test-compositor's boots (idle's too), test-restart, test-sysfs and
  test-jobs. Hooks whose guest powers off keep the grace: test-init,
  test-auth, the K7 boots, and test-audio's, whose WAV QEMU's backend is
  still writing.
- **The compositor's trailing 2 s read**, `read_more(|_| false)`, there for
  "whatever else the guest said by now" (1856fd08): now
  `Watching::read_what_was_said`, which reads until the guest has been
  quiet for 0.5 s, 2 s at most. The full 2 s stays where the frame reports
  it lets in are judged, since they come a second or more apart: the
  pointer boot's sweep count, the slide's frame bound, and the cursor
  boot's two reads, which bracket a frame count. The driver-restart boot's
  3 s is now a wait for devmgr's second `was started again and published`,
  which races the compositor's `the card is back` and is what
  `judge_restart` counts, then the same quiet read.
- **30 s waits for answers nothing printed.** Seven boots -- bar, submap,
  taskbar, screenshot, lock, typing and fuzzel -- press SUPER C and SUPER W
  after their pictures
  and wait out `SETTLE`, 30 s, for `hyprctl clients` to answer. Their
  configurations do not bind those keys, and none of them judges the
  answer. `ask_the_sockets` now runs only where the configuration binds
  both to `hyprctl` (a unit test holds the four that do).
- **Settles before the first keystroke**: test-restart and test-sysfs 3 s,
  test-jobs, test-auth and test-init's two boots 1.5 s. Now
  `Watching::wait_for_shell`, which types `echo xtask-shell-$((6 * 7))`
  until a line says `xtask-shell-42`, again every 2 s. A prompt ends no
  line, so the answer is the first thing that shows the shell reads; typed
  early, the line waits in the terminal. Under TCG it came 1.6 s after the
  marker. The 3 s were also for "the drivers to have published": restart
  and sysfs now wait for the kernel's `published` lines they need, which
  come before the marker.
- **test-jobs**: the settle after each `kill %1` is gone; the `jobs` after
  it is typed again every 0.5 s until it says `terminated`. The three before
  a pipeline's kill, Ctrl-Z and Ctrl-C stay: they wait for a job to be
  exec'd and hold the terminal, which nothing on the console says, and the
  shell that could be asked is waiting on that job.
- **test-init**: lazy.service's 1.5 s is now `svc status` asked until its
  `STATUS=` shows, and judged there as before; the grandchild's poll is
  0.5 s apart rather than 1.5 s, for the same 30 s in all.

Measured with `~/ferrix-logs/gate-time/tt2/measure.sh` (the same
`stamp.py`), each step alone, x86_64 under TCG, before on main a0772269 in
a worktree of its own and after on the branch, both warm. The host carried
other sessions' work (a Miri run, Steam's end-to-end): loads as given.

| Step | Before (2 runs) | Load | After (3 or 4 runs) | Load |
|---|---:|---:|---:|---:|
| `test-restart` | 23.2, 24.8 s | 4-12 | 17.0, 15.2, 19.8 s | 2.5-15 |
| `test-sysfs` | 19.4, 27.0 s | 5-15 | 15.2, 13.8, 18.8 s | 3.6-13 |
| `test-jobs` | 26.2, 35.6 s | 4.5-15 | 23.5, 19.6, 25.8 s | 4.3-12 |
| `test-init --arch x86_64` | 19.6, 24.9 s | 3.6-12 | 15.8, 16.7, 15.9 s | 6.2-11 |
| `test-init --arch all`, warm | 55.9 s | 7.3 | 38.6 s | 7.8 |
| `test-auth --arch x86_64` | 47.6, 49.4 s | 3.6-10 | 46.6, 45.6 s (and a boot hang, below) | 6.8-9.3 |
| `test-compositor --arch x86_64` | **900 s**, 33 boots | 5.3 | **512, 549, 616 s** (and two failures, below) | 9-18 |

`test-compositor --arch x86_64` ran to its end for the first time, before
and after. Per boot, the time from the guest's last line to QEMU being
stopped went from 13.2 s to 0.8 s on average (434 s to 27 s over the 33
boots); the guest's own time is 11.2 s either way. What is left of a boot's
15.5 s is the guest and about 4 s of building and starting.

One `test-auth` run hung in the kernel before stage 3, the shape of the
BACKLOG's open row for that hang; nothing this cut changes runs before the
marker. Two full `test-compositor` runs failed in the animation boot at
loads of 20 to 28, with the host saturated by other sessions: once on the
slowest frame (12.3 s, over the 5 s bound) and once on the slide's last
picture, both the shapes the BACKLOG's frame-budget row already records
under load. Run alone, alternating with main, six `--boot animation` runs
at loads of 20 to 36 all passed, three each.

Not cut, since each waits for something checked: fuzzel-user's 5 s and 3 s
(boots of the user's own configuration), idle-user's 3 s linger, the cursor
boot's reads, test-audio's settle and grace. The other gates' hooks
(test-input, test-pty, test-seat, test-display, test-clipboard, test-adb,
test-badapple, test-foot, test-vkgears, test-video, the chrome and bench
boots, `test-compositor --gl`) still take the grace; each can say
`stop_when_done` once it is read for whether its guest powers off, and its
gate is run. The grace's 5 s of an idle guest also ran under drcov; they
checked nothing, but the next coverage re-measure is the one to show
whether they reached a statement nothing else does.

## Cut 3: the architectures at once (2026-09-30)

xtask only; owner os-98. `test-boot`, `test-init` and `test-audio` with
more than one architecture now run one child an architecture, all at once (`tools/common/xtask/src/parallel.rs`): xtask starts
itself again with the same arguments and that one `--arch`, each child's
output goes to `build/<arch>/xtask-<command>.log`, each is said as it ends,
and when the last has ended every log is printed whole, in the
architectures' order, and then each one's verdict; a failure on one hides
none of the others. `FERRIX_ARCHES_IN_TURN=1` keeps the old order.

What the boots wrote in one place is one an architecture now: stage 12's
writable disk, `test-init`'s volumes and the fresh root a test boots are
under `build/<arch>/` (`btrfs_disk`); the read-only images were already
written whole and renamed into place. `run`'s own root, `build/root.img`,
stays where a person's system is. cargo takes its own lock on the target
directory, so the builds queue while the boots overlap.

Measured with `~/ferrix-logs/os98-par-measure.sh` on nazuna, main 70ad8969
against the branch, alternating, each warm, the host carrying other
sessions' work:

| Step | In turn (2 runs) | Load | At once (2 runs) | Load |
|---|---:|---:|---:|---:|
| `test-boot --arch all` | 139.1, 135.9 s | 54-60 | 46.4 s (and one stall, below) | 55 |
| `test-init --arch all` | 181.9, 170.7 s | 52-54 | 86.3, 72.0 s | 34-47 |

`test-compositor` stays in turn. Run at once in the gate (2026-09-30, load
37 to 51) its three suites took 1,200 s where they take about 1,800 s in
turn, but x86-64's failed on the frame budget -- 6.7 s against the 5 s a
frame under emulation is allowed -- while AArch64's and ARMv7-A's passed:
three compositors emulating side by side push a budget the host's load
already decides (its flake row in `docs/BACKLOG.md`) over more often, and a
gate that fails for its neighbours checks less. It can join the others once
its frames are judged in the guest's own time rather than the host's.

The other at-once `test-boot` stopped with both Arm guests silent after
stage 11 for 105 s: nazuna's disk had filled (another session freed 57 GB
at that moment), and QEMU pauses a guest whose disk image cannot grow,
which the stage 12 disk, written by every boot, then had to. The in-turn
`test-init` that failed did so on the semaphore self-check, a row of its
own in `docs/BACKLOG.md`.

## Next

Phase 2 in the order the numbers give: (3) stop rather than wait for a
finished guest, done for the gates in "Cut 2" above; (2) arches in
parallel, done in "Cut 3" for all but test-compositor; then (5), KVM by
default on x86-64. For (5), read on 2026-09-30 and not started: the choice
is `qemu::accelerator`, whose `None` means `tcg` today. The plan is `kvm`
for an x86-64 guest on an x86-64 Linux host whose QEMU lists it, and `tcg`
otherwise, so CI, which has no `/dev/kvm`, keeps its TCG boots unchanged;
`tcg` whenever `FERRIX_QEMU_PLUGIN` is set, since a coverage plugin sees
only translated blocks; and `seam.rs` passing `tcg` itself, since its
measurement was made under it. The frame budgets of `test-compositor`
are written for emulation and stay as they are, looser than KVM needs. Cut 1 was measured and
dropped (2026-09-28, above). Each lands as its own slice under `land.sh`,
with before and after from this table's method. Still to measure on a
quiet window the coordinator can call: `test-net`.
