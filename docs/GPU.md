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
* The Linux gate host's QEMU, 9.2.4, was built headless and offers no `-gl`
  device, though `libvirglrenderer` is installed there. Gating Path A on it
  needs that QEMU rebuilt (§3, step 5).
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
| 1 | **virtio-gpu 3D in the ring-3 driver.** `VIRTIO_GPU_F_VIRGL` and `CONTEXT_INIT` negotiated, `GET_CAPSET_INFO`/`GET_CAPSET`, `CTX_CREATE`/`CTX_DESTROY`/`CTX_ATTACH_RESOURCE`, `RESOURCE_CREATE_3D`, `SUBMIT_3D`, `TRANSFER_TO_HOST_3D`/`FROM_HOST_3D`, and fences. `libs/virtio::gpu` already has the 2D commands and their fuzz target, and this extends both. | 13 |
| 2 | **A render node and the `virtgpu` ioctls.** `/dev/dri/renderD128`, GEM handles, `DRM_IOCTL_VIRTGPU_GETPARAM`, `GET_CAPS`, `CONTEXT_INIT`, `RESOURCE_CREATE`, `RESOURCE_INFO`, `MAP`, `EXECBUFFER`, `TRANSFER_TO_HOST`/`FROM_HOST` and `WAIT`, in `libs/linux-abi` from a committed probe as every other ABI table is. And **scanout of a 3D resource**, so the finished frame never leaves the GPU: no per-frame transfer at all, where today's best is the damaged rectangles. | 13 |
| 5 | **The host half, in xtask.** `-device virtio-gpu-gl` and a GL display where QEMU has them, asked for as `window.rs` asks for a display today, with the 2D device otherwise. The gate host's QEMU rebuilt with OpenGL and virglrenderer, and `egl-headless` for judged boots. GPU output is not byte-exact across drivers, so a judged GPU boot compares within a stated tolerance, or against a software GL pinned on the gate host; the software renderer's images stay byte-exact. | 5 |
| 3b | **A GPU renderer for the compositor, in Rust.** A `compositor/virgl` crate that encodes virgl's command stream -- object creation, state, `draw_vbo`, resource transfers -- with shaders as the TGSI text virgl takes. Hyprland's effects are about eight shaders: the two blur kernels, `blurprepare`, `blurFinish`, the rounded texture, the border gradient, the shadow. `compositor/render` gains a renderer trait with the software renderer as the fallback the roadmap already requires -- the compositor is never GPU-only. Clients stay `wl_shm`; their damaged rectangles are uploaded as textures. | 21 |
| 4 | **`zwp_linux_dmabuf` and a GBM-shaped allocator.** Only once clients render on the GPU themselves. Not needed for 3b, and deferred with 3a. | 8 |

**52 points to a GPU-composited desktop** (1, 2, 5, 3b), in that order: the
driver and the node first because nothing above can be tested without them,
the host plumbing third so that the renderer is gated from its first commit.

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
  how a FreeBSD driver exists. Reusing them means writing that layer for
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
