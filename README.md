# Ferrix

An operating system written in Rust for x86-64, AArch64 and ARMv7-A, whose
acceptance test is that it compiles Rust.

Not "has a shell", not "draws a window": it hosts `rustc`. That is the hardest
thing a general-purpose OS is routinely asked to do, and the only goal that
forces every subsystem to be real — threads and futexes, demand paging over
gigabytes, `fork`/`execve`, the Linux system-call ABI, dynamic linking, and a
filesystem that survives a crash.

And it does. Since 2026-09-22, `cargo xtask test-rustc` boots Ferrix with a
btrfs volume holding the rust-lang.org release of `rustc` and Debian's glibc,
compiles `hello.rs` through `cc`, `collect2` and `rust-lld` — five programs
nobody here wrote, four `execve`s deep, over some 350 MiB of shared libraries
mapped from disk — and runs what it made:

```
rustc 1.97.1 (8bab26f4f 2026-07-14)
...
rustc-gate: hello from rustc on Ferrix
```

And since 2026-09-23 it builds itself. `cargo xtask test-selfhost` gives
Ferrix that toolchain with Cargo added, this tree and its vendored crates on
one btrfs volume, runs `cargo xtask build --arch x86_64` inside it -- the
command you run on a Linux host -- then takes the image Ferrix made off the
volume and boots it:

```
   93.49 |   image /data/src/build/x86_64/ferrix.img (63 KiB loader, 75510 KiB kernel, 10666 KiB initramfs)
  x86_64: Ferrix built its own image, and it booted
```

## What exists today

[The roadmap](docs/ROADMAP.md) is the authority; its *Where it stands* is
kept current with every landing. In short:

* **The kernel.** Stages 0–12 are in the boot test on all three
  architectures, which ends in `FERRIX-BOOT-OK stages 1-12`: UEFI hand-off,
  a buddy allocator and kernel heap, higher-half virtual memory with no
  mapping both writable and executable, traps, interrupts and timers, SMP
  with TLB shootdown and grace periods, preemptive tasks under an EEVDF fair
  class, and user mode.
* **Linux programs, unchanged.** The Linux system-call ABI, with threads,
  futexes, signals, `fork`/`execve`/`wait4`, `epoll`, pseudo-terminals and
  job control; a VFS with an initramfs root, tmpfs, `/proc` and `/dev`; and
  dynamic linking — `PT_INTERP`, position-independent executables, and
  glibc's own `ld-linux`, which is how `rustc` runs.
* **A native ABI and drivers in ring 3.** Handles, channels, ports, VMOs and
  jobs (stage 9), and userspace drivers behind an IOMMU (stage 10): VT-d on
  x86-64, the SMMUv3 on AArch64. virtio-blk, virtio-net, virtio-gpu,
  virtio-input and virtio-console are all ring-3 programs started by
  `devmgr`.
* **btrfs, read and write.** Mounted from a ring-3 disk driver, written with
  a log tree `fsync` replays, and judged by the host's `btrfs check` —
  including after QEMU is killed mid-write, 249 seeds of it. `run` boots with
  `/` on a persistent btrfs volume.
* **Networking.** Sockets, a net core and a ring-3 virtio-net driver: `curl`
  fetches over HTTPS and `git` clones inside the guest, and `sshdt` serves
  SSH from it.
* **A userland of its own.** [ferrousli](ferrousli/README.md), a C library
  written in Rust; [zinc](zinc/README.md), a zsh-compatible shell that runs
  oh-my-zsh and is `/bin/sh`; the uutils family for the utilities
  ([what is left of busybox](docs/UUTILS.md) is fourteen names).
* **A desktop.** [hyprix](compositor/README.md), a Hyprland-shaped Wayland
  compositor written from scratch, reads a real `hyprland.conf`, tiles
  windows, and composites on the host GPU through virgl
  ([docs/GPU.md](docs/GPU.md)); a terminal running zinc opens at boot.
