# Handover: Ferrix on the Pixel 7, from the Windows session (2026-09-25)

For the agent picking this up on nazuna. Read `README.md` in this directory
first; this file is the working state behind it: what is proven, how to drive
the phone from Linux, what bit us, and what to do next.

## State

* Branch `pixel7/bootloader`, pushed: two commits on top of `main` at
  `543428bb` -- `41fa9c54` (the probe) and `492b318b` (the loader) -- and this
  file. Not merged, not gated.
* **Ferrix boots on the phone to stage 2.** The loader places the kernel and
  initramfs, the kernel verifies the hand-off, manages 7582 MiB of frames,
  passes stage 2, then panics at stage 3 with `FX-0303 ... the machine has no
  readable ACPI tables` from `arch::init_interrupts`. That is the next job.
* The only kernel change is `kernel/src/arch/aarch64/console.rs` (+ a
  one-line signature change in `mod.rs`): a `ramoops` console backend chosen by
  `console=ramoops,<addr>,<size>`. QEMU behaviour is unchanged. It is in the
  "anything the image contains" row of `docs/BACKLOG.md`'s gate table and has
  **not** been gated; do that on nazuna before anything reaches `main`.
* `cargo clippy -p ferrix-boot-pixel7 --target aarch64-unknown-none-softfloat
  -- -D warnings`, the same for `ferrix-kernel`, `cargo fmt --check`,
  `scripts/check-asm-budget.py` and `cargo test -p xtask workspace` all passed
  on Windows. Nothing else was run.

## The phone

Pixel 7, `panther`, serial `28171FDH2001RC`. Stock Android 17, build
`CP2A.260705.006` (fingerprint `.../15641320`), slot `a`. Bootloader
**unlocked**; rooted with Magisk 30.7 (patched `init_boot_a`), and the Magisk
Superuser entry for *Shell* is switched on, so `adb shell su -c ...` works.
The owner is at the phone and can press buttons when asked.

