# Ferrix — roadmap

Ferrix is an operating system in Rust for x86-64, AArch64 and ARMv7-A. Its
first goal, compiling Rust on Ferrix, is met; the goal since is a
Hyprland-shaped desktop, and then Steam. This page is the short version; each
stage has its own page, and the details are linked at the end.

The sidebar marks each stage: ✓ done, ◐ in progress, ○ not started.

## Where it stands (2026-09-30)

**Done**

- Stages 0–12 run in every boot test on all three architectures; ARMv7-A also
  boots on an STM32MP157D-DK1 board.
- The first goal is met: `rustc` compiles and runs a program on Ferrix
  (stage 16).
- Display, input and the compositor (stages 17 and 18), networking, dynamic
  linking and sysfs.

**In progress**

- **Stage 13:** cgroups with pids, memory, OOM kill and CPU weight. Of the
  namespaces, per-mount flags (N1) and binds (N2) are in, and mount
  namespaces (N3) are next; seccomp is left.
- **Stage 15:** the init is done (L1 to L12): `/sbin/init` boots every image,
  starts `devmgr` and runs the desktop as a service; logins go through `authd`
  (`docs/AUTH.md` phase 1), and hyprlock's lock over it is parked on a branch.
- **Stage 19:** the desktop composites on the GPU, and yserver, an X server
  in Rust, shows X windows on it; client pages as texture backing and the
  second-pass effects are left. waybar, fuzzel and hypridle, rewritten in
  Rust, run the customer's own config on `run-compositor --everything`;
  hyprlock is parked on a branch.
- **Stage 20:** Ferrix builds its own x86-64 image.
- **Stage 22 (Steam):** sound plays through `/dev/snd` and a PulseAudio-protocol
  server, Chrome plays video with sound, 32-bit x86 programs run, and Valve's
  `steamcmd` logs in to Steam. The Steam client draws its sign-in window
  through yserver (2026-09-29), with launch-side workarounds
  (`docs/STEAM.md`).
- **The channel round trip, toward seL4 (440 ns):** 2,556 ns with every
  mitigation on, from 37 us; step 1 and 2a to 2e are in, 2f to step 5 are
  left (`docs/OPAQUE-KERNEL.md` §9.9).
- **The installer:** an MVP installs Ferrix on a VM's disk (2026-09-28).
- **Chrome** runs headless and in a window, on glibc and on ferrousli, Ferrix's
  own C library.
- **Pixel 7:** boots natively on all eight cores, runs the desktop in a VM,
  and streams its log over USB.

**Next:** cutting test and gate run time, the customer's priority one
(`docs/TEST-TIME.md`), then Steam's workarounds and the namespaces under
them; what is red, parked and waiting is in
[Where it stands](where-it-stands.md).

**Not started**

- Stage 14 (real-time) and stage 21 (bare metal with a GPU of Ferrix's own).

## Forecast

- About **442 sized points** were left on 2026-09-26, the last count.
  Since then stage 19's X server (40 of them), sound's alsa-lib and the
  32-bit ABI's I2 to I4 have landed; the forecast is recounted in
  [Status](status.md) at the next velocity count.
- Unsized work (self-hosting, bare metal, most of Steam) is not in any date.

## Details

- [Where it stands, in full](where-it-stands.md): every stage's state, one
  paragraph each.
- [Status, estimates and forecast](status.md): the status table, velocity,
  burndown and Gantt charts.
- [How this roadmap works](about.md): the two rules that order the stages, and
  how sizes are given.
- [How to edit the roadmap](HOW-TO-EDIT.md).

<!-- The stage index below is generated from the headings by `python3 tools/common/gen/split-roadmap.py index`; do not edit it by hand. -->

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
|  | [Claude Code — Anthropic's coding agent on Ferrix](claude-code.md) | ◐ in progress | the command line, 2026-09-30; the desktop app being assessed |
| 13 | [Namespaces, cgroups v2, seccomp](stage-13-namespaces-cgroups-v2-seccomp.md) | ◐ in progress | month |
| 14 | [Real-time domains](stage-14-real-time-domains.md) | ○ not started | month |
| 15 | [A real userland](stage-15-real-userland.md) | ◐ in progress | week |
| 16 | [`rustc`](stage-16-rustc.md) | ✓ done | the goal; ≈ 40 guessed, 8 spent |
| 17 | [Display and input](stage-17-display-input.md) | ✓ done | 74 points, spent |
| 18 | [The compositor](stage-18-compositor.md) | ✓ done | 96 points, spent |
| 19 | [Hyprland fidelity, and the GPU](stage-19-hyprland-fidelity-gpu.md) | ◐ in progress | 178 points, about 16 left |
| 20 | [Self-hosting](stage-20-self-hosting.md) | ◐ in progress |  |
| 21 | [Bare metal, and a GPU of Ferrix's own](stage-21-bare-metal-gpu-ferrix.md) | ○ planned | unsized, over 100 points |
| 22 | [Steam](stage-22-steam.md) | ◐ in progress | unsized, over 300 points |
|  | [Written ahead of their stage](written-ahead.md) |  |  |
|  | [Continuously, from stage 1](continuously.md) |  |  |

<!-- End of the generated stage index. -->
