# Ferrix — architecture

Ferrix is an operating system written in Rust for x86-64 and AArch64, whose
acceptance test is that it compiles Rust. Not "has a shell", not "draws a
window": it hosts `rustc`, which is the hardest thing a general-purpose OS is
routinely asked to do and the only goal that forces every subsystem to be real.

This document is the design. `docs/ROADMAP.md` is the order it gets built in.

---

## 0. The goal, read as a specification

"Runs the Rust compiler" is not an aspiration, it is a requirements list.
A statically linked musl `rustc` needs, at minimum:

| It needs | Which forces |
|---|---|
| `clone(CLONE_THREAD\|CLONE_VM\|CLONE_SETTLS)`, the `futex` family, `set_tid_address`, robust lists | 1:1 kernel threads, a real futex implementation, per-thread TLS registers |
| `mmap`/`mprotect`/`munmap` with `MAP_FIXED`, `MAP_NORESERVE`, 2–8 GiB of address space | Demand paging, a VMA tree, lazy anonymous memory, overcommit |
| A `SIGSEGV` handler on an alternate stack | Real signal delivery, `sigaltstack`, `rt_sigreturn` |
| `fork`/`execve`/`wait4` to run the linker | Copy-on-write fork, an ELF loader, process groups, exit status plumbing |
| `openat`, `getdents64`, `statx`, `renameat2`, `pread64`, ~150 syscalls in total | A VFS with inode and dentry caches |
| `/proc/self/maps`, `/proc/self/exe`, `/proc/self/fd` | A procfs backed by the real VM and fd table |
| A writable filesystem with room for a ~2 GiB sysroot | A block stack and an on-disk filesystem that survives a crash |

Every one of those is load-bearing. This is why the system below is shaped the
way it is, and why "toy OS" decisions — a fixed process count, an in-memory-only
filesystem, cooperative threading — are ruled out at the start rather than
discovered to be dead ends at the end.

---

## 1. Shape of the system

```
        ┌─────────────────────────────────────────────────────────────┐
        │ userspace                                                    │
        │   rustc, busybox, init      │ devmgr │ virtio-blk │ virtio-net│
        │   (static musl, Linux ABI)  │  (Ferrix native ABI, handles)  │
        └───────────┬─────────────────┴────────────┬───────────────────┘
                    │ syscall (Linux ABI)          │ channels, VMOs, IRQ objects
        ┌───────────┴──────────────────────────────┴───────────────────┐
        │ kernel                                                        │
        │  syscall layer (Linux ABI)   │  native ABI (capabilities)     │
        │  ── VFS · page cache · btrfs · tmpfs · procfs · devfs         │
        │  ── block core · net core                                     │
        │  ── scheduler (domains, classes) · futex · signals · IPC      │
        │  ── VM: VMOs, VMAs, buddy, slab, reclaim                      │
        │  ── namespaces · cgroups v2 · seccomp                         │
        │  ── arch: MMU, traps, timers, IRQ controller, IOMMU, SMP      │
        └───────────────────────────────────────────────────────────────┘
```

**Monolithic core, capability seams, userspace device drivers.** A syscall is a
function call, not four IPC hops — that is what a compiler workload needs. But
device drivers run as ordinary user processes holding capabilities, so a driver
fault is a process fault. The line is drawn at *devices*: filesystems and the
page cache stay in the kernel because `rustc` touches them on every path, while
the code that pokes a PCIe BAR does not.

The one in-kernel device is a serial port writer, used for early boot and panic
output only, when no userspace exists to talk to. It is named as an exception
here so that it stays one.

---

## 2. ABI: Linux is the native ABI

Syscall 0 is `read`. Not a compatibility layer over something else — the actual
native interface.

This is the highest-leverage decision in the project. It means the existing
static-musl world is our userland from the first day there is a userland, that
POSIX conformance is inherited rather than reimplemented, and that the goal is
reachable by writing an OS rather than by also porting LLVM to a new target.
(That alternative is worth naming: a native target with its own `std` needs LLVM
built for it, which drags a C++ runtime into the tree — making the project
*less* Rust, not more.)