* **A board.** ARMv7-A is the Cortex-A7 of the STM32MP157, and Ferrix boots
  an STM32MP157D-DK1 from its SD card ([the board guide](docs/stm32mp157-dk.md)).

Still to come, in the roadmap's order: the rest of stage 19 (XWayland and the
second-pass effects), namespaces, cgroups and seccomp (13), real-time domains
(14), a real init (the rest of 15), self-hosting (20), bare metal with a GPU
of Ferrix's own (21), and Steam (22).

Every boot proves its own claims rather than asserting them: the memory map
is checked against the loader's allocations, the direct map is checked to
alias physical memory, the allocators are hammered and required to give every
frame back, four processors increment one counter and must reach exactly
100,000, and the checks carry negative controls that must be seen to fail.

## No assembly at boot

Every architecture boots via UEFI — EDK2 on the 64-bit pair, U-Boot on
ARMv7-A — so firmware calls a Rust `efi_main` with a stack set up and the MMU
on. There is no bootstrap assembly on any of them, which is unusual and is a
direct consequence of choosing UEFI over Multiboot or a bare kernel boot.

The assembly that does exist — 437 lines against some 211,000 of Rust,
**99.79% Rust** — is confined to constructs the machine defines before a Rust
function could run: installing a translation regime and jumping to an address
that did not exist a moment earlier, trap and system-call entry, the context
switch, and the CPU primitives with no Rust spelling. `docs/ASSEMBLY.md` is
the argument for each one; `scripts/check-asm-budget.py` fails the build on
any site that is not on the list, on a file over its budget, and on an entry
that has gone stale.

That number is a trend, not a gate. Assembly here is a fixed cost that does not
grow with the system, so the percentage rises as the OS is written.

## Getting started

```
cargo xtask build     --arch all --release    # bootable images in build/
cargo xtask run       --arch x86_64           # boot it, serial on your terminal
cargo xtask run       --arch x86_64 --net     # ...with a network, 10.0.2.15 behind a NAT
cargo xtask run       --arch x86_64 --reset-root  # ...on a freshly installed btrfs root
cargo xtask run       --arch x86_64 --tmpfs-root  # ...with / in memory instead
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

`run` and `run-compositor` boot with `/` on btrfs: `build/root.img`, a 1 GiB
volume made empty from `mkfs.btrfs`'s `root` fixture the first time and kept
after that. The kernel starts on the initramfs as always, and once its disk
driver is up it switches `/` onto the volume, as Linux's `switch_root` does,
with `/dev`, `/proc` and `/tmp` mounted inside it. The first boot installs
the system — the whole initramfs — onto the volume; a later boot installs it
again only when the initramfs has changed, replacing the files it carries and
leaving everything else you put there. What a program writes is on the image
once it calls `sync` or `fsync`, and everything else within 30 seconds, when
the kernel commits it the way Linux's btrfs does by default; closing the
window can lose that last half-minute, and nothing more, because a commit is
all or nothing (stage 12's power-fail test is the evidence). The host can
read the image too — `btrfs check build/root.img`, or a loop mount on Linux.

`--reset-root` throws the volume away and installs the system on a new one.
`--tmpfs-root` boots with `/` in memory instead, as every boot did before,
and leaves the volume untouched for the next boot that wants it; on a kernel
command line, `ferrix.root=tmpfs` does the same. The test boots never attach
the volume, so their `/` is the tmpfs and what is on it cannot change what a
test sees.

On x86-64, `run` and `run-compositor` also attach the stage 16 rustc sysroot
when `scripts/fetch-rustc-sysroot.sh` has made
`~/.local/share/ferrix/rustc/rustc.img` (or `$FERRIX_RUSTC_SYSROOT/rustc.img`).
The kernel mounts that btrfs disk at `/data`; QEMU's snapshot mode keeps
guest writes from changing the source disk. The default system installs links
for glibc and gcc, plus `/bin/rustc`, `/bin/cargo` and `/bin/cc`, so
the Rust toolchain is on the PATH in an interactive shell or desktop terminal.
Run `scripts/fetch-rustc-sysroot.sh` again to add Cargo and the standard
libraries Ferrix's own image is built against to a sysroot made before
them. Boots with the toolchain use 4 GiB RAM unless `--memory` overrides
it. Without the fetched disk, those boots still start and say why rustc is
absent. The FAT boot image and 1 GiB `root.img` do not contain the 1.7 GiB
toolchain; the sysroot is a companion disk, so copying the FAT image alone
will not carry rustc to another host.

`cargo xtask test-selfhost --accel kvm` is stage 20's gate, on a Linux host
with `btrfs-progs`. It stages the sysroot's tree, every file git tracks here
as the work tree has it, and `cargo vendor --offline` of the workspace's
crates (run `cargo fetch` once if Cargo's cache lacks one) into
`build/x86_64/selfhost/`, makes a writable btrfs volume of them, and boots
Ferrix with 8 GiB to run `cargo xtask build --arch x86_64` on it. After the
guest powers off, `btrfs check` judges the volume, `btrfs restore` takes the
image out, and the boot test boots it. The build boot's serial log is kept in
`build/x86_64/selfhost/build-serial.log`.

`--net` gives the guest a virtio-net card whose other end is `xtask`'s own
gateway on a loopback UDP socket: the guest is `10.0.2.15`, `10.0.2.2` is the
host, `10.0.2.3` forwards DNS to the host's resolver, and TCP and UDP to
anywhere else are relayed through ordinary host sockets. It needs no
privilege, and works the same on Linux and on Windows. With `--init`, the
shell configures `eth0` by DHCP before its first prompt, with busybox's
`udhcpc`, so `wget http://example.com` and `ping 10.0.2.2` work at once.

