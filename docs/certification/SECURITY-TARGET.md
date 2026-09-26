# Security Target

Common Criteria (ISO/IEC 15408) Security Target for the Ferrix certified item.
Claimed assurance level **EAL5 augmented with ALC_FLR.2**.

This document closes finding F-21. It is a Security Target in structure and in
content; it has not been evaluated by anyone, and §9 records exactly where it
would not survive one.

| | |
|---|---|
| ST title | Ferrix Kernel Core Security Target |
| TOE | Ferrix certified item, as defined by `scripts/certification-item.json` |
| TOE version | `48d58fb0`, reference configuration in the manifest |
| CC version | 3.1 Revision 5 |
| Assurance level | EAL5+ (ALC_FLR.2) |
| PP conformance | None claimed — see §2.3 |

---

## 1. TOE description

### 1.1 TOE type

A general-purpose operating system kernel core providing memory isolation,
scheduling and capability-mediated access to resources, with device drivers
running as unprivileged user processes.

### 1.2 Physical and logical scope

The TOE is the `core` and `item` rings of [ITEM.md](ITEM.md): **51,525 lines of
Rust**, built for x86-64, AArch64 and ARMv7-A from the reference configuration.
It comprises the memory manager (buddy allocator, VMOs, address spaces, page
tables), the scheduler, the capability object system (handles, channels, ports,
jobs, interrupts, I/O mappings), the trap and system-call entry paths, the
IOMMU drivers (VT-d, SMMUv3), SMP bring-up and TLB shootdown, firmware table
parsing, device enumeration, and the panic path.

**Outside the TOE**, running on it without being trusted by it: the VFS, btrfs,
procfs, sysfs and tmpfs; the TCP/IP stack; the Linux system-call personality,
from its dispatcher to its `mmap`, `futex`, rlimits and POSIX threads; the
ring-3 device drivers; and all user software. 47,528 lines of kernel code
are in this category and the boundary is enforced at build time by
`scripts/check-item-boundary.py`.

The **loader** (`boot/`, 3,503 lines) is in the reference configuration but
outside the TOE; it is covered by A.FIRMWARE in §3.3.

### 1.3 TOE security functionality, in brief

* **Address space isolation.** Each process has a page table the TOE
  constructs; no mapping is simultaneously writable and executable, and every
  boot sweeps all mappings to prove it.
* **Capability-mediated access.** A process names a resource only through a
  handle it holds. Handles carry rights, are unforgeable, and are transferred
  only over channels.
* **Device containment.** A driver runs in ring 3 and reaches its device
  through an `IoMapping` and an IOMMU domain, so a compromised driver cannot
  DMA into memory it was not given.
* **Residual information protection.** A physical frame is zeroed before it is
  handed to a new owner.
* **Resource bounding.** Jobs carry quotas the TOE enforces.

---

## 2. Conformance claims

### 2.1 CC conformance
CC Part 2 conformant, CC Part 3 conformant, EAL5 augmented with ALC_FLR.2.

### 2.2 Rationale for the assurance level
EAL5 is the highest level whose `ADV_IMP.1` (implementation representation of
the TSF, sampled) and `ADV_INT.2` (well-structured internals) are plausible for
a 49,431-line TOE with the evidence described in §8. EAL6 requires `ADV_SPM`, a
formal security policy model, and `ADV_IMP.2` over the complete implementation
representation; neither is available, and [ITEM.md](ITEM.md) §3 names the
`core` ring as where that would later be attempted.

### 2.3 PP conformance
None. The natural candidates do not fit: the OS Protection Profile assumes
identification, authentication and audit functions this TOE deliberately places
outside its boundary (§9.1), and the Separation Kernel Protection Profile
assumes time and space partitioning the TOE does not yet offer as a service.

---

## 3. Security problem definition

### 3.1 Assets

| Id | Asset |
|---|---|
| AS.MEMORY | The contents of each process's address space |
| AS.KERNEL | The TOE's own code, page tables and object tables |
| AS.HANDLE | The handle tables that name every capability a process holds |
| AS.DEVICE | Device registers and DMA-capable memory |
| AS.CPU | Processor time and the scheduling invariants that apportion it |

### 3.2 Threats

The threat agent is **unprivileged code running on the TOE**: a user process, a
ring-3 device driver, or the Linux personality itself. All are outside the TSF
and all are assumed hostile, which is the central design claim being made.

