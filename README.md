# Ferrix

An operating system written in Rust for x86-64, AArch64 and ARMv7-A, whose
acceptance test is that it compiles Rust.

Not "has a shell", not "draws a window": it hosts `rustc`. That is the hardest
thing a general-purpose OS is routinely asked to do, and the only goal that
forces every subsystem to be real — threads and futexes, demand paging over
gigabytes, `fork`/`execve`, a hundred and fifty syscalls, and a filesystem that
survives a crash.

```
$ cargo xtask test-boot --arch all
  x86_64: booting under QEMU (timeout 120s)
    | Ferrix 0.1.0 on x86_64
    |   memory   507 MiB total, 499 MiB usable, 132 regions
    |   kernel   0x1df5a000 -> 0xffffffff80000000, 264 KiB
    |   physmap  0xffff800000000000 covering 512 MiB from 0x0
    |   tables   root 0x1dd5a000
    |   display  1280x800, stride 1280
    |   acpi     rsdp at 0x1fb7e014
    |   stage 1  loader hand-off verified
    |   traps    vectors installed
    |   frames   497 MiB managed, 497 MiB free, 127299 entries at 0x1780000 (2048 KiB)
    |   stage 2  frame allocator, heap and vmap arena verified
    |   clock    HPET at 100.000 MHz
    |   irqs     APIC, local APIC timer at 62.967 MHz
    |   stage 3  2 breakpoints, 4 page faults, 1001 ticks at 912 Hz
    |   cpus     4 described by firmware, 4 online, booted on APIC ID 0x0
    |   smp      100 rounds of work on every processor, 300 IPIs taken
    |   tlb      20 remaps seen by every processor, 21 shootdowns
    |   grace    100 grace periods against 411 reads, none of them stale
    |   counter  100000 of 100000, 4 of 4 shares overlapping, 35 updates lost without the lock
    |   stage 4  4 processors online, a contended counter came to 100000 of 100000
    |   w^x      354 mappings swept, 30 executable, none writable
    |   reclaim  4 MiB from the loader and ACPI, 501 free; arena 9 live, 180 KiB
    | FERRIX-BOOT-OK stages 1-5
  x86_64: boot ok
  aarch64: boot ok
  armv7a: boot ok
```

## What exists today

Stages 1 to 5 of `docs/ROADMAP.md`, on all three architectures. Each boots from
firmware to a Rust kernel which verifies the hand-off, brings up a buddy
allocator over every usable frame, starts a kernel heap — so `Box`, `Vec` and
`BTreeMap` work — installs its own trap vectors, services a page fault by
mapping the faulting address and letting the instruction retry, brings up an
interrupt controller, runs a clock, brings every other processor online, and
schedules a thousand kernel threads across them under an EEVDF fair class.

ARMv7-A is the Cortex-A7 of the STM32MP157, run on QEMU's `virt` machine under
U-Boot. It joined after stage 3 without a second loader, a second facade or a
line of bootstrap assembly; `docs/arm32.md` is the plan it followed, and the
decisions it had to argue — a PE32 loader converted from ELF because rustc has
no 32-bit UEFI target, a 32-bit address space, and a machine described by a
device tree instead of ACPI.

Virtual memory is finished rather than sketched: a `vmap` arena hands out
guard-paged ranges and kernel stacks over them, the loader's identity map is
dropped — which is what turns "higher-half" from a linker script's claim into a
demonstrated fact — the loader's own memory and the ACPI-reclaim regions go back
to the buddy allocator, empty slab pages are returned to it rather than hoarded,
and a sweep of the live page tables asserts that no mapping is both writable and
executable.

Time works and interrupts arrive. The local APIC and the GICv2 are programmed,
the local APIC timer is calibrated against the HPET, the two Arm architectures
use the architected virtual timer, and all of them are reached through one
facade — `irq::register`, `timer::after`, `trap::Frame`. What stage 3 still
owes is hardware Ferrix cannot currently be booted on to test: GICv3, and the
local APIC's TSC-deadline mode.

And there is more than one processor. Every vCPU QEMU is given comes online,
with a record of its own, IPIs, TLB shootdown where the hardware does not
broadcast invalidation itself, and grace periods; the proof is four processors
incrementing one counter under one ticket lock and required to reach exactly
100,000 with their shares overlapping in time. The scheduler, the syscall layer
and everything above them are ahead.