Two ABIs coexist:

* **Linux ABI**, syscall numbers 0.., x86-64 and AArch64 tables as Linux
  defines them. Everything about it is a compatibility obligation; we do not get
  to have opinions.
* **Ferrix native ABI**, syscall numbers from `0x1000`, capability-handle based.
  This is what device drivers, `devmgr` and anything else Ferrix-specific speaks.
  It is where the design opinions live.

A process may use both. `devmgr` is a normal musl binary that happens to call
native syscalls for the parts POSIX cannot express.

---

## 3. Kernel objects

The native ABI is built on typed, reference-counted kernel objects reached
through per-process handle tables. Nine of them:

| Object | What it is |
|---|---|
| `Vmo` | A pageable memory object: pages, not a mapping. Anonymous memory, page-cache pages, and DMA buffers are all VMOs. |
| `AddressSpace` | Page tables plus an interval tree of `Vma`s, each mapping a range of a VMO. |
| `Channel` | Bidirectional datagram pipe that carries bytes *and handles*. The basis of driver IPC. |
| `Port` | An event queue a thread waits on; how one driver thread services many sources. |
| `Interrupt` | A bindable hardware interrupt. A userspace driver waits on it via a `Port`. |
| `IoMapping` | An MMIO aperture, mappable into a driver's address space, with its IOMMU domain. |
| `Task` | A schedulable thread. |
| `Process` | A group of tasks sharing an `AddressSpace`, fd table, fs context and signal dispositions. |
| `Job` | A container of processes, and where resource limits and kill authority live. |

**The VMO unification is the load-bearing idea.** One page-list abstraction
serves the page cache, anonymous memory, shared memory, and driver DMA buffers.
A block driver reading into a page-cache page is not copying into a buffer and
then into the cache — it is filling the VMO the cache already holds, which is
what makes userspace drivers affordable for a compiler workload.

`Job`s exist because a userspace driver that wedges has to be killable as a
unit, together with anything it spawned.

---

## 4. Memory

**Physical.** Buddy allocator over the UEFI memory map, orders 0–10, with
per-CPU magazine caches so the common single-page allocation never takes the
global lock. Frames carry a `PageInfo` with refcount, owning VMO and flags —
this is what makes reclaim and copy-on-write tractable.

**Kernel heap.** Slab allocator over the buddy, with per-CPU free lists and
size-class caches for the general `alloc` path. Kernel object types get their
own slabs, so a `Task` allocation is a pop off a list.

**Virtual.** Four-level paging on both architectures, 48-bit addresses,
*identical layout constants* on each:

```
0x0000_0000_0000_0000 .. 0x0000_7FFF_FFFF_FFFF   user
0xFFFF_8000_0000_0000 .. 0xFFFF_FEFF_FFFF_FFFF   direct map of all physical RAM
0xFFFF_FF00_0000_0000 .. 0xFFFF_FFEF_FFFF_FFFF   kernel vmap (MMIO, guard-paged stacks)
0xFFFF_FFFF_8000_0000 .. 0xFFFF_FFFF_FFFF_FFFF   the kernel image
```

x86-64 puts the image in the top -2 GiB because that is what the "kernel" code
model addresses; AArch64 does not need to, and does anyway, because one layout
is one set of bugs instead of two.

**Address spaces.** A red-black interval tree of `Vma`s, each naming a VMO, an
offset, a protection and a share mode. `mmap` inserts, `munmap` splits,
`mprotect` splits and re-permissions. Anonymous memory is lazy; `fork` marks
both sides read-only and copies on fault; file mappings fault from the page
cache VMO directly, so a mapped file and a `read` file are the same pages.

**Reclaim.** Two-list LRU (active/inactive) with a shrinker interface for the
caches. `rustc` will exhaust memory on a small machine, so this path is a
correctness requirement, not a nicety: an OOM kill scoped by `Job` and cgroup,
never a livelock.

---