`--forward <host>:<guest>` goes the other way: the gateway listens on the
host's `127.0.0.1:<host>` and opens each connection to the guest's `<guest>`,
as slirp's `hostfwd` does. It turns `--net` on. With it, you can reach the
guest over SSH. Every x86-64 image with a busybox carries
[sshdt](https://crates.io/crates/sshdt), an SSH server written in Rust, once
`cargo xtask ports` has built it. Start it in the guest, then connect from the host:

```
cargo xtask run --arch x86_64 --init ferrousli --forward 2222:22

ferrix# sshdt -b 0.0.0.0 -p 22 --pubkey 'ssh-ed25519 AAAA... you@host' &
        # or --authorized-keys FILE, or --password SECRET

ssh -p 2222 root@127.0.0.1
```

sshdt makes a host key at `/.sshdt/host_ed25519` the first time it starts.
The initramfs is rebuilt every boot, so the key changes each time, and `ssh`
will warn that the host key changed. Add `-o UserKnownHostsFile=/dev/null
-o StrictHostKeyChecking=no` for a throwaway guest. Every session runs as the
user who started sshdt, whatever name the client logs in with.

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
`top` work where you type them. Where zinc is built (below), `sh` is zinc's
rather than busybox's, and on x86-64 the uutils family owns the names it
provides; busybox keeps the rest, which [docs/UUTILS.md](docs/UUTILS.md) §8
counts down.

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
written in Rust, whose goal is to run oh-my-zsh. It is `/bin/sh` as well, so
the shell the kernel starts is zinc. It is built for x86-64 and AArch64 by `cargo` alone, against the
target's own musl and linked by rust-lld, so Windows needs nothing else.

`cargo xtask test-shell --arch x86_64 --init target/zinc/x86_64-unknown-linux-musl/release/zinc`
runs stage 7's script with zinc as the first program instead of busybox.

The shell starts configured. `cargo xtask omz --from <DIRECTORY-OR-URL>`
installs a checkout of oh-my-zsh on the build machine -- from a directory, or
from a repository to clone, which is the one place a build reaches the network
-- and every image built afterwards carries it at `/usr/share/oh-my-zsh` with
an `/etc/zshrc` that sources it. A boot then ends at oh-my-zsh's prompt, with
its aliases and its completion, rather than at a bare shell somebody would
have to install it into over a network the guest may not have. An image built
on a machine without the checkout says so and boots without it.

oh-my-zsh calls zsh's own functions -- `compinit`, `is-at-least`,
`add-zsh-hook`, `colors` -- which zsh autoloads from its function tree rather
than building in. `cargo xtask zsh-functions --from <DIRECTORY>` installs that
tree from a Linux machine's `/usr/share/zsh/functions` (on Windows, WSL's, as
`\\wsl.localhost\<distribution>\usr\share\zsh\functions`) or from a zsh
source checkout, and every image carries it at `/usr/share/zsh/functions`,
where zinc's `fpath` looks. Without it oh-my-zsh starts with a
`command not found` for each of them.

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
cargo xtask run-compositor --arch x86_64 --no-gl     # the same, drawn in software
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
starts is yours. `--layout de,us` is `input:kb_layout`, and without it the
layout is this machine's own, read from `/etc/default/keyboard` or
`/etc/vconsole.conf` (`--layout us` for the old default); `--size <W>x<H>` is
the screen, 1920x1080 when not given. The boot has a network unless
`--no-net` -- `udhcpc` runs as an `exec-once`, so `curl` in the terminal
reaches the host's own resolver -- and `--vnc <DISPLAY>` serves the screen at
e.g. `:0` rather than opening a window, which is what a machine reached over
`ssh` wants. `--gl` asks QEMU for `virtio-gpu-gl-pci`, the 3D card, with this
host's GPU behind it through virglrenderer; [the GPU decision](docs/GPU.md)
says what that gives and what it does not, and `FERRIX_QEMU` names a QEMU
that is not the one on `PATH`. A screen served over VNC gets the 3D card
without being asked, where a QEMU on `PATH` has it and the host has a render
node to draw on, because a video wallpaper is 38 frames a second in software
and 61 on the GPU ([§3.9](docs/GPU.md)); `--no-gl` keeps it in software, and a
window on this host keeps the 2D card unless `--gl` asks.

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
decoder: `ffmpeg` takes ten seconds of it at thirty frames a second, the
screen's size, and keeps the frames as AV1 in IVF -- about a megabyte, where
a single raw 1920x1080 frame is 8.3 MB. `run-compositor` starts
`/bin/pattern --video` on it.

