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
| 2 | **A render node and the `virtgpu` ioctls.** *Begun: `libs/renderctl`, `kernel::render`, the ABI table and the node itself are written, a program opens it, and `RESOURCE_CREATE`/`RESOURCE_INFO` make a real resource through the open's handle table -- §3.4. `MAP`, the calls that need a context, and scanout are what is left.* `/dev/dri/renderD128`, GEM handles, `DRM_IOCTL_VIRTGPU_GETPARAM`, `GET_CAPS`, `CONTEXT_INIT`, `RESOURCE_CREATE`, `RESOURCE_INFO`, `MAP`, `EXECBUFFER`, `TRANSFER_TO_HOST`/`FROM_HOST` and `WAIT`, in `libs/linux-abi` from a committed probe as every other ABI table is. And **scanout of a 3D resource**, so the finished frame never leaves the GPU: no per-frame transfer at all, where today's best is the damaged rectangles. | 13 |
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

**A context and a resource are made on the device** through the seam §3.3
describes:

    render   renderD128 is `virtio_gpu`, version 1, capset 1, objects to 64 MiB
    render   renderD128 made context 1 on the device
    render   renderD128 made object 1 of 4096 bytes in context 1
    render   renderD128 gave object 1 back

`kernel::render` names no virtio type. What turns `MAKE_CTX` into
`CTX_CREATE`, `MAKE_OBJ` into `RESOURCE_CREATE_3D` with
`CTX_ATTACH_RESOURCE` behind it, and `DROP_OBJ` into `RESOURCE_UNREF`, is a
hundred lines in `user/gpu`: the adapter a second GPU replaces.

**READY carries the work VMO**, as `Ready::HANDLE_RIGHTS` always said it
would: a megabyte the core owns, handed over `READ | TRANSFER` with the
core's port beside it. An object's description and a command buffer are
ranges of it, and the core writes neither.

**The description the core sent was empty, and that is the seam working.**
`MAKE_OBJ` said four thousand and ninety-six bytes and nothing about what a
resource is; the driver chose `PIPE_BUFFER`, `VIRGL_FORMAT_R8_UNORM` and
`VIRGL_BIND_VERTEX_BUFFER` itself. Those numbers come from Mesa's
`p_defines.h` and virglrenderer's `virgl_hw.h`, because virgl validates them
and `libs/virtio` defines none of them -- they are the renderer's language,
not virtio's. A core that wrote them would be a core the NVIDIA path could
not reuse.

**What is left of step 1** is `SUBMIT_3D` against the device. It needs a
command buffer, and a command buffer is virgl's own language: the encoder is
step 3b's. A program can open the render node now (§3.4), so what is missing
is no longer somewhere to write one from -- it is the encoder itself, and the
`EXECBUFFER` path under it. The core will not invent a command buffer, for
the reason §3.3 gives.

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

### 3.4 Where step 2 stands (2026-09-18)

**The ABI table is written down**, in `libs/linux-abi/src/virtgpu.rs`, from
`probe/virtgpu.c` as every other table in that crate is: every `virtgpu`
ioctl, parameter, context and execbuffer flag, and twelve structure layouts,
printed from `<drm/virtgpu_drm.h>` at both widths and pinned by tests. It is
probed apart from `drm.c` because each test module requires every line of
the probe file it reads to be claimed by that module, and because a driver's
header is not DRM's. Nothing here has a width: these structures carry their
user pointers as `__u64`, so both probe files are identical and a test says
so, rather than listing exceptions the way `drm::Version` must.

**The renderer outlives its proof.** `kernel::render` used to prove a
context and an object and let its task return; a node's ioctls arrive later,
from whichever process opened it, so a `Renderer` is now published and
served -- replies taken off the channel, judged by the session, left where
the request waiting for one will find them. The proof runs before it is
published, so nothing can open the node while the core is still asking its
own questions.

