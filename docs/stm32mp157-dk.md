# Booting Ferrix on an STM32MP157-DK

What to do with a DK board and an SD card, and what to expect at each step.

Everything here is for the **DK1 and DK2**. The ED1 and EV1 have 1 GiB rather
than 512 MiB, which puts their RAM's identity range on top of the direct map;
`Layout::plan_identity_map` returns an error naming the collision rather than
guessing, so those boards need a trampoline page that is not written yet.

**What has run on hardware.** An STM32MP157D-DK1 has booted Ferrix through
stage 5, on 2026-09-12 at `6df0925`, with the firmware described in step 1.
Stages 6 onwards, and any program in user mode, have not yet run on the board:
`main` reports `FERRIX-BOOT-OK stages 1-9` under QEMU, which has no STM32MP1
model, so the board is still the first execution of everything after stage 5.
What *has* been checked is listed under
[What is actually verified](#what-is-actually-verified).

## TL;DR

Card prepared as in step 1, mounted on this machine, then:

```sh
cargo xtask deploy --arch armv7a
```

Card into the board, both **boot switches ON**, micro-USB into the **ST-LINK**
port, USB-C power in. Stop autoboot with any key, then send these one line at a
time:

```
STM32MP> setenv bootargs
STM32MP> load mmc 0:4 0xc2000000 EFI/BOOT/BOOTARM.EFI
STM32MP> bootefi 0xc2000000 ${fdtcontroladdr}
```

Success is a line beginning `FERRIX-BOOT-OK` — the stages after it say how far
the boot got — and `deploy` exits 0 on it.

If it goes wrong, three arguments in order of usefulness:

```
STM32MP> setenv bootargs 'console=stm32'   # loader talks, then silence
STM32MP> setenv bootargs 'nosmp'           # stops partway through stage 4
STM32MP> setenv bootargs 'noactlr'         # panics reading ACTLR
```

Each is explained under [When it goes wrong](#when-it-goes-wrong). The rest of
this document is why, and what to do when the short version does not work.

> **`deploy` must run in a shell that has the `dialout` group.** Adding
> yourself does not affect shells that are already open, and the symptom is a
> permission error opening the port rather than anything about the board. Either
> log out and back in, or prefix the command once:
> `sg dialout -c 'cargo xtask deploy --arch armv7a'`.

## What you need

* An STM32MP157A-DK1, STM32MP157D-DK1 or STM32MP157C-DK2.
* A micro-SD card, 8 GB or more.
* A USB-C cable and a 5 V / 3 A supply for power, and a micro-USB cable for the
  ST-LINK. The ST-LINK is powered from its own cable, so its serial port stays
  present while the board itself is off — a silent port is not proof the board
  is running.
* Your user in the `dialout` group, for the serial port:
  `sudo usermod -aG dialout $USER`, then log in again. `id -nG` should list it.

## 1. Put firmware on the card

Ferrix does not replace TF-A, OP-TEE or U-Boot, and cannot: the ROM loads TF-A
from a GPT partition it finds by name, TF-A loads OP-TEE and U-Boot, and only
then is there anything that can read a filesystem.

Two firmware chains can do that. ST's **OpenSTLinux starter package** is the
vendor's; its image is EULA-gated and has not been used with Ferrix. The chain
Ferrix has actually booted on is built from **mainline**, and it is the one this
document describes.

### The mainline chain

| Component | Version | Source |
|---|---|---|
| TF-A      | v2.14    | https://git.trustedfirmware.org/TF-A/trusted-firmware-a.git |
| OP-TEE OS | 4.10.0   | https://github.com/OP-TEE/optee_os.git |
| U-Boot    | v2026.07 | https://github.com/u-boot/u-boot.git |

The device tree is `stm32mp157a-dk1` for either DK1. Mainline carries no
`stm32mp157d-dk1.dts`; the D-grade DK1 is electrically the DK1, TF-A's
`stm32mp157d-dk1-fw-config.dts` is byte-identical to the `a-dk1` one, and the
only difference in the variant `.dtsi` is a crypto node the D grade lacks. The
one consequence is that PLL1 clocks the A grade's 650 MHz rather than the D's
800 MHz.

```sh
# OP-TEE (BL32)
make -C optee_os CROSS_COMPILE=arm-linux-gnueabihf- ARCH=arm \
     PLATFORM=stm32mp1-157A_DK1 O=out

# U-Boot (BL33). stm32mp15_defconfig is the FIP/TF-A configuration.
make -C u-boot stm32mp15_defconfig
./scripts/config --set-str DEFAULT_DEVICE_TREE "st/stm32mp157a-dk1"
./scripts/config --disable WDT_STM32MP        # see below: required on some DK1s
make -C u-boot olddefconfig && make -C u-boot CROSS_COMPILE=arm-linux-gnueabihf-

# TF-A (BL2 + FIP). arm-none-eabi-, NOT arm-linux-gnueabihf- — see below.
make -C tf-a CROSS_COMPILE=arm-none-eabi- PLAT=stm32mp1 ARCH=aarch32 \
     ARM_ARCH_MAJOR=7 AARCH32_SP=optee STM32MP_SDMMC=1 \
     DTB_FILE_NAME=stm32mp157a-dk1.dtb \
     BL33=../u-boot/u-boot-nodtb.bin BL33_CFG=../u-boot/u-boot.dtb \
     BL32=../optee_os/out/core/tee-header_v2.bin \
     BL32_EXTRA1=../optee_os/out/core/tee-pager_v2.bin \
     BL32_EXTRA2=../optee_os/out/core/tee-pageable_v2.bin \
     all fip
```

Two settings in there each cost a day to find:

* **TF-A must be built with `arm-none-eabi-`.** The Linux-targeted toolchain
  emits a `.note.gnu.build-id` that lands at address 0, pushing `.header` to
  `0x24` and making `objcopy -O binary` pad from 0 up to SYSRAM: an 805 MB
  `.stm32` whose header holds an address where its length belongs.
* **U-Boot's STM32MP watchdog driver must be off.** It decides whether the IWDG
  is already running from `SR_ONF` only on silicon with `IWDG_VERR >= 0x31`;
  below that it writes `RLR`, polls `RVU`, and treats success as proof. The DK1
  this was written against reads `VERR = 0x30`, the heuristic false-positives,
  and U-Boot force-starts a 32-second watchdog that nothing services once Ferrix
  runs. Every boot is then cut off at about 30 s with no panic and no output,
  which reads exactly like a hang. Rebuilding OP-TEE without its IWDG driver
  does **not** fix it. The consequence of turning it off is the one to remember:
  a Ferrix that panics stays halted until the board is reset.

### The card

| # | name | size | contents |
|---|---|---|---|
| 1 | `fsbl1`  | 2 MiB   | `tf-a-stm32mp157a-dk1.stm32` (BL2) |
| 2 | `fsbl2`  | 2 MiB   | an identical copy, the ROM's fallback |
| 3 | `fip`    | 4 MiB   | `fip.bin`, plus U-Boot's saved environment in the top 16 KiB |
| 4 | `bootfs` | 128 MiB | FAT32: Ferrix's loader, kernel and initramfs |

The ROM finds partitions 1 and 2 by name, so the GPT names matter. U-Boot's
own device tree sets `u-boot,mmc-env-partition = "fip"`, so `saveenv` writes to
the top of partition 3: do not shrink it below the FIP plus 16 KiB.

Before repartitioning, check nothing on the card is mounted. A desktop opens a
card as soon as it appears, and a mounted partition makes the kernel keep the
old geometry: the writes land at the wrong offsets while every command still
reports success. `lsblk` first, every time, and match the size to the card —
writing to the wrong device destroys that device.

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

To boot to a shell rather than stop after the self-checks, give `flash` or
`deploy` a static busybox, exactly as `build` and `run` take one:

```sh
cargo xtask deploy --arch armv7a --init /path/to/{arch}/bin/busybox.static
```

The kernel is built to start `sh -i` on the console, and the initramfs carries
busybox at `/bin/busybox` with a link beside it for every applet. `FERRIX_INIT`
works in place of `--init`. The binary must be static and hard-float ARMv7;
Alpine's `busybox-static` for `armv7` is.

> **The shell cannot read the keyboard on Arm yet.** Neither the STM32 USART
> driver nor the PL011 driver receives: `read_console_byte` returns nothing on
> both Arm architectures, so `sh -i` prints its prompt and never sees a
> keystroke. Only x86-64's 16550 reads input today.

Three files are copied, which are the three the image contains:

```
EFI/BOOT/BOOTARM.EFI    the loader, where firmware looks with no boot entry
FERRIX/KERNEL.ELF       the kernel, where the loader looks
FERRIX/INITRD.IMG       the initramfs, beside the kernel; busybox, given --init
```

Nothing else on the card is touched.

### Flashing with the card still in the board

Pulling the card for every build wears the slot and needs a hand at the board.
U-Boot can expose the card over the board's USB-C port instead:

```
STM32MP> ums 0 mmc 0
```

The desktop mounts `bootfs`; run `cargo xtask flash --arch armv7a --to <that
mount>`, then Ctrl-C at the U-Boot console to end mass-storage mode.

## 3. Tell U-Boot to boot it

The boot switches on the underside of the board select where the ROM looks:
**both ON boots from the SD card**. Both OFF is the ROM's USB DFU mode, which
is how the board arrives, and in which nothing at all appears on the serial
port.

Power on with the serial port attached and hit a key to stop autoboot. The
prompt on this firmware is `STM32MP>`, not the `=>` of other U-Boot builds — a
script waiting for `=>` never sees it. Then send, **one line at a time**:

```
STM32MP> setenv bootargs
STM32MP> load mmc 0:4 0xc2000000 EFI/BOOT/BOOTARM.EFI
STM32MP> bootefi 0xc2000000 ${fdtcontroladdr}
```

Wait for the prompt between lines. Two lines sent back to back overflow U-Boot's
UART input while it is busy with the first, and the tail of the second is
dropped without a word.

`mmc 0:4` is the `bootfs` partition of the layout in step 1; `mmc part` lists
them if yours differs. The device tree argument matters, and it must be
**`${fdtcontroladdr}`** — U-Boot's own live control tree, the real
`stm32mp157a-dk1` one. `${fdt_addr_r}`, which some guides give, is only an
empty *load address*: nothing puts a tree there unless a boot script does, and
`bootefi` fails with `invalid device tree`. The kernel finds its console, its
processors, its timer and its interrupt controller in the tree; without it
nothing works.

To make it the default once it boots:

```
STM32MP> setenv bootcmd 'load mmc 0:4 0xc2000000 EFI/BOOT/BOOTARM.EFI; bootefi 0xc2000000 ${fdtcontroladdr}'
STM32MP> saveenv
```

### Resetting the board from the host

The ST-LINK drives the processor's reset line, so a board that has halted —
which with the watchdog off is every board after a panic — can be restarted
without pressing its reset button. With OpenOCD installed (Ubuntu's `openocd`
package also installs the udev rules that let a logged-in user open the
ST-LINK):

```sh
openocd -f board/stm32mp15x_dk2.cfg -c "init; reset; shutdown"
```

OpenOCD ships no DK1 configuration, so this uses the DK2's, on the assumption
that the two boards wire the ST-LINK alike. **Neither that nor the reset itself
has yet been run against a powered board.** What is known is its
failure when the board is off: OpenOCD reaches the ST-LINK, reports
`Target voltage: 0.000000`, and fails with `init mode failed (unable to connect
to the target)`. A reading of 0 V means the board has no power — a reset cannot
help, and someone has to restore it.

## 4. What a good boot looks like

Abridged. The stage 5 lines are the board's own, from its 2026-09-12 run; the
lines after stage 5 have so far only been seen under QEMU.

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
  stage 4  2 processors online, a contended counter came to ... of ...
  cost     ms per check: one=0 sleep=20 many=322 fair=161 place=3 affin=65 load=105 bal=131 slice=13
  tasks    1000 threads run to completion on 2 processors (0b11), 2798 switches, 73 steals
  stage 5  1000 threads scheduled fairly across 2 processors
  ...
FERRIX-BOOT-OK stages 1-9
```

`watch-serial` exits 0 on that last line and non-zero on `FERRIX-PANIC`, so it
is a test rather than something to read. Stage 5 took 0.9 s of an 8.4 s boot,
counted from U-Boot's `reset`.

## When it goes wrong

### Nothing on the serial port at all

The loader's own lines come from firmware's console, so if even
`Ferrix loader` is missing the problem is before us. In order:

* **Is the board powered?** The ST-LINK's port exists whether it is or not.
  `openocd -f board/stm32mp15x_dk2.cfg -c "init; shutdown"` reports the target
  voltage; 0 V is a board with no power.
* **Are both boot switches ON?** Both OFF is USB DFU, which prints nothing.
* Is the micro-USB cable in the ST-LINK port, does `/dev/ttyACM0` exist, and
  are you in `dialout`?
* Is another program — a `picocom` left open — holding the port?

### It stops after about thirty seconds, with no panic

U-Boot's watchdog, started by the IWDG heuristic described in step 1. Rebuild
U-Boot with `WDT_STM32MP` disabled; a full boot then prints no `WDT:` line.

### `bootefi` says `invalid device tree`

The tree argument was `${fdt_addr_r}`. Use `${fdtcontroladdr}`, as in step 3.

### The loader talks, then silence

The most likely single failure, and the reason `console=` exists. The loader
prints through firmware; the kernel prints through its own driver, so silence
starting exactly at the hand-off means the kernel chose the wrong UART or
mapped the wrong address. UART4 is the DK's console and an STM32 USART, not a
PL011. Force it:

```
STM32MP> setenv bootargs 'console=stm32'
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
STM32MP> setenv bootargs 'nosmp'
```

One core reaching the end of stage 3 tells you the loader, the hand-off, the
page tables, the console and the timer are all correct, and that what is left
is the thing you just switched off.

### It panics reading ACTLR

`noactlr` skips that read. It should not be needed — the read is
architecturally permitted from the non-secure world — and on the DK1 it was
not, but an escape hatch that needs no rebuild is worth the line it costs on
silicon nobody has tried.

Options combine: `setenv bootargs 'console=stm32 nosmp'`.

### Test under QEMU at two processors first

The DK has two cores and `cargo xtask test-boot` defaults to four. Checks that
accept a threshold such as "at least two processors used" pass at four whatever
happens, and a two-core-only failure has twice reached the board that
`--smp 2` would have caught in thirty seconds:

```sh
cargo xtask test-boot --arch armv7a --smp 2 --timeout 600
```

Do not tune scheduler timing from QEMU's numbers alone, though: a context
switch under TCG costs around 200 µs of guest time, so anything that piles
tasks onto one queue reads many times slower there than on the board.

## What is actually verified

| Claim | How |
|---|---|
| Stages 1-5 run on an STM32MP157D-DK1 | booted at `6df0925` on 2026-09-12, mainline firmware above |
| Both processors come up and share work | the same boot: `2 online`, `1000 threads ... on 2 processors (0b11)` |
| `${fdtcontroladdr}` is the tree to pass; `${fdt_addr_r}` fails | on the board |
| Both boot switches ON boots the SD card; the prompt is `STM32MP>` | on the board |
| U-Boot's IWDG heuristic starts a 32 s watchdog on `VERR = 0x30` | on the board; gone with `WDT_STM32MP` off, and survives an OP-TEE without IWDG |
| The above-2-GiB identity-map placement | host unit tests in `libs/bootinfo` |
| STM32 USART register offsets | against Linux's `stm32h7_info`: `isr 0x1c`, `tdr 0x28`, `TXE` bit 7; and the board prints |
| UART4 is the DK console at `0x40010000` | `stm32mp151.dtsi`, and `stdout-path = serial0:115200n8` in `stm32mp15xx-dkx.dtsi` |
| DK RAM is 512 MiB at `0xc0000000` | `memory@c0000000 reg = <0xc0000000 0x20000000>` |
| CPU nodes carry no `enable-method` | the same device trees; hence the PSCI default |
| The kernel boots everywhere, `stages 1-9` | `cargo xtask test-boot`, three architectures, and armv7a at `--smp 2` |
| `watch-serial` detects both markers | against a pseudo-terminal |
| `flash` refuses wrong destinations | unit tests, and tried against `/boot/efi` |
| OpenOCD reads 0 V from an unpowered board | on the board, powered off |

Not verified: stages 6 onwards on the board; a busybox shell on the board,
which also cannot read input until the Arm UART drivers receive; the OpenOCD
reset against a powered board; and `console=`, `nosmp` and `noactlr` *as
selections* — their parsing has host tests, but the paths they choose are only
reachable on hardware, which is the point of them.

The upstream firmware facts behind this were checked against mainline TF-A,
OP-TEE, U-Boot and Linux — **not** against ST's forks, which is what
OpenSTLinux ships.