How much is kept is a person's to change, and is not a compositor option:
Hyprland has none for a wallpaper either, which is why this machine's
`hyprland.conf` says `exec-once = booru-wallpaper daemon` and that daemon
keeps its own commented file. So does this one --
`~/.config/ferrix/wallpaper.toml`, written with its defaults in it the first
time `wallpapers` runs:

```toml
fps = 30        # frames a second kept
seconds = 10    # seconds kept, after which it begins again
scale = 1.0     # frame size as a multiple of the screen; 0.5 is a quarter
                # of the pixels and a quarter of the guest's decoding
size = ""       # an exact frame size instead, as "1280x720"
name = ""       # which wallpaper to show, by part of its name
```

`--fps`, `--seconds`, `--video-size` and `--wallpaper` beat the file, the
file beats the defaults, and a line it cannot read is said and skipped
rather than fatal.
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

### Watching a desktop on another machine

A machine reached over `ssh` has no window to open on, so QEMU serves the
screen instead and a viewer here connects to it. Nothing extra has to be
turned on: `run-compositor` asks this QEMU what display backends it was built
with and this host whether there is a session to open a window on, and a
shell with no `DISPLAY` and no `WAYLAND_DISPLAY` gets VNC on
`127.0.0.1:5900` with a line saying so. `--vnc <DISPLAY>` asks for it
outright, which is what a host that *could* open a window -- Windows, macOS,
a desktop you are sitting at -- needs in order to serve one instead.

On the machine with the QEMU:

