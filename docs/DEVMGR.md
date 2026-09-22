# devmgr: what the kernel starts it with, and what it does

Version 1. Written by os-5b (stage 10), the shape decided by the product owner
os-23 on 2026-09-13; the object side reviewed by stage 9, the start shape by
stage 11. `docs/ARCHITECTURE.md` §7 is the architecture; this is the protocol
between the kernel and `devmgr`, and between `devmgr` and the drivers it
starts. The kernel's half of it — `device_info`, `device_quiesce`, bus
mastering at the first pin, START — is on develop; `devmgr` the program, on
`ferrix-rt`, is `user/devmgr`, started by `kernel/src/devmgr.rs`; the
messages are `libs/devmgr-proto`.

## 1. What devmgr is, and what it is not

`devmgr` is a native program in the initramfs, `/sbin/devmgr`, started by the
kernel once, after the boot checks and before `init`. It matches devices to
drivers and starts each driver in a job of its own with exactly what
ARCHITECTURE §7 says a driver gets: its device, the channel to the subsystem
it serves, and the means to claim that device's memory, interrupts and DMA.
It watches each driver and, when one dies, makes its device safe before
anything of it is reused.

It is **not** a driver, and it drives nothing: it never maps a device's
registers, never pins memory for one, never touches configuration space. It
is also **not** the enumerator: the kernel found the devices and keeps their
nodes; `devmgr` is handed them. And it does not read files. A native program
has native calls only — handles, channels, VMOs, ports, processes — and no
descriptors, so the kernel reads the driver images out of the initramfs and
hands them over as memory (§2). That is also what makes the rule in §5 hold
by construction.

## 2. Kernel → devmgr: the bootstrap channel

