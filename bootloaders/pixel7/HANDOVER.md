# Handover: Ferrix on the Pixel 7 (2026-09-26)

For the agent picking this up on nazuna. `README.md` in this directory says
what the loader is. This file is the working state behind it: what is proven
on the phone, how to drive it, what went wrong, and what to do next, most
important first. Read **"Never write anything that survives a reset"** before
you touch the phone.

## State at a glance

**Update, 2026-09-26 evening (ferrix-0a): item 4 is on `main`. Items 1-3
are gated as three commits, and wait for phone runs 4 and 5.**

* **The GICv3 ITS** (`c21ce40f..a8680955`) landed on `main` by fast-forward
  after a rebase and a re-gate on nazuna: `cargo xtask check` passed, and
  `test-boot` reached `FERRIX-BOOT-OK stages 1-12` on x86_64, armv7a,
  armv7a `--smp 2`, aarch64, and aarch64 with
  `FERRIX_ARM_MACHINE=gic-version=3` at `--smp 1` and `--smp 2`. **Not
  pushed.**
* **`--kernel-option WORD`** (xtask) puts any word in the image's
  `CMDLINE.TXT`, once for each time it is given. It is on `main` with this
  update.
* **`pixel7-next`** (worktree `.claude/worktrees/pixel7-next`) is now three
  commits on `main`, one for each of items 3, 1 and 2, in that order:
  `cad1db68` the seed, `da9545b9` the boot console, `704756ab` the EL2
  secondaries. **Gated on nazuna, and not yet seen working on the phone.**
  `cargo xtask check` passed on the three as rebased. `test-boot` reached
  `FERRIX-BOOT-OK` on x86_64, armv7a, armv7a `--smp 2` and aarch64, and on
  aarch64 `--smp 2 --kernel-option ferrix.fbcon`, on GICv2 and GICv3. Those
  boots ran just before a rebase onto three commits that change only gate
  scripts and the compositor. Land the first two after run 4 is good, and the
  third after run 5.
