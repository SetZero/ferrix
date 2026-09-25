# Handover: Ferrix on the Pixel 7 (2026-09-26)

For the agent picking this up on nazuna. `README.md` in this directory says
what the loader is. This file is the working state behind it: what is proven
on the phone, how to drive it, what went wrong, and what to do next, most
important first. Read **"Never write anything that survives a reset"** before
you touch the phone.

## State at a glance

* **Branch `pixel7/stage3`**, worktree `.claude/worktrees/pixel7-stage3`. It
  is eight commits on `pixel7/bootloader` (which is three on `main` at
  `543428bb`). Local only: **not pushed, not merged, not gated.** Pushes are
  the owner's to authorise.
* **Ferrix boots on the phone to `FERRIX-BOOT-OK stages 1-12`, on one core**,
  and **draws on the screen**: the kernel's panic screen has been seen on the
  panel, in the right colours.
* The commits, oldest first:

  | Commit | What |
  |---|---|
  | `40215968` | Device-tree path on AArch64 + GICv3 driver (`gicv3.rs`, `gic.rs` front) |
  | `0d0a3d1c` | Docs: boots to stage 12 on one core |
  | `0862480a` | Clippy fix for `gic.rs`; **`40215968` alone fails clippy** |
  | `502da10b` | `kernel/src/arch/aarch64/gs201.rs`: feed the watchdogs; end a boot by firing one |
  | `0d8245ba` | Loader: display dump, first light, park for the watchdog on failure |
  | `40d1715d` | Loader: hand ABL's framebuffer to the kernel as `BootInfo.framebuffer` |
  | `72cca027` | Docs: the screen, and the no-persistent-writes rule |

* **Checks run:** `cargo fmt --check`; clippy `-D warnings` on the kernel for
  aarch64, armv7a and x86_64, and on the loader; `check-asm-budget.py`;
  `cargo test -p xtask workspace`; `cargo xtask test-boot --arch aarch64`
  (default GICv2 + ACPI) passes stages 1-12.
  **`cargo xtask check` has not been run.** Nothing reaches `main` before it
  and the gate row in `docs/BACKLOG.md` pass on nazuna.

## The phone

Pixel 7, `panther`, serial `28171FDH2001RC`. Stock Android 17, build
`CP2A.260705.006`, slot `a`. Bootloader **unlocked**; rooted with Magisk 30.7
(patched `init_boot_a`); `su` works from `adb shell` (context
`u:r:magisk:s0`). The owner is at the phone and can press buttons or watch the
screen when asked. Ask them to watch before any display test, because only a
human can see the screen.

Tooling on nazuna, all without sudo:

* Google's platform-tools in `~/.local/share/ferrix/pixel7/platform-tools`,
  linked into `~/.local/bin` (`adb`, `fastboot`). Both reach the phone as the
  desktop user through `uaccess`, with no udev rule needed, and nazuna's adb
  key is authorised.
