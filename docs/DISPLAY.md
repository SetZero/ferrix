# The display: `/dev/dri/card0` over a ring-3 virtio-gpu driver

Version 1. Written by the GUI session (os-e5) for iteration 1 of the
compositor's path, the customer's order of 2026-09-16: *a blank screen on
Ferrix in QEMU*, pulled forward from stage 17. Approved by the product owner
(os-f6) on 2026-09-16, with the decisions in §5. Implemented: L5 and L6 landed
together as one reviewed stack (os-02), since L5 alone would have been dead
code or the kernel drawing; `cargo xtask test-display` passes on x86-64 and
AArch64. That is iteration 1. Iteration 2's planes and properties (E4, §2.3)
are implemented too, reviewed by os-02.

## 1. What this is, and what it is not

Stage 17 gives a Linux compositor what it needs to draw: `/dev/dri/card0` with
the DRM/KMS subset a software-rendered compositor uses. `docs/ARCHITECTURE.md`
§7 puts the driver in ring 3, and §1 says the kernel draws nothing but a panic.
This document specifies iteration 1's cut of that:

* **the kernel's display core**, which owns buffers and the card node and
  speaks DRM to the compositor;
* **the control protocol** between the core and the ring-3 virtio-gpu driver;
* **the DRM subset** iteration 1 answers;
* **what a panic does** once the driver owns scanout.

It is not the virtio-gpu protocol, which the driver speaks to the device
(`libs/virtio::gpu`, to be written). It is not the GPU: no 3D, no render node,
no dmabuf, no PRIME (stage 19). It is not input (virtio-input is its own
iteration).

**Exit of iteration 1:** on x86-64 and AArch64, the compositor's first binary
opens `/dev/dri/card0`, finds the connector and its preferred mode, creates a
dumb buffer, maps it, fills it with one colour, adds a framebuffer and sets the
CRTC. `cargo xtask test-display` reads QEMU's screendump of the virtio-gpu head
and requires every pixel to be that colour. `cargo xtask run --display` shows
the same on a person's screen.

## 2. The four questions

### 2.1 Who owns the scanout memory: the core, in one VMO per card

**The display core owns the pixels.** Each card has one kernel VMO, the *card
VMO*: anonymous, sparse and committed on demand. It is sized to the card's
buffer budget (iteration 1: 256 MiB), which is also the most memory an opener
can make the card commit. A dumb buffer is a page-aligned range of it, the
first free one that fits. `MODE_MAP_DUMB` returns the range's offset. A buffer
is at most `MAX_BUFFER_PAGES` (8192 pages, 32 MiB: a 4K buffer), which the
driver sizes its backing lists for.

* **The compositor maps it with no new mapping code.** `card0`'s devfs inode
  answers `Inode::mapping()` with the card VMO. `mmap(fd, len, PROT_READ |
  PROT_WRITE, MAP_SHARED, offset)` then goes through the file-backed
  `MAP_SHARED` path that already landed (`kernel/src/syscall/memory.rs`,
  `map_file`). DRM's "fake offsets" become real offsets into one object.
  mmap asks the *inode*, not the per-open object, so the buffers live in
  the node's card state, not in the opener's `File`.
* **The driver pins it read-only.** The core sends the driver a duplicate of
  the card VMO handle once, with `READ | TRANSFER` rights only, never `WRITE`
  or `MAP`. For each buffer the driver calls `VMO_PIN(device, card_vmo,
  offset, length, PIN_READ_ONLY)` and `VMO_PIN_ADDRESSES`. It then gives the
  device those pages as the resource's backing (`RESOURCE_ATTACH_BACKING`,
  one entry per run of consecutive device addresses). The device only reads
  guest backing, so the driver can neither write nor see the pixels.
  `vmo_pin` already allows exactly this: `READ` on the VMO, `MANAGE` on the
  device.
* **No copy in the guest.** The compositor writes pages that are the device's
  backing. virtio-gpu 2D does copy on the host side (`TRANSFER_TO_HOST_2D`),
  which is QEMU's business.