```
cargo xtask run-compositor --arch x86_64 --vnc :0
```

On the machine with the eyes, a tunnel and a viewer. Display *n* is port
5900 + *n*, as everywhere else that speaks the protocol:

```
ssh -L 5900:127.0.0.1:5900 <host>
vncviewer localhost:5900          # RealVNC, TigerVNC, Remmina, macOS Screen Sharing
```

Or both at once, the tunnel carrying the command that fills it:

```
ssh -L 5900:127.0.0.1:5900 <host> "cd <the checkout> && cargo xtask run-compositor --arch x86_64 --vnc :0"
```

The serial console stays where it always was: on that `ssh` session's own
output, which is where the compositor says what mode it set and where a
panic would be. The screen and the console are two different pipes and it is
worth keeping both in view.

**The address is a loopback one on purpose.** `-vnc` without `password=on`
accepts any client, and nothing here turns a password on, so a wider address
is a machine anyone who can reach it can drive. `--vnc` takes QEMU's own
spellings unchanged -- `:0`, `127.0.0.1:0`, `0.0.0.0:2` -- and a bare number
is that display on the loopback. Binding something other than `127.0.0.1` is
a thing you say deliberately, and then the tunnel is the part you have
dropped, not a step you have skipped.

What a viewer sees is the card the compositor draws on, not the firmware's
head: VNC can be told which console to serve and xtask tells it
(`display=<card>,head=0`), so the connection lands on the desktop rather
than on the loader's text. That is the one thing a window cannot do -- GTK
opens on whichever console QEMU made first and the other is a tab in the
View menu.

Everything else about the desktop is unchanged by being served rather than
shown. `--size`, `--layout`, `--variant`, `--config`, `--wallpaper`,
`--clipboard`, `--smp`, `--memory`, `--release`, `--no-net` and a
`hyprland.conf` of your own all mean what they mean above. Four things are
worth knowing before you go looking for a fault that is not there:

| | |
|---|---|
| `--gl` | wants a QEMU built `--enable-opengl --enable-virglrenderer`, and is what a served screen gets without it being said, where it can be had. A build without one has no `virtio-gpu-gl-pci`, a line says so, and the boot goes on with the 2D card -- it does not fail. With the 3D card present the screen is drawn off screen through `egl-headless` and copied out to the VNC server, so `--gl --vnc` is a real combination and not a fallback. On a machine with two QEMUs the first on `PATH` that has the card is used and a line says which: a source build is often the one *without* it, and the distribution's package the one with |
| `--rendernode <PATH>` | which GPU draws, as `/dev/dri/renderD128`. `run-compositor` takes the first node whose driver is not NVIDIA's proprietary one when nothing says; other boots leave it to QEMU, which takes the first render node it opens, which on a machine with one GPU is that GPU and on a machine with several is a guess by device number -- and the wrong guess is a proprietary driver that does not do what virglrenderer asks, or an idle card while the fast one watches. `ls -l /dev/dri/by-path/` says which node is which card, and the driver behind one is `basename $(readlink -f /sys/class/drm/renderD128/device/driver)`. Only `egl-headless` takes it, which is the served screen and the headless `test-*` boots; a window's GL goes to the host display's GPU and QEMU gives nobody a say in that |
| `--screens 2` | puts two cards on the bus and the compositor tiles across both, but a VNC server serves one console, and that console is the first card. The second monitor is there and being drawn; you are watching one of them. `test-compositor`'s monitor boot is what judges both |
| `SUPER` | every binding in the keyboard table above is on it, and whether a viewer passes it to the guest or eats it for its own menu is the viewer's business, not Ferrix's. Most have a setting for it, or a menu that sends one key |
| speed | a desktop over a tunnel is a desktop over a tunnel. VNC sends what changed, and the compositor already damages only the rows that did, so a shell prompt is fine and a video wallpaper on a machine that has to emulate is the frame a second it already was |

