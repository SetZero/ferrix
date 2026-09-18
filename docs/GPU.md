# The GPU: the host's driver first, a card of Ferrix's own later

Decided by the customer on 2026-09-18. `docs/ROADMAP.md` stage 19 left the
GPU path as the customer's choice and said where it would be recorded; this
is the reasoning, the order and the sizes, and `docs/BACKLOG.md` carries the
decision and the rows.

**Path A first**: GPU acceleration for Ferrix *as a guest*, through
virtio-gpu's 3D commands, which puts the host's own GPU driver -- NVIDIA's, on
both machines this is developed on -- behind the guest's rendering without a
line of it being ported. **Path B later**, in a stage of its own
(`docs/ROADMAP.md` stage 21), when the customer wants Ferrix on bare metal
with an NVIDIA card: a driver for the card itself.

The two are not alternatives and A is not a stopgap. Everything A builds
above the driver -- the render node, the GPU renderer in the compositor,
dmabuf -- is what B needs too. B replaces what is *under* it.

---

## 1. Why this is wanted

Every pixel is drawn on the CPU today. 2026-09-17 and -18 went into making
that bearable (`docs/COMPOSITOR-DAMAGE-HANDOFF.md`): a frame that changes
little costs little. But a frame that changes *everything* -- a video
wallpaper behind a translucent terminal, which is the customer's desktop --
is a dual-Kawase blur of two million pixels every frame, 20 ms across eight
cores on the host and far worse in a guest with one processor. Those are
shaders. Hyprland is fast on the same desktop because a GPU runs them.

## 2. What was checked before deciding

* Both development machines have an NVIDIA GeForce RTX 3090.
* The Windows machine's QEMU, 11.1.0, offers `virtio-gpu-gl-pci` and
  `virtio-vga-gl` and the `gtk`, `sdl` and `egl-headless` displays. That is
  the machine `cargo xtask run-compositor` is watched on, so **the host half
  of Path A is already installed where it matters**.
* The Linux gate host's QEMU, 9.2.4, was built headless and offered no
  `-gl` device, though `libvirglrenderer` was installed there. It has been
  rebuilt since, and does now: §3.1.
* Ferrix drives virtio-gpu in 2D only: `libs/virtio::gpu` has the 2D control
  commands, `/dev/dri/card0` has dumb buffers, `SETCRTC`, `PAGE_FLIP` and
  `DIRTYFB`, and there is no render node (`docs/DISPLAY.md`).

---

## 3. Path A: virtio-gpu 3D

A guest never sees the NVIDIA card. It sees virtio-gpu, and with the `-gl`
device QEMU replays the guest's GL command stream into the host's driver
through virglrenderer. Nothing of NVIDIA's is ported; all of it is used.

In dependency order, with sizes in story points:

| # | what | points |
|---|---|---|
| 1 | **virtio-gpu 3D in the ring-3 driver.** `VIRTIO_GPU_F_VIRGL` and `CONTEXT_INIT` negotiated, `GET_CAPSET_INFO`/`GET_CAPSET`, `CTX_CREATE`/`CTX_DESTROY`/`CTX_ATTACH_RESOURCE`, `RESOURCE_CREATE_3D`, `SUBMIT_3D`, `TRANSFER_TO_HOST_3D`/`FROM_HOST_3D`, and fences. `libs/virtio::gpu` already has the 2D commands and their fuzz target, and this extends both. **Mostly done -- §3.2.** | 13 |
| 2 | **A render node and the `virtgpu` ioctls.** `/dev/dri/renderD128`, GEM handles, `DRM_IOCTL_VIRTGPU_GETPARAM`, `GET_CAPS`, `CONTEXT_INIT`, `RESOURCE_CREATE`, `RESOURCE_INFO`, `MAP`, `EXECBUFFER`, `TRANSFER_TO_HOST`/`FROM_HOST` and `WAIT`, in `libs/linux-abi` from a committed probe as every other ABI table is. And **scanout of a 3D resource**, so the finished frame never leaves the GPU: no per-frame transfer at all, where today's best is the damaged rectangles. | 13 |
| 5 | **The host half, in xtask.** `-device virtio-gpu-gl` and a GL display where QEMU has them, asked for as `window.rs` asks for a display today, with the 2D device otherwise. The gate host's QEMU rebuilt with OpenGL and virglrenderer, and `egl-headless` for judged boots. GPU output is not byte-exact across drivers, so a judged GPU boot compares within a stated tolerance, or against a software GL pinned on the gate host; the software renderer's images stay byte-exact. **Judging one needs a way to read its pixels that is not `screendump` -- see §3.1.** | 5 |
| 3b | **A GPU renderer for the compositor, in Rust.** A `compositor/virgl` crate that encodes virgl's command stream -- object creation, state, `draw_vbo`, resource transfers -- with shaders as the TGSI text virgl takes. Hyprland's effects are about eight shaders: the two blur kernels, `blurprepare`, `blurFinish`, the rounded texture, the border gradient, the shadow. `compositor/render` gains a renderer trait with the software renderer as the fallback the roadmap already requires -- the compositor is never GPU-only. Clients stay `wl_shm`; their damaged rectangles are uploaded as textures. | 21 |
| 4 | **`zwp_linux_dmabuf` and a GBM-shaped allocator.** Only once clients render on the GPU themselves. Not needed for 3b, and deferred with 3a. | 8 |

**52 points to a GPU-composited desktop** (1, 2, 5, 3b), in that order: the
driver and the node first because nothing above can be tested without them,
the host plumbing third so that the renderer is gated from its first commit.

### 3.1 What was found when the gate host was given the device (2026-09-18)

The Linux gate host's QEMU was rebuilt as step 5 asks: the 9.2.4 tree at
`~/Documents/qemu/qemu`, reconfigured with `--enable-virglrenderer
--enable-opengl`, which gives it `virtio-gpu-gl-pci`, `virtio-vga-gl`,
`egl-headless` and `gtk`. Two things had to be worked around and one is a
real constraint on the plan.

* **`--disable-werror` is needed on this host.** QEMU 9.2.4 does not
  compile with the distribution's current GCC: `util/log.c` trips
  `-Werror=discarded-qualifiers`. Nothing to do with the GPU, and no reason
  to patch someone else's tree over it.

* **`xtask --gl` boots.** Ferrix comes up on `virtio-gpu-gl-pci` exactly as
  on the 2D device -- the 3D card is a superset, and the 2D driver drives it
  unchanged. That is the whole of the host half working.

* **`FERRIX_QEMU` points at the rebuilt one.** A hand-built QEMU does not
  have to be installed over the machine's, which needs root and replaces
  something a person may be relying on. `FERRIX_QEMU=~/Documents/qemu/qemu/build
  cargo xtask test-display --gl` is the whole invocation on this host.

* **`screendump` cannot read a GL console, and this is not a setting.**
  QEMU 9.2.4's `qmp_screendump` (`ui/ui-qmp-cmds.c`) asks for a
  `DisplaySurface` and gives up with *"no surface"* when there is none;
  there is no GL path in it. With `egl-headless` the console holds a scanout
  texture on the host GPU and no surface, so every judged boot's way of
  reading pixels stops working the moment the card is the 3D one. `cargo
  xtask test-display --gl` shows it: the guest boots, the compositor sets
  its scanout, and the dump fails.

  So step 5 owes a second way to read a frame, and the cheapest one is
  probably from *inside* the guest -- the compositor already knows how to
  write its own frame out (`compositor/shot`), and a picture judged there
  needs no host console at all. That also sidesteps the tolerance question
  for the 2D path, where the guest's bytes are still the renderer's own.
  Whether a newer QEMU grew a GL screendump is worth checking before
  writing anything.

### 3.2 Where step 1 stands (2026-09-18)

**The wire format is complete** and every command is unit-tested against
`virtio_gpu.h` by hand and fuzzed: the capability sets, the four context
commands, `RESOURCE_CREATE_3D`, both 3D transfers and `SUBMIT_3D`, with the
control header's context, fence and ring (`gpu::Context`), which a 2D
driver had been writing as zeros.