* **Freeing.** `MODE_DESTROY_DUMB` sends `DETACH` without waiting, or, while
  a framebuffer still refers to the buffer, only takes the handle away and
  detaches with the last `RMFB`, as Linux keeps the object. Closing the card
  turns the scanout off and detaches every buffer the open made. A range
  comes back when `DETACHED` reports success, or when `ATTACHED` reports a
  failure: the card's task decommits it, so the next buffer there starts
  zeroed and shows nothing of the last one, then gives it back. A refused
  `DETACHED` keeps the range, and the id, out of use for good.
* **Buffer ids are the card's, not the open's.** The core numbers buffers on
  the card and never reuses an id, so no reply is taken for a later buffer's
  request and the driver never sees an id again that the device may still
  hold pages under. The DRM handle is the open's own name for one.
* **Nothing is left behind by a timeout.** A request waits 5 seconds for its
  reply. An attach that times out, and a buffer let go of while a flush of
  it is still in flight, become the card's orphans: the card's task detaches
  each as soon as the session allows. A reply nobody waits for any more is
  dropped.
* **A driver that is behind is not a dead one.** The kernel is the control
  channel's only writer and asks for room before the session commits to a
  request: a full channel answers `EBUSY` to the program, and a closed
  open's scanout-off and detaches wait for room instead of taking the card
  down.
* **A range a driver still pins is never reused.** After the decommit the
  core asks the card VMO whether any page of the range is still held for
  the device; one that is means the driver replied before it unpinned, and
  the range stays out of use for good, with a line on the console.
* **A driver refused before READY does not stop the boot.** devmgr waits for
  PUBLISHED or for the driver's exit, whichever comes first, and counts a
  driver that exits unpublished as failed.

