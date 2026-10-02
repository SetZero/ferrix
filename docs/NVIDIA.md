# NVIDIA's own driver on Ferrix

Version 1, a draft written on 2026-10-02 for the customer, who decides §9.
The customer chose the path that day (`docs/BACKLOG.md` Decisions): a real
NVIDIA driver, not nouveau and not NVK. NVIDIA's
[open-gpu-kernel-modules](https://github.com/NVIDIA/open-gpu-kernel-modules)
is ported so that its OS-agnostic core runs as a Ferrix driver. NVIDIA's own
userspace (`libnvidia-*`, the Vulkan and GL ICD, and later CUDA) then runs
unmodified under the Linux personality. It talks to `/dev/nvidiactl`,
`/dev/nvidia0`, `/dev/nvidia-modeset` and `/dev/nvidia-uvm` as it does on
Linux. This document is the feasibility pass (§2) and the design that comes
out of it. Nothing in it is built yet beyond the probe in §2.3.

It replaces the sketch in `docs/GPU.md` §4 ("Path B") and gives
`docs/roadmap/stage-21-bare-metal-gpu-ferrix.md` its first sizing. One of that
sketch's premises no longer holds: it called glibc-built closed libraries
"the part most likely to decide the whole path". Since then Chrome and
yserver run from data volumes through Debian's `ld-linux`, and NVIDIA's
libraries load the same way.

## 1. What this is, and what it is not

This is:

* **the driver**: NVIDIA's resource manager (`src/nvidia`, "RM") and its
  display half (`src/nvidia-modeset`, "NVKMS"), built from NVIDIA's own
  makefiles. They run in a ring-3 process, `nvrm`, under an OS layer written
  for Ferrix (§4.1–4.3);
* **the device files**: a kernel core that serves NVIDIA's character
  devices, `/proc/driver/nvidia` and the rest of what the userspace probes,
  by forwarding each request to `nvrm` (§4.4);
* **how frames reach hyprix, yserver, Chrome and Steam** (§4.6);
* **the milestones N0–N6 with points** (§7).

It is not nouveau, NVK or Nova. Those were weighed in `docs/GPU.md` §4, and
the customer has now decided against them.

It is not a GPU of Ferrix's own design. Everything above the OS layer is
NVIDIA's code, at a pinned release, unmodified.

It is not something every Ferrix machine gets. It needs an NVIDIA GPU that
Ferrix owns, either on bare metal or passed through by KVM. The WHPX world
on the customer's Windows machine has no passthrough, so this path does not
exist there (§8, R6).

**Exit of this design (N1):** on nazuna, in the libvirt domain `ferrix-3060`,
NVIDIA's GSP firmware boots on the RTX 3060. NVIDIA's unmodified
`nvidia-smi`, run from a data volume, lists `NVIDIA GeForce RTX 3060` with
its memory and its PCI address. N2–N4 then put pixels and programs on it.

## 2. What the feasibility pass found (2026-10-02)

### 2.1 The release, and what is already on nazuna

* **Release**: open-gpu-kernel-modules **580.173.02**. Its tag exists
  upstream, and it is exactly the version of the NVIDIA userspace installed
  on nazuna (Ubuntu's `nvidia-driver-580` 580.173.02).
* **Firmware**: nazuna also has the matching GSP firmware in
  `/lib/firmware/nvidia/580.173.02/`. `gsp_ga10x.bin`, for Ampere, is
  75,012,080 bytes; `gsp_tu10x.bin`, for Turing, is 30,471,256 bytes.
* **Newer releases**: the newest upstream tag is 615.71.09. Pinning to the
  host's version means the userspace and firmware can be read locally
  without downloading anything.
* **Download size**: the matching `.run` for a fetch script is 398 MB
  (`NVIDIA-Linux-x86_64-580.173.02.run`). The `-no-compat32` one, without
  the 32-bit libraries, is 326 MB. Neither was downloaded: the analysis
  used the installed copies.
* **The tree**: a shallow clone is 152 MB, in
  `~/.local/share/ferrix/nvidia-ref/ogkm-580.173.02` (not committed).

### 2.2 The core builds as freestanding objects

`make` in `src/nvidia` and in `src/nvidia-modeset`, with NVIDIA's own
makefiles and the host's gcc 15, built both objects on the first try:

| Object | Text | Defined globals | Undefined |
|---|---|---|---|
| `nv-kernel.o` (RM) | 12.2 MB | 13,372 | 406 |
| `nv-modeset-kernel.o` (NVKMS) | 1.5 MB | 2,694 | 69 |

What RM needs from the OS (the full lists are in
`~/.local/share/ferrix/nvidia-ref/*.undef`):

* **150 `os_*` functions.** By area:
  * memory: `os_alloc_mem`, `os_alloc_pages_node`, `os_get_page`;
  * locks: mutex, rwlock, semaphore, spinlock;
  * waits: wait queues, `os_wait_*`, `os_wake_up`;
  * PCI: config access through `os_pci_read_*` and `os_pci_write_*`,
    port I/O through `os_io_*`;
  * time: `os_get_monotonic_time_ns`, `os_delay_us`;
  * work queues: `os_queue_work_item`, `os_flush_work_queue`;
  * user memory: `os_memcpy_from_user` and `os_memcpy_to_user`;
    `os_lock_user_pages` to pin;
  * mappings: `os_map_kernel_space`;
  * the registry, files, process identity (`os_get_current_process`,
    `os_get_euid`, `os_is_administrator`), random bytes, and SMBIOS/ACPI
    tables.
* **117 `nv_*` functions.** By area:
  * page allocation: `nv_alloc_pages`, `nv_free_pages`;
  * DMA mapping: `nv_dma_map_alloc`, `nv_dma_map_mmio`, `nv_dma_map_peer`;
  * mappings: `nv_alloc_user_mapping`, `nv_alloc_kernel_mapping`,
    `nv_add_mapping_context_to_file`;
  * events: `nv_post_event`, `nv_get_event`;
  * firmware: `nv_get_firmware`;
  * timers: `nv_create_nano_timer`, `nv_start_rc_timer`;
  * ACPI: `nv_acpi_*`;
  * I²C;
  * Tegra and SoC functions: 26 of the 117, which a discrete GPU never
    calls.
* **71 `libspdm_*` functions**: crypto for Confidential Computing, which
  stubs out on a GeForce.
* **53 `nvswitch_*` and `nvlink_*` functions**: also stubbed.
* **13 retpoline thunks.**

NVKMS needs 56 `nvkms_*` functions: allocation, timers, semaphores,
`nvkms_call_rm`, `nvkms_copyin` and `nvkms_copyout`.

In the other direction, Linux's glue calls 161 distinct `rm_*` entry points
into the core.

**Both objects link into an ordinary user program.** The test was a static
non-PIE `ET_EXEC` linked at the usual 4 MiB address, with every import
stubbed. It linked into a 15 MB program, with one clash to rename
(`nvstatusToString`, defined in both objects). The objects are built
`-mcmodel=kernel -fno-pic`, and their 58,445 `R_X86_64_32S` relocations
resolve in the low 2 GiB as well as the top. So the core needs no rebuild
to leave the kernel. It does need the kernel's codegen restrictions on its
own floating-point state: `-mno-sse` and `-mgeneral-regs-only`, which are
harmless in a user process.

**The Linux glue, by size:**

| Part | Lines | What it is |
|---|---|---|
| `kernel-open/nvidia`, all | 47,812 | Linux glue for RM |
| … of which a discrete GPU uses | ≈ 24,900 | `nv.c` 6.3k, `os-interface.c` 2.7k, `nv-pci.c` 2.2k, `nv-dmabuf.c` 1.9k, `nv-procfs.c` 1.5k, `nv-acpi.c` 1.6k, `nv-mmap.c` 1.0k, `nv-dma.c` 1.0k, … |
| `kernel-open/nvidia-modeset` | 3,143 | Linux glue for NVKMS |
| `kernel-open/nvidia-drm` | 15,321 | a Linux DRM driver over NVKMS; no OS-agnostic core |
| `kernel-open/nvidia-uvm` | 146,210 | the unified memory driver, written against Linux's mm; no OS-agnostic core |
| `src/nvidia` + `src/common` + `src/nvidia-modeset` | ≈ 1,980,000 | the OS-agnostic cores, MIT |

The core is portable as NVIDIA says. The two pieces that are not are UVM,
which CUDA needs (§7 N5), and nvidia-drm, which NVIDIA's Wayland WSI needs
(§4.6).

**What the userspace needs besides ioctls.** This was read from the strings
of the 580.173.02 libraries on nazuna (`libnvidia-glcore`, `libnvidia-eglcore`,
`libnvidia-glsi`, `libcuda`, `libnvidia-ml`):

* **Character devices, numbered as Linux numbers them**:
  * `/dev/nvidiactl` is 195:255, `/dev/nvidia0` is 195:0, and
    `/dev/nvidia-modeset` is 195:254;
  * `/dev/nvidia-uvm`, `/dev/nvidia-uvm-tools` and `/dev/nvidia-caps/*`
    have dynamic majors, which the libraries look up by name in
    `/proc/devices`;
  * `/dev/char/<maj>:<min>` links.
  If a node is missing, the libraries run `/usr/bin/nvidia-modprobe` to
  make it, so Ferrix should simply have the nodes.
* **`/proc/driver/nvidia/`**: `params` (DeviceFileUID/GID/Mode and
  ModifyDeviceFiles are read before any open), `gpus/<bdf>/…` (`information`,
  `numa_status`), `capabilities/…` (MIG only), and `version`.
* **`/sys`**: `/sys/bus/pci/devices` (which Ferrix has) and
  `…/<bdf>/rescan`. Also `/sys/devices/system/memory/…` (NUMA onlining, for
  coherent platforms only), `/sys/module/<name>/initstate`, and CPU
  topology under `/sys/devices/system/cpu`.
* **`/proc/self/maps`, `/proc/<pid>/exe`, `/proc/modules`,
  `/proc/sys/kernel/modprobe`**.
* **The ioctl ABI.** There are three families:
  * RM's: magic `'F'`, base 200. `NV_ESC_CARD_INFO`,
    `NV_ESC_REGISTER_FD`, `NV_ESC_ALLOC_OS_EVENT` … `NV_ESC_WAIT_OPEN_COMPLETE`;
    plus the RM API escapes 0x27–0x5F (`NV_ESC_RM_ALLOC`, `…_CONTROL`,
    `…_MAP_MEMORY`, …), all `_IOWR('F', nr, size)`;
  * NVKMS's single `_IOWR('m', 0, struct NvKmsIoctlParams)`;
  * UVM's **raw numbers** (`UVM_INITIALIZE` is `0x30000001`, and
    `UVM_IOCTL_BASE(i)` is `i`), which encode no size at all.
  
  The arguments carry **user pointers that RM follows itself**. For
  example, `NV_ESC_RM_CONTROL`'s `params` points to a command-specific
  struct that may point further, and RM copies through
  `portMemExCopyFromUser` → `os_memcpy_from_user`
  (`src/nvidia/src/kernel/rmapi/param_copy.c`). No kernel table can know
  those layouts.
* **mmap.** An `NV_ESC_RM_MAP_MEMORY` records a mapping context on the
  file. A later `mmap(fd, offset)` on `/dev/nvidia0` or `/dev/nvidiactl`
  then maps one of three things, cached as RM says (write-combined for BAR1
  and most system memory):
  * BAR0 registers (USERD and doorbells);
  * BAR1 video memory;
  * system memory that RM allocated or pinned.
  
  RM can later revoke the mappings (`nv_revoke_gpu_mappings`).
* **fd identity.** Several calls hand RM a file descriptor number and
  expect it to resolve to an NVIDIA file of the same process:
  `NV_ESC_REGISTER_FD`, `NV_ESC_RM_EXPORT_OBJECT_TO_FD` and
  `…_IMPORT_OBJECT_FROM_FD`, `nvkms_fd_is_nvidia_chardev`.
* **Presentation.** On Wayland, NVIDIA's Vulkan WSI and EGL
  (`libnvidia-egl-wayland`) use the DRM render node of nvidia-drm.
  `libnvidia-glcore` references `renderD`, `drmPrimeHandleToFD`,
  `drmSyncobj*`, `zwp_linux_dmabuf_v1` and `wp_linux_drm_syncobj_*`. On
  X11, `libGLX_nvidia` wants either NVIDIA's own `NV-GLX` server extension
  or DRI3. Its Vulkan WSI says
  "Failed to create DRI3 Pixmap, using fallback presentation path", so a
  copy path exists for X11 Vulkan.

### 2.3 The bring-up probe on the RTX 3060

**Setup.**

* **Domain**: `ferrix-3060` in `qemu:///system`. Its XML is
  `~/ferrix-nvidia-vm/ferrix-3060.xml`, and Appendix A has a copy.
* **Image**: the `nvidia` branch's x86-64 image, copied to
  `~/ferrix-nvidia-vm/ferrix.img`.
* **Machine**:
  * q35 with OVMF (`OVMF_CODE_4M.fd`, no Secure Boot) and KVM;
  * 2 vCPUs (`host-passthrough`, `maxphysaddr` passthrough) and 4 GiB;
  * a VT-d unit (`intremap='off' caching_mode='on' aw_bits='48'`), with
    `caching_mode` because VFIO requires it behind a vIOMMU;
  * a virtio-rng that goes through the IOMMU (`<driver iommu='on'/>`);
  * no video;
  * serial over TCP to a capture script.
* **The GPU**: the 3060's function 0, `0000:01:00.0`, as an unmanaged
  hostdev (`managed='no'`), so libvirt never rebinds a host driver.

Before every start, the customer's domains that share the card (GameLab,
win11, manjaro, manjaro-kde-test, manjaro-xfce-test, manjaro-sway-test) were
checked to be shut off. They were each time. The domain was shut off after
each boot. Six boots were made; the logs are in `~/ferrix-nvidia-vm/serial*.log`.