| Id | Threat |
|---|---|
| T.MEMORY | A process reads or writes memory belonging to another process or to the TOE. |
| T.ESCALATE | Unprivileged code causes the TOE to execute attacker-chosen code in ring 0, e.g. by corrupting a page table or a return path. |
| T.FORGE | A process fabricates or guesses a handle to obtain a capability it was never granted. |
| T.DMA | A compromised ring-3 driver programs its device to read or write memory outside the region it was granted. |
| T.RESIDUAL | A process recovers data left in a physical frame by a previous owner. |
| T.EXHAUST | A process consumes memory, CPU or object-table capacity so as to deny service to others. |
| T.CONFUSE | A process induces the TOE to act on a user-supplied pointer or length without validation. |

### 3.3 Assumptions

| Id | Assumption |
|---|---|
| A.PHYSICAL | The platform is physically protected. No defence is claimed against an attacker with bus access, cold-boot or fault injection. |
| A.FIRMWARE | UEFI, TF-A and the loader behave as specified and deliver an unmodified TOE image. The TOE performs no secure or measured boot (§9.2). |
| A.ADMIN | Whoever composes the system image and selects which drivers run is trusted to do so competently. |
| A.HARDWARE | The MMU, IOMMU and interrupt controller behave as their specifications state. |
| A.PROCESSOR | The processor offers the speculation controls [SPECULATION.md](SPECULATION.md) builds on, and they behave as the vendor states: SAFETY-MANUAL AoU-11, checkable from the boot log. |

### 3.4 Organisational security policies
None claimed.

---

## 4. Security objectives

### 4.1 For the TOE

| Id | Objective | Counters |
|---|---|---|
| O.ISOLATE | Separate address spaces so that no process can name memory it was not granted. | T.MEMORY, T.ESCALATE |
| O.WXN | Ensure no mapping is both writable and executable. | T.ESCALATE |
| O.CAPABILITY | Mediate every access to a kernel object through an unforgeable handle carrying explicit rights. | T.FORGE, T.MEMORY |
| O.DMA | Confine every device's memory access to an IOMMU domain the TOE programmed. | T.DMA |
| O.SCRUB | Zero a physical frame before a new owner can read it. | T.RESIDUAL |
| O.QUOTA | Bound the memory, objects and CPU a job may consume. | T.EXHAUST |
| O.VALIDATE | Validate every user-supplied pointer, length and handle at the system-call boundary before use. | T.CONFUSE, T.MEMORY |
| O.FAILSAFE | On detecting an inconsistent internal state, halt rather than continue. | T.ESCALATE |

### 4.2 For the operational environment

| Id | Objective |
|---|---|
| OE.PHYSICAL | The platform is physically protected (A.PHYSICAL). |
| OE.FIRMWARE | Firmware delivers an unmodified image (A.FIRMWARE). |
| OE.ADMIN | Image composition is performed competently (A.ADMIN). |
| OE.HARDWARE | MMU, IOMMU and interrupt controller conform to specification (A.HARDWARE). |
| OE.PROCESSOR | The TOE runs, built `--mitigations on`, only on a processor whose boot log reports no side-channel hazard uncovered (A.PROCESSOR). |

---

## 5. Security functional requirements

Drawn from CC Part 2. Operations: **assignment**, *selection*, refinement.

### FDP — user data protection

**FDP_ACC.1** Subset access control. The TSF shall enforce the **Capability
Access Control SFP** on **subjects: processes and threads; objects: VMOs,
channels, ports, jobs, interrupts, I/O mappings; operations: all operations
named by the native ABI**.

**FDP_ACF.1** Security attribute based access control. The TSF shall enforce
the Capability Access Control SFP based on **the handle a subject presents and
the rights that handle carries**. A subject may perform an operation only if it
presents a handle naming the object and that handle carries the right the
operation requires. A handle is valid only in the handle table of the process
holding it.

**FDP_IFC.1 / FDP_IFF.1** Subset information flow control. The TSF shall
enforce the **Address Space Separation SFP**: information flows between two
processes only through an object both hold a handle to. No implicit flow
through memory is permitted, since no physical frame is mapped into two address
spaces unless a shared VMO says so.

**FDP_RIP.2** Full residual information protection. The TSF shall ensure that
any previous information content is made unavailable upon **allocation** of a
physical frame to any object.

### FMT — security management

**FMT_MSA.1** Management of security attributes. The TSF shall restrict the
ability to *reduce* the **rights carried by a handle** to **the process holding
it**. Rights may never be raised.