Two ways this refuses rather than guesses. `--vnc` given to a boot with no
screen at all says so and names the two commands that have one, because a
display to serve is the thing it was missing. And a QEMU built
`--disable-vnc` -- asked with `-vnc help`, and judged by what it printed --
says that too, rather than starting and dying on an option it does not have.

#### One command for a machine you use often

The four steps above -- send the code, boot it there, tunnel, open a viewer
-- are the same every time except for the first, and that is the one worth
getting right: a boot of yesterday's code looks exactly like a boot of
today's. `remote-desktop` does all four from a file of answers.

```
cargo xtask remote-desktop
```

Copy [`scripts/remote-desktop.toml.example`](scripts/remote-desktop.toml.example)
to `~/.config/ferrix/remote.toml` -- or to `remote-desktop.toml` here, which
git ignores, or anywhere and name it with `--config` or `$FERRIX_REMOTE` --
and fill in the one key that has no default:

On Windows, that home configuration is
`%USERPROFILE%\.config\ferrix\remote.toml` (in PowerShell,
`$env:USERPROFILE\.config\ferrix\remote.toml`). The command reads existing
configurations from the former Python script unchanged; Python is no longer
needed for this command.

```toml
[remote]
host = "the-name-in-your-ssh-config"

[boot]
args = ["--release", "--size", "1600x900", "--layout", "de,us"]
```

The copied example is the full desktop flavour: release builds, 3D graphics,
clipboard sharing, 1920x1080, two keyboard layouts, a wallpaper, four CPUs
and 1 GiB of memory. Replace its wallpaper placeholder and pare flags back
for a smaller profile.

`host` is an `ssh` destination and nothing more: the user, the key and a
`ProxyJump` two hops away stay in `~/.ssh/config`, where they already are.
No machine is named anywhere in this repository, which is the rule the rest
of `xtask` follows too -- no host is a default and the network is only what
an argument asked for. `--host <destination>` is the other way to name one,
for a machine tried once, and it needs no file at all.

What the run does, and the two parts of it that are not obvious:

| | |
|---|---|
| sends the **working tree** | uncommitted changes and all, because "does my change work" is the question being asked. It builds the commit in a private `GIT_INDEX_FILE`, so your index is untouched and no branch moves; `[source] send = "head"` sends your last commit instead, and `--send head` says so for one run |
| pushes to a **side ref** | `refs/ferrix-desktop/head` in a checkout it makes the first time, not a branch -- a checkout refuses a push to the branch it has checked out, and a ref outside `refs/heads` is never that branch. The remote `target/` survives between runs, so the second boot builds almost nothing |
| one `ssh` | carries both the forward and the boot, so the tunnel lives exactly as long as the machine does and neither can outlive the other |
| waits for `RFB` | the protocol's own greeting, not a connection: `ssh` accepts on a forwarded port from the moment it starts, so a connection proves only that `ssh` is running |
| opens the viewer | RealVNC, TigerVNC, UltraVNC, Remmina or macOS Screen Sharing, found on this machine; `[screen] viewer` takes a command line instead, or `none`. Closing the window stops the boot, and the boot ending closes the window |

`--print-command` says what all of that would be without doing any of it,
which is the first thing to run when something is not where you expected it.
`--stop` ends a boot left running over there, which happens when the command
does not get to clean up after itself -- Ctrl-C, a hard kill, a laptop
closing -- and matters because a QEMU nobody is watching still holds that
machine's memory and its VNC port against the next run. Closing the
connection is not enough on its own; that is measured, not assumed.
`--vnc :1` and `--local-port` move the screen for a run -- two people on one
machine want two displays -- and `[screen] local_port = "auto"`, the default,
steps past a port this machine is already using rather than showing you
someone else's desktop. `--send head` boots your last commit instead of your
working tree for one run, and `--no-viewer` opens the tunnel and nothing
else. Anything after `--` goes to the remote `cargo xtask` as it stands,
whether or not this end knows the option:

```
cargo xtask remote-desktop -- --wallpaper none --smp 8
```

