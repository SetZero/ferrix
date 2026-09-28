# The live installer

Version 1, written on 2026-09-28 and approved by the customer that day
(§11).
The customer asked that day for "a live installer like we know it from
Linux", and chose, from four questions:

* **real PCs too**, not only virtual machines;
* **graphical from the start**, a desktop app on hyprix;
* **beside other operating systems**, not only a wiped disk;
* **a raw `.img`, a hybrid `.iso`, and both attached to GitHub releases.**

This document is the design those answers ask for, written before anything
is built. It follows `docs/YSERVER.md`'s shape: what it is, what exists,
the design, the tests, the slices and their points, and what is left for
the customer to decide.

## 1. What this is, and what it is not

What a person does with it, start to finish:

1. Download `ferrix-<version>-x86_64.iso` (or `.img`) from a GitHub release.
2. Write it to a USB stick with `dd`, Rufus or balenaEtcher, or attach it to
   a virtual machine as a CD or a disk.
3. Boot it. Ferrix starts from the stick with `/` in memory -- the *live
   session* -- and the desktop comes up with the installer open. Nothing on
   the machine's disks has been touched, and the live session is a working
   Ferrix to try first.
4. In the installer: pick a keyboard layout; pick a disk; choose **Erase the
   disk** or **Install beside** what is on it (§4.4); make a user and a
   password; read the summary; press **Install**.
5. The installer partitions, formats, copies the system, installs the boot
   loader, and says to remove the stick and restart.
6. The machine boots Ferrix from its own disk -- through Ferrix's boot menu,
   which also offers the other system when there is one.

It is not a package manager, an updater or a recovery tool. It installs the
system the live medium carries and nothing else; network installs, updates
of an installed system and repair are later work, not in this design.

It is not a Secure Boot story. Ferrix's loader is not signed, and a machine
with Secure Boot on refuses it. The first release asks the person to turn
Secure Boot off, as Arch's image does; §10 decision 4 is how it could change.

**Exit of this design:** on x86-64, two gates and one machine.

* `cargo xtask test-install` (VM, in `check`): the live ISO boots under QEMU
  with a blank virtio disk and a second disk holding a fake "other system";
  the installer, driven by an answer file, installs beside it; the guest
  powers off; QEMU then boots the target disk alone, the boot menu chooses
  Ferrix, and Ferrix reaches `FERRIX-BOOT-OK` with `/` on the installed btrfs
  root. The other system's partitions are compared byte for byte, unchanged.
* `cargo xtask test-compositor --boot installer`: the graphical installer's
  pages match their screenshots, and a click-through with a virtual pointer
  ends in the same installed disk.
* The customer's reference PC (§10 decision 3), booted from a USB stick made
  from the release's ISO, installs beside its existing system and boots both.

## 2. What exists today (surveyed 2026-09-28)

Ferrix has half an installer already, and none of the other half.

**What is there:**

* Every `run` boot *installs* already: the kernel starts on the initramfs
  in tmpfs, finds a btrfs volume labelled `ferrix-root`, unpacks the
  initramfs onto it, and switches `/` there (`kernel/src/fs/root_disk.rs`,
  `GUIDE.md` §Getting started). An installed system is exactly that volume
  plus something that boots it.
* `ferrix.root=tmpfs` (and `--tmpfs-root`) is a live session: `/` in memory,
  no disk written.
* The UEFI loader `boot/uefi` reads `FERRIX/KERNEL.ELF`, `FERRIX/INITRD.IMG`
  and `FERRIX/CMDLINE.TXT` from the FAT volume it was loaded from, and hands
  the kernel the firmware's GOP framebuffer.
* `xtask/src/fat.rs` writes FAT32 in plain Rust, with no mtools.
* `libs/fs/btrfs-write` writes btrfs that `btrfs check` accepts, including
  allocating new chunks (`grow.rs`) -- what a fresh mkfs'd volume needs to
  grow into its device.
* ACPI, PCI with MSI, and an IOMMU are in the kernel; ring-3 drivers sit
  behind `blkring` (`docs/BLOCK-RING.md`), so a new disk driver is a new
  process speaking an existing protocol.
