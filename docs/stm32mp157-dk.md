# Booting Ferrix on an STM32MP157-DK

What to do when the SD card arrives, and what to expect at each step.

Everything here is for the **DK1 and DK2**. The ED1 and EV1 have 1 GiB rather
than 512 MiB, which puts their RAM's identity range on top of the direct map;
`Layout::plan_identity_map` returns an error naming the collision rather than
guessing, so those boards need a trampoline page that is not written yet.

**Nothing in this document has been run on hardware.** QEMU has no STM32MP1
model, so the board is the first execution of the paths described here. What
*has* been checked is listed under [What is actually verified](#what-is-actually-verified).

## TL;DR

Card in the reader, board off:

```sh
lsblk -o NAME,SIZE,TYPE,TRAN                     # find the card. CHECK THIS.
sudo dd if=FlashLayout_sdcard_stm32mp157x-dk-optee.raw of=/dev/sdX bs=8M conv=fsync status=progress
```

Re-insert the card so the desktop mounts it, then:

```sh
cargo xtask deploy --arch armv7a
```

Card into the board, micro-USB into the **ST-LINK** port, power on, stop
autoboot with any key:

```
=> load mmc 0:4 0xc2000000 EFI/BOOT/BOOTARM.EFI
=> bootefi 0xc2000000 ${fdt_addr_r}
```

Success is a line beginning `FERRIX-BOOT-OK` — the stages after it say how far
the boot got — and `deploy` exits 0 on it.

If it goes wrong, three arguments in order of usefulness:

```
=> setenv bootargs 'console=stm32'   # loader talks, then silence
=> setenv bootargs 'nosmp'           # stops partway through stage 4
=> setenv bootargs 'noactlr'         # panics reading ACTLR
```

Each is explained under [When it goes wrong](#when-it-goes-wrong). The rest of
this document is why, and what to do when the short version does not work.

> **`deploy` must run in a shell that has the `dialout` group.** Adding
> yourself does not affect shells that are already open, and the symptom is a
> permission error opening the port rather than anything about the board. Either
> log out and back in, or prefix the command once:
> `sg dialout -c 'cargo xtask deploy --arch armv7a'`.

## What you need

* An STM32MP157A-DK1 or STM32MP157C-DK2.
* A micro-SD card, 8 GB or more.
* A USB-C cable for power, and a micro-USB cable for the ST-LINK.
* Your user in the `dialout` group, for the serial port:
  `sudo usermod -aG dialout $USER`, then log in again. `id -nG` should list it.

## 1. Put the vendor's firmware on the card

Ferrix does not replace TF-A, OP-TEE or U-Boot, and cannot: the ROM loads TF-A
from a partition it finds by name, TF-A loads OP-TEE and U-Boot, and only then
is there anything that can read a filesystem. So the card starts as a normal
OpenSTLinux card.

Download the **OpenSTLinux starter package** for your board from ST, and write
its image to the card:

```sh
# Whole-card image from the starter package. CHECK THE DEVICE NAME FIRST:
lsblk -o NAME,SIZE,TYPE,MOUNTPOINT,TRAN
sudo dd if=FlashLayout_sdcard_stm32mp157x-dk-optee.raw of=/dev/sdX bs=8M conv=fsync status=progress
```

`dd` to the wrong device destroys that device. `lsblk` first, every time, and
match the size to the card.

Then re-insert the card. The desktop mounts its FAT partition — the one
OpenSTLinux calls `bootfs` — somewhere under `/media/$USER/` or `/run/media/$USER/`.

## 2. Build, flash and watch, in one command

```sh
cargo xtask deploy --arch armv7a
```

That builds the loader and kernel, copies them onto the card, and then watches
the ST-LINK serial port for the boot report. It is three commands in one; each
also exists alone:

```sh
cargo xtask flash        --arch armv7a [--to /media/you/bootfs]
cargo xtask watch-serial [--port /dev/ttyACM0] [--timeout 120]
```

`--to` and `--port` are optional. Without them each finds the only candidate
and **refuses to choose when there is more than one**, because guessing wrong
means either watching a port nothing is driving, or writing a boot loader onto
the wrong filesystem. `flash` additionally refuses `/boot/efi` by name: it is a
mounted FAT like any other and would otherwise pass every check, and it is the
one destination that can stop *this* computer booting.

Two files are copied, which are the two the image contains:

```
EFI/BOOT/BOOTARM.EFI    the loader, where firmware looks with no boot entry
FERRIX/KERNEL.ELF       the kernel, where the loader looks
```

Nothing else on the card is touched.

## 3. Tell U-Boot to boot it

Power on with the serial port attached and hit a key to stop autoboot, then:

```
=> setenv bootargs ''
=> load mmc 0:4 0xc2000000 EFI/BOOT/BOOTARM.EFI
=> bootefi 0xc2000000 ${fdt_addr_r}
```

`mmc 0:4` is the `bootfs` partition on the standard OpenSTLinux layout; `mmc
part` lists them if yours differs. Passing `${fdt_addr_r}` matters — that is
the device tree, and the kernel finds its console, its processors, its timer
and its interrupt controller in it. Without it nothing works.

To make it the default once it boots:

```
=> setenv bootcmd 'load mmc 0:4 0xc2000000 EFI/BOOT/BOOTARM.EFI; bootefi 0xc2000000 ${fdt_addr_r}'
=> saveenv
```

## 4. What a good boot looks like

```
Ferrix loader 0.1.0 (armv7a)
  device tree copied, ...
  direct map of 0xc0000000..0xe0000000, kernel at ...
Ferrix 0.1.0 on armv7a
  memory   511 MiB total, ...
  fdt      st,stm32mp157...
  stage 1  loader hand-off verified
  ...
  coherency ACTLR.SMP set on all 2 processors
  cpus     2 described by firmware, 2 online, booted on MPIDR 0x0
  stage 4  2 processors online, a contended counter came to 100000 of 100000
  tasks    1000 threads run to completion on 2 processors, ... switches, ... steals
  sleep    one task slept ... us and came back
  fair     ... spinners on every processor, worst lag ... within a bound of ...
  stage 5  1000 threads scheduled fairly across 2 processors
  w^x      ... mappings swept, ... executable, none writable
  reclaim  ... MiB from the loader and ACPI, ... free
FERRIX-BOOT-OK stages 1-5
```

`watch-serial` exits 0 on that last line and non-zero on `FERRIX-PANIC`, so it
is a test rather than something to read.

## When it goes wrong

### Nothing on the serial port at all

The loader's own lines come from firmware's console, so if even
`Ferrix loader` is missing the problem is before us: check the micro-USB cable
is in the ST-LINK port, that `/dev/ttyACM0` exists, and that you are in
`dialout`.

### The loader talks, then silence

The most likely single failure, and the reason `console=` exists. The loader
prints through firmware; the kernel prints through its own driver, so silence
starting exactly at the hand-off means the kernel chose the wrong UART or
mapped the wrong address. UART4 is the DK's console and an STM32 USART, not a
PL011. Force it:

```
=> setenv bootargs 'console=stm32'
```

`console=pl011` is the other way. A name that matches nothing falls back to the
ordinary search, so a stale value cannot take the console away permanently.

### It stops partway through stage 4

Suspect the second core, and read the `coherency` line above it.

A Cortex-A7 must have `ACTLR.SMP` set before its caches come on or it is not
coherent with the other core, and the non-secure world is not allowed to set
it — so it is firmware's job. A core without it runs, takes interrupts, passes
everything that reads only its own memory, and then loses counts in stage 4's
shared counter. That reads as a bug in the lock or the barriers, which is why
the bit is reported before the test that depends on it.

* `set on all 2 processors` — coherency is fine, look elsewhere.
* `set on 1 of 2 processors` — unambiguous: firmware missed a core.
* `clear on all 2 processors` — ambiguous. Either firmware left it, or the
  register is not implemented. QEMU reports exactly this and passes anyway.

To take the second core out of the picture entirely:

```
=> setenv bootargs 'nosmp'
```

One core reaching the end of stage 3 tells you the loader, the hand-off, the
page tables, the console and the timer are all correct, and that what is left
is the thing you just switched off.

### It panics reading ACTLR

`noactlr` skips that read. It should not be needed — the read is
architecturally permitted from the non-secure world — but this kernel has never
run on this silicon, and an escape hatch that needs no rebuild is worth the
line it costs.

Options combine: `setenv bootargs 'console=stm32 nosmp'`.

## What is actually verified

| Claim | How |
|---|---|
| The above-2-GiB identity-map placement | host unit tests in `libs/bootinfo` |
| STM32 USART register offsets | against Linux's `stm32h7_info`: `isr 0x1c`, `tdr 0x28`, `TXE` bit 7 |
| UART4 is the DK console at `0x40010000` | `stm32mp151.dtsi`, and `stdout-path = serial0:115200n8` in `stm32mp15xx-dkx.dtsi` |
| DK RAM is 512 MiB at `0xc0000000` | `memory@c0000000 reg = <0xc0000000 0x20000000>` |
| CPU nodes carry no `enable-method` | the same device trees; hence the PSCI default |
| The kernel still boots everywhere | `cargo xtask test-boot --arch all`, three architectures |
| `watch-serial` detects both markers | against a pseudo-terminal |
| `flash` refuses wrong destinations | unit tests, and tried against `/boot/efi` |

Not verified: the board. Also unverified are `console=`, `nosmp` and `noactlr`
*as selections* — their parsing has host tests, but the paths they choose are
only reachable on hardware, which is the point of them.

The upstream firmware facts behind this were checked against mainline TF-A,
OP-TEE, U-Boot and Linux — **not** against ST's forks, which is what
OpenSTLinux actually ships.
