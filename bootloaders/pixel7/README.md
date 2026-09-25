# The Pixel 7 loader

A second stage that starts Ferrix on a Pixel 7 (`panther`, Tensor G2 /
gs201). The phone's own bootloader, ABL, is signed and stays; with the
bootloader unlocked it will boot an Android boot image whose kernel is this
program, which is to do for Ferrix what `boot/` does under UEFI.

**Where it stands (2026-09-25):** a probe. It runs on the phone, drops from
EL2 to EL1, reports what ABL handed over, and resets. It does not load Ferrix
yet; the kernel has no way to run on this machine yet either (see below).

## Building and running it

```
cargo build -p ferrix-boot-pixel7 --target aarch64-unknown-none-softfloat --release
llvm-objcopy -O binary target/aarch64-unknown-none-softfloat/release/ferrix-boot-pixel7 Image
python3 bootloaders/pixel7/mkbootimg.py Image boot.img
avbtool add_hash_footer --image boot.img --partition_size 67108864 \
        --partition_name boot --algorithm NONE
```

`llvm-objcopy` is in the toolchain's `llvm-tools` component; `avbtool` is
AOSP's `external/avb/avbtool.py`, which needs `/dev/urandom` and so runs under
WSL on Windows. Then, from ABL's fastboot mode:

```
fastboot stage vendor_boot.img     # the stock one, from the factory image
fastboot boot boot.img
```

Nothing is written to the phone. Both steps are required, and neither is
optional in a way that says so: without the staged `vendor_boot`, or without
the AVB footer, ABL accepts the upload, resets within a second, and comes back
up in fastboot mode or in Android. A raw arm64 `Image` is accepted; it need not
be LZ4-compressed as the stock kernel is.

**Never boot an Android kernel through this path.** A boot image without the
stock AVB properties carries no boot security patch level, KeyMint refuses the
user's keys, and Android falls into its "cannot load Android system" rescue
screen. "Try again" recovers it; "Factory data reset" would erase the phone.

## Reading what it did

The loader writes its log as a console record in the `ramoops` region
(`0xfd3ff000`, 2 MiB, no ECC). A hardware-watchdog reset preserves it, and the
next Android boot shows it, after ABL's own log, in
`/sys/fs/pstore/console-ramoops-0` (root needed). ABL overwrites the record's
header on an ordinary reset, so what survives is the text, found by searching
for `ferrix-pixel7`. `fastboot oem dmesg` prints ABL's log of the current
session, including why the phone last reset.

## What the probe established

| | |
|---|---|
| Load address | `0x80000000`, as the header asks (`text_offset` 0, placement near the base of DRAM) |
| Device tree | at `0x8a000000`, valid, 384 KiB |
| Entry | EL2, MMU and caches off, `HCR_EL2` = `0x80000002` |
| EL2 to EL1 | works with the sequence in `src/entry.rs` |
| Generic timer | 24.576 MHz |
| Watchdogs | both **running** at hand-off (`WTCON` bit 5): the loader must stop them or the phone resets |
| Reboot reason | the PMU register at `0x18060810` ignores a non-secure write |
| Panel | DSI **command mode**: it shows only frames the display controller is told to send, so writing a framebuffer changes nothing on screen |

## What Ferrix needs before it can run here

The AArch64 kernel is written for QEMU's `virt` machine. On this phone it
would need, besides this loader building a `BootInfo`:

* the GIC found from the device tree rather than ACPI's MADT, and GICv3
  redistributors (distributor `0x10400000`, redistributors `0x10440000`);
* a console that is not the PL011 at `0x09000000` -- the `ramoops` record is
  the obvious first one;
* PSCI's conduit (`smc`) from the device tree rather than the FADT;
* the watchdogs stopped (`WTCON` at `0x10060000` and `0x10070000`);
* and, to be useful, drivers: there is no virtio here.