The kernel proves it on every boot rather than asserting it: the memory map is
checked to be sorted and to describe the loader's own allocations, the direct
map is checked to alias physical memory by reading the kernel's first bytes
through both mappings, and the allocators are hammered with four thousand
blocks and required to give every frame back.

## No assembly at boot

Every architecture boots via UEFI — EDK2 on the 64-bit pair, U-Boot on
ARMv7-A — so firmware calls a Rust `efi_main` with a stack set up and the MMU
on. There is no bootstrap assembly on any of them, which is unusual and is a
direct consequence of choosing UEFI over Multiboot or a bare kernel boot.

The assembly that does exist — 280 lines, **99.10% Rust** — is confined to
constructs the machine defines before a Rust function could run: installing a
translation regime and jumping to an address that did not exist a moment
earlier, and the CPU primitives with no Rust spelling. `docs/ASSEMBLY.md` is the
argument for each one; `scripts/check-asm-budget.py` fails the build on any site
that is not on the list, on a file over its budget, and on an entry that has
gone stale.

That number is a trend, not a gate. Assembly here is a fixed cost that does not
grow with the system, so the percentage rises as the OS is written.

## Getting started

```
cargo xtask build     --arch all --release    # bootable images in build/
cargo xtask run       --arch x86_64           # boot it, serial on your terminal
cargo xtask run       --arch x86_64 --net     # ...with a network, 10.0.2.15 behind a NAT
cargo xtask test-boot --arch all              # boot it and assert it came up
cargo xtask test-boot --accel auto            # ...on the real MMU, where it can
cargo xtask check                             # every gate CI runs
```

`--accel auto` boots on the host processor instead of QEMU's interpreter —
`whpx` on Windows, `kvm` on Linux, `hvf` on macOS, and `tcg` when there is
none or when the guest is not the host's architecture. Worth running before
believing a change to page tables or invalidation, for the reason
[Reliability](docs/RELIABILITY.md) gives: an interpreted `MMU` has no `TLB`, so
a stale translation is a bug the default gate structurally cannot see. `run`
uses `auto` unless it is given `--accel` or `--gdb`; the tests keep `tcg`.
Under `whpx` the guest gets one processor unless `--smp` says otherwise:
QEMU 11.1's WHPX emulation of device registers faults ring-3 drivers with
more than one, and `/sbin/blk` dies at boot. `--smp N` still works there,
with a warning.

`--net` gives the guest a virtio-net card whose other end is `xtask`'s own
gateway on a loopback UDP socket: the guest is `10.0.2.15`, `10.0.2.2` is the
host, `10.0.2.3` forwards DNS to the host's resolver, and TCP and UDP to
anywhere else are relayed through ordinary host sockets. It needs no
privilege, and works the same on Linux and on Windows. With `--init`, the
shell configures `eth0` by DHCP before its first prompt, with busybox's
`udhcpc`, so `wget http://example.com` and `ping 10.0.2.2` work at once.

You need QEMU 8.1 or later, for the AArch64 SMMUv3's stage 2, and UEFI
firmware. Debian and Ubuntu: `qemu-system-x86`,
`qemu-system-arm`, `ovmf`, `qemu-efi-aarch64` and `u-boot-qemu`. Windows:
`winget install SoftwareFreedomConservancy.QEMU`, which ships the 64-bit
firmware too but not U-Boot; for ARMv7-A, `sudo apt install u-boot-qemu` in
WSL's default distribution, where `xtask` looks, or point `FERRIX_UBOOT` at a
`qemu_arm` `u-boot.bin`. Nothing else — the FAT32 image is written by `xtask`,
so there is no `mtools` or `dosfstools` to install and the image is
byte-for-byte reproducible.

`cargo xtask check --ferrousli` gates the C library too, whose tests build and
run Linux programs. On Windows those steps run in WSL's default distribution,
which needs rustup and `build-essential` installed inside it; the first step
says so if they are missing.

### A busybox shell

Given a static busybox with `--init`, `build` and `run` put it in the kernel,
which starts `sh -i` on the console, and in the initramfs at `/bin/busybox`
with every applet linked beside it, so `ls /proc`, `cat /proc/self/maps` and
`top` work where you type them.