**What Ferrix saw:**

```
  nvidia   0000:00:05.0: 10de:2504 rev a1 class 030000, memory decoding on
  nvidia   0000:00:05.0: BAR0 memory 32-bit 16384 KiB at 0x80000000
  nvidia   0000:00:05.0: BAR1 memory 64-bit prefetchable 16777216 KiB at 0x381000000000
  nvidia   0000:00:05.0: BAR3 memory 64-bit prefetchable 32768 KiB at 0x381400000000
  nvidia   0000:00:05.0: BAR5 I/O 128 bytes at port 0x6000
  nvidia   0000:00:05.0: capabilities 01@0x60 05@0x68 09@0xb4; extended 0002@0x100 0018@0x250 0004@0x128 0001@0x420 000b@0x600 0015@0xbb0
  nvidia   0000:00:05.0: BAR0 read: NV_PMC_BOOT_0 0xb76000a1, NV_PMC_BOOT_42 0x176a1000
  nvidia   0000:00:05.0: resizable BAR1: now 16384 MiB, sizes 0x40000 (bit n+4 = 2^n MiB), more 0x0
  iommu    pci 0000:00:05.0 behind the VtD unit at 0xfed90000 as stream 0x28
FERRIX-BOOT-OK stages 1-12
```

What these lines show:

* **Enumeration finds the card.** `10de:2504` (GA106, GeForce RTX 3060)
  is found, and every BAR is sized correctly:
  * BAR0 is 16 MiB of registers;
  * BAR1 is **16 GiB, 64-bit, prefetchable**, which is the whole 12 GB of
    video memory, since Resizable BAR is on in the host's firmware;
  * BAR3 is 32 MiB;
  * BAR5 is I/O.
  
  OVMF placed the 64-bit BARs at about 56 TiB. Ferrix's 64-bit sizing
  (`src/lib/platform/pci/src/bar.rs`) needed no change.
* **The CPU reaches the chip's registers.** `NV_PMC_BOOT_42` `0x176a1000`
  is architecture 0x17 (Ampere), implementation 6 (GA106), revision A1.
* **Resizable BAR** shows one size per BAR. QEMU passes only the current
  size, so the guest cannot change BAR1's size, and does not need to.
* **There is no MSI-X capability, only MSI** (`05@0x68`). The same holds
  behind a root port, where the PCIe capability (`10@0x78`) appears too.
  Ferrix delivers only MSI-X (`src/kernel/src/discovery/pci.rs`,
  `src/kernel/src/arch/x86_64/msi.rs`). So **today the 3060 gets 0
  interrupt vectors**, and the boot's `devices` line says
  `0 vectors`. This is prerequisite N0b.
* **The IOMMU path works when the card is on the root bus.** Placed at
  `00:05.0`, the card gets a translated VT-d domain (stream 0x28). The
  same unit's out-of-domain probe on the virtio-rng faulted as it must,
  and the boot ended `FERRIX-BOOT-OK stages 1-12`. The card made no DMA,
  because there was no driver for it yet. The first real test of its DMA
  is GSP boot itself, which fetches its firmware from system memory (N1d).
* **Behind a PCIe root port, where libvirt puts a hostdev by default**, the
  card is one of 5 `unresolved` functions. QEMU's DMAR describes the root
  ports as *sub-hierarchy* scopes. `place_dmar` (`src/kernel/src/iommu.rs`)
  does not follow those scopes, so it reports them unresolved rather than
  guessing. The card then gets no domain at all. This is prerequisite N0a.
  NVIDIA's driver prefers a slot behind a root port, because it reads the
  PCIe capability for link state, and QEMU hides that capability on the root
  bus.

**What failed on the way**, all in the domain rather than in Ferrix, and
written down so that the `run-nvidia` command in N0e avoids each one:

1. **Both functions of the card in one domain behind a vIOMMU** were
   refused: `vfio 0000:01:00.1: group 14 used in multiple address spaces`.
   QEMU's VT-d gives each function its own address space unless both sit
   behind one conventional PCI bridge. The HDMI audio function is not
   needed, so only function 0 is passed. VFIO accepts this, because all of
   group 14 stays bound to `vfio-pci` on the host.
2. **libvirt's virtio devices bypass the vIOMMU** unless they are given
   `<driver iommu='on'/>`, which xtask's `iommu_platform=on` gives them.
   Without it, Ferrix's stage 10 out-of-domain probe sees an unmapped write
   complete with no fault, and panics (FX-1001). That is correct: in that
   configuration it is unsafe.
3. **libvirt's QEMU cannot traverse `~/.local/share`** (mode 0700), and
   virtlogd's serial log files are root's. So the image lives in
   `~/ferrix-nvidia-vm/` (0755), and the serial port is a TCP socket on
   127.0.0.1:47060 read by `capture.py`.
4. **libvirt picks `OVMF.amdsev.fd`** when `firmware='efi'` is left to it.
   The loader is named explicitly.