**The size `card0` reports.** `card0`'s inode reports `st_size` equal to the
card VMO's size, `CARD_BUDGET`. `map_file`'s bounds check, which is what makes
touching a `MAP_SHARED` mapping past the end raise `SIGBUS`, then holds with no
devfs special case (os-f6's decision).

The budget is one named constant with its arithmetic beside it: a 4K mode is
3840 × 2160 × 4 bytes, about 33 MiB a buffer, so double-buffered 4K is 66 MiB.
256 MiB leaves room for a cursor and a third buffer and still bounds what one
opener can commit.

### 2.2 The driver protocol: a control channel, `libs/displayctl`

Frames do not move, so there is no data ring. There is one control `Channel`
per card, with the message shape `libs/blkring/src/control.rs` uses: a
fixed-size little-endian message, a type, a version, validation in the order
fields are read, and handles alongside. It goes in a new host-tested crate,
`libs/displayctl`, with a fuzz target, as `libs/blkring` did.

**Bring-up follows blk.** devmgr's table gets `(0x1AF4, [0x1050], b"gpu",
Kind::Display)`. `DEVMGR.md` already names 0x1050. `start_display` asks the
kernel for the control channel with a new native call,
`DISPLAY_CONTROL_CREATE` (0x104C, beside `BLOCK_RING_CREATE`). It then sends
`START` (blk's layout: the four capability `Block`s, MSI-X size, device id,
location, name) to `/lib/drivers/gpu` and waits for `PUBLISHED`, like the
others.

| Type | Direction | Body | Handles |
|---|---|---|---|
| `HELLO` | driver → core | version; scanout count (1 in iteration 1); for each scanout the preferred mode `width, height` from `GET_DISPLAY_INFO` and whether it is enabled | driver port (`WRITE \| TRANSFER`) |
| `READY` | core → driver | card id | card VMO (`READ \| TRANSFER`), core port (`WRITE`) |
| `REFUSED` | core → driver | reason | — |
| `ATTACH` | core → driver | buffer id, offset, length, width, height, stride, format (`XRGB8888` only) | — |
| `ATTACHED` | driver → core | buffer id, status | — |
| `SCANOUT` | core → driver | scanout, buffer id (0 turns the scanout off), source rectangle | — |
| `FLUSH` | core → driver | buffer id, damage rectangle, sequence | — |
| `FLIPPED` | driver → core | sequence, status | — |
| `DETACH` | core → driver | buffer id | — |
| `DETACHED` | driver → core | buffer id | — |
| `STOP` / `STOPPED` | as blk | | |

**Each message maps to device commands:**

* `ATTACH`: pin, then `RESOURCE_CREATE_2D` (format `B8G8R8X8_UNORM`, which
  is little-endian `XRGB8888`), then `RESOURCE_ATTACH_BACKING`. A refused
  create is unpinned; refused backing is unreferenced, then unpinned; either
  way `ATTACHED` carries the failure.
* `SCANOUT`: `SET_SCANOUT`.
* `FLUSH`: `TRANSFER_TO_HOST_2D` over the damage, from the offset of the
  rectangle's first pixel in the backing, then `RESOURCE_FLUSH`. `FLIPPED`
  is sent when the flush's response arrives; a refused transfer skips the
  flush and `FLIPPED` says so.
* `DETACH`: `RESOURCE_DETACH_BACKING`, `RESOURCE_UNREF`, then the pin's
  handle is closed.

**Pages the device may still hold are never unpinned.** If the device
refuses `RESOURCE_DETACH_BACKING`, the driver does not unreference the
resource and does not close the pin: the range stays pinned for good, and
`DETACHED` reports `DeviceRefused`, which tells the core never to hand that
range out again. Unpinning it would let the device write into memory that
belongs to someone else (os-f6's decision, 2026-09-16; `libs/virtio-gpu`'s
`pipeline` implements it and its fuzz target checks it).

The driver runs commands one at a time on the control queue: a frame is two
commands, and at 60 frames a second a queue per frame is not worth its
complexity.

**Doorbells.** A message on the channel is the doorbell both ways, and each
side waits on its port for `PACKET_SIGNAL` on the channel. This is blk's STOP
path, which already works. The ports in `HELLO` and `READY` are for later
(cursor queue, display-change events) and cost nothing now.

**What the core never trusts:** a buffer id it did not send, a sequence out
of order, a `HELLO` with more scanouts than it can publish, or a mode above
8192×8192. Any of these is `REFUSED`, then quiesce, the same as a blk driver
that lies.

**`PUBLISHED`** is sent when the core has registered `card0`, which happens
after `HELLO` is accepted, as blk registers its disk.

### 2.3 The DRM subset

`card0` answers these ioctls. Numbers and layouts go into `libs/linux-abi`
(`drm` module), from a probe compiled on nazuna against `/usr/include/drm`
(`linux-libc-dev`), at both pointer widths, and pinned by tests. The probe
source is committed this time.

| ioctl | Iteration 1 |
|---|---|
| `DRM_IOCTL_VERSION` | name `virtio_gpu`, so drm-rs and Smithay identify the card |
| `DRM_IOCTL_GET_CAP` | `DRM_CAP_DUMB_BUFFER` = 1, `DUMB_PREFERRED_DEPTH` = 24, `DUMB_PREFER_SHADOW` = 0, `TIMESTAMP_MONOTONIC` = 1, `CRTC_IN_VBLANK_EVENT` = 1; others `EINVAL` |
| `DRM_IOCTL_SET_CLIENT_CAP` | `UNIVERSAL_PLANES` takes 0 or 1 (E4) and changes only what `GETPLANERESOURCES` lists; `ATOMIC` refused with `EOPNOTSUPP`, so clients fall back to legacy; the rest `EINVAL` |
| `DRM_IOCTL_SET_MASTER`, `DROP_MASTER` | succeed; the exclusive open (below) is the master |
| `MODE_GETRESOURCES` | a CRTC, an encoder and a connector per scanout the driver reported, and the framebuffer ids |
| `MODE_GETCONNECTOR` | `Virtual-<n>`, connected when the scanout is enabled, the preferred mode from `HELLO` plus the standard modes that fit |
| `MODE_GETENCODER`, `MODE_GETCRTC` | the head's own, each encoder driving the one CRTC of its head |
| `MODE_CREATE_DUMB`, `MODE_MAP_DUMB`, `MODE_DESTROY_DUMB` | §2.1; `bpp` 32 only |
| `MODE_ADDFB`, `MODE_ADDFB2`, `MODE_RMFB` | `XRGB8888` only; a framebuffer names one dumb buffer |
| `MODE_SETCRTC` | `SCANOUT` then `FLUSH` of the whole buffer |
| `MODE_PAGE_FLIP` | `SCANOUT` if the buffer changed, then `FLUSH`; `DRM_MODE_PAGE_FLIP_EVENT` queues a `drm_event_vblank` when `FLIPPED` arrives |
| `MODE_DIRTYFB` | `FLUSH` of the clip rectangles |
| `read()` | `drm_event_vblank` records; blocks while none are queued, `EAGAIN` under `O_NONBLOCK` |
| `MODE_GETPLANERESOURCES` | E4: one primary plane a head to an open that set `UNIVERSAL_PLANES`; no plane to one that did not |
| `MODE_GETPLANE` | E4: format `XRGB8888`, `possible_crtcs` the bit of its head's CRTC, and the CRTC and framebuffer `SETCRTC` or `PAGE_FLIP` last showed on that head, 0 and 0 while nothing is; the formats are copied only into an array with room for all of them; another id `ENOENT` |
| `MODE_OBJ_GETPROPERTIES` | E4: the plane has `type` (property 5) at `Primary` (1); the CRTC and the connector have none; the encoder, a framebuffer and the property have no property list, `EINVAL`; an id of another type than the one asked for, or no object, `ENOENT` |
| `MODE_GETPROPERTY` | E4: property 5, `type`, `DRM_MODE_PROP_ENUM \| DRM_MODE_PROP_IMMUTABLE`, values 0, 1 and 2 named `Overlay`, `Primary` and `Cursor`; another id `ENOENT` |

**Iteration 2's planes and properties (E4, 3 points, the GUI session
os-e5; decided by os-f6, 2026-09-16; implemented and reviewed by os-02 the
same day).** This table first assumed Smithay's legacy path needs no planes. It
does: `create_surface` enumerates planes even there, reads each plane's
properties to find `type` and reaches `unreachable!()` for a plane without
one, and keeps only primary planes when universal planes are refused; with
none it fails with `NoPlane`. It also reads the connector's properties to look
for `DPMS`, where an empty list is fine (`docs/INPUT.md` §2.3 has the source
lines). So `card0` has one primary plane with a `type` property, and answers
the four ioctls above. `GETPROPBLOB` and `OBJ_SETPROPERTY` stay out while the
plane has no `IN_FORMATS` or `SIZE_HINTS` and the connector no `DPMS`. The
numbers and `struct drm_mode_get_plane_res`, `drm_mode_get_plane`,
`drm_mode_obj_get_properties`, `drm_mode_get_property` and
`drm_mode_property_enum` come from `probe/drm.c`, extended, at both widths.
The plane type values, which the UAPI headers do not export, are written down
from the kernel's `enum drm_plane_type`, as the connector status values were.

**What Linux does, read before the code** (`drivers/gpu/drm/` at `master` on
2026-09-16 and at `v6.12`, the same in both):

* **The primary plane is hidden without the capability.**
  `drm_mode_getplane_res` (`drm_plane.c`) skips every plane whose type is not
  `DRM_PLANE_TYPE_OVERLAY` unless `file_priv->universal_planes` is set. Smithay
  asks for the capability first (`device/mod.rs`) and keeps only primary
  planes when it is refused, so refusing it would leave Smithay with no plane
  at all. So `card0` accepts it, the product owner's choice.
* **The capability drags in nothing atomic.** `drm_setclientcap`
  (`drm_ioctl.c`) takes `DRM_CLIENT_CAP_UNIVERSAL_PLANES` with a value of 0 or
  1, anything larger `EINVAL`, and sets only `universal_planes`. The implication
  runs the other way: `DRM_CLIENT_CAP_ATOMIC` sets `universal_planes` too, and a
  driver without `DRIVER_ATOMIC` refuses it with `EOPNOTSUPP`, as `card0` does.
  No deviation is needed.
* **A legacy primary plane carries `type` and, unless the driver opts out,
  `IN_FORMATS`.** `__drm_universal_plane_init` (`drm_plane.c`) attaches
  `plane_type_property` to every plane; the `FB_ID`, `CRTC_ID`, `CRTC_*` and
  `SRC_*` properties only under `DRIVER_ATOMIC`; and `IN_FORMATS` whenever the
  plane has format modifiers, which it has unless the driver set
  `mode_config.fb_modifiers_not_supported`. That flag is also what
  `DRM_CAP_ADDFB2_MODIFIERS` reports. `card0` is such a driver: no modifiers,
  no `IN_FORMATS`. Linux would answer that capability 0 where `card0` answers
  `EINVAL`, and Smithay reads `IN_FORMATS` only when the answer is 1, so both
  lead it to the plane's format list from `GETPLANE`.
* **`type` is an immutable enum.** `drm_mode_create_standard_properties`
  (`drm_mode_config.c`) makes it with `drm_property_create_enum` and
  `DRM_MODE_PROP_IMMUTABLE`, which adds `DRM_MODE_PROP_ENUM`, from
  `drm_plane_type_enum_list`: `Overlay` 0, `Primary` 1, `Cursor` 2.
  `drm_property_add_enum` keeps the values in that order in `values`.
* **Counts, then arrays.** `drm_mode_obj_get_properties_ioctl`
  (`drm_mode_object.c`) finds the object with `drm_mode_object_find`, which
  answers nothing for an id of another type unless `DRM_MODE_OBJECT_ANY` was
  asked (`ENOENT`), and refuses an object with no property list (`EINVAL`).
  `drm_mode_object_get_properties` skips `DRM_MODE_PROP_ATOMIC` properties
  for a client without atomic, copies each id and value while the caller's
  count has room, and writes back the full count. `drm_mode_getproperty_ioctl`
  (`drm_property.c`) copies the name and flags, each value while
  `count_values` has room, each enum record while `count_enum_blobs` has room,
  and writes back both counts. `drm_mode_getplane_res` and the formats of
  `drm_mode_getplane` count the same way, except that the formats are copied
  only when all of them fit. drm-rs's `get_plane_resources`, `get_plane`,
  `get_properties` and `get_property` (drm-ffi 0.9.0, which Smithay 0.7.0
  takes) call each twice, counts first.
* **A legacy plane shows what the legacy calls set.** `drm_mode_getplane`
  reads `plane->crtc` and `plane->fb` for a plane without atomic state, which
  `__drm_mode_set_config_internal` (`drm_crtc.c`, under `drm_mode_setcrtc`)
  sets to the CRTC and framebuffer, or to none when the CRTC is turned off.
* **CRTCs and connectors.** `__drm_crtc_init_with_planes` attaches CRTC
  properties only under `DRIVER_ATOMIC`, so a legacy CRTC's list is empty, as
  `card0`'s is. `drm_connector_init_only` (`drm_connector.c`) attaches
  `DPMS`, `link-status`, `non-desktop` and `TILE` to every connector, and
  `EDID` unless it is virtual. `card0`'s connector has none, as decided above:
  Smithay's legacy path sets `DPMS` only on a connector that has it, and
  `EDID` and `TILE` are blobs, which need `GETPROPBLOB`.

**Object ids.** Linux numbers all of a device's mode objects from one idr, so
an id names one object whatever its type, and `DRM_MODE_OBJECT_ANY` lookups
are well defined. `card0` keeps that: each head has a block of four ids of
its own, starting at 1 -- the CRTC, the encoder, the connector and the
primary plane, so head 0 is 1 to 4 and head 1 is 5 to 8 -- the `type`
property is above every head's block, the ids below 128 are kept for fixed
objects, and framebuffers are numbered from 128 up and never reused within an
open. In iteration 1 framebuffers were numbered from 1, the same ids as the
CRTC, encoder and connector, which no call could tell apart until
`OBJ_GETPROPERTIES` took `DRM_MODE_OBJECT_ANY`.

**More than one head (2026-09-17).** A card publishes one connector per
scanout the driver's `HELLO` reported, which is what Linux's own virtio-gpu
driver does: a scanout the host has nothing attached to is a connector
reporting `disconnected`, not a connector that is missing. `SETCRTC` and
`PAGE_FLIP` name a head's CRTC and carry its scanout number to the driver, so
two monitors on one card are two framebuffers flipped apart from one another.

QEMU enables a virtio-gpu's second *output* only when a host window manager
resizes its window, which a headless test has nothing to do, so
`cargo xtask test-compositor --screens 2` gives the guest two virtio-gpu
*devices* instead: two cards, one screen each, which is the other shape a
two-monitor machine comes in and the one a test can drive.

**How it is checked.** `compositor/blank` asks for universal planes after its
modeset, reads the planes as Smithay does, and ends its marker line with
`plane 4 Primary`: the plane whose `type` value is named `Primary` in the
property's own enum list, and which must show the framebuffer and CRTC the
program set. `cargo xtask test-display` requires `plane <id> Primary` at the
end of the marker line on x86-64 and AArch64.

**Legacy is not rework.** Linux keeps `SETCRTC` and `PAGE_FLIP` alongside
atomic, and Smithay's DRM backend falls back to them. Atomic commit is an
additive later landing. Stage 17's text names it and stays as written.

**Kernel plumbing this needs:**

* **devfs subdirectories.** `lookup` refuses anything but the root today, so
  `/dev/dri/` becomes the first subdirectory.
* **An ioctl branch** in `sys_ioctl` for the card node, beside the console
  branch. A general `Inode::ioctl` hook is better, but it's a separate
  refactor that I'd rather not smuggle in.
* **An exclusive open.** A second `open` gets `EBUSY` until the first closes.
  That stands in for DRM master and keeps one opener's buffers from another
  in iteration 1. It does not end a mapping: a program that mapped the card
  and closed it can still read whatever the next opener draws. The node is
  `0660` root's, as Linux's `video` group has it, until per-open windows onto
  the card VMO (stage 19's render node) close that.
* **`poll`/`epoll` readiness** on the node when os-26's epoll lands. Iteration
  1's binary doesn't wait on events.

### 2.4 A panic once the driver owns scanout

The kernel still draws only a panic, and only into the firmware's framebuffer
from `BootInfo`. What changes is what a person sees:

* **QEMU, x86-64.** q35's default VGA stays (no `-vga none`) and virtio-gpu
  is a second display device. The firmware framebuffer is the VGA BAR. A
  panic is drawn there as today, on QEMU's VGA console, while the virtio-gpu
  console keeps its last frame. The panic is **not lost, but it's on the
  other head**. Serial carries it as always, and the screendump test names
  the virtio-gpu device explicitly.
* **QEMU, AArch64.** The same, with `ramfb` as the firmware's head, once the
  loader chooses it (below).
* **The hazard, which the first run found.** If the firmware binds the
  virtio-gpu itself (OVMF and AAVMF have `VirtioGpuDxe`), its framebuffer is
  *guest RAM* in boot-services data, which the frame allocator hands out
  again, not a BAR or a reserved region. AAVMF does exactly that on AArch64
  (§3). A panic would then draw over frames that belong to someone else, and
  once the driver resets the device nobody would see it.
* **The rule (os-f6, 2026-09-16).** The loader looks at every graphics
  output, not only the first, and prefers one whose framebuffer the
  allocator will not own (`ramfb`'s reserved pages, the VGA BAR) over one it
  will (`VirtioGpuDxe`'s), and its boot line says which it took. It writes
  `BootInfo.framebuffer.reclaimable` (BootInfo version 4) from the final
  memory map with `ferrix_bootinfo::allocator_owns`. That one field has two
  readers: the panic screen draws only when it is 0, and the display core
  refuses to publish `card0` over a framebuffer where it is 1. Nothing is
  reserved to protect a picture nobody would see.
* **Real hardware with one display controller** (the DK1's LTDC, P3): the
  firmware's framebuffer is the one that controller scans out, so once a
  driver takes it over **a panic's picture is lost and serial is not**; where
  that framebuffer is reclaimable RAM, the flag is 1 and the panic goes to
  serial only. The alternative, a driver-independent "restore scanout" in the
  kernel, is drawing by another name, and ARCHITECTURE §1 rules it out.

## 3. QEMU and xtask

* **Devices.** `-device virtio-gpu-pci,id=gpu0,disable-legacy=on,iommu_platform=on,xres=1024,yres=768`
  on x86-64 and AArch64, only under `--display` and `test-display`, so no
  existing gate changes. The size differs from the firmware's usual 1280×800,
  so the kernel's `display` boot line says which device the firmware drew
  on. ARMv7-A's machine has no PCI virtio-gpu in this tree's configuration
  and doesn't take part (the roadmap says so already).
* **`test-display`.** Builds `compositor/blank` as init and boots it with the
  device, waits for a line starting `compositor: `, then asks QEMU over QMP
  (TCP on localhost on every host) for `screendump device=gpu0 head=0` in
  QEMU's default PPM, and compares every pixel. A negative control, the
  program built with `negative-control`, fills pixel (0, 0) differently, and
  the check must fail on exactly that pixel. If the program prints
  `compositor: failed`, the test stops at once and reports what QEMU's first
  console showed.
* **What the first run showed (2026-09-16, before L5 and L6).** On both
  architectures the program ran as init and failed as it should, with no
  `/dev/dri/card0`, and the QMP screendump and PPM parse worked. On x86-64 the
  firmware framebuffer is q35's VGA (`display 1280x800`). **On AArch64 it is
  the virtio-gpu** (`display 1024x768`): AAVMF's `VirtioGpuDxe` took the boot
  framebuffer over `ramfb`, which is §2.4's hazard. The fix is decided before
  L5.
* **`run --display` and `run-compositor`** are the two boots a person
  watches, and `xtask/src/window.rs` decides where their screen goes.
  `run-compositor` builds the same image `test-compositor` boots -- the
  compositor as init, its clients, `hyprctl` and a `hyprland.conf` -- and
  gives it a screen, this terminal as its serial port, and the host's own
  accelerator, which is what `run` does too. `--config <PATH>` carries a real
  configuration instead of the small one it writes; zinc is carried at
  `/bin/zinc`, so `SUPER+RETURN` opens a shell on a pseudoterminal.
* **Where the screen goes is asked, not assumed (2026-09-17).**
  `-display default` is no good: QEMU's `default` is whichever local backend
  was compiled in, and a build with none fails at startup rather than falling
  back. So `window.rs` asks QEMU (`-display help`) and the host, in this
  order:
  1. `gtk`, then `sdl`, when QEMU has one *and* this host can open a window:
     always on Windows and macOS, and on a POSIX host when `DISPLAY` or
     `WAYLAND_DISPLAY` is set.
  2. VNC otherwise, at `127.0.0.1:0` -- the loopback, because `-vnc` without
     `password=on` lets any client in -- with the port printed and the `ssh
     -L` line that reaches it from another machine. `--vnc <display>` asks for
     VNC anyway, and is the only way anything here binds a wider address.
  3. Neither, which is an error naming what QEMU did offer and what to
     install.
  The two hosts this is developed on are exactly the two cases: the Windows
  QEMU 11.1 offers `gtk` and `sdl`; the Linux box the gates run on builds QEMU
  9.2.4 headless, offers `none`, `spice-app` and `dbus`, is reached over
  `ssh`, and shows its screen over VNC.
* **A watched boot has two heads, and the card is the second one.** Firmware
  keeps the machine's own display device -- q35's VGA, `virt`'s `ramfb` -- and
  the kernel's driver drives the virtio-gpu, so QEMU has two consoles and
  shows the first. VNC is told which console to serve
  (`display=gpu0,head=0`), so a viewer sees the compositor and nothing else.
  GTK cannot be told which tab to open on, only to show the tab bar
  (`show-tabs=on`), so the run prints which tab the compositor is and that
  `Ctrl-Alt-2` is its shortcut. Taking VGA away instead (`-vga none`) would
  leave the loader's framebuffer on the card the driver later takes over,
  which is §2.4's hazard and not something a convenience should decide.
* **What a watched boot showed (2026-09-17).** On Windows, `cargo xtask
  run-compositor` opens a GTK window and the guest reaches `hyprix: card0
  Virtual-1 640x480`, with `seat 2 devices [event0 QEMU Virtio Keyboard,
  event1 QEMU Virtio Tablet], 15 binds` and both `/bin/pattern` clients
  started: the binds are pressed by pressing them. The card is 640×480 rather
  than the `xres=1024,yres=768` a headless boot reports, because a console
  with a UI attached takes its size from the window; the compositor follows
  whatever the card says, which is why the picture is right either way.
  Input injection over QMP (`input-send-event`) is `test-input` and
  `test-seat`, and needs no window.

## 4. Landings and points

Each is a small landing on main, gated on nazuna. The first four touch no
kernel code.

| # | Landing | Kernel? | Points |
|---|---|---|---|
| L1 | `libs/linux-abi::drm`: ioctl numbers, `drm_mode_*` and `drm_event_vblank` layouts at both widths, from a committed probe | no | 3 |
| L2 | `libs/virtio::gpu`: 2D control commands and responses encoded and decoded, config, features, hostile-device tests, fuzz target; checked against QEMU 9.2.4's `virtio_gpu.h` | no | 5 |
| L3 | `libs/displayctl`: §2.2's messages and validation, host-tested, fuzzed | no | 3 |
| L4 | `libs/virtio-gpu`: driver logic over `libs/virtio-blk`'s traits (bring-up, the command queue, resource lifecycle), tested against a simulated device | no | 5 |
| L5 | `user/gpu` driver, devmgr's table entry, `DISPLAY_CONTROL_CREATE`, the core's control task and the §2.4 check; exit: the boot line names the mode the driver reported | yes | 8 |
| L6 | devfs `/dev/dri/card0`: subdirectory, exclusive open, the ioctl branch and §2.3's subset, `mapping()` to the card VMO, event `read` | yes | 8 |
| L7 | `compositor/` first binary (open, mode, dumb buffer, fill, `SETCRTC`), built into the initramfs; `xtask test-display` with QMP screendump and its negative control; `xtask run --display` | no | 5 |
|  | **Iteration 1** |  | **37** |

That is 3 more than the 34 first given. The difference is devfs
subdirectories, the exclusive open and §2.4's check, none of which were
counted before. After L7, the next iterations each end in a screendump the
customer can look at: two pattern clients tiled (the layout core, the protocol
server), then input.

## 5. Decisions and open questions

**Decided by os-f6, 2026-09-16:**

1. **The card VMO's budget is 256 MiB** for iteration 1, as one named
   constant with its arithmetic (§2.1). Ranges are decommitted and reused
   (added in os-02's review round, which found the budget could be spent for
   good).
2. **The exclusive open stands in for DRM master**, as a written deviation
   from Linux: a second `open` of `card0` answers `EBUSY`, and `SET_MASTER`
   and `DROP_MASTER` answer for the one client. Linux's many opens with one
   master come with the render node in stage 19, and `docs/BACKLOG.md` has a
   row saying so.
3. **Legacy-only DRM** in iteration 1: atomic is refused through
   `SET_CLIENT_CAP` so Smithay falls back, and Linux keeps legacy.
4. **`card0` reports the card VMO's size** as `st_size` (§2.1).
5. **§2.4's rule** — refuse to publish `card0` when the firmware framebuffer
   lies in allocator-owned memory, and say so on the boot line — is right.

**Still open, for the kernel reader of L5 and L6:** whether the ioctl branch
is acceptable or `Inode::ioctl` should come first; and who reserves the
firmware framebuffer's frames, mm or the reader, if §2.4's check ever fires.