## 5. Scheduling: selectable per domain, at runtime

The scheduler is not one policy. CPUs are partitioned into **scheduling
domains**, each of which runs in one of three modes, changeable at runtime:

| Mode | Classes, highest first | Preemption | Interrupts |
|---|---|---|---|
| `Throughput` | EEVDF fair → idle | Preemption points, deferred for cache locality | In-context |
| `SoftRt` | FIFO/RR (1–99) → EEVDF → idle | Fully preemptible kernel, PI mutexes | Threaded |
| `HardRt` | EDF with CBS admission control → nothing else | Fully preemptible, bounded sections | Threaded, bandwidth-reserved |

**Per domain, not global.** A four-core machine can run a `HardRt` partition on
one core and `Throughput` on the other three, with `rustc` on the latter. A
global switch would be less useful and much harder to make safe, since it would
have to quiesce a machine that is doing something.

**EEVDF, not CFS**, for the fair class: it gives each task an actual eligible
time and a deadline, so latency has a bound instead of only fairness having one.

**Switching modes** takes the domain through a quiescent point: stop admitting,
wait out an RCU grace period, migrate tasks whose policy the new mode does not
offer, swap the class stack, resume. Tasks that cannot be represented in the new
mode are demoted with an errno the switching caller sees, never silently.

**What `HardRt` does and does not promise.** It promises EDF with admission
control that refuses an unschedulable set, bounded kernel critical sections on
the RT path, preallocated pools instead of dynamic allocation there, and
interrupts that cannot steal unaccounted time. It does not promise a certified
worst-case execution time for the whole kernel — no OS that also hosts LLVM can
offer that, and claiming it would be the kind of statement that gets believed.

Per-CPU runqueues; work stealing in `Throughput`, none in `HardRt` (partitioned
scheduling is what makes the admission test valid).

Linux's `sched_setscheduler` maps onto this: `SCHED_FIFO`/`SCHED_RR` to the
soft-RT classes, `SCHED_DEADLINE` to EDF, `SCHED_OTHER`/`BATCH`/`IDLE` to fair.

---

## 6. Processes, threads, isolation

**1:1 threading**, forced by the ABI. `clone` flags compose independently
shareable objects — address space, fd table, fs context, signal handlers,
namespace set — exactly as Linux does, because a process is precisely a
particular sharing arrangement of those.

**Namespaces**, all eight, designed in from the start rather than retrofitted:
pid, mount, uts, ipc, net, user, cgroup, time. Retrofitting namespaces means
finding every global table years later; designing them in means every such table
is reached through the task's `NsSet` from the first line. A `pid_t` is
meaningless without saying in which pid namespace.

**cgroups v2**, one unified hierarchy, with the `cpu`, `memory`, `io` and `pids`
controllers. `cpu` is not a separate mechanism — it is bandwidth and weight
handed to the scheduling classes above. `memory` is where reclaim and the OOM
killer are scoped.

**seccomp**, classic BPF filters evaluated on syscall entry, with an interpreter
that is itself a pure function over bytes and therefore fuzzable and Miri-able
in `libs/`.

**Credentials** are Unix: uid, gid, supplementary groups, POSIX capability sets,
no-new-privs. They sit on top of the handle system rather than beside it.

---

## 7. Device drivers in userspace

The kernel enumerates buses, because that needs ACPI (x86-64) and device tree
(AArch64) and privileged access. It does not drive devices.

For each device found, the kernel creates a device node and hands `devmgr` a
handle. `devmgr` matches a driver, spawns it in its own `Job`, and gives it:

* an `IoMapping` for each BAR or MMIO window, and nothing outside it,
* an `Interrupt` object per vector, bound to a `Port`,
* a `Vmo` for DMA, whose device addresses come from an IOMMU domain scoped to
  that device — a driver cannot DMA over the kernel, or over another driver,
* a `Channel` to the kernel subsystem it serves (block, net, input).