The busybox Ferrix is measured with is built against
[ferrousli](ferrousli/README.md), this repository's C library, and
`--init ferrousli` names it. It is x86-64 only for now.

On Windows, once, from PowerShell:

```
winget install SoftwareFreedomConservancy.QEMU
winget install Git.Git
winget install LLVM.LLVM
winget install StrawberryPerl.StrawberryPerl
```

On Debian or Ubuntu, `sudo apt install build-essential curl bzip2 file`.

Then, in the checkout:

```
cargo xtask busybox                              # build busybox against ferrousli
cargo xtask run --arch x86_64 --init ferrousli   # boot to a busybox shell
```

Quit QEMU with `Ctrl-A x`.

`cargo xtask busybox` runs `ferrousli/tools/busybox/build.sh`, or on Windows
`build-windows.sh` in Git for Windows' bash. Either downloads busybox 1.37.0
and Alpine's configuration for it, both checked against pinned sums, builds
ferrousli and busybox against it under `~/.local/share/ferrix/busybox/ferrousli`
(`%USERPROFILE%\.local\share\ferrix\busybox\ferrousli` on Windows), and
installs `x86_64/bin/busybox.static` there, where `--init ferrousli` looks. On
Windows clang cross-compiles and links it, Strawberry Perl's gcc and gmake run
busybox's own build, and the kernel headers busybox includes come from Alpine's
`linux-headers` package, pinned the same way. It takes a few minutes the first
time. Run it again after `ferrousli/` changes; `--init ferrousli` uses whatever
it last installed.

`cargo xtask test-shell --arch x86_64 --init ferrousli` runs a script in that
shell instead of waiting for you, and fails unless the script's output comes
back; `test-vfs` runs the file system's commands and applets the same way.
`--init` also takes the path of any other static busybox, with `{arch}` in it
replaced by each architecture's name; that is how the gates give it the musl
and glibc builds they check alongside.

### zinc, a zsh-compatible shell

`build` and `run` with a program also put [zinc](zinc/README.md) in the
initramfs, at `/bin/zinc` with `/bin/zsh` beside it: a zsh-compatible shell
written in Rust, whose goal is to run oh-my-zsh. Type `zsh` at the busybox
prompt. It is built for x86-64 and AArch64 by `cargo` alone, against the
target's own musl and linked by rust-lld, so Windows needs nothing else.

`cargo xtask test-shell --arch x86_64 --init target/zinc/x86_64-unknown-linux-musl/release/zinc`
runs stage 7's script with zinc as the first program instead of busybox.

### The display

```
cargo xtask run --arch x86_64 --display --init blank    # a window showing Ferrix's screen
cargo xtask test-display --arch x86_64                  # the same, judged pixel by pixel
```

`--display` puts a virtio-gpu card on the bus, and with `run`, opens QEMU's
window. The window starts on the firmware's console (VGA on x86-64, ramfb on
AArch64), where the loader draws; the card is the other console in the
window's View menu. `--init blank` builds the compositor's first program,
[`compositor/blank`](compositor/README.md), and boots it as init: it opens
`/dev/dri/card0` through Ferrix's Linux DRM subset, sets the preferred mode
(1024×768) and fills the screen with one colour, `#1E1E2E`. Its serial
line says `compositor: scanout ...`, or why it failed. There is no input and
no windows yet; [the display design](docs/DISPLAY.md) says what comes next,
and [the GPU decision](docs/GPU.md) how the pixels leave the CPU.
x86-64 and AArch64 only: QEMU's ARMv7-A `virt` machine has no virtio-gpu.

`test-display` boots the same program with QEMU's window off, asks QEMU for a
screendump over QMP, and requires every pixel to be that colour; then it
boots a build that draws one pixel wrong and requires the check to catch
exactly that pixel.

### The desktop, and zinc in a window

```
cargo xtask run-compositor --arch x86_64             # a desktop in a window, with a shell in it
cargo xtask run-compositor --arch x86_64 --vnc :0    # the same, served over VNC rather than shown
cargo xtask run-compositor --arch x86_64 --gl        # the same, on the 3D card
```