From a new host, expect to have to: accept the USB-debugging prompt on the
phone for nazuna's adb key, and have udev rules that let you use fastboot
without root (Google's `51-android.rules`, vendor `18d1`).

## Files you need that are not in the repo

* **The stock `vendor_boot.img`** for this build. From the factory image,
  `https://dl.google.com/dl/android/aosp/panther-cp2a.260705.006-factory-ed94a24e.zip`
  (SHA-256 `ed94a24e693a28f236e87c9e03436871c2dd6b03b4b56c5839598115d0372b0b`),
  then `image-panther-cp2a.260705.006.zip` inside it. 3.9 GB download; keep it
  in `~/.local/share/ferrix/pixel7/`.
* **`avbtool.py`**, from `https://android.googlesource.com/platform/external/avb/+/refs/heads/main/avbtool.py?format=TEXT`
  (base64; `| base64 -d`). Needs only python3.
* `llvm-objcopy` is in the pinned toolchain's `llvm-tools` component:
  `$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy`.

## One run, end to end

```sh
P=~/.local/share/ferrix/pixel7            # vendor_boot.img, avbtool.py live here
cargo xtask flash --arch aarch64 --release --stage "$P/stage"
FERRIX_PIXEL7_KERNEL="$P/stage/FERRIX/KERNEL.ELF" \
FERRIX_PIXEL7_INITRD="$P/stage/FERRIX/INITRD.IMG" \
    cargo build -p ferrix-boot-pixel7 --target aarch64-unknown-none-softfloat --release
"$(rustc --print sysroot)"/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy \
    -O binary target/aarch64-unknown-none-softfloat/release/ferrix-boot-pixel7 "$P/Image"
python3 bootloaders/pixel7/mkbootimg.py "$P/Image" "$P/boot.img"
python3 "$P/avbtool.py" add_hash_footer --image "$P/boot.img" \
    --partition_size 67108864 --partition_name boot --algorithm NONE

adb reboot bootloader                      # or: already in fastboot
fastboot stage "$P/vendor_boot.img"
fastboot boot "$P/boot.img"
# ~100 s later the watchdog has reset the phone and Android is back:
adb wait-for-device
until [ "$(adb shell getprop sys.boot_completed | tr -d '\r')" = 1 ]; do sleep 2; done
adb exec-out su -c 'cat /sys/fs/pstore/console-ramoops-0' > "$P/run.log"
sed -n '/ferrix-pixel7 loader/,/welcome to lk/p' "$P/run.log"
```

`adb shell getprop sys.boot.reason` says `watchdog` after a run that reached
the loader. The build of `KERNEL.ELF` is the slow step (about two minutes);
the loader rebuilds in a second. Nothing is ever written to the phone's flash.

## What bit us, so it does not bite you

* **`fastboot boot` alone does nothing.** Without `fastboot stage
  vendor_boot.img` first, *or* without the AVB footer, ABL accepts the upload,
  resets within a second, and falls back. Both are required. A raw arm64
  `Image` is fine; it need not be LZ4 like the stock kernel.
* **After `adb reboot bootloader` the reboot mode `0xfc` is sticky**: a reset
  soon after lands back in ABL's fastboot menu, not Android. The menu offers
  Start / Restart / ...; if the owner presses *Start*, Android boots. So "the
  phone came back in fastboot" does not mean the loader asked for it.
* **Never boot an Android kernel through this path.** A repacked boot image has
  no AVB security-patch properties, KeyMint refuses the user's keys, and after
  a few boots Android shows *"Cannot load Android system. Your data may be
  corrupt."* It happened once; **"Try again" fixed it. "Factory data reset"
  would erase the phone** -- tell the owner which to press if it recurs.
* **The screen shows nothing we do.** The panel is DSI command mode: it only
  shows frames the display controller is triggered to send. The owner sees
  ABL's orange "bootloader is unlocked" warning and then black or the last
  frame. All feedback is through `ramoops`.
* **`ramoops` survives a watchdog reset, not reliably any other.** ABL writes
  its own log into the same console zone and rewrites the header on every
  boot, so Android reports "found existing invalid buffer" and the file shows
  ABL's log *followed by* ours. Search for the text; do not trust the header.
  After a normal `adb reboot` the zone came back empty.
* **Both watchdogs are running at hand-off** (`WTCON` 0x1af39 / 0x18021) and
  nothing kicks them, so every run ends in a watchdog reset about 90 s after
  `fastboot boot`. That is currently the feedback loop, so the loader leaves
  them alone. When the kernel needs to run longer, stop them (clear `WTCON`
  bits 5 and 0 at `0x10060000` and `0x10070000`, as Linux's `s3c2410_wdt`
  does) and accept that a hang then needs the owner to hold Power + Volume
  Down.
* **PSCI `SYSTEM_RESET` over `smc` is unproven.** The probe logged "resetting"
  and the reset that followed was the watchdog's. Treat reset as "wait for the
  watchdog" until shown otherwise.
* **The reboot-reason register (`0x18060810`) ignores non-secure writes.**
* `fastboot oem dmesg` prints ABL's log for the *current* ABL session,
  including `Reboot Info` (why the phone last reset). `fastboot oem help` does
  not exist; `fastboot oem watchdog [enable|disable]` and `fastboot oem uart
  ...` do (see Linaro's `pixelscripts` Makefile, `prepare-device`).
* Hand-off facts, measured: loaded at `0x80000000`, device tree at
  `0x8a000000` (384 KiB), entered at EL2 with MMU and caches off
  (`HCR_EL2` 0x80000002, `SCTLR_EL2` 0x30c50830), counter 24.576 MHz.

## Update from nazuna (2026-09-25, branch `pixel7/stage3`)

Steps 1 to 3 below are written, pass under QEMU, and **boot on the phone to
`FERRIX-BOOT-OK stages 1-12`** on one core (run at 23:38, `nosmp`): GICv3,
virtual timer at 24.576 MHz, 251 ticks at 999 Hz, 7582 MiB managed, every
self-check through stage 12 passing, then the watchdog reset. Three things the
log says are missing: `random NOT SEEDED` (no firmware entropy and no `RNDR`
on these cores -- `/chosen`'s `rng-seed` or `kaslr-seed` is the obvious
source), `firmware has no clock` (`CLOCK_REALTIME` starts at the epoch), and
the second to eighth cores.

* `kernel/src/arch/aarch64/gicv3.rs` is the GICv3 driver (distributor,
  redistributor walk and wake, `ICC_*_EL1` through four new accessors in
  `cpu.rs`, which is at 94 of 100 lines). `gic.rs` is now the front both
  drivers sit behind, found from the MADT or the device tree.
* `init_interrupts`, `describe_cpus` and the PSCI conduit take the device
  tree when `rsdp == 0`, ACPI otherwise, so QEMU's gates did not move.
* `FERRIX_ARM_MACHINE` appends a `-machine` to xtask's Arm QEMU line.
  `test-boot --arch aarch64` reaches stages 3, 4 and 5 with
  `gic-version=3` (ACPI) and with `gic-version=3,acpi=off` (device tree),
  and fails at stage 10 in both because a GICv3 has no MSI vectors until an
  ITS driver exists; the phone has no virtio, so that does not block it.
  The default GICv2 boot still passes stages 1-12.
* A `ramoops` console reports no receive interrupt: SPI 1 is QEMU's PL011
  and some other device on the phone.
* **The loader now passes `nosmp`.** TF-A's PSCI starts a secondary at the
  highest non-secure level, EL2 here, and `smp.rs`'s entry sequence is
  written for EL1. The next kernel step after a phone run is that entry
  dropping to EL1 the way `bootloaders/pixel7/src/entry.rs` does (it will
  need its assembly budget raised), then removing `nosmp`.
* On nazuna: Google's platform-tools in `~/.local/share/ferrix/pixel7/
  platform-tools`, linked into `~/.local/bin`; the factory zip,
  `vendor_boot.img`, `avbtool.py` and the last run's `run.log` beside them.
  `adb` and `fastboot` both reached the phone as the desktop user through
  `uaccess`, and nazuna's adb key is authorised.

## What to do next: stage 3

The kernel's AArch64 `init_interrupts` (`kernel/src/arch/aarch64/mod.rs`
~1105) opens ACPI and wants a GICv2 from the MADT; the timer's interrupt comes
from the GTDT (`timer.rs`), CPUs and the PSCI conduit from the MADT and FADT
(`smp.rs`). The loader passes `rsdp = 0` and a device tree, so:

1. **Device-tree path on AArch64.** `kernel/src/arch/armv7a/mod.rs`
   `init_interrupts` (~1166) is the template: `crate::fdt::open(view)`,
   `tree.interrupt_controller()`, `timer::init(&tree)`, `tree.psci_conduit()`.
   Take it when `view.raw().rsdp == 0`, keep ACPI otherwise, so QEMU's gates
   do not move. `libs/fdt` already has `GicVersion::V3` and
   `InterruptController::redistributor()`, and `cpus` / `enable-method`.
2. **A GICv3 driver.** Neither Arm port has one (`armv7a` refuses V3 by
   name). Distributor `0x10400000`, redistributors `0x10440000`
   (`/proc/iomem`: GICR `10440000-1053ffff`), CPU interface through
   `ICC_*_EL1` system registers -- the loader already set `ICC_SRE_EL2`
   SRE|Enable so EL1 can use them. New `mrs`/`msr` accessors go in
   `kernel/src/arch/aarch64/cpu.rs`, which is at 83 of its 100-line assembly
   budget; argue any increase in `scripts/asm-allowlist.json`.
3. **Test it in QEMU before the phone**: `-machine virt,gic-version=3`
   exercises the driver; a device-tree boot of the kernel under QEMU is what
   the rest needs, which today only this loader produces -- worth checking
   whether `-machine virt,virtualization=on,gic-version=3 -kernel Image`
   runs it (it links at `0x80000000`; QEMU puts RAM at `0x40000000`, so it
   may need `-m` and a check that its PC-relative code survives, or a
   `text_offset`).
4. Then SMP (eight cores, PSCI `CPU_ON` via `smc`), the watchdogs, and only
   then devices: there is no virtio here; display is a Samsung DECON/DSIM in
   command mode, storage is UFS.

The phone's live device tree is `/sys/firmware/fdt`: `adb exec-out su -c
'cat /sys/firmware/fdt' > panther.dtb`, then `dtc -I dtb -O dts` reads it. Useful nodes: `/reserved-memory`
(`ramoops_mem@fd3ff000`), `/psci` (`smc`), `watchdog_cl0@10060000`,
`drmdecon@1C240000`, `pixel-reboot` (syscon `0x18060000`, offset `0x810`).

## House rules that apply here

`docs/CONVENTIONS.md`: no `Co-authored-by` or tool trailer in any commit, the
hooks enforce it (`git config core.hooksPath .githooks`); commit from a
worktree of your own; `cargo xtask check` before calling anything done.
`docs/BACKLOG.md`: `main` moves only after the change's gate row has passed on
nazuna; pushes are the owner's to authorise. The certification work
(`docs/certification/`) landed on `main` at `543428bb` without its gate having
run; the owner chose that knowingly.
