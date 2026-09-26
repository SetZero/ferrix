# Ferrix — roadmap

Ferrix is an operating system in Rust for x86-64, AArch64 and ARMv7-A. Its
first goal, compiling Rust on Ferrix, is met; the goal since is a
Hyprland-shaped desktop, and then Steam. This page is the short version; each
stage has its own page, and the details are linked at the end.

The sidebar marks each stage: ✓ done, ◐ in progress, ○ not started.

## Where it stands (2026-09-26)

**Done**

- Stages 0–12 run in every boot test on all three architectures; ARMv7-A also
  boots on an STM32MP157D-DK1 board.
- The first goal is met: `rustc` compiles and runs a program on Ferrix
  (stage 16).
- Display, input and the compositor (stages 17 and 18), networking, dynamic
  linking and sysfs.

**In progress**

- **Stage 13:** cgroups with pids, memory, OOM kill and CPU weight;
  namespaces and seccomp are left.
- **Stage 15:** the init is done (L1 to L11): `/sbin/init` boots every image
  and runs the desktop as a service; authentication (`docs/AUTH.md`) is next.
- **Stage 19:** the desktop composites on the GPU; XWayland and the
  second-pass effects are left. waybar, fuzzel, hyprlock and hypridle are being
  rewritten in Rust.
- **Stage 20:** Ferrix builds its own x86-64 image.
- **Stage 22 (Steam):** sound plays through `/dev/snd`, Chrome plays video
  with sound, and 32-bit x86 programs run.
- **Chrome** runs headless and in a window, on glibc and on ferrousli, Ferrix's
  own C library.
- **Pixel 7:** boots natively on all eight cores, runs the desktop in a VM,
  and streams its log over USB.

**Not started**

- Stage 14 (real-time) and stage 21 (bare metal with a GPU of Ferrix's own).

## Forecast

- About **442 sized points** are left.
- At the recent pace the sized work ends in the **first days of October**;
  unsized work (self-hosting, bare metal, most of Steam) is not in that date.

## Details

- [Where it stands, in full](where-it-stands.md): every stage's state, one
  paragraph each.
- [Status, estimates and forecast](status.md): the status table, velocity,
  burndown and Gantt charts.
- [How this roadmap works](about.md): the two rules that order the stages, and
  how sizes are given.
- [How to edit the roadmap](HOW-TO-EDIT.md).

<!-- The stage index below is generated from the headings by `python3 scripts/gen/split-roadmap.py index`; do not edit it by hand. -->

## The stages

One file a section, in the roadmap's order. The status is the heading's
✅ or `done`, and otherwise the status table's rows for that stage;
the size is what the heading says. [HOW-TO-EDIT.md](HOW-TO-EDIT.md) says
how to change a stage, add one, or carry a branch's edits of the old
single file across.

| Stage | Section | Status | Size, as the heading gives it |
|---|---|---|---|
| 0 | [Foundation](stage-00-foundation.md) | ✓ done |  |
| 1 | [Boot, both architectures](stage-01-boot-both-architectures.md) | ✓ done |  |
| 2 | [Physical and virtual memory](stage-02-physical-virtual-memory.md) | ✓ done |  |
| 3 | [Traps, interrupts, time](stage-03-traps-interrupts-time.md) | ✓ done |  |
| 4 | [SMP](stage-04-smp.md) | ✓ done |  |
|  | [ARMv7-A — a third architecture](armv7a.md) | ✓ done |  |
| 5 | [Tasks and the scheduler](stage-05-tasks-scheduler.md) | ✓ done |  |
| 6 | [User mode](stage-06-user-mode.md) | ✓ done |  |
| 7 | [The Linux syscall ABI](stage-07-linux-syscall-abi.md) | ✓ done |  |
| 8 | [VFS, initramfs, the pseudo-filesystems](stage-08-vfs-initramfs-pseudo-filesystems.md) | ✓ done |  |
| 9 | [The native ABI: handles, channels, ports, VMOs](stage-09-native-abi.md) | ✓ done |  |
| 10 | [Userspace drivers](stage-10-userspace-drivers.md) | ✓ done |  |
| 11 | [Block core and btrfs, read](stage-11-block-core-btrfs-read.md) | ✓ done |  |
|  | [Networking — sockets, a net core, virtio-net](networking.md) | ✓ done |  |
|  | [Dynamic linking — PIE, `PT_INTERP`, a loader](dynamic-linking.md) | ✓ done | done 2026-09-23: 39 points, and ferrousli's port at ≈ 34 |
| 12 | [btrfs, write](stage-12-btrfs-write.md) | ✓ done | ≈ 60 points, spent |
|  | [sysfs — the device tree, fed by the services that own it](sysfs.md) | ✓ done | 26 points, spent |
|  | [Chrome — a browser on Ferrix](chrome.md) | ◐ in progress | headless and in a window, 2026-09-24; the DK1 ≈ 45–55 points |
| 13 | [Namespaces, cgroups v2, seccomp](stage-13-namespaces-cgroups-v2-seccomp.md) | ◐ in progress | month |
| 14 | [Real-time domains](stage-14-real-time-domains.md) | ○ not started | month |
| 15 | [A real userland](stage-15-real-userland.md) | ◐ in progress | week |
| 16 | [`rustc`](stage-16-rustc.md) | ✓ done | the goal; ≈ 40 guessed, 8 spent |
| 17 | [Display and input](stage-17-display-input.md) | ✓ done | 74 points, spent |
| 18 | [The compositor](stage-18-compositor.md) | ✓ done | 96 points, spent |
| 19 | [Hyprland fidelity, and the GPU](stage-19-hyprland-fidelity-gpu.md) | ◐ in progress | 178 points, about 56 left |
| 20 | [Self-hosting](stage-20-self-hosting.md) | ◐ in progress |  |
| 21 | [Bare metal, and a GPU of Ferrix's own](stage-21-bare-metal-gpu-ferrix.md) | ○ planned | unsized, over 100 points |
| 22 | [Steam](stage-22-steam.md) | ◐ in progress | unsized, over 300 points |
|  | [Written ahead of their stage](written-ahead.md) |  |  |
|  | [Continuously, from stage 1](continuously.md) |  |  |

<!-- End of the generated stage index. -->