`run-compositor` boots [`compositor/hyprix`](compositor/README.md) as init on
a virtio-gpu card: the compositor reads a `hyprland.conf`, listens on a
Wayland socket, tiles what connects to it and puts the frame on the screen.
The configuration it writes into the initramfs starts a terminal first --
`exec-once = /bin/term /bin/zinc` -- so the boot ends at a shell prompt rather
than at a picture. [`compositor/term`](compositor/README.md) is the terminal,
a character grid with the escape sequences a shell actually sends, and it runs
the program it is given on a pseudoterminal; that program is
[zinc](zinc/README.md), with the busybox applets, the uutils and the ported
programs the image carries beside it.

What the keyboard does, in the configuration it writes itself:

| keys | |
|---|---|
| `SUPER`+`RETURN` | another terminal running zinc |
| `SUPER`+`P` | a `compositor/pattern` client, the picture the gates tile |
| `SUPER`+`Q` | close the focused window |
| `SUPER`+`F`, `SUPER`+`V` | fullscreen, floating |
| `SUPER`+`H`, `SUPER`+`L` | move the focus; with `SHIFT`, move the window |
| `SUPER`+`1`, `SUPER`+`2` | workspaces; with `SHIFT`, send the window to one |
| `SUPER`+`C`, `SUPER`+`W` | `hyprctl clients`, `hyprctl activewindow` |

`--config <PATH>` carries a real `hyprland.conf` instead, and what that one
starts is yours. `--layout de,us` is `input:kb_layout`; `--size <W>x<H>` is
the screen, 1920x1080 when not given. The boot has a network unless
`--no-net` -- `udhcpc` runs as an `exec-once`, so `curl` in the terminal
reaches the host's own resolver -- and `--vnc <DISPLAY>` serves the screen at
e.g. `:0` rather than opening a window, which is what a machine reached over
`ssh` wants. `--gl` asks QEMU for `virtio-gpu-gl-pci`, the 3D card, with this
host's GPU behind it through virglrenderer; [the GPU decision](docs/GPU.md)
says what that gives and what it does not, and `FERRIX_QEMU` names a QEMU
that is not the one on `PATH`.

The wallpaper comes from this machine's pictures and from nowhere else.
`cargo xtask wallpapers --from <directory>`, or `--from host:directory` for a
machine `ssh` reaches, converts them with `ffmpeg` -- scaled until it covers
the screen -- and keeps the rows under
`~/.local/share/ferrix/wallpapers`, or `$FERRIX_WALLPAPERS`. A run reads that
directory and opens no connection of its own; with nothing in it the
background is plain and a line says how to change that. `--wallpaper <NAME>`
picks one by part of its name.

A video in that directory becomes a wallpaper that moves, which on a Linux
desktop is `exec-once = mpvpaper ALL <file>` and here is the same
`background` layer surface with the decoding done on the machine that has a
decoder: `ffmpeg` takes four seconds of it at ten frames a second, a quarter
of the screen each way, and the frames are kept run-length encoded against
the frame before them -- five seconds of `testsrc` is 3.7 MB where its raw
rows are 20.7 MB. `run-compositor` starts `/bin/pattern --video` on it.
The client damages the rows that changed rather than the screen, and waits
for a frame callback before drawing the next one, so a wallpaper under a
full-screen window stops playing on its own -- what `mpvpaper-stop` is for.
[The display design](docs/DISPLAY.md) says what it costs, which on a machine
that has to emulate is about a frame a second, and says why that is the
compositor rather than the video.

`cargo xtask test-video` boots a wallpaper that moves -- two frames, each one
flat colour, made in code so the gate needs no `ffmpeg` -- and requires the
screen to show both of them in turn.

`cargo xtask test-compositor` judges the same picture pixel by pixel,
`test-input` and `test-seat` send a key and a touch through QEMU and require
them back, and `test-pty` runs a program on a pseudoterminal with no window
at all. x86-64 and AArch64 only, as `--display` is.

### Everything at once

One command with every part of Ferrix that has a switch turned on:

```
FERRIX_QEMU=<a QEMU with a window and virglrenderer, if the one on PATH has neither> \
cargo xtask run-compositor --arch x86_64 --gl --clipboard \
    --size 1920x1080 --layout de,us --smp 4 --memory 1024
```