**FMT_MSA.3** Static attribute initialisation. The TSF shall provide
*restrictive* default values: a newly created process holds no handles other
than those explicitly transferred to it.

### FPT — protection of the TSF

**FPT_FLS.1** Failure with preservation of secure state. The TSF shall preserve
a secure state — halting with a diagnostic — when **an internal consistency
check fails**.

**FPT_STM.1** Reliable time stamps. The TSF shall provide reliable time stamps
from the platform timer.

**FPT_TDC.1** Inter-TSF basic TSF data consistency. The TSF shall consistently
interpret **handles, VMO offsets and lengths supplied by untrusted subjects**
when shared with the TSF.

### FRU — resource utilisation

**FRU_RSA.1** Maximum quotas. The TSF shall enforce maximum quotas of
**physical memory, kernel objects and CPU time** that **a job** can use
**simultaneously**.

### FIA — identification and authentication
**None claimed.** See §9.1.

### FAU — security audit
**None claimed.** See §9.1.

---

## 6. Security assurance requirements

EAL5 as defined in CC Part 3, augmented with **ALC_FLR.2** (flaw reporting
procedures). No other augmentation is claimed; in particular `AVA_VAN.5` is not
claimed, and `AVA_VAN.4`'s moderate-attack-potential analysis has not been
performed (F-21 successor finding in §9.3).

---

## 7. TOE summary specification

How the TOE meets each objective, with the evidence that exists today.

| Objective | Implementation | Evidence |
|---|---|---|
| O.ISOLATE | Per-process page tables built by `kernel/src/user/space.rs`; higher-half kernel mapping; TLB shootdown on SMP. Against speculative reads, the defences in `kernel/src/arch/speculation.rs` and each architecture's `speculation.rs`: program-chosen indices clamped at the system call boundary, the processor's speculation controls, a predictor barrier at each switch of address space. | `user/check.rs`, `user/rmap_check.rs`; `arch/speculation_check.rs`, every boot: *"speculation defences read back on 4 processors"* |
| O.WXN | Enforced at map time; `WXN`/`NX` set on all three architectures. | Every boot sweeps all mappings: *"w^x 2387 mappings swept, 899 executable, none writable"* |
| O.CAPABILITY | `kernel/src/object/`: handle tables, rights masks, transfer only over channels. | `object/check.rs`, 3,318 lines; *"18 refusals as specified"* |
| O.DMA | `kernel/src/iommu/{vtd,smmuv3}.rs`; a driver receives an `IoMapping` and a domain. | `iommu/gate.rs`; `scripts/check-device-access.py` holds the seam at build time |
| O.SCRUB | `mm::zero_frame` on every frame handed to a VMO. | `kernel/src/mm.rs:1140`, called from `user/vmo.rs` at three sites |
| O.QUOTA | `kernel/src/object/job.rs`. | `object/check.rs` |
| O.VALIDATE | `kernel/src/syscall/uaccess.rs`, backed on x86-64 by SMEP and SMAP since 2026-09-25, and on AArch64 by PAN where the CPU has it. The reference `cortex-a72` does not, and ARMv7-A cannot (V-01). | `syscall/check.rs`, 9,537 lines, 427 refusal assertions; the boot reports *SMEP on, SMAP on* |
| O.FAILSAFE | `kernel/src/panic.rs` with a catalogue of explanations. | `scripts/check-panic-audit.py`; `gen-panic-catalog.py --check` |

---

## 8. Rationale

### 8.1 Threats to objectives
Each threat in §3.2 is countered by at least one objective in §4.1, as the
*Counters* column records. T.MEMORY and T.ESCALATE are each countered by more
than one, since they are the threats the TOE exists to address.

### 8.2 Objectives to SFRs

| Objective | SFRs |
|---|---|
| O.ISOLATE | FDP_IFC.1, FDP_IFF.1 |
| O.WXN | FDP_IFF.1 (refinement) |
| O.CAPABILITY | FDP_ACC.1, FDP_ACF.1, FMT_MSA.1, FMT_MSA.3 |
| O.DMA | FDP_ACF.1, FDP_IFF.1 |
| O.SCRUB | FDP_RIP.2 |
| O.QUOTA | FRU_RSA.1 |
| O.VALIDATE | FPT_TDC.1 |
| O.FAILSAFE | FPT_FLS.1 |