* hyprix, and `userland/compositor/toolkit` with `render` and `text` for
  drawing a client's window; `authd` for accounts and passwords
  (`docs/AUTH.md`).

**What is missing:**

| Gap | Where it shows |
|---|---|
| Userland cannot open a disk | `kernel/src/fs/devfs.rs:1355`: every block device is `ENXIO` on open |
| No partition tables | root is found by label on a *whole* virtio disk, and only `vdd`..`vdg` (`root_disk.rs:78`) |
| No mkfs for btrfs | every volume is unpacked from a host fixture |
| No FAT in the running system | the ESP can be written on the host only |
| No image with a partition table, no ISO | `ferrix.img` is a bare FAT32 volume; releases carry notes only |
| No boot menu | the loader boots Ferrix and nothing else |
| No UEFI variables from the OS | the kernel does not keep the firmware's runtime services |
| Only virtio disks | no NVMe, AHCI, or USB mass storage |
| No USB 3, no PS/2 | `usb-host` is EHCI; a modern PC's keyboard and sticks sit on xHCI or i8042 |
| No display without virtio-gpu | on a PC with no driver for its GPU, hyprix has no screen |
| No widgets | the toolkit draws surfaces; buttons, lists and text fields are to write |

## 3. The medium

### 3.1 One layout for both files

The `.img` and the `.iso` carry the same partitions, so there is one thing
to test:

```
GPT (protective MBR; on the .iso also an ISO 9660 volume and El Torito)
 1  ESP, FAT32, 64 MiB   EFI/BOOT/BOOTX64.EFI      the loader
                         FERRIX/KERNEL.ELF
                         FERRIX/INITRD.IMG          the whole system, installer included
                         FERRIX/CMDLINE.TXT         "ferrix.live ferrix.root=tmpfs"
```

The live system *is* the initramfs, as every test boot is today, so the
medium needs no root partition and no squashfs. The initramfs is also the
installer's source: what it copies to the target is what the live session
is running. A live session keeps the medium's ESP label `FERRIX-LIVE` so
the installer knows which disk not to offer.

The `.iso` is the `.img` with an ISO 9660 filesystem and an El Torito catalog
added in the space GPT leaves free, the layout `xorriso -as mkisofs
-isohybrid-gpt-basdat` makes: firmware booting it as a CD finds the ESP
through El Torito's EFI entry, and firmware booting it as a disk (after
`dd`) finds it through the GPT. It is written by `xtask/src/iso.rs` in Rust,
as `fat.rs` is, so neither Windows nor Linux build hosts need `xorriso`.
The ISO 9660 side carries a single `README.TXT`; nothing boots from it.

### 3.2 Which architectures

x86-64 is the target of every choice above. AArch64 gets the same `.img` and
`.iso` for virtual machines (UTM, Parallels, QEMU `virt`), since its loader
is the same code and virtio is its only hardware. ARMv7-A, the DK1 and the
Pixel keep their `xtask flash` paths; an installer on them is not asked for.

### 3.3 Building and releasing

`cargo xtask live --arch x86_64 --release` writes
`build/x86_64/ferrix-live.img` and `ferrix-live.iso`. The release workflow
(`.github/workflows/release.yml`) builds both for x86-64 and AArch64, names
them `ferrix-<tag>-<arch>.{img,iso}`, writes `SHA256SUMS`, and attaches all
of it to the release it creates. The release notes gain a short "Install"
section pointing at `docs/INSTALL.md`, the user-facing guide (§8).

## 4. The installer

### 4.1 Two programs, one engine

```
userland/installer/
  engine/   ferrix-installer-engine: a library, no I/O of its own beyond a Disk trait
  cli/      /bin/ferrix-install: runs an answer file, prints progress; the gates use it
  gui/      /bin/ferrix-installer: the desktop app, on toolkit + render + text
```

The engine takes a **plan** and turns it into a list of **steps**, each
of which reports progress and can be dry-run:

