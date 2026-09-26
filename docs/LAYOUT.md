# Layout

Where everything in this repository lives, and where a new thing goes. The
top level groups the tree by *what a part is*: firmware-facing loaders, the
kernel, the logic both share, the programs that run on Ferrix, the tests
around them, and the host tools that build and drive it all.

```
boot/        loaders: what runs before the kernel
kernel/      the kernel
libs/        host-testable logic, grouped by layer
native/      ring-3 programs on Ferrix's own ABI: runtime, devmgr, drivers
userland/    Linux-ABI programs, each its own cargo workspace
tests/       test programs and fuzzing that live outside any one crate
assets/      data files that ship in the image
xtask/       the host build driver (`cargo xtask …`)
scripts/     quality gates, generators and fetchers, grouped by role
tools/       host-side applications for particular hardware
docs/        design documents, the roadmap, the SysML model, certification
```

The root `Cargo.toml` is one workspace: `boot/`, `kernel/`, `libs/`,
`native/` and `xtask/`. Everything under `userland/`, `tests/` and `tools/`
is a workspace of its own, built by xtask from inside its directory.

## `boot/`

| Path | What |
|---|---|
| `boot/uefi/` | The UEFI loader (`ferrix-boot`) for x86-64, AArch64 and ARMv7-A. Reads the kernel, builds the address space, leaves firmware. |
| `boot/pixel7/` | The second-stage loader the Pixel 7's Android bootloader starts, and `mkbootimg.py`. |

Only `boot/*` and `kernel/` are freestanding; `xtask/src/workspace.rs` holds
that list.

## `kernel/`

The kernel crate. Its source paths are load-bearing: the certification
evidence (`docs/certification/coverage-*.json`) and the baselines in
`scripts/data/` anchor to `kernel/src/**` by file and line, so a file moved
inside `kernel/src` means regenerating that evidence.

## `libs/`

Architecture-neutral, host-testable logic — the only code `cargo test`, Miri
and the fuzzers can reach, which is why it is kept out of `kernel/`.
`scripts/check/check-crate-layering.sh` enforces that no lib depends on the
kernel or a loader. Each crate sits in exactly one group:

| Group | Holds | Crates |
|---|---|---|
| `libs/proto/` | The interfaces between components: the ABIs the kernel offers, the rings and control protocols it shares with ring-3 drivers, the loader hand-off | `linux-abi` `native-abi` `native` `bootinfo` `devmgr-proto` `blkring` `netring` `displayctl` `renderctl` `inputctl` `sndctl` |
| `libs/kernel/` | Kernel-internal cores: memory, scheduling, synchronisation, objects, randomness, the process stack image, the vDSO, the panic screen | `frame` `heap` `kmem` `paging` `vma` `sched` `sync` `objects` `fallible` `crng` `ustack` `vdso` `qr` `fbtext` |
| `libs/platform/` | Parsers for what firmware and the boot medium hand over | `acpi` `fdt` `pci` `elf` |
| `libs/fs/` | Storage and filesystems, including the text of the pseudo-filesystems | `vfs` `block` `btrfs` `btrfs-vfs` `btrfs-write` `cpio` `procfs` `sysfs` `cgroupfs` |
| `libs/network/` | The network stack | `net` `netwire` `nettcp` `netlink` |
| `libs/drivers/` | Device logic over an abstract transport, and the serve loops ring-3 drivers run | `virtio*` `usb-host` `gc400` `stm32-display` `blkserve` `netserve` `vdagent` |
| `libs/init/` | The service manager's pure core and its wire formats | `svc` `svc-proto` |

**Choosing a group for a new lib.** Ask what the crate *is*, not who uses it
first. A format two components agree on is `proto`, even if only the kernel
reads it today. Logic that drives a device is `drivers`. Something the
kernel alone needs and that is not storage, network or a device is `kernel`.
Add the crate to the root `Cargo.toml`'s `members` and
`[workspace.dependencies]`, and to the table above.

## `native/`

Programs that run in ring 3 on Ferrix's native ABI, built into the
initramfs by xtask:

| Path | What |
|---|---|
| `native/rt/` | The runtime every native program links: entry, the system-call instruction, exit, panic. |
| `native/devmgr/` | Matches devices to drivers and starts each in a job of its own. |
| `native/drivers/<name>/` | One process per driver: `blk` `net` `gpu` `ltdc` `input` `usbhid` `gc400` `vport` `snd`. The logic lives in `libs/drivers/`; the program is the thin shell around it. |
| `native/pong/`, `native/channel-echo/` | Small native test programs the boot gates start. |

`xtask/src/native.rs` lists which of these go into the image.

## `userland/`

Programs that run on Ferrix's Linux ABI. Each is a separate cargo workspace
with its own `Cargo.lock` and lints, built by xtask from inside its
directory:

| Path | What |
|---|---|
| `userland/compositor/` | hyprix, the terminal and every Wayland piece. |
| `userland/init/` | `/sbin/init`, getty and the unit files. |
| `userland/zinc/` | The zsh-compatible shell. |
| `userland/statd/` | The stat service. |
| `userland/ferrousli/` | The C library written in Rust, its dynamic linker, and the ports built against it (`tools/ports/`). |

## `tests/`

| Path | What |
|---|---|
| `tests/fuzz/` | `cargo fuzz` targets over `libs/`, and their committed corpus. |
| `tests/threads/` | Stage 7's threads exit test: a static musl program using `std::thread`. |

A crate's own tests and test data stay beside it (`libs/fs/btrfs/testdata/`,
`userland/compositor/render/tests/data/`). `tests/` is for programs that
exercise the system from outside any one crate.

## `assets/`

| Path | What |
|---|---|
| `assets/fonts/` | Inter and Liberation, with their licences, and `fonts.conf`. xtask puts them into the image under `/usr/share/ferrix/fonts`. |

## `scripts/`

| Path | What |
|---|---|
| `scripts/check/` | The quality gates `cargo xtask check` and CI run: layering, assembly budget, unsafe and panic audits, the certification item boundary, complexity, commit authors. `rustlex.py` is their shared Rust lexer. |
| `scripts/gen/` | Generators and their `--check` modes: the architecture document (with its `sysml/` reader), the panic catalogue, fonts, Wayland protocol tables, XKB tables, SOUP, coverage justification, fuzz corpus seeds. |
| `scripts/fetch/` | Fetch pinned downloads: the rustc sysroot, busybox, Chrome. |
| `scripts/test/` | Test drivers run by hand: the self-host matrix, the host `btrfs check` oracle. |
| `scripts/data/` | The allow-lists, baselines and registers the checks read. |

## `tools/`

| Path | What |
|---|---|
| `tools/pixel7/` | The Pixel 7 launcher app (`android/`), its host helper and the desktop monitor (`monitor/`). |

## Not in the repository

| Where | What |
|---|---|
| `build/<arch>/` | Images, initramfs and serial logs from xtask. Git-ignored. A running VM may hold these open. |
| `target/`, `*/target/` | Cargo output. Git-ignored. Worktrees each use their own `CARGO_TARGET_DIR`. |
| `.claude/worktrees/` | Worktrees of the sessions working on this repository. Git-ignored. Remove a worktree with `git worktree remove` once its branch is on `main`. |
| `~/.local/share/ferrix/` | Downloaded reference sources, busybox builds, firmware, per-stream target directories and logs. |

Scratch files belong in `$TMPDIR` or a session's scratch directory, never at
the top of a checkout: a file there is either committed by the next `git add
-A` or left for someone else to wonder about.
