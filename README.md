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
cargo xtask test-boot --arch all              # boot it and assert it came up
cargo xtask test-boot --accel auto            # ...on the real MMU, where it can
cargo xtask check                             # every gate CI runs
```

`--accel auto` boots on the host processor instead of QEMU's interpreter —
`whpx` on Windows, `kvm` on Linux, `hvf` on macOS, and `tcg` when there is
none or when the guest is not the host's architecture. Worth running before
believing a change to page tables or invalidation, for the reason
[Reliability](docs/RELIABILITY.md) gives: an interpreted `MMU` has no `TLB`, so
a stale translation is a bug the default gate structurally cannot see.

You need QEMU and UEFI firmware. Debian and Ubuntu: `qemu-system-x86`,
`qemu-system-arm`, `ovmf`, `qemu-efi-aarch64` and `u-boot-qemu`. Windows:
`winget install SoftwareFreedomConservancy.QEMU`, which ships the 64-bit
firmware too but not U-Boot; for ARMv7-A, point `FERRIX_UBOOT` at a `qemu_arm`
`u-boot.bin`. Nothing else — the FAT32 image is written by `xtask`, so there is
no `mtools` or `dosfstools` to install and the image is byte-for-byte
reproducible.

### A busybox shell

Given a static busybox with `--init`, `build` and `run` put it in the kernel,
which starts `sh -i` on the console, and in the initramfs at `/bin/busybox`
with every applet linked beside it, so `ls /proc`, `cat /proc/self/maps` and
`top` work where you type them.

The busybox Ferrix is measured with is built against
[ferrousli](ferrousli/README.md), this repository's C library, and
`--init ferrousli` names it. It is x86-64 only for now, and built with a Linux
C compiler, so on Windows that one step runs in WSL; QEMU and everything else
run on Windows itself.

On Windows, once, from PowerShell:

```
winget install SoftwareFreedomConservancy.QEMU
wsl --install -d Ubuntu
wsl --set-default Ubuntu        # xtask uses the default distribution
```

and once inside Ubuntu (`wsl`):

```
sudo apt install build-essential curl bzip2 file
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then, from PowerShell in the checkout:

```
cargo xtask busybox                              # build busybox against ferrousli
cargo xtask run --arch x86_64 --init ferrousli   # boot to a busybox shell
```

Quit QEMU with `Ctrl-A x`.

`cargo xtask busybox` runs `ferrousli/tools/busybox/build.sh` in WSL, reaching
the checkout through `/mnt`. The script downloads busybox
1.37.0 and Alpine's configuration for it, both checked against pinned sums,
builds ferrousli and busybox against it under `~/.local/share/ferrix/busybox/ferrousli`
inside WSL, and copies the result to
`%USERPROFILE%\.local\share\ferrix\busybox\ferrousli\x86_64\bin\busybox.static`,
where `--init ferrousli` looks. It takes a few minutes the first time. Run it
again after `ferrousli/` changes; `--init ferrousli` uses whatever it last
installed. On Linux the same two commands work without WSL.

`cargo xtask test-shell --arch x86_64 --init ferrousli` runs a script in that
shell instead of waiting for you, and fails unless the script's output comes
back; `test-vfs` runs the file system's commands and applets the same way.
`--init` also takes the path of any other static busybox, with `{arch}` in it
replaced by each architecture's name; that is how the gates give it the musl
and glibc builds they check alongside.

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