```rust
pub struct Plan {
    pub disk: DiskId,                // by-path and serial, never "vda"
    pub layout: Layout,              // Erase | Beside { free: Range } | Replace { partition }
    pub esp: EspChoice,              // New | Existing(PartitionId)
    pub root_size: Option<u64>,      // default: all of it
    pub user: NewUser,               // name, full name, password (goes to authd, never stored)
    pub hostname: String,
    pub keyboard: String,            // an xkb layout, as hyprix's config takes it
    pub timezone: String,
}
```

`Plan::validate(&disks)` refuses a plan that would write outside the space
it names, and `Plan::steps()` is pure, so the rule "no byte outside the
chosen space is written" (§4.4) is tested on the host with no disk at all.
An answer file is the plan in TOML; the GUI builds the same struct.

### 4.2 The steps

1. **Partition.** Write the new GPT entries (both headers, both entry
   arrays, the protective MBR) with `libs/fs/partition`. On *Erase*: ESP
   512 MiB, root the rest. On *Beside*: root in the chosen free range, and a
   new ESP only if the disk has none. Then `BLKRRPART` and wait for the
   kernel to list the new partitions.
2. **Format.** `mkfs.btrfs` of our own (§5.3) on root, label `ferrix-root`,
   a new filesystem UUID. `mkfs.fat` on a new ESP.
3. **Copy the system.** Mount the new root read-write and unpack the running
   initramfs onto it, which is the kernel's `root_disk::install` done from
   userland -- the same function, moved into a library both call, so the
   installed system is what a `run` boot's would be, stamp and all.
4. **Configure.** `/etc/hostname`, the keyboard layout in the desktop's
   config, the time zone, and the account through `authd` (a `useradd`-like
   request with `--root` pointing at the new volume, `docs/AUTH.md`).
5. **Boot files.** Copy `BOOTX64.EFI`, `KERNEL.ELF` and `INITRD.IMG` into
   `EFI/ferrix/` on the ESP, and write `EFI/ferrix/CMDLINE.TXT` with
   `ferrix.root=PARTUUID=<root's>`. On an ESP of its own, also the fallback
   `EFI/BOOT/BOOTX64.EFI`; on a shared one, never (it belongs to the other
   system).
6. **Boot entry.** Add a `Boot####` variable "Ferrix" pointing at
   `\EFI\ferrix\BOOTX64.EFI` and put it first in `BootOrder`, through
   `/sys/firmware/efi/efivars` (§5.5). A firmware that forgets it (some do)
   still finds Ferrix through the fallback path on an ESP of its own; on a
   shared ESP the summary says to pick "Ferrix" in the firmware's boot menu.
7. **Sync and unmount**, then say to remove the medium and restart.

A failure before step 1 finishes changes nothing. After it, the installer
says which step failed and what is on the disk; it does not try to undo a
partition table, which is a second chance to lose data.

### 4.3 Which disks it offers