## 3. The build and the sources

Nothing NVIDIA ships is committed here: not the kernel modules, not the
firmware, and not the userspace. Instead, `tools/common/fetch/fetch-nvidia.sh`
does the following, as `fetch-steam.sh` and `fetch-yserver.sh` do:

* **The sources.** It fetches open-gpu-kernel-modules at a pinned tag
  (580.173.02), checks a pinned tarball sha256, and unpacks it under
  `~/.local/share/ferrix/nvidia/`.
* **The build.** It builds `nv-kernel.o` and `nv-modeset-kernel.o` with
  NVIDIA's own makefiles, unmodified. Ferrix needs no patch to the core;
  if one is ever needed, it is carried as a numbered patch in
  `tools/common/fetch/nvidia/` and justified there.
* **The userspace.** It downloads the matching `.run` (pinned sha256),
  extracts it with `--extract-only`, and builds a data volume of the
  libraries, the ICD and EGL manifests, `nvidia-smi` and the firmware. The
  volume is built like Chrome's and yserver's. The firmware goes at
  `lib/firmware/nvidia/580.173.02/gsp_ga10x.bin`, where `nvrm` reads it.

The license allows vendoring: the core is MIT file by file (2,751 `SPDX: MIT`
headers, and nothing else in `src/`). The GPLv2 half of NVIDIA's dual
license applies only "when linked together to form a Linux kernel module",
which Ferrix never does. Vendoring is still not recommended, because it
would put about 2 million lines and 150 MB into this repository for code that
is never edited (decision D2). The firmware and the userspace are under
NVIDIA's proprietary licenses, which allow redistributing them unmodified
with a driver. They are fetched by each user and never committed.

Ferrix's own code is the OS layer and the shim around NVIDIA's objects. It
lives in `src/user/system/linux/drivers/nvrm/`, and `xtask` links it with
the fetched objects into `/lib/drivers/nvrm`. A machine with no fetch has
no `nvrm`, and devmgr leaves an NVIDIA function undriven, as it does today.

## 4. The design

### 4.1 Where the core runs: `nvrm`, a ring-3 process that hosts it

The decision of 2026-09-13 keeps drivers out of the kernel, and it applies
here too. Because the core links as an ordinary user program (§2.2),
running it in ring 3 costs nothing at link time.

**Which runtime.** Ferrix's native programs (`ferrix-rt`) have no threads
and no clock (`docs/DEVMGR.md` §3). RM needs both:

* a thread that takes the interrupt and runs `rm_isr`;
* the bottom half `rm_isr_bh` and the work items `os_queue_work_item`
  queues;
* the 1 Hz RC timer and the nanosecond timers;
* a thread per blocked client ioctl, since RM sleeps in `os_wait_*`.

A process may make both Linux and native calls (`docs/ARCHITECTURE.md` §2;
`syscall/mod.rs` dispatches by number from the same entry). So **`nvrm` is
a static Linux-personality program built against ferrousli** and written in
Rust and C:

* threads are `clone`, locks are futexes, and time comes from
  `clock_gettime` and `timerfd`;
* device access uses the native calls a ring-3 driver already uses:
  `io_mapping_create`/`_map` for BAR0, `interrupt_create`/`_bind`/`_ack`,
  and `vmo_create` + `vmo_pin` for DMA.

Rejected alternatives:

* threads in the native runtime: a runtime of its own for one driver;
* a kernel driver: refused by 2026-09-13, and 13 MB of C in ring 0.

**How it starts.** devmgr gains a match for vendor `0x10de`, class `03`.
It starts `nvrm` through the personality's exec path rather than
`process_create`, because a ferrousli program needs `argv` and `auxv` on
its stack. devmgr then gives it the device and control handles on its
bootstrap channel, as START gives them to the other drivers. This is a new
driver kind, `Gpu`. Like `Engine` (gc400), it is not restarted at first.
Restart comes at N2, once `nvrm` can tear down a GSP that it did not boot
(R2).

### 4.2 The OS layer

The ≈ 260 functions of §2.2 come in four groups. The estimates are lines of
Ferrix code.

1. **C kept from `kernel-open/nvidia` (MIT), with its Linux calls
   replaced.** About 9k of the 25k lines survive. Most of it is the
   ioctl dispatcher and per-file state in `nv.c`, the mapping-context logic
   in `nv-mmap.c`, the registry parser in `os-registry.c`, and the
   `/proc/driver/nvidia` text. This logic is OS-neutral in all but its
   calls, and rewriting it would only add risk. FreeBSD's NVIDIA driver is
   built the same way.
2. **A Rust OS library under it, `ferrix-nvos`, about 5k lines.** It
   provides:
   * memory: `os_alloc_mem` from the process heap, and `nv_alloc_pages` as
     VMOs pinned into the device's domain;
   * locks, semaphores and wait queues: futex-based, with RM's spinlocks as
     spin-then-futex mutexes;
   * threads: a work-queue thread pool and timers;
   * PCI configuration through the native window of N0d;
   * MMIO through `IoMapping`s;
   * firmware: read from the volume;
   * random bytes, from `getrandom`;
   * logging, to the kernel log through the driver log channel.
3. **Stubs**: Tegra, NVSwitch, NVLink, libspdm, IMEX, vGPU, NUMA onlining,
   and ACPI. A desktop GPU in a VM has no `_DSM` worth calling; real ACPI
   for laptop muxes and power can come later.
4. **The request bridge** to the device core of §4.4: client copies, client
   pins, mapping replies and fd identity.

### 4.3 Memory, DMA and interrupts

* **DMA.** RM allocates system memory through `nv_alloc_pages`, maps it
  for the device with `nv_dma_map_alloc`, and uses the returned addresses.
  On Ferrix:
  * each allocation is a VMO pinned with `vmo_pin` into the card's IOMMU
    domain;
  * `VMO_PIN_ADDRESSES` gives the device addresses. These are
    identity-mapped, each page at its own physical address
    (`src/kernel/src/iommu.rs`);
  * RM needs no physically contiguous memory on Ampere. GSP's firmware is
    reached through a radix-3 page table of 4 KiB pages.
  
  `nv_dma_map_mmio` and `nv_dma_map_peer` (peer-to-peer) return
  unsupported.