`--layout de` is the guest's keyboard layout, and `--viewer` picks the
viewer, over `[screen] viewer`. The two named ones send the keyboard
differently. `tigervnc` sends keys, which the guest's layout reads as a
real keyboard's, dead keys and `AltGr` included. `realvnc` sends
characters, which QEMU turns back into keys through a keymap, so the boot
is given `--keymap` with the first layout as well:

```
cargo xtask remote-desktop --layout de                     # TigerVNC, keys
cargo xtask remote-desktop --layout de --viewer realvnc    # RealVNC, -k de
```

The serial console comes back on this terminal throughout, which is where a
boot that never reaches a screen says why.

The guest can be logged into as well. `remote-desktop` asks `run-compositor`
for `--ssh 22022`, which starts sshdt in the guest from the compositor's
`exec-once`. The same `ssh` connection carries a second tunnel, and the run
prints the command, usually `ssh -p 22022 root@127.0.0.1`. The keys that may
log in to the remote machine may log in to the guest: its `authorized_keys`
and the public keys beside it. The guest's host key is kept over there, in
`~/.local/share/ferrix/ssh`, so it is the same every boot. `[ssh] port = 0`
turns it off. `cargo xtask run-compositor --ssh <port>` does the same on this
machine, with this machine's keys.

None of this is needed to *test* a desktop on a remote machine.
`test-compositor`, `test-display`, `test-video`, `test-input`, `test-seat`
and `test-pty` read the screen with a QMP screendump and open no window and
no server at all; they are what `cargo xtask check` runs and they run over
`ssh` with nothing forwarded. VNC is for the case where the judging is
yours.

### Everything at once

One command with every part of Ferrix that composes with the others turned
on. The few that do not are under it: a screen served instead of shown, a
second monitor, and the network, which is on already.

```
FERRIX_QEMU=<a QEMU with a window and virglrenderer, if the one on PATH has neither> \
cargo xtask run-compositor --arch x86_64 --release --gl --clipboard \
    --size 1920x1080 --layout de,us --variant nodeadkeys, \
    --wallpaper <part of a name> --smp 4 --memory 1024
```

For the same full flavour on a remote machine, copy the full-profile example,
replace its host and wallpaper placeholder, then run:

```
cargo xtask remote-desktop
```

The profile supplies release builds, 3D, clipboard, 1920x1080, both keyboard
layouts, a wallpaper, four CPUs and 1 GiB of memory. `remote-desktop` supplies
`--vnc :0` itself, so the remote QEMU serves the desktop through the tunnel
instead of opening a window on the build machine, and a served screen gets
the 3D card and a render node by itself. Set `[remote.env] FERRIX_QEMU` only
when no QEMU on that machine's `PATH` has virglrenderer, and `--rendernode`
only to overrule the node it picks.

That is the desktop on the 3D card, in a window of this host's, at
1920x1080, with two keyboard layouts a switch moves between and no dead keys
on the first, that wallpaper, four CPUs, twice the default memory, a
network, and a terminal running zinc already open. Every flag is described
above or in `cargo xtask --help`; these are the ones worth saying twice,
the last three being the ones not in the line:

| | |
|---|---|
| `FERRIX_QEMU` | a QEMU that is not the one on `PATH`, a directory of its binaries or one binary. Distributions often build QEMU without a local display backend, and then there is no window to open -- xtask asks whichever QEMU it is what it has, falls back to VNC when it has none, and says which it chose. `--gl` wants virglrenderer as well |
| `--gl` | the 3D card, with this host's GPU behind it. Turns `--display` on by itself. A served screen has it without the flag where it can be had; `--no-gl` is the 2D card, on which the compositor composites on the CPU |
| `--rendernode` | which GPU, on a machine with more than one, as `/dev/dri/renderD128`. Only `egl-headless` takes it -- a served screen and the headless `test-*` boots. `run-compositor` takes the first node not driven by NVIDIA's proprietary driver when nothing says; other boots leave it to QEMU, whose pick is a guess by device number rather than by which card can do the work |
| `--smp`, `--memory` | 4 and 512 MiB by default. A desktop with a video wallpaper is the one workload here that notices more of either |
| `--release` | builds the loader and kernel with optimisations. Worth it for a desktop somebody is going to use rather than watch boot |
| `--wallpaper` | matches part of the name of one kept by `cargo xtask wallpapers`. Leave it out and a run picks one of them, a different one each time; `--wallpaper none` for a plain background. A **video** in that directory becomes a wallpaper that moves, which is the most expensive thing this desktop does -- about a frame a second on a machine that has to emulate |
| `--vnc :0` | instead of a window, for a machine reached over `ssh`. `--gl` still works: the frames are drawn off screen and copied out. [Watching a desktop on another machine](#watching-a-desktop-on-another-machine) is the whole of it -- the tunnel, the viewer, and what a served screen does not carry |
| `--screens 2` | two cards, so two monitors, which the compositor tiles across and `test-compositor`'s monitor boot judges. Accepted by `run-compositor` too, though the boots that are gated are the headless ones |
| `--no-net` | the one thing in that command that is on by default and can only be turned *off* |

`--everything` is all of it at once, for a desktop somebody is going to use:
`--gl`, `--release`, `--clipboard` and `--chrome`, with `rustc` and `cargo`
in the shell beside Chrome. The kernel mounts one data disk, so it makes a
third volume at `~/.local/share/ferrix/everything` out of the two trees the
fetch scripts keep -- hard links, not a copy -- and makes it again when
either is fetched again. Both volumes have to have been fetched. It is
`run-compositor`'s alone: the gates still boot exactly the devices each asks
for, since every device a boot does not need is one fewer on the bus, and
several of them exist to assert exactly what a machine enumerates.

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
whoever is watching is there and QEMU is talking on it. In the guest, the
ring-3 driver `user/vport` opens the port and offers it at `/tmp/vport`, but
**nothing speaks the agent protocol over it yet** -- the vdagent program and
the terminal's paste are still to come, so copy and paste between Ferrix and
the host does not work, and pressing `CTRL`+`V` will do nothing across that
boundary.
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
| `libs/` | Architecture-neutral logic, some forty crates: the hand-off ABI, the ELF reader, paging, the allocators, the scheduler's queues, the VFS, btrfs read and write, the net stack, the virtio device protocols, the native ABI. Host-testable **by design** — it is the only code `cargo test`, Miri and the fuzzers can reach. |
| `boot/` | The UEFI loader. Reads the kernel, builds the address space, leaves firmware. |
| `kernel/` | The kernel. |
| `user/` | Native ring-3 programs: `devmgr`, the block, net, GPU, input and console drivers, and the runtime they share. |
| `ferrousli/` | [A C library written in Rust](ferrousli/README.md), with its own dynamic linker. Its own workspace. |
| `zinc/` | [A zsh-compatible shell](zinc/README.md). Its own workspace. |
| `compositor/` | [hyprix, the terminal and the Wayland pieces](compositor/README.md). Its own workspace. |
| `xtask/` | Host build driver: cross-compiles every half, converts the 32-bit loader from ELF to PE, writes the FAT32 image and initramfs, drives QEMU and every `test-*` gate. |
| `fuzz/` | The fuzz targets over `libs/`. |
| `scripts/` | The quality gates, generators and sysroot fetchers. |
| `docs/` | [Architecture](docs/ARCHITECTURE.md) · [Roadmap](docs/ROADMAP.md) · [Backlog](docs/BACKLOG.md) · [Assembly](docs/ASSEMBLY.md) · [Reliability](docs/RELIABILITY.md) · [Boot log](docs/BOOT-LOG.md) · [Display](docs/DISPLAY.md) · [GPU](docs/GPU.md) · [Conventions](docs/CONVENTIONS.md) · [SysML v2 model](docs/sysml/README.md) |

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