Every disk the kernel lists, except the one the live system booted from,
with its model, size, and what is on it: the partitions, their file system
by magic (FAT, NTFS, ext4, btrfs, swap, BitLocker, Apple's APFS), their
labels, and "Windows", "Ubuntu" and so on when an ESP holds a loader Ferrix
recognises (§5.6). A disk with an MBR partition table rather than a GPT is
offered only for *Erase*: converting it would change what the other system
boots from.

### 4.4 Beside another system

The page offers three ways, as Ubuntu's does:

* **Install beside `<system>`**, in the largest unpartitioned range of at
  least 16 GiB, with a slider for how much of it Ferrix takes.
* **Replace a partition**: pick one, its data is lost, Ferrix goes there.
  The page shows what it holds before it is picked.
* **Erase the disk.**

What it does **not** do in version 1 is shrink the other system's
partition. Shrinking NTFS safely means an NTFS implementation Ferrix does
not have (`ntfsresize` is 2,000 lines of C over libntfs-3g, GPL), and a
Windows partition that is hibernated, fast-started, or BitLocker-encrypted
must not be touched at all. The installer detects all three and says so.
When there is no free space, the page says: shrink the volume in Windows'
*Disk Management* (or GNOME Disks), then restart the installer. §10
decision 1 is whether to write a shrinker anyway.

The rule that makes *beside* safe: **no byte outside the chosen range and
the GPT's own sectors is written**, and the ESP is written only inside
`EFI/ferrix/`. The gate (§7) checks it by hashing every other partition
before and after.

### 4.5 The graphical installer

A toplevel window on hyprix, 900 × 640, one page at a time with *Back* and
*Next*, as Calamares and Ubuntu's installer are laid out:

1. **Welcome**: "Try Ferrix" (closes the installer) or "Install Ferrix".
2. **Keyboard**: a list of layouts and a field to try it in.
3. **Disk**: the disks of §4.3 as bars coloured by partition, and the three
   choices of §4.4 under the chosen disk; the bar shows the result.
4. **You**: name, user name, password twice, hostname.
5. **Summary**: every change in words, what will be erased in red, and
   **Install**.
6. **Installing**: a progress bar and the current step; a *Details* toggle
   shows the CLI's log.
7. **Done**: *Restart now*.

The widgets are new: a button, a label, a list, a radio group, a text field
(with a password mode and the toolkit's clipboard), a progress bar, the
partition bar and a slider. They go in `userland/compositor/widgets` beside
the toolkit, so later apps get them too. They draw with `render` and `text`
as the other clients do, and follow the desktop's theme colours.

The live session's hyprix config starts the installer on login, and the live
session logs in without a password as user `ferrix`.

## 5. What the kernel and libraries gain

### 5.1 Block devices from userland

`devfs` opens block nodes (`devfs.rs:1355`): `read`, `write`, `pread`,
`pwrite`, `lseek` (with `SEEK_END` giving the size), `fsync`, and the
`ioctl`s the installer and busybox use: `BLKGETSIZE64`, `BLKSSZGET`,
`BLKPBSZGET`, `BLKRRPART`, `BLKFLSBUF`. I/O goes through the block core to
the driver's ring, as a mounted filesystem's does. Opening a disk that has a
mounted partition for writing is `EBUSY`, and a partition's node is limited
to its range, as on Linux. Only root opens them.

### 5.2 Partitions

`libs/fs/partition`: GPT read and write (CRC-32 of headers and entry
arrays, the backup at the end of the disk, the protective MBR), MBR read.
Pure Rust, `no_std`, fuzzed like `libs/fs/btrfs`. The kernel's block core
reads each disk's table when the driver announces it and on `BLKRRPART`,
and makes `/dev/vda1`, `/dev/nvme0n1p1`, `/dev/sda1` with Linux's minor
numbers, plus `/sys/class/block/<name>/{partition,start,size}` and
`/dev/disk/by-partuuid/`.

Root is then found by `ferrix.root=PARTUUID=…` or `ferrix.root=LABEL=…` on
any disk or partition; with neither, as today, by the label `ferrix-root`,
now scanned on every disk rather than `vdd`..`vdg`. The live medium is found
the same way by its ESP label, so the loader need not say which disk it was.

### 5.3 mkfs.btrfs

`libs/fs/btrfs-mkfs` and `/sbin/mkfs.btrfs`: what `mkfs.btrfs` makes by
default for one device -- `SINGLE` data, `DUP` metadata, skinny metadata,
`NO_HOLES`, the free-space tree, CRC-32C -- built as trees in memory and
written in one pass, then grown by `btrfs-write`'s existing chunk
allocation. Tested by `btrfs check` on the host over sizes from 256 MiB to
2 TiB (sparse files), and by mounting what `mkfs.btrfs` of btrfs-progs
makes of the same size and comparing their trees item by item.

### 5.4 FAT

`xtask/src/fat.rs` moves into `libs/fs/fat`, and gains the half it lacks:
opening an existing FAT32 (or FAT16) volume, walking directories, long
names, and adding files to it -- which is what writing into another
system's ESP needs. It is a library the installer links, not a kernel
filesystem: nothing mounts the ESP while Ferrix runs. `xtask` keeps using it
for its images.

### 5.5 UEFI variables

The loader passes the firmware's runtime services table and the memory map's
runtime regions to the kernel (`ferrix-bootinfo`), which calls
`SetVirtualAddressMap` and keeps a ring-0 call path for `GetVariable`,
`GetNextVariableName` and `SetVariable`, serialised by one lock, with the
calls' memory mapped only while one runs. `/sys/firmware/efi/efivars` is
Linux's interface to them, so the installer's `Boot####` code is what
`efibootmgr` does. On a machine where the firmware faults in a call, the
kernel turns the variables off and says so, as Linux's `efi=noruntime` does,
and the installer falls back to §4.2 step 6's second path.

### 5.6 A boot menu

The loader gains a menu, shown for three seconds (and until a key is
pressed) when the ESP it is on holds another system's loader:
`EFI/Microsoft/Boot/bootmgfw.efi`, `EFI/ubuntu/shimx64.efi` or
`grubx64.efi`, `EFI/fedora/…`, `EFI/debian/…`, `EFI/arch/…`, any
`EFI/*/BOOTX64.EFI`. Choosing one starts it with `LoadImage` and
`StartImage`. It draws on the GOP framebuffer with `fbtext`'s font and
takes the firmware's keyboard, so it works before any Ferrix driver.
`EFI/ferrix/MENU.TXT` sets the default and the time-out. With no other
loader there is no menu and no wait, as today.

## 6. Real PCs

Everything above runs in a VM on virtio. A PC also needs Ferrix to see its
disk, the stick it booted from, its keyboard and mouse, and its screen.
Each is a ring-3 driver behind an existing protocol, started by `devmgr` on
the PCI class it matches:

| Slice | Driver | Behind | Why a PC needs it |
|---|---|---|---|
| H1 | NVMe | `blkring` | the disk of nearly every PC since 2018 |
| H2 | AHCI (SATA) | `blkring` | older PCs, and SATA SSDs |
| H3 | xHCI | `usb-host` | every USB port on a PC since 2012; sticks, keyboards, mice |
| H4 | USB mass storage (BOT + SCSI) | `blkring` | the stick the live system is on |
| H5 | i8042 keyboard and touchpad | `input` | laptops' built-in keyboards |
| H6 | the firmware's framebuffer | `display` | a screen when there is no driver for the GPU; stage 21 is the one for NVIDIA |

H6 is the plain answer for a PC's screen: hyprix draws into the GOP
framebuffer the loader already hands over, at the mode the firmware chose,
with no mode setting, no vsync and no GPU. That is how every Linux live
image starts before its GPU driver loads, and it is enough for an installer
and a desktop. Stage 21's NVIDIA driver is not needed for any of this.

Network on a PC (Intel and Realtek Ethernet, Wi-Fi) is not needed to
install, since the medium carries the system, and is not in this design.

The unknown is bring-up: Ferrix has never booted on an x86 PC, only under
QEMU and on two Arm boards. ACPI tables, interrupt routing, and firmware
that behaves unlike OVMF will each cost something no estimate can see. §9's
H0 is a slice for exactly that, run on the reference machine before the
drivers, with its points a guess.

## 7. Tests

| Gate | What it proves | In `check` |
|---|---|---|
| host tests of `partition`, `btrfs-mkfs`, `fat`, `installer-engine` | tables, volumes, and plans, including "no write outside the range", with `btrfs check` and `sgdisk --verify` on what they write | yes |
| `test-install --arch x86_64` | ISO boots under OVMF, answer file installs on a blank disk, power off, target boots alone to `FERRIX-BOOT-OK` with `/ is btrfs on vda2` | yes |
| `test-install --beside` | as above on a disk pre-made with an ESP holding a stub `bootmgfw.efi` (an EFI app of ours that prints `OTHER-OS`) and an NTFS-looking partition; after install, both partitions hash unchanged; the menu boots Ferrix, and with a key press the stub | yes |
| `test-install --arch aarch64` | the first gate on AArch64 | yes |
| `test-install --usb-live` | the ISO attached as `usb-storage` on `qemu-xhci`, target on `nvme`: H1, H3 and H4 under QEMU | after H4 |
| `test-install --ahci` | target on QEMU's `ich9-ahci` | after H2 |
| `test-compositor --boot installer` | each page's screenshot; a click-through by virtual pointer ends in the same disk as the answer file | yes |
| the reference PC | §1's exit, by hand, with a log kept in this document | no |

QEMU emulates NVMe, AHCI, xHCI, USB storage and i8042 faithfully enough
that every driver of §6 is gated before it meets the reference PC, which
then only tests the machine.

## 8. Documentation

`docs/INSTALL.md`, for someone who has never built Ferrix: where to
download, how to write a stick on Windows, macOS and Linux, turning Secure
Boot off, what "beside" can and cannot do, and how to boot a VM from the
ISO in QEMU, VirtualBox, virt-manager and UTM. `README.md` and the website
point at it before they point at `cargo xtask run`.

## 9. The slices and their points

In landing order. The VM path is first and complete on its own: after I9
anyone can install Ferrix in a VM from a release. The PC slices can start
beside it (they touch nothing the VM slices do) once H0 has shown the
reference machine boots.

| # | Slice | Points |
|---|---|---|
| I1 | Block devices from userland (§5.1) | 5 |
| I2 | `libs/fs/partition`, partitions in the block core, root by `PARTUUID`/`LABEL` on any disk (§5.2) | 8 |
| I3 | `libs/fs/btrfs-mkfs`, `/sbin/mkfs.btrfs` (§5.3) | 8 |
| I4 | `libs/fs/fat` with read-and-add, `mkfs.fat` (§5.4) | 5 |
| I5 | The engine and `/bin/ferrix-install`, answer files; `root_disk::install` shared (§4.1, §4.2) | 8 |
| I6 | `xtask live`: GPT `.img`, hybrid `.iso` (§3.1) | 8 |
| I7 | `test-install`, blank disk and `--beside`, x86-64 and AArch64 (§7) | 5 |
| I8 | The loader's boot menu (§5.6) | 5 |
| I9 | UEFI runtime variables and `efivars` (§5.5) | 8 |
| I10 | Widgets (§4.5) | 13 |
| I11 | The graphical installer, its screenshot gate, the live session's autostart and login | 13 |
| I12 | Release images, checksums, `docs/INSTALL.md`, README and website (§3.3, §8) | 3 |
| | **VM path** | **89** |
| H0 | First boot on the reference PC: serial or screen, ACPI, interrupts, timer (a guess) | 13 |
| H1 | NVMe | 8 |
| H2 | AHCI | 8 |
| H3 | xHCI with HID | 21 |
| H4 | USB mass storage | 5 |
| H5 | i8042 keyboard and touchpad | 5 |
| H6 | Firmware framebuffer display for hyprix | 5 |
| H7 | The reference PC's exit, and what it finds | 8 |
| | **Real PCs** | **73** |
| | **Total** | **162** |

At the fleet's measured pace these are days, not weeks; the PC half's
risk is H0 and H7, which no estimate covers.

## 10. For the customer to decide

1. **Shrinking the other system.** Version 1 installs into free space or a
   partition the person gives up, and tells them to shrink Windows in
   Windows (§4.4). *Recommended.* The alternative is an NTFS shrinker of our
   own: unsized, at least 40 points, and the one part of this where a bug
   loses someone's files.
2. **The live session's account.** A user `ferrix` with no password, logged
   in automatically, as Ubuntu's live session does. *Recommended.* Or a
   login prompt with a password printed on the boot screen.
3. **The reference PC.** Which machine H0 and H7 are run on, and whether the
   product owner may boot it from a stick (`hardware-use` rule: the product
   owner alone decides hardware use; nothing is written to its internal disk
   except in H7, and only beside what is there).
4. **Secure Boot.** Version 1 asks the person to turn it off. *Recommended.*
   Booting with it on means shim, signed by Microsoft for a distribution,
   which Ferrix cannot get today; enrolling Ferrix's own key (as a MOK) is a
   middle way, at about 8 points.
5. **A text-mode fallback.** The CLI of §4.1 exists anyway for the gates; it
   can gain a menu for machines where the desktop does not come up (2
   points). *Recommended*, since H6 is the only screen a PC is sure of.

## 11. Where it stands

2026-09-28: this design, written, and approved by the customer the same
day. §10: decisions 1, 2, 4 and 5 as recommended; decision 3, the reference
PC, is open, and the VM path does not wait on it. I1 is being built.