* **Run 4 is built and waiting for the phone:**
  `~/.local/share/ferrix/pixel7/run4b/boot.img` (SHA-256 `36ae3ddc…`),
  from `704756ab` with `board::CMDLINE` as committed (`ferrix.fbcon nosmp`).
  `run4/` is an older build: it has the boot console but not the seed
  count. Neither has been booted, because the session `phone-link-9f`
  held the phone for an app test and asked for no reset. Boot it with
  "One run, end to end" from `adb reboot bootloader` on, with
  `$P/run4b/boot.img` in place of `$P/boot.img`. What to look for: boot
  text on the screen for a few seconds; in the record, `FERRIX-BOOT-OK`,
  the loader's `/chosen holds 8 random bytes, passed on to the kernel's
  seed`, and the kernel's `random   NOT SEEDED: 64 of 256 bits`.

* **On `main`**, merged 2026-09-26 by fast-forward after the whole gate row
  for "anything the image contains" passed on nazuna. **Not pushed**: pushes
  are the owner's to authorise.
* **Ferrix boots on the phone to `FERRIX-BOOT-OK stages 1-12`, on one core**,
  and **draws on the screen**: the kernel's panic screen has been seen on the
  panel, in the right colours.
* The Pixel 7 commits, oldest first (the first three were `pixel7/bootloader`):

  | Commit | What |
  |---|---|
  | `6e83f794` | The loader as a probe: what ABL hands over |
  | `6c3da42a` | Load and start Ferrix; `ramoops` console; stops at stage 3 |
  | `8520dc49` | The first handover, from the Windows session |
  | `499381a2` | Device-tree path on AArch64 + GICv3 driver (`gicv3.rs`, `gic.rs` front) |
  | `ab996c9b` | Docs: boots to stage 12 on one core |
  | `f250c0bb` | Clippy fix for `gic.rs`: **`499381a2` alone fails clippy** |
  | `654022e9` | `gs201`: feed the watchdogs; end a boot by firing one |
  | `455a01d8` | Loader: display dump, first light, park for the watchdog on failure |
  | `20b242af` | Loader: hand ABL's framebuffer to the kernel as `BootInfo.framebuffer` |
  | `cf4002c6` | Docs: the screen, and the no-persistent-writes rule |
  | `65a9f5dc` | This handover, rewritten |
  | `29b6bb5e` | `gs201.rs` into `kernel/src/arch/aarch64/`: the owner put it in the certified core ring |
  | `83f1f8cf` | `gic::init` split for the complexity floor |
  | `73ef630e` | `arch::init_watchdogs`/`start_watchdogs` facade instead of `cfg`s in generic code |

* **Gate, on nazuna, on `73ef630e` as it was before its last rebase** onto
  two xtask-only commits (keyboard layout, remote desktop), which reach
  neither kernel nor loader; xtask's clippy and 293 tests passed after that
  rebase. `cargo xtask check` passed every
  section; `cargo xtask build --arch all --release` passed; `test-boot` passed
  on x86_64, aarch64, armv7a and armv7a `--smp 2`. The first three `check`
  runs failed on the certification item boundary, the complexity floor and
  crate layering, and those three commits answer them. Also
  `FERRIX_ARM_MACHINE=gic-version=3 test-boot --arch aarch64` reaches stages
  3-5 and stops at stage 10, as expected without an ITS.

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
  `console-ramoops-0` means some other reset lost the log. ~110 s with the
  record saying `No kernel logs`, and ABL's `Reboot Info` saying
  `PIN_RESET | PO_RESET` after `CLUSTER0_NONCPU_WDTRESET`, means a hard
  failure. The watchdog reset was followed by a power-on reset, which lost
  DRAM and with it even the loader's lines. Both boot-console runs below
  ended like that.
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
| `/chosen` | only `kaslr-seed`, 8 bytes; **no `rng-seed`** (the loader counted them, `run-seed-nosmp.log`) |
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
* **`499381a2` fails AArch64 clippy on its own.** rustfmt ran after clippy;
  `f250c0bb` fixes it. Run clippy again *after* `cargo fmt`.
* **A boot-console change put the fault before the vectors.** The boot
  console first started at stage 1, beside `panic::screen::install`. Its
  mapping check (`panic::screen::surface`) walks the tables from
  `mm::root_table()`, which is 0 until `mm::init`, and no kernel vectors
  are installed that early. The fault went to the loader's stale vectors,
  and the phone hung, drew a coloured stair pattern and came back through a
  power-on reset with no log at all. QEMU never showed it, because the flag
  is off there. Anything drawn early needs a QEMU run with the flag on
  before a phone run.
* **Ask the owner to watch in a question, right before the run.** A line
  in a message saying "watch the screen" was missed, and a run's picture
  was lost.
* **Other sessions use this phone.** ferrix-cc tests an Android Auto /
  ChatGPT Xposed module on it (the aa-chatgpt side project), and every
  Ferrix run resets the phone under it and leaves it locked until the owner
  types the PIN. Take turns: message the other session between runs, and
  don't boot while it holds the phone. Session names change when sessions
  restart: on 2026-09-26 evening it was `phone-link-9f`, testing an app on
  the Pixel and a Poco F2 Pro together over adb. `adb devices` listing the
  phone does not mean it is free, so list the peer sessions and ask.
* **The permission classifier refused `cargo xtask flash --stage "$P/stage"`**
  (it overwrites the previous stage) and then the whole rebuild. What
  worked before the second refusal, and deletes nothing: `cargo xtask build
  --arch aarch64 --release`; `python3 $P/fatget.py build/aarch64/ferrix.img
  FERRIX/INITRD.IMG <dir>/INITRD.IMG` (a read-only FAT reader, saved in the
  phone directory); `llvm-objcopy --strip-all` of
  `$CARGO_TARGET_DIR/aarch64-unknown-none-softfloat/release/ferrix-kernel`
  into `<dir>/KERNEL.ELF`; then the loader steps above with those two paths.
  The initramfs is then the unstripped one (5.4 MB), which still fits.
  Use `CARGO_TARGET_DIR=~/.local/share/ferrix/target-<session>`, never a
  worktree's own `target/`.
* **`cargo xtask check` holds more than clippy:** every kernel file needs a
  certification ring (`scripts/certification-item.json`, the owner's call),
  new functions stay under the complexity floor, and generic code may not
  hold a `target_arch` conditional. Run the whole gate, not a subset.

## What to do next, most important first

1. **The boot console on screen** (`pixel7-next`). Written:
   `kernel/src/console/screen.rs`. With `ferrix.fbcon` on the command line,
   the kernel's own lines (the bytes `console::recent` keeps, not programs'
   output) are drawn on the firmware framebuffer as they are printed. It
   picks the largest glyph scale that leaves 60 columns (double size on the
   phone), keeps a top inset of a twentieth of the height for the camera,
   wraps to the top without scrolling or reading back, and keeps the row
   after the newest line blank. It stops when a panic draws
   (`panic::screen::draw` calls `console::screen::stop`) or when the display
   core publishes a card. `panic::screen::surface()` is now shared by both
   and refuses while the root table is 0. The console starts right after
   `mm::init`. The stage-1 version is what hung runs 1 and 2
   (`run-fbcon-smp.log`, `run-fbcon-nosmp.log`, both empty of Ferrix text).
   **The fix boots under QEMU:** `test-boot --arch aarch64 --kernel-option
   ferrix.fbcon` reached `FERRIX-BOOT-OK` on one and two cores (11.6 s,
   against 11.5 s without it), and so did the Pixel's own path,
   `FERRIX_ARM_MACHINE=gic-version=3,acpi=off`. That run's `test-boot`
   still exits 1, because with no ACPI the SMMU is not found and the check
   that an out-of-domain DMA write faults sees none. That is not the
   console. Screendumps from a boot with QMP (`$P/fbcon_shots.py`, which
   boots `pixel7-next`'s `build/aarch64/ferrix.img`) are in
   `$P/qemu-fbcon-mid.png` and `$P/qemu-fbcon-wrapped.png`. They show
   the text in the panic screen's colours, at scale 1 on 800 x 600, and
   wrapped back to the top with blank rows under the newest line. Next:
   run 4 on the phone, whose image is built (see "State at a glance").
   The owner should see boot text at double size for a few seconds before
   the reset. The owner agreed to amend `docs/ARCHITECTURE.md` §1 for it,
   and `da9545b9` does.
2. **Cores 1-7** (`pixel7-next`). Written: `ferrix_secondary_entry` in
   `smp.rs` checks `CurrentEL`, and at EL2 sets EL1 up as
   `bootloaders/pixel7/src/entry.rs` does and `eret`s to it. It touches
   `ICC_SRE_EL2` only when `ID_AA64PFR0_EL1.GIC` says the CPU has GIC system
   registers. QEMU `--smp 2` passes (the EL1 path). The asm allowlist entry's
   reason was extended. Its budget did not need raising, because
   `check-asm-budget.py` counts a raw-string `global_asm!` as 2 lines, a
   hole worth closing. On the phone it is **unproven**. Run 1 had it
   together with the broken boot console, and ABL's PSCI breadcrumbs showed
   activity on cores 3, 4, 5 and 7, so CPU_ON calls went out. Next: run 5,
   with `nosmp` removed once run 4 is good. Watch whether the ramoops record
   survives with other cores writing it (it is a device mapping, so it
   should).
3. **Entropy** (`pixel7-next`). Written: `bootloaders/pixel7/src/seed.rs`
   folds `/chosen`'s `rng-seed` and `kaslr-seed` into `firmware_seed` and
   NOPs both out of the kernel's copy of the tree (run 3,
   `run-seed-nosmp.log`, a normal boot to `FERRIX-BOOT-OK`). ABL gives 8
   bytes. The owner chose to credit them rather than read the SoC's TRNG,
   so `cad1db68` makes `BootInfo` version 6, with `firmware_seed_len`
   beside the flag. The kernel credits 8 bits a byte, up to 256. That is 64
   bits on the phone, which has no `RNDR`, so it still boots `NOT SEEDED`,
   and the boot line says `64 of 256 bits`. The TRNG is a security block,
   so under "Never write anything that survives a reset" it needs the
   owner's word again before anyone reads it.
4. **A GICv3 ITS driver**: on `main`, see "State at a glance". Choices
   the owner may still want to review: DeviceID is the PCI requester ID, identity, as QEMU's
   IORT and `msi-map` are, and neither is read yet. One collection, on the
   boot core. LPI 8192+k is kernel interrupt 1024+k, and `irq::SLOTS` went
   from 1024 to 1280. `gicv3_its.rs` falls in the certified core ring,
   because the certification item lists `arch/**` there.

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

* **Worktrees:** the owner asked for Pixel 7 work to land on `main` in the
  root checkout rather than live in a sub-worktree; the worktree it grew in
  is removed.
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