### 8.3 Why EAL5 is the right claim
§2.2. The three arguments that carry it: the TOE is 51,525 lines and 100%
first-party source, so `ADV_IMP.1` is satisfiable; the reference configuration
has zero Cargo features and one two-valued build switch, `--mitigations`, of
which only `on` is evaluated, so the configuration space is enumerated in a
sentence; and
eleven build-time gates support `ADV_INT.2`'s well-structuredness in a way
review notes cannot.

---

## 9. Limitations — where this ST would not survive evaluation

Stated here rather than discovered by an evaluator.

### 9.1 Two SFR families are deliberately absent
There is **no audit function** (FAU) at all, and **no identification or
authentication** (FIA) inside the TOE — POSIX credentials live in
`syscall/credentials.rs`, which is in the uncertified load ring.

For a TOE of this type that is defensible: it is an isolation kernel, and
identity is a personality concern. It is also why no OS Protection Profile can
be claimed (§2.3), and an evaluator would press hard on whether a TOE that
cannot record a security-relevant event can meaningfully claim EAL5.

### 9.2 No trusted boot path
The TOE does not verify its own integrity. A.FIRMWARE carries the whole of that
burden, which is a large assumption to place on the environment.

### 9.3 The vulnerability analysis found a single point of failure
[VULNERABILITY-ANALYSIS.md](VULNERABILITY-ANALYSIS.md) now covers `AVA_VAN.4`
over all seven threats. It found five residual vulnerabilities, of which V-01
bears on three: **no SMAP, SMEP or PAN was enabled**, so the software bound
check in `uaccess` was the only barrier between a user pointer and kernel
memory at kernel privilege.

F-32 has since enabled SMEP and SMAP on x86-64 and PAN on AArch64, and no
product-code path needed an access window. V-01 remains on Arm: the reference
`cortex-a72` is ARMv8.0 and lacks PAN, and the Cortex-A7 of ARMv7-A cannot
have it. There the bound check is still the only barrier.

An earlier draft of this ST claimed SMAP and PAN were enforced. That was wrong
— the tree's 56 apparent references to "smap" are `smap_base` and `smap_len`,
the system memory map — and §7 is corrected above. The mitigation is sound and
centralised; on Arm it still has no hardware defence in depth.

### 9.4 The design evidence does not yet reach the TOE's modules
`ADV_TDS.3` needs a semiformal design decomposing the TSF into subsystems and
modules. `docs/sysml/` is the right notation and describes Ferrix rather than
the TOE, at system granularity (F-15).

### 9.5 The TSF randomises its layout but neither hides nor partitions
The side-channel defences (F-31, [SPECULATION.md](SPECULATION.md)) stop a
program steering a speculative read, on processors A.PROCESSOR admits. KASLR
(§6.1 there) moves the kernel image, the direct map and the vmap arena each
boot, so an exploit needs a disclosure as well as a corruption. The TSF does
not unmap the kernel from a program's tables (no KPTI — not needed on the
reference processors against Meltdown), so a program with a timer can still
find the kernel, and it does not partition a cache, so two processes sharing
one can time each other (V-06). No security objective rests on KASLR. An
evaluator at `AVA_VAN.4` would accept KPTI's absence as argued and press on
the cache for any deployment where processes share one.

### 9.6 The TSF's independence of the load is checked by name, not by type
No reference reaches by name from the TOE into the uncertified load ring, down
from 94 when the audit began (F-07, F-09 and F-33 closed 2026-09-26; the 29 and
62 given here before that day were lower bounds, FINDINGS.md §A). The trap
entry and return paths reach the personality only through what it registers
(F-02, F-02a, F-09); board support, bring-up, power, the native ABI's
subsystems and native process creation register with the TOE rather than
being named by it (F-04, F-07, F-08); the core no longer names the Linux
personality's process or thread (F-01, F-06).

What an evaluator would still press on is what the gate cannot see. It reads
names, so a load-ring value reaching the TOE through a trait object or a
function pointer -- which is exactly how every registration above works -- is
not an edge to it; the interfaces are the TOE's types, but what runs behind
them is not the TOE's code. The crate root, `main.rs`, composes the load with
the TOE and is exempt by file, with 37 edges argued in ITEM.md §2 rather than
checked. And the load ring runs in ring 0, in the same address space and heap:
the boundary is one of dependency and assurance, not of protection, which is
why A.ADMIN and the SAFETY-MANUAL's assumptions of use carry the rest.