* **How much is pinned.** GSP boot pins the firmware image and its logs,
  some tens of MiB. After that, every Vulkan or CUDA allocation in system
  memory is pinned too, which can reach gigabytes. The pin quarantine's cap
  (`QUARANTINE_CAP_PAGES`, about 520 MiB, set for a 256 MiB largest driver
  pin; `src/kernel/src/object/pin.rs`) has to become a per-driver budget the
  driver declares (N0f). Until then, a crash of a busy `nvrm` would be
  refused new pins until the next HELLO.
* **Client pages.** `os_lock_user_pages` pins a *client's* pages. This
  happens for `NV01_MEMORY_SYSTEM_OS_DESCRIPTOR`, which GL and Vulkan use
  for imported host memory, and CUDA for `cudaHostRegister`. It becomes a
  native call scoped to an in-flight request (§4.4): the kernel pins the
  calling process's range into the card's domain and returns the
  addresses. The pin belongs to `nvrm` and is quarantined like `nvrm`'s
  own pins.
* **Interrupts.** The 3060 has MSI and no MSI-X (§2.3). N0b adds MSI: one
  vector, 64-bit address, not maskable per vector. It is minted from the
  same 64-vector pool and delivered as `PACKET_INTERRUPT`. The interrupt
  thread in `nvrm` calls `rm_isr`. If that asks for the bottom half, the
  thread runs `rm_isr_bh` on the work queue and then acknowledges. Ferrix's
  storm bound stays in force.
