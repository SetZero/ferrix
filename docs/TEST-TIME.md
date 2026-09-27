# Test and gate run time

The customer's priority one from 2026-09-27: *"we seriously need to bring down
test run time."* This is phase 1's record -- where a landing gate's time goes --
and the cuts it points to. Only the Arm firmware waits have been cut since (item 4). Owner: ferrix-90.

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
kernel/src/main.rs`):

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

## Where the time goes: the five likely costs

From a read of `xtask/src` and `kernel/src` (file and line in the survey,
kept in the owner's handover), checked against the timings above:

1. **The kernel crate rebuilds whenever a gate embeds a different init.**
   `FERRIX_INIT`, `FERRIX_INIT_SCRIPT`, `FERRIX_INIT_COMMANDS` and their
   digests are `rerun-if-env-changed` (`kernel/build.rs`), and every flavour
   builds into one target dir. So a gate that alternates `test-boot`,
   `test-shell`, `test-vfs` and `test-net`, or `test-audio`'s three boots,
   recompiles the whole kernel each time. **Cut:** a target subdirectory
   per init flavour (keyed by the digest), or the init passed to the image
   rather than built into the kernel.
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
   for, and fixed settles become waits on the line they wait for.
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

## Next

Phase 2 in the order the numbers give: (1) per-flavour kernel target dirs,
(3) stop rather than wait for a finished guest, (2) arches in parallel, then
(5). Each lands as its own slice under `land.sh`, with before and after from
this table's method. First, finish measuring on a quiet window the
coordinator can call: `test-compositor` to its end, `test-net`, and a warm
`test-init --arch all`.