That is the desktop on the 3D card, in a window of this host's, at
1920x1080, with two keyboard layouts a switch moves between, four CPUs,
twice the default memory, a network, a wallpaper from
`~/.local/share/ferrix/wallpapers` and a terminal running zinc already open.
Every flag is described above or in `cargo xtask --help`; the ones worth
saying twice:

| | |
|---|---|
| `FERRIX_QEMU` | a QEMU that is not the one on `PATH`, a directory of its binaries or one binary. Distributions often build QEMU without a local display backend, and then there is no window to open -- xtask asks whichever QEMU it is what it has, falls back to VNC when it has none, and says which it chose. `--gl` wants virglrenderer as well |
| `--gl` | the 3D card, with this host's GPU behind it. Turns `--display` on by itself. Without it the card is the 2D one and the compositor composites on the CPU |
| `--smp`, `--memory` | 4 and 512 MiB by default. A desktop with a video wallpaper is the one workload here that notices more of either |
| `--vnc :0` | instead of a window, for a machine reached over `ssh`. `--gl` still works: the frames are drawn off screen and copied out |
| `--no-net` | the one thing in that command that is on by default and can only be turned *off* |

There is no `--everything`, deliberately: every device a boot does not need
is one fewer on the bus, and several of the gates exist to assert exactly
what a machine enumerates.

The same shape for `run`, which boots a program of your choosing rather than
the compositor -- here `compositor/blank`, and `--init ferrousli` or a path
to a busybox for a shell instead:

```
cargo xtask run --arch x86_64 --display --gl --input --net --clipboard --init blank
```

`--input` is the keyboard and the tablet, which `--display` brings along
anyway; `run` is the one command that wants it said.

**What `--clipboard` does today.** It puts a `virtio-serial` device on the
bus with the port SPICE's agent protocol uses, and QEMU's own half of that
protocol behind it, so the wire between the guest and the clipboard of
whoever is watching is there and QEMU is talking on it. **Nothing in the
guest answers yet** -- there is no driver for the device, no
`/dev/vport0p1` and no agent, so copy and paste between Ferrix and the host
does not work, and pressing `CTRL`+`V` will do nothing across that boundary.
Copy and paste *between two Ferrix programs* is a different path and does
work. [The clipboard design](docs/CLIPBOARD.md) is the whole plan and §8
says which parts of it are built.

### On an STM32MP157-DK1 board

The same ARMv7-A image boots the STM32MP157D-DK1 from its SD card, under
mainline TF-A, OP-TEE and U-Boot. The whole story — building that firmware,
partitioning the card, and what to do when a boot goes wrong — is in
[the board guide](docs/stm32mp157-dk.md); this is the short version.