**Four of them are proved against real virglrenderer**, not just against
the header. `cargo xtask test-display --gl` prints

    display  card0 is a 3D card: virgl, 2 capability sets,
             the first #1 of 308 bytes

which is feature negotiation granting `VIRTIO_GPU_F_VIRGL`,
`num_capsets` read from the configuration block, `GET_CAPSET_INFO`
answering, and `GET_CAPSET` returning `virgl_caps_v1`'s 308 bytes. Without
`--gl` the same boot says `card0 is a scanout: no 3D`.

Two things a person picking this up should know:

* **QEMU offers two capability sets and index 0 is `VIRGL` (#1), not
  `VIRGL2` (#2).** A renderer that wants VIRGL2 walks the indices; taking
  index 0 gets the older set.
* **The driver's response buffer is a page now** (`RESPONSE_BYTES`), up
  from 512 bytes, because a capability set is longer than anything the 2D
  half ever read. `CAPSET_ROOM` is what that leaves, and the driver
  declines to ask for a set larger than it rather than have the device
  write past the end.

**What is left of step 1** is a driver that actually *uses* the commands:
making a context, creating a 3D resource, submitting a stream. That is
bound up with step 2, because what a context is *for* is the render node,
and neither is testable without the other.

### 3.3 The render node's seam, and what Path B inherits (2026-09-18)

Decided when step 2 was started, because where the seam goes is the whole
of whether Path B reuses this or rewrites it. The customer asked for a
driver adapter that an NVIDIA driver can be loaded behind later; this is
what that means concretely, and what it honestly cannot mean.

**The seam is the control protocol, not the ioctls.** Three layers, and
only the middle one is new work per device:

1. **`/dev/dri/renderD<N>`, the node.** The kernel owns it, as it owns
   `/dev/dri/card<N>`. What lives here is device-*independent*: the inode,
   the GEM handle table, an object's lifetime and its reference counts,
   mapping an object into a process, and the generic ioctls (`VERSION`,
   `GEM_CLOSE`, and the `PRIME` pair when dmabuf lands). Linux keeps the
   same things in `drm_gem.c` for the same reason.

2. **`libs/renderctl`, the control protocol.** The sibling of
   `libs/displayctl`: a fixed-size little-endian message on one `Channel`
   per device, `HELLO`/`READY`/`REFUSED`, a doorbell each way, validation
   in the order fields are read, host-tested and fuzzed. Its messages are
   what *every* GPU can do -- make a context, allocate an object of so many
   bytes, give an object to a context, map it, submit a command buffer,
   move bytes in or out, wait for a fence -- and nothing narrower.

3. **The ring-3 driver**, which maps those onto its device. virtio-gpu maps
   `SUBMIT` onto `SUBMIT_3D`, `CREATE_OBJECT` onto `RESOURCE_CREATE_3D`,
   `WAIT` onto a fenced header. An NVIDIA driver would map the same
   messages onto a channel and a pushbuffer. Drivers stay in ring 3, which
   the decision of 2026-09-13 requires of this one as of any other.

**What is deliberately opaque, and why pretending otherwise would be
worse.** A command buffer's *contents* and an object's format and binding
words are the renderer's own language: virgl's TGSI and `PIPE_*`
enumerations here, NVIDIA's classes and methods there. There is no honest
portable abstraction over them -- Linux does not attempt one either, which
is why `DRM_IOCTL_VIRTGPU_*` and `DRM_IOCTL_NOUVEAU_*` are different
ioctls and why Mesa has a back end per driver. So they pass through as
bytes, and userspace learns which language to speak from the node's driver
*name*, exactly as it does on Linux. An abstraction that claimed to hide
this would be a lie that costs a rewrite the first time it is tested.

**What Path B therefore inherits, unmodified:** the node, the handle table
and object lifetime, the protocol's shape and its refusal rules -- including
"pages the device may still hold are never unpinned" (§2.2 of
`docs/DISPLAY.md`), which is a property of the *core*, not of virtio --
the fence and wait model, devmgr bring-up and the `START` message, the
ring-3 placement, and the gates. What it adds is one more implementation of
`libs/renderctl` and one more driver-specific ioctl range.

**What it does not inherit** is the command encoder: `compositor/virgl`
(step 3b) speaks virgl, and an NVIDIA path needs its own, or Mesa (§3a).
That is the same boundary Linux draws and it is drawn here on purpose.

### 3a, which was not chosen for the compositor

Mesa's virgl driver built on ferrousli would give *every client* OpenGL ES
as well as the compositor. It is a large C and C++ port -- libdrm, a C++
standard library, EGL and GBM -- it loads its drivers with `dlopen`, which
waits on the dynamic linking stage, and it is the C device stack
`compositor/README.md` says the compositor never takes. 40 points or more,
most of it unknown. It stays the way clients get GL, later, beside step 4;
the compositor does not wait for it.

The customer's word was for Path A and its order. Taking 3b rather than 3a
for the compositor's own renderer is the recommendation that word was given
on, recorded here so that it can be overruled by name.

### What Path A does not give

* **Vulkan on the Windows host.** Venus, Vulkan over virtio-gpu, needs a
  Linux host with KVM. Under `whpx` it is virgl, which is OpenGL.
* **More processors under `whpx`.** The one-processor limit is QEMU 11.1's
  own fault (`docs/BACKLOG.md`) and has nothing to do with the GPU. It
  matters less once the GPU draws the pixels.
* **Anything on real hardware.** That is Path B.

---

## 4. Path B: an NVIDIA card under Ferrix itself

For the day Ferrix runs on bare metal with an NVIDIA card in it. Not sized:
well over a hundred points, most of them unknowns, and it accelerates nothing
that runs today -- the one real board in this tree, the DK1, has no NVIDIA
GPU. What it would take:

* **The kernel side.** NVIDIA's open kernel modules (MIT and GPLv2, Turing
  and newer) are an OS-agnostic core over an OS interface layer, which is
  how a FreeBSD driver exists. What it plugs into on this side is
  `libs/renderctl` and the render node above it, which §3.3 built to take a
  second implementation. Reusing them means writing that layer for
  Ferrix: PCI configuration and BARs, MSI-X, DMA mappings under the IOMMU,
  threads, timers, locks, allocation and firmware loading, and then the
  modesetting and DRM halves. On Ferrix that is a very large C program in a
  ring-3 driver process. The device objects stage 10 built -- apertures,
  vectors, IOMMU domains -- are the right shape for it, and the decision of
  2026-09-13 that drivers stay in ring 3 applies to it as to any other.
* **The firmware.** Tens of megabytes of GSP firmware, loaded by the driver,
  which then mostly speaks RPC to it.
* **The userspace.** NVIDIA's GL, Vulkan and CUDA libraries are closed
  shared objects built against glibc. They need a dynamic linker and
  `dlopen` (the dynamic linking stage), glibc's versioned ABI, and the
  `/dev/nvidia*`, `/proc` and `/sys` surface they probe. ferrousli is a
  static, musl-shaped libc that must never copy glibc. This is a project of
  its own and the part most likely to decide the whole path.
* **The open alternative**, to be weighed when the stage opens rather than
  now: Mesa's NVK over the nouveau interface, or upstream Linux's Rust
  driver for the same GPUs, Nova. Nova is the nearest in spirit -- Rust, and
  most of the logic is in the GSP firmware, so the driver is largely RPC --
  but both are written against Linux's DRM internals, and both need Mesa in
  userspace, which is §3's 3a again.

What Path A leaves in place for it: the render node, the renderer trait and
the GPU renderer's shaders, dmabuf, and gates that already know how to judge
a GPU's picture. What B adds is a second thing under the render node.

---

## 5. What does not change

* The software renderer stays, tested byte for byte, as the fallback and as
  the reference a GPU's picture is judged against.
* Drivers stay in ring 3.
* No C in the compositor. Path A's 3b keeps that; 3a and Path B's userspace
  are where it would be argued again, by name, when they come up.
