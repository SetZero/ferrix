# Pixel 7 USB device driver: handover

**Status, 2026-09-26: not started.** Nothing of this is written. This document
is the brief for whoever takes it up: why it is wanted, what the hardware is,
what Ferrix already has to build it from, the rules that bound it, and the
order to do it in. Read `boot/pixel7/HANDOVER.md` first; it is the
phone's own state, and its rules apply here unchanged.

## 1. Why, and what done looks like

When the Pixel 7 boots Ferrix natively (`fastboot boot` from nazuna), nothing
reaches the PC until the run is over. Android is gone, Ferrix has no USB, and
so the phone is off USB for the whole run. Ferrix's console and the stat
service's samples (`userland/statd/`, `ferrix-statd`) go to the `ramoops` record in RAM,
which Android reads back after the watchdog reset. `tools/pixel7/monitor` then
loads the record and fills its graphs after the fact. The owner wants them
live.

Done means: **during a native boot, Ferrix presents a USB serial port (CDC-ACM)
on the phone's USB-C port, nazuna sees it as `/dev/ttyACM*`, and the monitor
streams the boot's stages and `ferrix-statd`'s lines from it live**, as it
already does for a crosvm guest's 16550. The `ramoops` record stays as the
record of a run that fails before USB is up.

A guest of the phone's own crosvm (the monitor's "Run in a VM") already
streams live. This is only for native boots.

## 2. The rules

These come from the owner, who has lost a device's touchscreen calibration to
an agent before. `boot/pixel7/HANDOVER.md`, "Never write anything that
survives a reset", is the full text.

* **Only volatile state may be written**: RAM, and SoC controller registers
  such as DECON's, or here the DWC3's and the USB PHY's own.
* **Never** the PMIC or regulators, **power domains**, fuses, security
  blocks, UFS, or the USB-C port controller (the MAX77759 TCPC at I²C `0x25`).
  Never `fastboot flash`, `erase` or `oem`.
* The PMU's USB PHY isolation bit and the HSI0 power domain (§3) are power
  domains. **Writing either needs the owner's word first.** Ask before that
  step, not after.