The kernel creates a channel, keeps one end, and starts `/sbin/devmgr` with
the other as its bootstrap handle (`Handle(1)`, which `ferrix-rt`'s
`Bootstrap` adopts; `docs/BLOCK-RING.md` §6.4 says the same of a driver's).
Before starting it the kernel has written one message on its end:

```
DEVICES  kernel -> devmgr, 24 + 32 x drivers bytes
0   4  type = 1
4   4  length
8   4  devices      how many devices in this message; each is two handles
12  4  drivers      how many (name, image) pairs follow; 0 unless first
16  4  more         how many devices still come in later DEVICES messages
20  4  flags        bit 0: the first message, which carries the job and
                    the images
24  32 name[0]      the first driver's program name, NUL-padded, as
                    process_create takes it (PROCESS_NAME_MAX)
56  32 name[1]      ...
handles: [job (first only), device 0, device 0 again, ..., image 0 ...
          (first only)]
```

* **The job** comes first: a root job of `devmgr`'s own, with every right a
  job carries, under which it makes a job per driver. A native program is
  given no job otherwise, and `process_create` needs one.

* **The devices** are every node `device::devices()` publishes, in that order,
  each as two handles. The first has `DEVICE_RIGHTS` (`TRANSFER | MANAGE`) and
  is the handle the driver will be given in START, unchanged. The second stays
  with `devmgr` for the quiesce in §4, because a handle given away is gone. It
  has `DEVICE_RIGHTS | DUPLICATE`, so a driver started again after a death
  gets a duplicate of it and `devmgr` still has one for the next quiesce.
  `device_info` (0x1049) on either says what the device is.
* **The drivers** are every program in one fixed initramfs directory,
  `/lib/drivers/`, in the order of `/lib/drivers/MANIFEST`, one name per
  line, which `xtask` writes when it builds the image: the kernel has no
  directory listing of its own to spend on this. For each, the kernel reads
  the file into a VMO it creates for the purpose — anonymous memory, filled
  by the kernel from the initramfs — and hands the VMO with
  `READ | TRANSFER`. The name is the file's name. `process_create` (0x1030)
  takes exactly this: a job, an image VMO with `READ`, a name.
* A channel message carries at most `CHANNEL_MAX_HANDLES` (64) handles, and
  a device takes two: ARMv7-A's machine publishes 36 nodes, its 32
  virtio-mmio transports among them. So the kernel sends as many DEVICES
  messages as it takes — the first with the job and the images, the rest
  with devices only — each saying in `more` how many devices are still to
  come, and `devmgr` reads until that is zero.

`devmgr` answers on the same channel once it has done what §3 says:

```
REPORT   devmgr -> kernel, 16 bytes
0   4  type = 2
4   4  length = 16
8   4  started      drivers started and whose disks the kernel accepted
12  4  failed       devices that matched a driver and got none
```

The kernel waits for REPORT with a deadline, prints one line for the boot log
— `devmgr   N devices, M drivers, K started, F failed` — and goes on to
`init`. `xtask test-boot` requires that line on the machines it configures
with a disk, with `started` at least 1 and `failed` 0, so a `devmgr` that
starts nothing fails the boot rather than leaving `/dev/vda` quietly absent.
The kernel keeps its end open: a later message from `devmgr` is a report of
a driver's death or of its restart (§4), printed the same way.

## 3. What devmgr does with them

1. `device_info` on every device. The identity decides the driver, by a table
   in `devmgr` and nowhere else: vendor `0x1AF4` with device `0x1042` or
   `0x1001` is virtio-blk, driven by `blk`; stage 17 adds virtio-gpu
   (`0x1050`) and virtio-input (`0x1052`) to the same table, and their
   drivers to the same directory, with no change to the kernel. A device
   nobody drives is left alone, and its handle closed.
2. Devices of one kind are named in PCI order — `vda`, `vdb`, … for disks, as
   `docs/BLOCK-RING.md` §6.1 decides — so names are stable on a given machine.
3. For each match, in that order:
   * `block_ring_create` (0x1048) on the device: the driver's end of the
     ring's control channel. For a device whose subsystem has no ring yet,
     the kernel has no call and `devmgr` starts nothing.
   * `job_create`: a job of the driver's own, under `devmgr`'s.
   * `process_create` in it, from the driver's image VMO, named after the
     driver and the disk (`blk:vda`).
   * A channel pair; on `devmgr`'s end, START (`docs/BLOCK-RING.md` §6.4)
     built from the `DeviceInfo` — the blocks, the multiplier, the MSI-X
     table size, the PCI device identifier, the location, the name — with
     handles `[device, control]`, exactly `DEVICE_RIGHTS` and
     `CONTROL_RIGHTS`. `devmgr` gives the device away here and keeps no
     handle to it: the driver holds it, and the kernel's node outlives both.
   * `object_wait_async` on the process handle for `TERMINATED`, on
     `devmgr`'s one port, with the disk's index as the key.
   * `process_start` (0x1031) with the other end of the pair as the
     bootstrap.
4. `devmgr` does not wait for the driver's HELLO — that is between the driver
   and the kernel — but its REPORT counts a driver as started only once the
   kernel has published the disk. `devmgr` cannot look in `/dev`, so the
   kernel tells it: after accepting a HELLO for a device it handed `devmgr`,
   the kernel writes on the bootstrap channel:

```
PUBLISHED  kernel -> devmgr, 16 bytes
0   4  type = 3
4   4  length = 16
8   4  location   the device's PCI address word
12  4  reserved   zero
```

   `devmgr` starts one driver, waits for its PUBLISHED, then starts the
   next, so disks register in PCI order and no two drivers race to be `vda`;
   then it sends REPORT. A native program has no clock, so the deadline is
   the kernel's patience for REPORT: a driver that never publishes holds
   `devmgr` there, and the boot fails saying so. A driver whose start fails
   outright counts as failed.

## 4. When a driver dies

A `TERMINATED` packet on `devmgr`'s port names the disk. The kernel's ring
has already failed every outstanding read with `EIO` and unpublished the node
(`docs/BLOCK-RING.md` §6.3), and the driver's handles have closed: its pins
are gone from any translated domain, and kept, leaked, in an untranslated
one. Nothing has reset the device, and until something does the kernel
refuses a new ring for it (`ALREADY_BOUND`). `devmgr`:

1. calls `device_quiesce` (0x104A) on the device — bus mastering off, so the
   device reaches nothing, and the ring's claim released. `TERMINATED` fires
   when the driver's handles close, which queues `PEER_CLOSED` for the ring's
   task but does not wait for it, so the quiesce may arrive before the ring
   has ended: the kernel then waits, bounded, for the ring to let the device
   go, since the driver's end of the channel is provably closed. The display
   and render cores are waited for the same way (`kernel/src/claim.rs`), so a
   quiesced device has no claim left on it and a driver started again gets its
   channel. Only a driver still holding its end gets `BAD_STATE`, which
   `devmgr` never retries; a core that has not let go within the kernel's
   patience answers `TIMED_OUT`, which `devmgr` retries until it succeeds,
   since the device must not stay on;
2. writes on the bootstrap channel a DIED message, which the kernel prints:

```
DIED     devmgr -> kernel, 16 bytes
0   4  type = 4
4   4  length = 16
8   4  location
12  4  status     the driver's exit status; 137 if killed
```

   It reports the status as 137 whether the driver was killed or exited,
   since a `TERMINATED` packet carries no status;
3. starts a **display** driver again, once the quiesce succeeded: in a new
   job, on a duplicate of its kept device handle, with the START it was
   first given, and waits for PUBLISHED as at boot. The kernel numbers
   cards and render nodes lowest-free, so the card comes back as the
   `card<N>` it was, and a compositor that waits for it (hyprix does) opens
   it again. A driver that publishes gets RESTARTED, which the kernel
   prints:

```
RESTARTED devmgr -> kernel, 16 bytes
0   4  type = 5
4   4  length = 16
8   4  location
12  4  restarts   how many times this device's driver was started again
```

   A native program has no clock, so the budget is a count, not a rate:
   eight restarts per device, after which the device stays quiesced, as does
   one whose restarted driver dies before it publishes.

Every other kind is not started again. A dead disk driver is a dead disk,
said on the console, and the device stays quiesced: what a mounted
filesystem should do with a disk that went and came back is its own
decision. The net, input and serial cores do not yet wait for a dead
driver's claim in the quiesce, so a driver started again would be refused
its channel; each can be added to `restarted` in `user/devmgr` once its core
does.

It began as a bug. With no restart, `kill -9` of the `gpu` driver under the
desktop took the card away for good, the compositor ended on `ENODEV`, and
the compositor was init, so the machine powered off.
`cargo xtask test-restart` (a shell kills it twice) and
`cargo xtask test-compositor --boot restart` (a script kills it twice under
hyprix) are the gates.

`device_quiesce` needs `MANAGE`, and `devmgr` gave one device handle away in
START; the quiesce goes through the second handle §2 gave it for exactly
this.

## 5. No driver faults on the disk it serves

The decision of 2026-09-13 (`docs/BACKLOG.md`): a driver serving a disk must
never take a page fault that fills through that disk, or it waits on its own
completion forever. Under this design it holds by construction rather than by
`devmgr` checking anything:

* a driver's image is an anonymous VMO the kernel filled from the initramfs
  before the driver existed; `process_create` reads it into the new process's
  memory, and nothing maps the file;
* a native program has no `mmap` of files — it maps VMOs, its own or ones it
  was handed — and the driver's are the ring VMO, the data VMO and its DMA
  memory, all anonymous;
* `devmgr` itself is started the same way, before any mount of a disk.

What is left to enforce is the future: a pivot onto btrfs must not re-exec or
remap a running driver from the new root, and a driver started after the
pivot must still get its image from the initramfs copy the kernel keeps.
`devmgr` is the one process that starts drivers, so it is where that rule
lives when the pivot exists.

## 6. What this settles for stage 10's exit

The exit criterion needs a sector read through a ring-3 driver. Stage 11's
boot check starts `blk` from the check itself with a kernel-driven parent,
sending the same START (`block_ring::start_for`), so the driver is proven
before `devmgr` exists; `devmgr` then replaces that parent for the running
system, with no change to the driver, and the boot check's line above is what
`xtask` reads. `devmgr` is the last program on stage 10's list; the BAR trust
row is beside it, not on the path.