**Once.** A card with the firmware on it (the guide's step 1), both boot
switches on the underside **ON**, a micro-USB cable into the **ST-LINK** port and
the USB-C cable to this computer. The micro-USB cable has to carry data: a
charging-only one looks identical and nothing enumerates at all. The ST-LINK is
powered from that cable, so its serial port is there even when the board is off.

**Build.** The board needs a static, hard-float ARMv7 busybox (Alpine's
`busybox-static` for `armv7` is one; `--init ferrousli` is x86-64 only):

```
cargo xtask build --arch armv7a --init PATH/{arch}/busybox.static
```

Add `--reset` to put `FERRIX/CMDLINE.TXT` with `ferrix.onexit=reset` in the image:
the board then resets itself back to U-Boot when the shell exits, instead of
powering off.

**Watch the console first.** The ST-LINK's port is the board's console, at
115200 8N1 with no flow control. U-Boot boots straight on, so open the port
*before* resetting the board, and only one program can hold it at a time.

- Linux: `/dev/ttyACM0`, in the `dialout` group — `cargo xtask watch-serial`
  (which exits 0 on `FERRIX-BOOT-OK`), or `picocom -b 115200 /dev/ttyACM0` to type.
- Windows: `cargo xtask watch-serial` finds the ST-LINK's `COMn` itself
  (`--port COMn` when there are several), or open it in PuTTY as *Serial* at
  115200 to type — Device Manager → *Ports (COM & LPT)* →
  *STMicroelectronics STLink Virtual COM Port (COMn)*.

Bytes that arrive the moment the port opens are the ST-LINK's buffer from an
earlier boot; trust what follows a reset.

**Flash.** Reset the board, press a key to stop U-Boot's countdown, and let U-Boot
expose the card over USB-C:

```
STM32MP> ums 0 mmc 0
```

- Linux: the desktop mounts `bootfs`; `cargo xtask flash --arch armv7a --to
  <that mount>`. (`cargo xtask deploy --arch armv7a` builds, flashes and watches
  in one command.)
- Windows: `bootfs` appears as a drive — cancel any offer to format the card's
  other partitions, they hold the firmware — and `cargo xtask flash --arch
  armv7a --to E:\` with that drive's letter copies the loader, kernel and
  initramfs, and flushes each one and the volume. `FERRIX\CMDLINE.TXT`, from a
  `--reset` build, is not among them on either host: copy that one by hand
  (`7z x build\armv7a\ferrix.img -oflash`). `deploy` works as on Linux.

Press Ctrl-C at the console to end mass-storage mode.

**Run.** At the `STM32MP>` prompt, one line at a time, waiting for the prompt
between them:

```
STM32MP> setenv bootargs
STM32MP> load mmc 0:4 0xc2000000 EFI/BOOT/BOOTARM.EFI
STM32MP> bootefi 0xc2000000 ${fdtcontroladdr}
```

The boot report ends in `FERRIX-BOOT-OK`, then busybox's `ferrix#` prompt, which
takes what you type over the same port. To boot this way by default:
`setenv bootcmd 'load mmc 0:4 0xc2000000 EFI/BOOT/BOOTARM.EFI; bootefi 0xc2000000 ${fdtcontroladdr}'`
and `saveenv`.

**Afterwards.** Leaving the shell powers the board off, and from there its reset
button does nothing: unplug the USB-C cable and plug it back in. With
`ferrix.onexit=reset` in `CMDLINE.TXT` the board comes back to `STM32MP>` by
itself instead.

## Layout

| | |
|---|---|
| `libs/` | Architecture-neutral logic: the hand-off ABI, the ELF reader, page table construction, the buddy allocator, the kernel heap. Host-testable **by design** — it is the only code `cargo test`, Miri and the fuzzers can reach. |
| `boot/` | The UEFI loader. Reads the kernel, builds the address space, leaves firmware. |
| `kernel/` | The kernel. |
| `xtask/` | Host build driver: cross-compiles both halves, converts the 32-bit loader from ELF to PE, writes the FAT32 image, drives QEMU. |
| `scripts/` | The quality gates. |
| `docs/` | [Architecture](docs/ARCHITECTURE.md) · [Roadmap](docs/ROADMAP.md) · [Assembly](docs/ASSEMBLY.md) · [Reliability](docs/RELIABILITY.md) · [Boot log](docs/BOOT-LOG.md) · [Conventions](docs/CONVENTIONS.md) · [SysML v2 model](docs/sysml/README.md) |

## Quality gates

Ported from the [Starling](https://github.com/Fancy-Mumble/starling) workspace:
`cargo fmt --check`, clippy at `-D warnings` on all six targets, `cargo-deny`,
Miri, fuzzing, and a lint table that denies `unwrap`, `expect`, `panic!`,
`unreachable!` and unchecked indexing in production code — a kernel that cannot
go on says so with `fatal!`, which names the catalog entry explaining the
failure — with every exemption argued at the site and checked by
`scripts/check-panic-audit.py`.

Three gates are this project's own:

* **The assembly allow-list**, above.
* **The unsafe audit.** Starling sets `unsafe_code = "deny"` and means it; a
  kernel cannot, because writing a page table entry *is* the program. So unsafe
  is not forbidden here, it is made expensive: a `SAFETY:` comment on every
  block, one unsafe operation per block, a `# Safety` section on every unsafe
  function, and `scripts/check-unsafe-audit.py` in CI so that a clippy release
  which softens a nursery lint cannot quietly retire the rule.
* **The boot test.** Everything else checks the source. This one boots it, on
  every architecture. An OS that compiles and does not boot is not a passing
  build.

## Licence

MIT. See [LICENSE](LICENSE).