**The data path is not per-request IPC.** Driver and kernel share a descriptor
ring in a VMO and ring a doorbell; requests batch. This is why userspace drivers
are affordable under a compiler's I/O load, and it is the same shape virtio and
NVMe already use, so the driver's own ring and ours line up.

**Bootstrap.** The root filesystem needs a block driver that lives in userspace
that lives on the root filesystem. The loader breaks the cycle: it loads an
initramfs into RAM containing `devmgr`, the virtio-blk driver and `init`. The
kernel mounts that as root, starts init, drivers come up, and the system pivots
onto btrfs. This is exactly Linux's answer, and it works for exactly the same
reason.

**IOMMU is not optional here.** A userspace driver without an IOMMU is a
userspace process that can write to any physical address, which is worse than an
in-kernel driver, not better. VT-d/AMD-Vi on x86-64, SMMUv3 on AArch64. Where no
IOMMU exists, drivers run in a degraded trusted mode and the kernel says so
loudly at boot.

---

## 8. Storage

**Block core** in the kernel: request queues, merging, an I/O scheduler with
per-cgroup bandwidth, and the ring protocol to userspace drivers.

**VFS** in the kernel: inode cache, dentry cache with negative entries, mount
table per mount namespace, page cache unified with VMOs. `rustc` opens tens of
thousands of files during a build; this path is a performance requirement.

**Filesystems.** tmpfs, devfs, procfs, sysfs and cgroupfs in-kernel and small.
The real one is **btrfs, read and write**, staged:

* **Stage A — read.** Superblock, chunk tree (logical→physical), root tree, fs
  trees, extent data both inline and regular, directory and inode items, crc32c
  verification, zstd/zlib/lzo decompression. Enough to mount what `mkfs.btrfs`
  produced and read a sysroot out of it.
* **Stage B — write.** Copy-on-write block allocation through the extent tree,
  delayed refs, transaction commit against both superblock copies with the right
  flush/FUA ordering, free-space tree, log tree and its replay.
* **Stage C — subvolumes and snapshots**, then the rest.

Single device to begin with; no RAID 5/6, which is where btrfs itself is
weakest. This is the largest single piece of work in the project and the
schedule should say so.

**How it is verified**, which matters more than the feature: every CI run builds
an image with real `mkfs.btrfs`, mounts it under Ferrix in QEMU, runs a
workload, and then hands the resulting image to `btrfs check` on the host. A
filesystem that only our own reader can read is not a filesystem. Power-fail
injection — killing QEMU mid-transaction and checking what survives — is the
test that a CoW filesystem actually has to pass.

---

## 9. What is written where, and why

| Layer | Lives in | Reachable by tests |
|---|---|---|
| Byte-level logic: ELF, cpio, btrfs item parsing, seccomp BPF, page-table arithmetic, allocators | `libs/` | `cargo test`, Miri, fuzzers |
| Loader | `boot/` | QEMU boot test |
| Kernel | `kernel/` | QEMU boot test, in-kernel test harness |
| Host tooling | `xtask/` | `cargo test` |

The split is not cosmetic. Nothing in `kernel/` can be run by `cargo test`,
Miri cannot interpret a privileged instruction, and a fuzzer cannot drive a
page fault handler. So anything expressible as a pure function of bytes is
written as one, in `libs/`, where all three tools reach it. `scripts/
check-crate-layering.sh` keeps that from eroding.

Architecture-specific code lives under `arch/` and is reached through one
facade. Generic code never names `x86_64` or `aarch64`, and no
`#[cfg(target_arch)]` appears outside those directories — both enforced by the
same script, because a facade maintained by convention is a facade for about six
weeks.

---

## 10. Assembly

There is none at boot on either architecture: UEFI calls a Rust `efi_main` in
64-bit mode. What assembly exists is confined to constructs the machine defines
before a Rust function could run — exception vectors, the syscall trampoline,
context switch, and the CPU primitives with no Rust spelling. `docs/ASSEMBLY.md`
is the list, `scripts/asm-allowlist.json` is its machine-readable form, and CI
fails on an assembly site that is not in it.