* Before any test that writes hardware, list the exact addresses, check them
  against `~/.local/share/ferrix/pixel7/panther.dts`, tell the owner, and make
  each write conditional on the hardware being as expected (as
  `boot/pixel7/src/display.rs`'s `take_over` does).
* Architecture: the kernel enumerates devices and drives none of them
  (`docs/ARCHITECTURE.md` §1 and §7; `scripts/data/device-access-allowlist.json` is
  the gate). The controller is driven by a ring-3 driver. The kernel does only
  what the chip shares, as `kernel/src/stm32mp1_usb.rs` does for the DK1.

The phone is shared with other sessions (`phone-link-9f`'s PhoneLink app,
`dev.phonelink`, stays installed, with its data). `adb devices` listing it
does not mean it is free: list the peer sessions, and ask the owner before
every native boot.

## 3. The hardware

From ABL's device tree (build CP2A.260705.006), saved as
`~/.local/share/ferrix/pixel7/panther.dts` (and `.dtb`). Line numbers are that
file's.

| Block | Where | What the tree says |
|---|---|---|
| USB wrapper | `usb@11210000` (l. 14335) | `samsung,exynos9-dwusb`, 64 KiB at `0x1121_0000`, SPI `0x17b` (379) level-high, `power-domains` pd-hsi0, clocks `aclk`/`sclk`/`bus` |
| DWC3 core | child `dwc3` (l. 14362) | `synopsys,dwc3`, same window and interrupt, **`dr_mode = "peripheral"`**, `maximum-speed = "super-speed-plus"`, no `dma-coherent`, `memory-region` `xhci_dma@97000000`, and the quirks `snps,dis-u1-entry-quirk`, `snps,dis-u2-entry-quirk`, `snps,usb2-gadget-lpm-disable`, `snps,quirk-frame-length-adjustment = <0x20>` |
| USB PHY | `phy@11200000` (l. 14388) | `samsung,exynos-usbdrd-phy`, windows `0x1120_0000`+`0x200`, `0x110f_0000`+`0x2800` (combo PHY), `0x1110_0000`+`0x800`, and the core's; SPIs `0x178` and `0x176`; reference clock 19.2 MHz (`0x124f800`); `phy_version 0x301`, `sub_phy_version 0x404`, `has_combo_phy`; `hs_tune`/`ss_tune` tables (hs disabled) |
| PMU | `system-controller@18060000` (l. 12798) | `samsung,gs101-pmu`. The PHY's `pmu_offset = 0x3eb0`, so its isolation control is **`0x1806_3eb0`** (DP's `0x1806_3eb4`). Power domain. |
| Power domain | `pd-hsi0@18062080` (l. 7219) | `samsung,exynos-pd`, 32 bytes at `0x1806_2080`. Power domain. |
| Clocks | `clock-controller@1e080000` (l. 12845) | `samsung,gs201-clock`; the core uses IDs `0x22f` (aclk), `0x227` (sclk, and the PHY's `phy_ref`), `0x3a` (bus) |
| DMA guard | `s2mpu_hsi0@11070000` (l. 9537) | `google,s2mpu`, 64 KiB: the stage-2 MPU that decides what memory HSI0's masters, the USB among them, may reach |
| DMA pool | `xhci_dma@97000000` (l. 273) | `shared-dma-pool`, 4 MiB at `0x9700_0000`, `no-map`: Android's host-mode pool |
| USB-C | `max77759tcpc@25` (l. 8152) | the `extcon` both nodes name. **Never touched.** |

The interrupt is SPI 379, so GIC INTID 411.

What the tree cannot tell, and phase 1 has to find:

* **What ABL leaves.** ABL runs fastboot over this controller, in device
  mode, until it jumps to the loader. The phone drops off USB the moment
  it does, so ABL at least stops the controller or disconnects. Whether the
  PHY is still powered and un-isolated, the clocks still on, and the power
  domain up decides the whole job. If they are, Ferrix need only reprogram
  the controller (volatile). If not, bringing them back is the
  power-domain write that needs the owner (§2).
* **The S2MPU.** Whether ABL leaves HSI0 able to reach arbitrary RAM, or only
  some region. Ferrix's IOMMU layer knows VT-d and SMMUv3, not the S2MPU. If
  it is restrictive, the driver's buffers must live where it allows. Android
  uses `xhci_dma@97000000`, and ABL may use yet another region.
* **Reading a block that is unclocked or powered down can hang the bus.**
  That is not persistent, since the watchdog resets the phone with the log
  intact, but read the power domain's and the clock gate's state before the
  controller's.

## 4. What Ferrix already has

* **The device model.** The kernel publishes a device tree node for a binding
  it knows (`kernel/src/device.rs`: `BoardBinding`, `BoardDevice`,
  `DmaShape`). `native/devmgr`'s `TREE_DRIVERS` table maps the binding to a
  driver, which `devmgr` starts in a job of its own with the node's
  apertures and interrupt (`docs/DEVMGR.md` §3). Bindings are numbered in
  `libs/proto/native-abi/src/types.rs`: `TREE_STM32_HDMI = 1`, `_USBH = 2`,
  `_GPU = 3`. A new one takes 4.
* **The precedent to copy.** The DK1's USB host:
  * `kernel/src/stm32mp1_usb.rs` turns on clocks, resets, regulators and the
    PHY's PLL, with values read back from U-Boot, and publishes the node.
  * `native/drivers/usbhid` drives the EHCI controller.
  * `libs/drivers/usb-host` is the host-testable logic, with a register model under
    `src/tests/model.rs`.
  * `docs/INPUT.md` §7 is its design.

  The DWC3 is device-side, which nothing in the tree is yet, but the split is
  the same.
* **DMA.** `DmaShape`'s coherence flag (the DWC3 has no `dma-coherent`). The
  pinned VMOs `vmo_pin`, with `PIN_COHERENT` for uncached memory.
  `arch::clean_for_device` and `arch::flush_for_device`.
* **The phone's board support.** `kernel/src/arch/aarch64/gs201.rs` covers the
  two watchdogs only. The kernel feeds them while it runs, and ends a run by
  letting one fire (`reset_now`), which keeps the `ramoops` record.
* **The console.** `kernel/src/arch/aarch64/console.rs` has three backends:
  PL011, 16550 over MMIO, and the phone's `ramoops` zone. `ferrix-statd`
  writes its `FERRIX-STAT` lines to its standard output, which as pid 1 is
  the console. `userland/statd/README.md` has the format. How a ring-3 USB driver
  gets the console's bytes is a design decision still to make (§5, phase 4).
* **The loader.** `boot/pixel7`, which runs with the MMU off, so all
  memory is Device memory: aligned, word-sized, volatile accesses only. It
  is where a read-only survey is cheapest (`display.rs` reports DECON's
  registers the same way).

## 5. The plan

Each phase ends with something run on the phone and written down here.

1. **Survey, read only.** In the loader, log to `ramoops` the power
   domain's status, the clock gates, the PMU word `0x1806_3eb0`, the S2MPU's
   control registers, the PHY's first words, and the DWC3's global and
   device registers. The DWC3 offsets below are from Linux's
   `drivers/usb/dwc3/core.h`; check them before relying on them:
   * global: `GSNPSID` `0xc120`, `GCTL` `0xc110`, `GUSB2PHYCFG(0)` `0xc200`,
     `GUSB3PIPECTL(0)` `0xc2c0`, `GEVNTADRLO(0)` `0xc400`;
   * device: `DCFG` `0xc700`, `DCTL` `0xc704`, `DEVTEN` `0xc708`, `DSTS`
     `0xc70c`.

   `GSNPSID` names the core's version. `DCTL`'s run/stop bit and `DSTS`
   say what ABL left. No writes. The result decides everything after, so
   write it here.
2. **Kernel plumbing.** A binding, `TREE_GS201_DWC3`, published by the
   Pixel's board support with the core's aperture, SPI 379 and a
   non-coherent `DmaShape`. Only the shared parts go in the kernel, and only
   if phase 1 says they need touching. The power domain and PMU writes wait
   for the owner.
3. **The driver.** A host-testable library, say `libs/dwc3`: event buffer,
   TRB rings, event decoding, and endpoint 0's control state machine, tested
   against a register model as `libs/drivers/usb-host` is. Device descriptors and the
   CDC-ACM class go in `libs/usb-device`. The ring-3 program, say
   `user/usbdev`, goes in `devmgr`'s table. Run at high speed (USB 2.0) first:
   `DCFG` can hold the core there, which keeps the combo SuperSpeed PHY out of
   the first bring-up. The endpoints are endpoint 0, one bulk IN and one bulk
   OUT for ACM data, and an interrupt IN for ACM notifications.
4. **Console over USB.** Decide how the driver gets the bytes. Kernel log
   records to a ring-3 reader (`sys_syslog` returns nothing today)? A tty
   the driver serves, which statd opens? Or both? Then send the boot's lines
   and statd's.
5. **The monitor.** `tools/pixel7/monitor` reads `/dev/ttyACM*` while the
   phone is in Ferrix and feeds the same parser it uses for a guest
   (`vm-line`). The owner is in `dialout`, and `cdc_acm` loads on demand.

QEMU has no model of a DWC3 in device mode, so the tests are the register
model (phase 3) and the phone.

## 6. The phone loop

`P=~/.local/share/ferrix/pixel7`. Two scripts there, outside the repo:

* **`WT=<worktree> $P/build-run.sh <name>`** builds `<worktree>`'s aarch64
  image with `--statd`, the loader with the payload, and
  `$P/<name>/boot.img`. It keeps the tree's diff, the commit, and a debug
  kernel beside it. It refuses an existing `<name>`.
* **`$P/boot-run.sh <name>`** runs `adb reboot bootloader`, `fastboot stage
  vendor_boot.img`, `fastboot boot`, waits for Android, and saves the
  `ramoops` record as `$P/<name>/run.log`. Nothing is flashed.

The launcher's helper (`tools/pixel7/helper.py`) does the same
cycle over HTTP, with `POST /boot?stats=N` for the stat service. The monitor
drives it.

A native run takes about 75 s back to Android, plus any stat service time.
Each run leaves the phone locked. adb and `su` work while it is locked, and
the unlock steps are the owner's to give. A hang ends in a watchdog reset
with the log kept.

## 7. Before starting

* This is a new roadmap item. It needs a `docs/BACKLOG.md` entry, and the
  product owner session (`ferrix-32`) has the say on where it goes.
* Tell the owner before phase 1's first boot, which is read-only. Ask the
  owner before any power-domain write.