**That cost the display, and the fix is worth writing down.** A card has two
conversations on one thread and the driver served the render one in a loop
that ended only when the core closed the channel -- which the core used to
do, by returning. With the renderer kept alive the display was never served
again and the compositor timed out. The driver now takes render requests
from its own event loop, between the display's: the device runs one command
at a time, so a render command goes only when the pipeline is idle and
nothing is in flight. Underneath was an older bug: a command waiting on the
device dropped every packet that was not its interrupt, while `wait_async`
delivers one once, so a display message arriving during the render proof was
a wakeup nobody would ever hear again. Such packets are kept and given back.

**The node is in `/dev/dri`**, and a program opens it:

    render: renderD128 virtio_gpu 3d 1 capsets 0x2

It takes any number of opens, unlike `card<N>`, which takes one: a render
node exists so that a client can render without being the display's master,
which is why §3.3 puts the handle table at the open. `DRM_IOCTL_VERSION`
reports the driver's own name, from its HELLO, because that is what
userspace picks a back end by; `VIRTGPU_GETPARAM` is answered from the HELLO
too, so neither call wakes the driver. The two display boots are a pair and
each is the other's control: with `--gl` the node must be there and name its
driver, without it there is none and the program must say so.

**What is left, and what is in the way of each.**

* **The handle table, `RESOURCE_CREATE` and `RESOURCE_INFO`.** *Done.* The
  open holds the table, which is why a render node takes any number of opens:
  two programs' `bo_handle` 1 are different objects. The core holds the
  object-id counter and hands ids out in turn, so a driver's late reply names
  an object that is gone rather than one just made; an id the device would
  not let go of is stepped over for good. A resource's `res_handle` is the
  core's object id, because the driver names the device's resource by it.
  The judged `--gl` boot now makes one and reads it back, which is the first
  thing in this path that costs the device a message rather than being
  answered from the driver's HELLO.

  Two things it does not do yet, neither of them in the way of step 3b:
  **a handle released on purpose**, because `DRM_IOCTL_GEM_CLOSE` is not in
  `libs/linux-abi`, which takes every number from a committed probe -- adding
  it means running `probe/drm.sh` on a Linux host, and until then an open's
  objects go when the open does; and **a resource's shape**, because
  `MAKE_OBJ`'s description carries target, format and bind and nothing else,
  so every resource is a buffer. Carrying width, height and stride is a
  change to the description both sides read, and belongs with the transfers
  that would be the first to need it.
* **`MAP`, and mapping an object into a process.** Blocked on the protocol:
  `MakeObject` carries a size and no backing, and the driver attaches none,
  so an object has no guest pages to map. `flags::MAPPABLE` is defined and
  nothing implements it. Deciding where an object's backing comes from is
  the next seam question, not an implementation detail.
* **`GET_CAPS`.** The core knows a capability set's *number*, not its bytes;
  the driver read the bytes. Either the core asks for them or the node
  passes the question down.
* **`EXECBUFFER`, the transfers and `WAIT`.** They need step 3b's encoder to
  have something to say, and `SUBMIT`/`WAIT` are already in the protocol.
* **Scanout of a 3D resource**, which is the other half of this row.

### 3.5 The build plan for steps 2 and 3b, researched (2026-09-19)

The customer chose Path A first, then the AV1 wallpaper (§3.6). What the
software renderer can give was reached on 2026-09-19: a video frame in the
guest is 46 ms, of which 37 is the renderer's own arithmetic and the rest a
serial copy (`docs/COMPOSITOR-DAMAGE-HANDOFF.md` §2.7). 60 fps (16.7 ms) is
below that floor; only the GPU drawing the pixels reaches it. This is the
path, in the order each piece can be *tested*, with the wire details already
looked up so the next session does not repeat the reading. Reference sources
are cloned at `~/.local/share/ferrix/virgl-ref`: virglrenderer 1.2.0
(`src/virgl_protocol.h`, `src/virgl_hw.h`) and Mesa's virgl driver
(`src/gallium/drivers/virgl/virgl_encode.c`, the encoder to copy). QEMU's
`hw/display/virtio-gpu-virgl.c` at `~/Documents/qemu/qemu` shows the host
side of each command.