* **Write-combining.** BAR1, and most of RM's system memory mapped into
  clients, must be write-combined. Uncached BAR1 makes every vertex upload
  a series of single PCIe writes. x86-64 Ferrix does not program the PAT
  (`src/kernel/src/arch/x86_64/mod.rs`: "Write-combining needs the PAT, which
  is not programmed"). N0c programs it and adds a `write_combining` flag to
  `IoMapping` and to client mappings.
* **Apertures.** BAR1 is 16 GiB at about 56 TiB. `nvrm` maps only what it
  touches: BAR0 whole, and BAR1 windows on demand. Clients' BAR1 windows
  are mapped by the kernel from the aperture (§4.4). `DeviceInfo` reports
  aperture *counts*, not addresses, and its `DeviceBlock.length` is a
  `u32`. N0d makes both 64-bit addresses and lengths.

### 4.4 The device files: a forwarding core

The existing cores for `/dev/dri`, `/dev/snd` and `/dev/input` decode every
ioctl in the kernel, copy fixed sizes, and send typed messages to their
driver. RM's ioctls cannot be decoded that way (§2.2: pointers RM follows
itself, and UVM's raw numbers). So a new kernel core,
`src/kernel/src/interfaces/chardev/`, forwards requests **undecoded**. It
sits in the `load` ring beside display and render (§6).

* **Registration.** `nvrm` registers device numbers with the core:
  * 195:0, 195:255, 195:254;
  * later `nvidia-uvm`'s dynamic major;
  * a name for `/proc/devices` for each.
  
  The core makes the devfs nodes and `/dev/char` links, and lists the
  names in `/proc/devices`, which Ferrix does not have yet.
* **Requests.** `open`, `ioctl(cmd, arg)`, `mmap(offset, len, prot)`,
  `poll` and `release` each become a message on `nvrm`'s port. A message
  carries:
  * a request handle;
  * the client's file identity and its credentials (euid, pid, namespace);
  * the raw `cmd` and `arg`.
  
  The client's thread sleeps in the core until `nvrm` replies.
* **Client memory.** The request handle authorizes three native calls,
  and only while that request is outstanding:
  * `request_copy_in(request, address, buffer, len)` and
    `request_copy_out(…)`: the kernel copies to or from the waiting
    client's address space, with the same checks as `copy_from_user`.
    These back `os_memcpy_from_user`, `os_memcpy_to_user` and
    `nvkms_copyin`;
  * `request_pin(request, address, len, device)`: the client-page pin of
    §4.3;
  * `request_file(request, fd)`: resolves one of the client's descriptors
    to a file identity of this core, or fails. This backs
    `NV_ESC_REGISTER_FD`, export and import to fd, and
    `nvkms_fd_is_nvidia_chardev`.
  
  Once the reply is sent, the handle is dead. `nvrm` can never reach a
  process that is not, at that moment, waiting in a call to it.
* **mmap.** For `mmap(fd, offset)`, `nvrm` looks up the mapping context
  that `NV_ESC_RM_MAP_MEMORY` left on that file. It replies with one of
  two things, plus the caching (UC or WC):
  * an aperture range of its device: BAR0 doorbell pages, or a BAR1
    window;
  * a VMO range: system memory.
  
  The kernel maps that into the client: `map_window` for apertures,
  checked against the device's apertures; a shared VMO mapping for
  memory. Revocation is a later message that makes the kernel unmap every
  client mapping of a file.
* **Events.** `NV_ESC_ALLOC_OS_EVENT` and `nv_post_event` become readiness
  on the file. `nvrm` sends `EVENT(file)`, and the core wakes the file's
  `poll` and `epoll` waiters.
* **Cost.** Every RM ioctl is a round trip to `nvrm`. Ioctls are used for
  allocation, mapping and control, not for submitting work: Vulkan and GL
  ring doorbells through the USERD and doorbell pages they have mapped.
  The round trip therefore stays off the per-frame path. The measured
  native round trip is 2.6–6.5 µs (`docs/BACKLOG.md`, 2026-10-01).

`/proc/driver/nvidia/{params,version,gpus/<bdf>/information}` are text
files that the core asks `nvrm` for, under a `procfs` hook. A `/sys/module/
nvidia/initstate` reading `live` is added to sysfs, which already serves
`/sys/bus/pci/devices`.

### 4.5 NVKMS and the display

NVKMS (`nv-modeset-kernel.o`) links into the same `nvrm` process, so
`nvkms_call_rm` is a direct call. `/dev/nvidia-modeset` is served by the
same forwarding core. That is enough for:

* the userspace's own use of `nvidia-modeset`, since EGL and GLX open it
  for display queries and surface memory;
* the later choice of scanning out on the 3060's own outputs (N6).

Without N6, the card drives no monitor, and Ferrix's screen stays
virtio-gpu, seen over VNC or SPICE.

### 4.6 How frames reach hyprix, yserver, Chrome and Steam

The card renders. The problem is getting its pixels to a compositor that
today takes only `wl_shm` (`docs/YSERVER.md` §1: no `zwp_linux_dmabuf_v1`).
There are three steps, and each one stands on its own:

1. **Copy presentation (N3a).** A Vulkan layer of Ferrix's, `VK_LAYER_FERRIX_wsi`,
   implements `VK_KHR_wayland_surface` and the swapchain over NVIDIA's
   driver:
   * the swapchain's images are ordinary device images;
   * at present, a `vkCmdCopyImageToBuffer` copies the image into a
     host-visible, linear buffer in system memory;
   * the frame is then attached as a `wl_shm` buffer.
   
   This needs nothing from nvidia-drm. At 1080p it moves 8 MB per frame
   over PCIe, which is cheap. The CPU copy into hyprix's shm and hyprix's
   CPU compositing are not cheap (39 ms at 1080p, `docs/GPU.md`). Under
   libvirt, hyprix has no virgl, because virgl would render on the host's
   3090, which this work must not touch.
2. **dmabuf (N3b).** This step makes NVIDIA's own WSI, EGL and GBM work
   unmodified, which Chrome and every EGL program need:
   * Ferrix serves `/dev/dri/renderD129` with nvidia-drm's subset over RM
     and NVKMS. That subset is GEM handles, PRIME export and import,
     `drm_syncobj`, and NVIDIA's private ioctls `0x00`–`0x18` (Appendix B).
     It is written in `nvrm`, not ported from nvidia-drm's 15k Linux-DRM
     lines, and served through the forwarding core;
   * hyprix gains `zwp_linux_dmabuf_v1` and `wp_linux_drm_syncobj_v1`. It
     advertises the linear modifier only, so that it can map a buffer from
     the CPU while it still composites on the CPU.
3. **The compositor on the card (N3c).** hyprix's renderer, which already
   draws through a render node (`docs/GPU.md` §3.3), runs on NVIDIA's
   Vulkan and imports clients' dmabufs without a copy. Only the finished
   frame is copied, into virtio-gpu's scanout, or not at all with N6.

**yserver.** Its X clients get Vulkan through NVIDIA's X11 fallback
presentation (§2.2), which works with no change to yserver if `MIT-SHM` and
`PutImage` are enough. GL clients need GLX. NVIDIA's `libGLX_nvidia` needs
`NV-GLX`, which only NVIDIA's X driver has, or DRI3 with dmabufs from
nvidia-drm. That leaves two options:

* **(a)** DRI3 and Present in yserver over N3b;
* **(b)** Mesa's GLX over zink on NVIDIA's Vulkan, presenting through the
  same X11 path. That costs one Mesa build in the volume and no NVIDIA
  GLX.

(b) needs less from Ferrix. (a) is what NVIDIA supports (D4).

**Chrome.** Chrome runs `--enable-gpu` over EGL or ANGLE-Vulkan, with
dmabuf presentation to Wayland. It needs N3b.

**Steam.** The client's web helper keeps its software path. Games need
either GL or Vulkan through yserver, as above.

## 5. What other parts of Ferrix need

* **The kernel**, as prerequisites N0a–N0d and N0f (§7):
  * DMAR scopes through bridges;
  * MSI;
  * the PAT and write-combining;
  * a driver's configuration-space window and 64-bit aperture addresses;
  * a per-driver pin budget.
  
  All of them are inside the certified item's `core` and `item` rings, so
  each needs the consultant's OK.
* **devmgr**: the `0x10de` match, the `Gpu` kind, and starting a
  Linux-personality driver.
* **procfs**: `/proc/devices`, and a hook for `/proc/driver/<name>`.
* **sysfs**: `/sys/module/<name>/initstate`.
* **hyprix**: `zwp_linux_dmabuf_v1` and the syncobj protocol (N3b).
* **xtask**:
  * `run-nvidia`, which checks that the shared domains are shut off,
    defines `ferrix-3060` from a template, copies the image, captures
    serial, and shuts the domain down at the end;
  * the gates that follow `test-yserver`'s shape, but run only on nazuna
    and only when the card is free (§6).

## 6. Certification, and the tests

**The certified item stays free of NVIDIA code**:

* `nvrm` is a ring-3 process, so none of NVIDIA's C, its headers, the
  OS layer or the firmware is in `src/kernel` at all. It is outside the
  item as every ring-3 driver is (`docs/certification/ITEM.md` §2: "Device
  enumeration is inside; device drivers are not").
* The forwarding core `interfaces/chardev` is a GPU driver's kernel half.
  It goes in the `load` ring with `interfaces/display` and `render`, in
  `tools/common/data/certification-item.json`.
* Its native calls (`request_copy_*`, `request_pin`, `request_file`, mmap
  and event replies) are registered through `syscall::native::serve` and
  answered above the item, as the `*_control_create` calls are.
* `check-item-boundary.py` already enforces the rings. A new line in it
  refuses any path under `src/kernel` that names `nvidia` or includes a
  header from the fetched tree.

**What enters the item** is only the platform work of N0: MSI, DMAR
bridge scopes, the PAT, the configuration window and the pin budget. These
are generic. A future AMD or Intel driver needs the same, and the
certification consultant reviews each one before landing, as usual.

`request_copy_*` and `request_pin` widen what a driver can do. A driver can
now read and write a client's memory and pin it, but only for a request
that client made to that driver's own device file, and only while the
client waits. They are new findings to be argued in
`docs/certification/FINDINGS.md`: the threat is a compromised `nvrm`
reading a client of its own. Linux has the same exposure, because the RM
there runs in ring 0.

**Tests.** The GPU is shared and is not in CI, so the gates are xtask
commands run on nazuna:

* `test-nvidia-probe` checks the §2.3 lines;
* `test-nvidia-smi` is N1's exit;
* `test-nvidia-vulkan` renders `vkcube`'s offscreen frames, reads them
  back and checks their hash range, for N2;
* the presentation gates reuse `test-display`'s picture judge.

Every gate first checks that the shared domains are shut off, and refuses
to start otherwise. CI keeps what it can run anywhere: the kernel
prerequisites' boot checks under QEMU, where an emulated MSI device and a
root port in `xtask`'s machine cover N0a and N0b, and `nvrm`'s OS-layer
unit tests.

## 7. Milestones and points

| Slice | What | Points |
|---|---|---|
| N0a | DMAR sub-hierarchy scopes followed through bridges; `xtask`'s q35 gets a root port to prove it | 2 |
| N0b | MSI next to MSI-X: enumeration, vectors, delivery, and a boot check on an emulated MSI-only device | 3 |
| N0c | PAT programmed; write-combining `IoMapping`s and client windows | 3 |
| N0d | 64-bit aperture addresses and lengths in `DeviceInfo`; a driver's configuration-space window with the kernel-owned registers (command, BARs, MSI and MSI-X) refused | 3 |
| N0e | `run-nvidia` and `test-nvidia-probe` (libvirt, the shared-domain guard, capture) | 2 |
| N0f | Per-driver pin budget in place of the quarantine's fixed cap | 2 |
| N1a | `fetch-nvidia.sh`: sources, objects, `.run` extraction, the volume | 3 |
| N1b | `nvrm` skeleton: a ferrousli static program started by devmgr, with handles over bootstrap | 4 |
| N1c | The OS layer (§4.2), with the 9k lines of kept C and `ferrix-nvos` | 10 |
| N1d | GSP boots on the 3060: firmware load, booter, WPR, the first RPCs | 6 |
| N1e | The forwarding core, `/dev/nvidia{ctl,0}`, `/proc/driver/nvidia`, `/proc/devices` | 6 |
| N1f | `nvidia-smi` from the volume lists the GPU (`test-nvidia-smi`) | 3 |
| N2 | Vulkan offscreen: mmap contexts, client pins, events and `poll`, fd identity; `vulkaninfo` and `vkcube` offscreen (`test-nvidia-vulkan`) | 14 |
| N3a | Copy presentation layer: `vkcube` in a hyprix window | 6 |
| N3b | nvidia-drm subset as `renderD129`; hyprix `zwp_linux_dmabuf_v1` and syncobj; NVIDIA's own WSI and EGL | 16 |
| N3c | hyprix composites on the 3060 | 8 |
| N4 | Chrome `--enable-gpu` on the 3060; GLX for yserver (D4); a Steam game on the 3060 | 20 |
| N5 | CUDA: a port of `nvidia-uvm` (146k Linux-bound lines), at least external allocations and VA management | 40+ |
| N6 | Scan-out on the 3060's own outputs through NVKMS | 15 |

Each milestone's total, and what it shows:

* **N0**: 15 points.
* **N1**: 32 points, the exit of §1.
* **N2**: 14 points. It is where "real driver" becomes "renders".
* **N3**: 30 points.
* **N4**: 20 points.

That is **111 points from here to Chrome and Steam on the card**. N5 (CUDA)
and N6 (its own monitor) are sized apart and decided apart.

`docs/GPU.md` §4 said "well over a hundred points". It still is. The
difference is that every part is now named.

N1d is the widest estimate. GSP boot is where RM's assumptions meet a new
OS for the first time, and NVIDIA's firmware tells you very little when it
refuses (R1).

## 8. Risks

* **R1 — GSP boot.** On Ampere, RM must do all of the following before
  anything else works:
  * read the VBIOS through BAR0's PROM window;
  * run FWSEC from it, then the booter;
  * load a 75 MB firmware file whose RM image goes into WPR in video
    memory;
  * exchange RPCs over a message queue in system memory.
  
  Any of the DMA, timing or interrupt plumbing being wrong shows up as a
  GSP that never answers. Mitigations:
  * the probe's VT-d domain and VFIO path already work (§2.3);
  * RM's own logging (`NVreg_RmMsg`) goes to the kernel log;
  * the same release runs on the host's 3090. Comparing register traces is
    possible in principle, but **not done**: the 3090 is off limits to this
    work.
* **R2 — the card's state across guests.** The 3060 is also GameLab's and
  win11's. VFIO resets the card when the domain starts, and Ampere resets
  cleanly on a secondary bus reset. A `nvrm` restart *within* one boot
  needs RM's own unload path (`rm_shutdown_adapter`) and a GSP reset,
  which is why restart waits.
* **R3 — RM's Linux assumptions.**
  * Process identity: `os_get_current_process`, `os_get_euid`, pid
    namespaces through `os_find_ns_pid`. These come from the request's
    credentials.
  * `os_is_administrator`: root in the client's user namespace.
  * Per-file private data, and mapping contexts that outlive the ioctl
    that made them.
  * `os_get_max_user_va`: RM wants the client's address-space limits.
  
  Each has a Ferrix answer in §4.4. The risk is the ones not found
  until N2.
* **R4 — mapping volume.** Vulkan maps many small BAR1 and system-memory
  ranges into clients, and RM may revoke them. The kernel's client-window
  bookkeeping was sized for one virtio-gpu window (`docs/GPU.md` §6.1).
* **R5 — the userspace's other probes.** NVML reads PCI configuration
  through sysfs (`config`, `resource`), which Ferrix lacks
  (`docs/SYSFS.md`), and it may refuse a GPU without them. `nvidia-smi` at
  N1f is where this is found out.
* **R6 — where it runs.** Only on nazuna under libvirt with KVM and VFIO,
  or on bare metal with an NVIDIA card. Under WHPX there is no
  passthrough, the machine has one vCPU, and the path does not exist.
  Under KVM, the frames are only seen through VNC or SPICE of a virtio-gpu
  screen, with a copy, unless N6 drives a monitor attached to the 3060.
* **R7 — the card is shared.** Every boot needs the customer's five
  domains shut off. No gate can be scheduled; it runs when the card is
  free.
* **R8 — pinned version.** RM and the userspace must match exactly
  (`NV_ESC_CHECK_VERSION_STR`). Moving the pin means a new fetch, rebuilt
  objects, and N1f to N2 run again. The host's own driver updates are
  independent of Ferrix.
* **R9 — CUDA.** UVM is 146k lines written against Linux's memory manager:
  `mmu_notifier`, HMM, `migrate_vma`, and GPU fault replay. libcuda opens
  `/dev/nvidia-uvm` and manages GPU virtual address space through it even
  for plain `cudaMalloc`. N5 is a port of a Linux subsystem's worth of
  assumptions, not glue.

## 9. Decisions for the customer

Taken on 2026-10-02:

1. **A real NVIDIA driver**: NVIDIA's open modules, their GSP firmware and
   their unmodified userspace. Not nouveau or NVK.

Open:

* **D1 — where the core runs.**
  * Recommended: `nvrm`, a ring-3 Linux-personality process using native
    calls for the device (§4.1).
  * Alternatives: a native program after native threads are built, or the
    kernel (against 2026-09-13).
* **D2 — the sources.**
  * Recommended: fetched at a pinned tag and built out of tree; only
    Ferrix's OS layer is in the repository (§3).
  * Alternative: vendor the 2-million-line core.
* **D3 — the release.**
  * Recommended: 580.173.02, which is nazuna's own userspace and firmware.
  * Alternative: the newest, 615.71.09.
* **D4 — GL for X clients.**
  * Option (a): DRI3 and Present in yserver over the nvidia-drm subset.
    This is what NVIDIA supports.
  * Option (b): Mesa zink over NVIDIA's Vulkan. This is less work for
    Ferrix (§4.6).
* **D5 — presentation order.** N3a's copy layer first, which is the
  smallest step to a picture, or straight to N3b. Also whether N3c (hyprix
  on the card) is in scope.
* **D6 — a monitor on the 3060.** Is one attached, or can one be? N6
  replaces VNC with real scan-out.
* **D7 — CUDA (N5)**: now, later, or not at all. At 40+ points it is as
  large as N0–N2 together.
* **D8 — the platform changes in the item** (N0a–d, f). Accept them as
  generic prerequisites for any PCIe GPU, reviewed by the consultant.
* **D9 — the card's time.** May an agent start `ferrix-3060` whenever the
  five shared domains are shut off, as was done for this pass? Or only in
  windows the customer names?

## 10. Where it stands

* **2026-10-02 — feasibility pass.**
  * Sources at 580.173.02 fetched outside the repository. The two objects
    were built, their imports listed and counted, and they link into a
    user program.
  * The `ferrix-3060` domain is defined and shut off.
  * Ferrix enumerates the 3060, sizes its 16 GiB BAR1, reads its
    registers, and gives it a VT-d domain on the root bus. It does not
    give it one behind a root port (N0a), and it gives it no interrupt
    (N0b).
  * The probe is a never-land commit on branch `nvidia`.
  * Nothing is built toward N1 until the customer answers §9.

---

## Appendix A: the probe's domain

`~/ferrix-nvidia-vm/ferrix-3060.xml`, defined with
`virsh -c qemu:///system define`. The image is the branch's
`build/x86_64/ferrix.img`, copied next to it. Serial output is read with
`python3 capture.py serial.log` before `virsh start`.

```xml
<domain type='kvm' xmlns:qemu='http://libvirt.org/schemas/domain/qemu/1.0'>
  <name>ferrix-3060</name>
  <memory unit='GiB'>4</memory>
  <vcpu>2</vcpu>
  <os>
    <type arch='x86_64' machine='q35'>hvm</type>
    <loader readonly='yes' type='pflash' format='raw'>/usr/share/OVMF/OVMF_CODE_4M.fd</loader>
    <nvram template='/usr/share/OVMF/OVMF_VARS_4M.fd' templateFormat='raw' format='raw'>/var/lib/libvirt/qemu/nvram/ferrix-3060_VARS.fd</nvram>
  </os>
  <features><acpi/><apic/></features>
  <cpu mode='host-passthrough' check='none'><maxphysaddr mode='passthrough'/></cpu>
  <on_poweroff>destroy</on_poweroff><on_reboot>destroy</on_reboot><on_crash>destroy</on_crash>
  <devices>
    <disk type='file' device='disk'>
      <driver name='qemu' type='raw'/>
      <source file='/home/sebastian/ferrix-nvidia-vm/ferrix.img'/>
      <target dev='sda' bus='sata'/><boot order='1'/>
    </disk>
    <serial type='tcp'>
      <source mode='bind' host='127.0.0.1' service='47060'/><protocol type='raw'/><target port='0'/>
    </serial>
    <rng model='virtio-non-transitional'>
      <backend model='random'>/dev/urandom</backend><driver iommu='on'/>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>
    </rng>
    <iommu model='intel'><driver intremap='off' caching_mode='on' aw_bits='48'/></iommu>
    <video><model type='none'/></video>
    <memballoon model='none'/>
    <hostdev mode='subsystem' type='pci' managed='no'>
      <source><address domain='0x0000' bus='0x01' slot='0x00' function='0x0'/></source>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x05' function='0x0'/>
    </hostdev>
  </devices>
  <qemu:commandline>
    <qemu:arg value='-device'/><qemu:arg value='isa-debug-exit,iobase=0xf4,iosize=0x04'/>
  </qemu:commandline>
</domain>
```

`ferrix-3060-rootport.xml` next to it is the same domain with the
hostdev's guest address left to libvirt, which puts it behind a root port.
That is the run that showed N0a.

## Appendix B: the ioctls `nvrm` answers

* **`/dev/nvidiactl` and `/dev/nvidia0`**:
  * `_IOWR('F', 200 + n)`: `CARD_INFO` 0, `REGISTER_FD` 1, `ALLOC_OS_EVENT` 6,
    `FREE_OS_EVENT` 7, `STATUS_CODE` 9, `CHECK_VERSION_STR` 10,
    `IOCTL_XFER_CMD` 11, `ATTACH_GPUS_TO_FD` 12, `QUERY_DEVICE_INTR` 13,
    `SYS_PARAMS` 14, `EXPORT_TO_DMABUF_FD` 17, `WAIT_OPEN_COMPLETE` 18;
  * the RM API escapes 0x27–0x5F (`src/nvidia/arch/nvalloc/unix/include/nv_escape.h`),
    `IOCTL_XFER_CMD` carrying any of them past the 14-bit size field.
* **`/dev/nvidia-modeset`**: `_IOWR('m', 0, struct NvKmsIoctlParams)`.
* **`/dev/nvidia-uvm`** (N5): raw numbers from `UVM_INITIALIZE`
  (`0x30000001`) and `UVM_IOCTL_BASE(i)`.
* **`renderD129`** (N3b):
  * DRM core: `GEM_CLOSE`, `PRIME_HANDLE_TO_FD` and `PRIME_FD_TO_HANDLE`,
    `SYNCOBJ_*`, `GET_CAP`, `VERSION`;
  * nvidia-drm's private ioctls 0x00–0x18
    (`kernel-open/nvidia-drm/nvidia-drm-ioctl.h`).