* In `~/.local/share/ferrix/pixel7/`: the factory zip (SHA-256
  `ed94a24e…0372b0b`), `vendor_boot.img` extracted from it, `avbtool.py`,
  `panther.dts` (the phone's live device tree), `display-src/gs-display`
  (Google's display driver, the source of the register offsets), and the
  logs of every run (`run*.log`).
* `llvm-objcopy`: `$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy`.

## One run, end to end

```sh
P=~/.local/share/ferrix/pixel7
cargo xtask flash --arch aarch64 --release --stage "$P/stage"      # ~2 min
FERRIX_PIXEL7_KERNEL="$P/stage/FERRIX/KERNEL.ELF" \
FERRIX_PIXEL7_INITRD="$P/stage/FERRIX/INITRD.IMG" \
    cargo build -p ferrix-boot-pixel7 --target aarch64-unknown-none-softfloat --release
"$(rustc --print sysroot)"/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy \
    -O binary target/aarch64-unknown-none-softfloat/release/ferrix-boot-pixel7 "$P/Image"
python3 bootloaders/pixel7/mkbootimg.py "$P/Image" "$P/boot.img"
python3 "$P/avbtool.py" add_hash_footer --image "$P/boot.img" \
    --partition_size 67108864 --partition_name boot --algorithm NONE

adb reboot bootloader
fastboot stage "$P/vendor_boot.img"
fastboot boot "$P/boot.img"
# Wait for fastboot to *disappear* first, or a wait loop sees it still
# listed and thinks the phone is back. Then about 52 s to adb:
until [ "$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ]; do sleep 2; done
adb exec-out "su -c 'cat /sys/fs/pstore/console-ramoops-0'" > "$P/run.log"
sed -n '/ferrix-pixel7 loader/,/welcome to lk/p' "$P/run.log"
```

* **Timing tells you what happened:** ~52 s means the boot ended normally,
  ~105 s means the loader parked and a watchdog fired, and ~29 s with no
  `console-ramoops-0` means some other reset lost the log.
* **Quote `su` correctly:** `adb shell su -c 'a; b'` runs only `a` as root,
  because the device shell splits on `;`. Use `adb shell "su -c 'a; b'"`.
* **ABL's own log:** `fastboot oem dmesg` prints it for the *current* fastboot
  session, including `Reboot Info` for the previous reset.

## What is measured on the phone

| | |
|---|---|
| Hand-off | loaded at `0x80000000`, device tree at `0x8a000000` (384 KiB), EL2, MMU and caches off, `HCR_EL2` `0x80000002`, counter 24.576 MHz |
| Interrupts | GICv3 from the device tree; virtual timer; 251 ticks at 999 Hz |
| Memory | 7747 MiB, 43 regions; kernel manages 7582 MiB |
| Watchdogs | `google,gs201-cl{0,1}-wdt` at `0x10060000`/`0x10070000`, **running at hand-off**; the kernel feeds them every 500 ms (a 120 s hold survived) |
| Display | DECON0 at `0x1C240000` running in command mode, TE trigger **masked by ABL** (`TRIG_CON` `0x3070`); window 5 from DPP0; framebuffer 1080 x 2400 at `0xFAC00000`; power domains on; display SysMMU reads as off |
| Pixel order | bytes B, G, R, unused, measured with four colour bands: UEFI's `Bgrx8888`, whatever the DPP format's name says |
| Frames | unmasking `TRIG_CON` (`0x3070` to `0x3061`): 12 frames in 200 ms, 60 Hz |
| Missing | no entropy (`random NOT SEEDED`), no RTC (the clock starts at the epoch), cores 1-7 (`nosmp`) |

Register offsets come from Google's gs201 display driver
(`samsung/cal_9845/regs-decon.h`, `regs-dpp.h`; gs201's `cal_9855` builds on
them). Check any new one against the phone before you rely on it.

## What bit us, so it does not bite you

* **`fastboot boot` needs `fastboot stage vendor_boot.img` first, and the AVB
  footer.** Without either, ABL accepts the upload and falls back.
* **Never boot an Android kernel through this path.** KeyMint refuses the
  keys and Android shows "Cannot load Android system". **"Try again"** fixes
  it; **"Factory data reset" would erase the phone**.
* **After `adb reboot bootloader`, reboot mode `0xfc` is sticky.** A reset
  soon after lands in ABL's menu, not Android, until the owner presses *Start*.
* **Only a watchdog reset keeps the `ramoops` log.** PSCI `SYSTEM_RESET` over
  `smc` resets at once and loses it. So the loader parks and waits for a
  watchdog on any failure (`entry::wait_for_watchdog`), and the kernel's
  `shutdown`/`reset` fire a watchdog (`gs201::reset_now`). ABL rewrites the
  record's header, so search the file for text rather than trusting it.
* **Some registers fault on read** (synchronous external abort,
  ESR `0x96000010`): the display SysMMU at `+0xC`, and `pd-disp` at `+0x14`
  (`pd-dpu` was only ever read up to `+0x10`). The loader's display dump logs one register per line, so the
  fault's `FAR` names the refused one.
* **The panel is command mode.** Nothing you write shows until DECON is
  triggered; `display::take_over` unmasks the TE trigger once, and from then
  on every framebuffer write reaches the glass.
* **The kernel only draws on a panic.** To see the screen, build with
  `ferrix.onexit=panic` added to `board::CMDLINE` (temporarily; do not commit
  it). The panic screen stays up until the watchdog fires, 30-60 s.
* **`40215968` fails AArch64 clippy on its own.** rustfmt ran after clippy;
  `0862480a` fixes it. Run clippy again *after* `cargo fmt`.

## What to do next, most important first

1. **Gate and land.** Run `cargo xtask check` in the worktree, fix what it
   finds, rebase on `main`, and follow `docs/BACKLOG.md`'s gate row. The
   kernel changes (`gic*.rs`, `smp.rs`, `timer.rs`, `mod.rs`, `console.rs`,
   `gs201.rs`, `power.rs`, `main.rs`) are in the "anything the image
   contains" row. The owner decides the push.
2. **The boot console on screen.** The framebuffer reaches the kernel already;
   what is missing is drawing the console there during boot, not only on a
   panic. The panic screen's renderer (`kernel/src/panic/screen.rs`) is the
   starting point.
3. **Cores 1-7.** TF-A's PSCI starts a secondary at EL2, and the kernel's entry
   sequence in `kernel/src/arch/aarch64/smp.rs` expects EL1. It needs the
   EL2-to-EL1 drop that `bootloaders/pixel7/src/entry.rs` does, which will
   need `smp.rs`'s budget in `scripts/asm-allowlist.json` raised with an
   argument. Then remove `nosmp` from `board::CMDLINE`. Test on QEMU with
   `--smp 2` first, and on the phone.
4. **Entropy.** `/chosen` should carry `rng-seed` or `kaslr-seed`. The loader
   could pass it on as the boot info's `firmware_seed`.
5. **A GICv3 ITS driver.** Without one a GICv3 has no MSI vectors, so the
   QEMU runs with `FERRIX_ARM_MACHINE=gic-version=3` stop at stage 10. This
   does not matter on the phone, which has no virtio.

## Starting Ferrix from Android (stopped)

The owner asked for an Android app that starts Ferrix without the PC. This
kernel has no `kexec` (`CONFIG_KEXEC` and `CONFIG_KEXEC_FILE` are not set).
An empty kernel module was built from Google's source for this exact kernel
(`android14-6.1-2025-12_r9`, work in `~/.local/share/ferrix/pixel7/kmod/`),
and it loaded and unloaded cleanly. An earlier build of it panicked the phone
once, because its `struct module` layout was wrong. **Work beyond that was
stopped by a safety classifier.** Do not take it up again without talking to
the owner first. The route that needs neither the classifier nor any writes
is a script on this PC that runs the whole cycle above as one command.

## Never write anything that survives a reset

The owner has lost a device's touchscreen calibration to an agent before, and
it must not happen here. On this phone, only volatile things may be written:
RAM, and SoC controller registers such as DECON's.

* Never `fastboot flash`, `erase` or an `oem` write.
* Never a DSI command to the panel: an OLED panel's MTP can be written.
* Never the touch controller, the PMIC or regulators, power domains, fuses or
  security blocks, or UFS.

Before any test that writes hardware, list the exact addresses, check them
against `panther.dts`, tell the owner, and make the write conditional on the
hardware being as expected, as `display::take_over` is.

## Working here

* **Commits:** `docs/CONVENTIONS.md` applies. No `Co-authored-by` or tool
  trailer (the hooks refuse it), commit from your own worktree, read
  `git diff --cached --stat` before each commit, and never move work with
  `git stash`.
* **Refusals:** the auto-mode permission classifier refused, for this
  session, fetching Google's kernel source, preparing it, and loading modules
  on the phone. When it refuses, stop and hand the step to the owner.
* **Commands for the owner:** they run them in their own zsh, where a
  leading `!` is negation, not Claude Code's prefix. Give them commands
  without it.