**rav1d builds for the target already** (proven 2026-09-19): a static musl
binary against the repo toolchain, C-shaped API at `rav1d::src::lib::dav1d_*`,
`default-features=false, features=["bitdepth_8"]`, BSD-2-Clause (already on
deny.toml's allow-list). That is what §3.6 stands on and is why AV1 waits
without risk.

The pieces, each landable on its own:

1. **`CONTEXT_INIT` and `GET_CAPS` through the node** (step 2 tail, ~3 pts).
   `context_init` in `user/gpu` maps onto `MAKE_CTX` with a capset; the node
   answers `VIRTGPU_GETPARAM` from the HELLO already. `GET_CAPS` needs the
   core to carry the capset *bytes*: add a `Capset` message to
   `libs/renderctl` (core asks, driver runs `GetCapset`, bytes ride a work
   VMO range as a description does). Testable by the `--gl` display gate
   reading a capset back, as it reads a resource now.

2. **`MAP`, and an object's backing** (step 2, ~5 pts). This is the seam
   question §3.4 flags: `MakeObject` carries a size and no backing. Decision
   to make: the object's guest pages come from the *work VMO's* allocator
   extended, or a VMO per object. Recommend a VMO per mappable object owned
   by the core (like the card's one big VMO but per resource), attached to
   the device with `ResourceAttachBacking` (the driver already pins for the
   display's `attach`). The node's `MAP` returns an mmap offset into that
   VMO exactly as `map_dumb` does (`kernel/src/display/drm.rs:789`, and
   `devfs::mapping` at `kernel/src/fs/devfs.rs:938` is the hook — the render
   inode needs its own `mapping()` returning the object's VMO). Testable:
   a program creates a resource, maps it, writes a byte, reads it back.

3. **`compositor/virgl`, the command encoder** (step 3b core, ~8 pts). A new
   crate, pure Rust, encoding virgl's command stream into a byte buffer the
   compositor hands to `EXECBUFFER`. The header is
   `VIRGL_CMD0(cmd,obj,len) = cmd | obj<<8 | len<<16`, len in dwords
   (`virgl_protocol.h:141`). The commands the compositor needs, with their
   dword layouts already in `virgl_protocol.h` and Mesa's writer in
   `virgl_encode.c`:
   * `CREATE_OBJECT`/`BIND_OBJECT`/`DESTROY_OBJECT` for blend, rasterizer,
     DSA, vertex-elements, sampler-state, sampler-view, surface, shader
     (object types in `enum virgl_object_type`).
   * `SET_FRAMEBUFFER_STATE`, `SET_VIEWPORT_STATE`, `SET_VERTEX_BUFFERS`,
     `SET_SAMPLER_VIEWS`, `BIND_SAMPLER_STATES`, `BIND_SHADER`,
     `SET_CONSTANT_BUFFER`, `CLEAR`, `DRAW_VBO` (layouts at the matching
     `VIRGL_*` defines; Mesa's `virgl_encoder_draw_vbo` etc. are the model).
   * `TRANSFER3D`/`RESOURCE_INLINE_WRITE` to upload a client's `wl_shm`
     damaged rectangles as textures.
   Shaders are TGSI *text* (Mesa's `virgl_encode_shader_state` dumps
   `tgsi_dump_str`), uploaded in a `CREATE_OBJECT VIRGL_OBJECT_SHADER`.
   Hyprland's effects are about eight shaders: the two blur kernels,
   `blurprepare`, `blurFinish`, the rounded-texture sampler, the border
   gradient, the shadow. Write them as TGSI by hand or translate the GLSL.
   Everything here is host-testable without a device by asserting the byte
   stream, the way the software renderer's images are golden.

4. **`EXECBUFFER`, transfers and `WAIT` through the node** (step 2 + 3b glue,
   ~4 pts). The node's `EXECBUFFER` copies the command bytes into a work VMO
   range and sends `SUBMIT`; `SUBMIT`/`WAIT` are already in `libs/renderctl`
   and the session. The driver's `run_command`/`serve_render` loop already
   runs one device command at a time between the display's — `Submit3d`
   carries the bytes (`libs/virtio::gpu::Command::Submit3d`). `WAIT` maps
   onto a fenced header (FLAG_FENCE). Fences are encoded but the driver does
   not offer the `FENCES` feature yet; offer it and wire `on_interrupt`'s
   fence to `WAITED`.

5. **A renderer trait in `compositor/render`** (step 3b integration, ~5 pts).
   `render_onto` and the `Canvas` operations become a trait with two impls:
   the software one that exists, and a `virgl` one that emits commands. The
   fallback is mandatory (roadmap) — the compositor is never GPU-only, and
   the software images stay the byte-exact reference. `hyprix` opens
   `/dev/dri/renderD128` when it is there and falls back when it is not.

6. **Scanout of a 3D resource** (step 2, ~4 pts). `SET_SCANOUT` on the 3D
   resource the compositor drew into, so the finished frame never leaves the
   GPU — no per-frame `TRANSFER_TO_HOST` at all, which is the 5.5 ms the
   software path spends in `DIRTYFB`. QEMU's `virgl_cmd_set_scanout`
   (`dpy_gl_scanout_texture`) is the host side; the display core's `scanout`
   path takes a buffer id, and a 3D resource id has to reach it. Needs the
   card and render conversations, today separate, to name one resource.

7. **The host half** (step 5, ~5 pts, mostly done). `--gl` boots; what is
   left is judging a GPU frame — `screendump` cannot read a GL console
   (§3.1), so judge from inside the guest with `compositor/shot`, or check
   whether a newer QEMU grew a GL screendump.

Order 1→2→3→4→6→5, with 5's renderer trait landing beside 3. Each of 1, 2,
4, 6 is a display-gate boot that proves the new call; 3 and 5 are
host-tested byte-for-byte. The whole is ~30 points and cannot be verified
piecemeal below the level of these seven — a virgl command means nothing
until `EXECBUFFER` carries it and a scanout shows it, so land 1–2 first to
have somewhere to submit from.

### 3.6 The AV1 wallpaper, after the GPU (2026-09-19)

The customer asked (2026-09-19) that the wallpaper load a real container
rather than the run-length `.fxvid` frames, which for a long clip run to
gigabytes. The path chosen is **AV1 decoded by rav1d in the guest**:

* **Host side** (`xtask/src/wallpaper.rs`): transcode the source to a small
  AV1 elementary stream (or keep an `.mp4`/`.webm`'s AV1 track), rather than
  decoding to `.fxvid` frames. `ffmpeg` on this host has `libaom-av1`,
  `librav1e` and `libsvtav1`. Keep the container/stream on the image; it is
  megabytes, not the initramfs-busting hundreds of `.fxvid`.
* **Guest side** (`compositor/pattern`): add rav1d as a dependency, demux
  the stream (a small IVF or a minimal mp4/Matroska demuxer for the AV1
  track), feed OBUs to `dav1d_send_data`, pull frames with
  `dav1d_get_picture`, convert YUV→RGB into the buffer the client already
  scales and damages. `Movie` becomes a decoder rather than a run-length
  reader; `Movie::damage` can stay whole-frame or diff decoded frames.
* rav1d is BSD-2-Clause and builds static-musl for the target already
  (proven). It is a large dependency; deny.toml allows its licence. The
  decode cost is real CPU, but a wallpaper under an opaque window pauses on
  frame callbacks as it does now, and the GPU by then draws the compositing.

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
