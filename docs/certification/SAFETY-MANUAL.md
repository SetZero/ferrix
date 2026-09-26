# Safety manual

The certified item in [ITEM.md](ITEM.md) is developed as a **safety element out
of context**: a component with no application of its own, certified against
*assumed* safety requirements and shipped with the conditions an integrator
must discharge to use the result.

This is how a general-purpose kernel is certified at all. QNX, PikeOS and
VxWorks 653 all ship a manual of this shape, because none of them knows what
their customers are building either. Every standard has a name for the route:

| Standard | Name |
|---|---|
| ISO 26262 | Safety Element out of Context (Part 10) |
| EN 50716 / EN 50128 | Generic software, with exported application conditions |
| DO-178C | Reusable Software Component (§12.1.4, FAA AC 20-148) |
| IEC 61508 | Certified element with a safety manual |
| IEC 62304 | Supplied component evidence; the device manufacturer still owns §7 |

**The obligation is not removed, it is relocated.** A system hazard analysis
still has to happen. It happens at the integrator, and this item's certificate
— were one ever issued — would be valid only inside the assumptions in §4.

---

## 1. What the element is

| | |
|---|---|
| Element | Ferrix certified item, `core` + `item` rings |
| Size | 49,431 lines of product code, 38,989 of it in `core` |
| Scope | memory protection, scheduling, capability objects, trap and syscall entry, IOMMU, SMP, device enumeration |
| Not in scope | VFS, btrfs, network stack, Linux personality, ring-3 drivers — 44,215 lines of uncertified load |
| Reference configuration | x86-64, AArch64, ARMv7-A; release profile; rustc 1.97.1; zero Cargo features; built `--mitigations on`, the default |

The boundary is enforced on every build by `scripts/check-item-boundary.py`, so
what this manual describes and what ships cannot drift apart silently. The
manual's own claims are held the same way: every requirement and failure mode
below names its evidence in `scripts/safety-requirements.json`, and
`scripts/check-safety-requirements.py` fails the build when a citation stops
resolving or when the manual and the register disagree about which ids exist.
It caught a wrong citation the first time it ran.

---

## 2. Assumed safety requirements

What the element assumes a safety application will need of an isolation kernel.
These are *assumed*, not derived — that is what out-of-context means. An
integrator whose system needs something else must say so (§4, AoU-1).

| Id | Assumed requirement | Implemented by | Evidence |
|---|---|---|---|
| ASR-1 | A partition shall not read or write memory belonging to another partition or to the element. | per-process page tables, `user/space.rs` | `spaces` check on every boot; SMEP+SMAP (x86-64), PAN (AArch64) |
| ASR-2 | No mapping shall be simultaneously writable and executable. | map-time enforcement | every boot sweeps all mappings: *1,874 swept, 387 executable, none writable* |
| ASR-3 | A partition shall reach a resource only through a capability it holds. | `object/`, handle tables | `object/check.rs`, 135 refusal assertions |
| ASR-4 | A device shall not access memory outside the region its driver was granted. | VT-d, SMMUv3 | *9 PCI functions behind one unit, 0 bypassing, 0 unresolved* |
| ASR-5 | Memory released by one partition shall not be readable by the next. | `mm::zero_frame` on allocation | called at three sites in `user/vmo.rs` |
| ASR-6 | On detecting an inconsistent internal state, the element shall enter its safe state rather than continue. | `panic.rs` with a catalogued explanation | `check-panic-audit.py`; the safe state is defined in §3 |
| ASR-7 | Data supplied by a partition shall be validated before use. | `syscall/uaccess.rs` | 427 refusal assertions in `syscall/check.rs` |
| ASR-8 | Admission of a real-time workload shall be refused when the set is unschedulable. | EDF with CBS admission | **partially met** — see AoU-4 |

ASR-1 to ASR-7 are met on the reference configuration, with the architecture
exceptions in §4. **ASR-8 is partially met** and is the one an integrator must
read most carefully.

---

## 3. Safe state

**The element's safe state is a halted processor with a diagnostic on the
serial console and, where firmware left a framebuffer, on the screen.**

It is entered on any detected internal inconsistency: a failed boot self-check,
a failed invariant, an allocation failure at bring-up or in the uncertified
load (§4, AoU-5), or an unhandled kernel fault. The report names the condition
and carries a catalogued explanation.

**AoU-2 below is the obligation this creates.** A halt is only a *safe* state in
a system where stopping is safe. In a system where the controlled process must
keep being controlled — a moving train, an infusion in progress — the
integrator must provide an external mechanism: a watchdog, a hardware
interlock, a redundant channel. The element does not fail over, does not
restart itself, and does not degrade gracefully.

---

## 4. Assumptions of use

Every one of these is an obligation on the integrator. A certificate over this
element would be void outside them.

### AoU-1 — the system hazard analysis is the integrator's
The element assumes the requirements in §2. The integrator shall perform the
system-level hazard analysis (ISO 14971, EN 50126, ARP4761 as applicable),
apportion safety requirements to software, and **verify that the apportioned
requirements are a subset of §2**. Where they are not, the element does not
cover the difference.

### AoU-2 — halt must be safe, or be made safe
See §3. The integrator shall ensure that a halted processor is a safe outcome
in the system, or provide external means to reach a safe outcome from it.

### AoU-3 — the uncertified load is untrusted
The VFS, btrfs, the network stack, the Linux personality and all ring-3 drivers
are outside the element and carry no assurance claim. The integrator shall not
place a safety function in them, and shall treat their output as untrusted
input. The element's own enforcement is what bounds their failure.

### AoU-4 — no worst-case execution time is provided
The element provides admission control, partitioned scheduling, bounded
critical sections on the real-time path and interrupts that cannot steal
unaccounted time. It does **not** provide a certified WCET, and
`docs/ARCHITECTURE.md` §5 is explicit that no OS which also hosts a compiler
can. An integrator whose safety requirement depends on a proven response time
shall establish it by measurement on their own configuration and workload, and
shall treat ASR-8 as unmet until they have. (Finding F-24.)

### AoU-5 — the heap is not bounded per partition, and exhaustion outside the element is fatal
The element allocates dynamically, and reports allocation failure at every
site in its own source. A native call answers `NO_MEMORY`, a Linux call
`ENOMEM` (`EAGAIN` from `madvise`), and the element carries on.
`scripts/check-fallible-alloc.py` fails the build on an allocation that does
not report failure, and every boot proves the handling by failing allocations
under the native calls ([MEMORY-AND-TIMING.md](MEMORY-AND-TIMING.md) §1). Two
cases still reach the safe state of §3. An allocation failure during bring-up,
before the first program runs, stops the element with FX-0007. One in the
uncertified load after boot, whose allocations are not fallible and which
shares the element's heap, stops it with FX-0008 -- including the load code
two native calls run, `process_create` and `process_start`, to make a
process and its first thread (MEMORY-AND-TIMING.md §1.3 lists it).

Since 2026-09-26 the element bounds what a partition's programs hold, when
the partition is a job with limits set: the frames of their memory and their
page tables, the native objects they make and their tasks, each refused at
its limit while the other partitions go on (F-35, `FRU_RSA.1`). It does not
bound its own working set, nor the heap the Linux personality allocates for a
partition's programs (F-37, V-05). The integrator shall put each partition in
a job of its own, with memory, object and task limits whose sum the machine
can hold, shall provision the heap so that exhaustion does not occur in
normal operation, and shall treat FX-0007 and FX-0008 as transitions to the
safe state. An application that runs on the element shall handle `NO_MEMORY`
and `ENOMEM` as an outcome of any call that allocates, and of a job at its
limit, not as an impossibility. (Finding F-23, closed for the element's own
allocations; F-35; F-37; V-05.)

### AoU-6 — ARMv7-A carries reduced claims
On ARMv7-A the element provides **no ASR-4** (the reference board has no IOMMU)
and no hardware backstop for ASR-1 (PAN is an ARMv8.1 feature; the Cortex-A7 is
ARMv7-A, so the software bound check in `uaccess` is the only barrier). An
integrator requiring either on that architecture shall not use it.

### AoU-7 — no audit and no authentication
The element provides no security event log and no identification or
authentication; POSIX credentials live in the uncertified load. An integrator
needing either shall provide it above the element. (Finding F-21b.)

### AoU-8 — the configuration is the one in §1
The claims hold for the reference configuration and no other. Changing the
toolchain, enabling a Cargo feature, building with `--mitigations off`, or
moving a file between rings changes what is claimed. The boundary gate makes
the last of these visible, and the boot log says which build it is — *"speculation
defences off: built with --mitigations off"* — so the third is visible on the
running system; the first two are the integrator's to control.

### AoU-9 — no independent assessment has been performed
No accredited laboratory, notified body or independent assessor has examined
this element. Every analysis in `docs/certification` was produced by the same
process that wrote the code. **This is the assumption most likely to be
unacceptable to an integrator**, and it is stated first among the residuals for
that reason. (Finding F-27.)

### AoU-10 — no field history
The element has no operational history. Proven-in-use and prior-use credit
(IEC 61508 route 2s, EN 50716's equivalent) are unavailable. (Finding F-30.)

### AoU-11 — the processor is one the side-channel defences cover
ASR-1's separation holds against speculative reads only on a processor that
offers what [SPECULATION.md](SPECULATION.md) builds on, and the element cannot
supply what the processor lacks. The integrator shall run the element, built
`--mitigations on`, only on processors whose boot log lines
`cpu      speculation exposure:` (on AArch64, one for each kind of core the
machine has) name nothing **NOT covered** and nothing **EXPOSED** — which on x86-64 means an IBRS form (enhanced, automatic, or
always-on), `IBPB`, `SSBD` unless the part says `SSB_NO`, and a part not
affected by Meltdown; on AArch64, for every core, `CSV2`, a core Arm lists
as unaffected by Spectre v2 (Cortex-A35, A53, A55), or firmware implementing
SMCCC `ARCH_WORKAROUND_1`, `SSBS` or `ARCH_WORKAROUND_2`, and a core Arm lists as
unaffected by Meltdown or reporting `CSV3`; on ARMv7-A, a core Arm lists as
unaffected, or firmware that set `ACTLR.IBE` on one that is not. On an
MDS-affected x86-64 part the integrator shall disable SMT. Partitions that
must not learn each other's cache access patterns shall not share a cache: no
cache is partitioned (V-06). QEMU's TCG, on which most gates run, offers no
speculation controls and executes no speculation, so its log lines are not a
counter-example; the gate under KVM is where the controls are exercised.
For KASLR the integrator shall provide firmware offering `EFI_RNG_PROTOCOL`,
or on x86-64 a processor with `RDRAND`. The boot line `kaslr    image, direct
map and arena moved` must name one of the two, not the cycle counter. On
x86-64 the processor shall offer UMIP (`UMIP on`), without which `SIDT` reads
the image's slide. The integrator shall not rely on KASLR for separation: without
KPTI a program with a timer can locate the kernel, and no ASR rests on it; and
shall treat a program that can read the framebuffer the boot console drew on
as able to read the slide the log printed there. (Finding F-31.)

---

## 5. Element failure analysis

The hazard analysis the element *can* do: not what harm the system causes —
that is AoU-1 — but how the element itself can fail to deliver §2. This is the
safety counterpart to [VULNERABILITY-ANALYSIS.md](VULNERABILITY-ANALYSIS.md),
which asks the same questions with an attacker rather than a fault as the
cause.

| Id | Failure mode | Effect at the element boundary | Detection | Mitigation | Residual |
|---|---|---|---|---|---|
| FM-1 | Separation lost: one partition reaches another's memory | ASR-1 violated silently | boot-time sweep; SMEP/SMAP/PAN fault on the wrong access; stage 4 checks that a page table an unmap empties is freed only by its shootdown (F-36) | per-process tables; hardware backstop; nothing a translation reached -- frame or table -- is given back until every processor that may cache it has flushed | ARMv7-A has no backstop (AoU-6); a walk through a freed table cannot be provoked under emulation, so the table order rests on the check and the rule |
| FM-2 | A mapping becomes writable and executable | ASR-2 violated | every boot sweeps all mappings and fails | enforced at map time | detection is per boot, not continuous |
| FM-3 | A capability is honoured that was never granted | ASR-3 violated | 135 refusal assertions | per-process handle tables, unforgeable | none identified |
| FM-4 | A device writes outside its granted region | ASR-4 violated, arbitrary corruption | IOMMU fault | VT-d / SMMUv3 domains; a domain's emptied tables are freed only after the unit's invalidation completes (F-36) | no IOMMU on ARMv7-A (AoU-6) |
| FM-5 | A frame is reused without being cleared | ASR-5 violated, data disclosure | none at runtime | zeroed on allocation | zeroing is on allocation, not free (V-04) |
| FM-6 | The element continues in a corrupt state | any ASR may be violated silently | invariant checks | safe state on detection | detection is not exhaustive |
| FM-7 | A partition exhausts memory | calls that allocate fail with `NO_MEMORY` or `ENOMEM` for every partition; the safe state if the load's allocation fails | allocation failure, reported at every site in the element (gate and boot check) | a job's memory, object and task limits, refused at the limit while other jobs go on (`quota` boot line); job limits on depth and descendants; capped queues | the Linux personality's heap is not charged to a job (F-37, V-05); the load's allocations are fatal (AoU-5) |
| FM-8 | A partition is starved of processor time | ASR-8 violated | none at runtime; the `quota` boot line checks one job's share against another's | EEVDF eligibility, EDF admission; a job's share of a contended processor is its weight's, whatever its task count | no WCET, so no bound is provable (AoU-4) |
| FM-9 | Kernel stack overflow | page fault at the instruction that overflowed | **guard page below every kernel stack**, and a boot check that the guard is unmapped | `vmap` reserves an unmapped page on each side of every allocation; no recursion in the element | the loader-provided boot stack is not guarded (early boot only) |
| FM-10 | A processor stops answering a TLB shootdown or grace period (x86-64) | none while the wait lasts: nothing is freed and no narrowed permission relied on until every processor answers; then the safe state (FX-0001, FX-0002, FX-0003) | `smp::wait_for` and `take_turn`: a wall-clock floor (1 s, 5 s) **and** a count of the waiter's own polls, which stretches with the emulator's slowness | the count is in guest units, so a slow machine is not called stuck; a stuck processor answers no count and is still found (negative control: 1.8 s under KVM, 5.1 s under `tcg`, 32 s under the coverage plugin) | a host that stops running one virtual processor and keeps running the waiter can still end the wait early: availability lost, never integrity |

**FM-9 was recorded as the worst entry in this table and that was wrong.**
Every kernel stack is guard-paged at both ends: `crate::vmap` reserves an
unmapped page on each side of every allocation, *inside* the range the arena
hands out so the guard cannot be handed to anybody else, and the module's own
documentation says the guard below a stack is why it exists. `check_stacks`
asserts it on every boot — it writes the first and last usable words, then
requires that `stack.base - PAGE_SIZE` and `stack.top` translate to nothing,
failing with *"a kernel stack has no guard page below it, so an overflow would
be silent"*. So an overflow is a page fault at the instruction that caused it,
not silent corruption, and that is verified rather than intended.

The narrow residual: the **boot stack the loader allocates**
(`MemKind::BootStack`) is an ordinary pool allocation with no guard. It carries
early boot, before the arena the guarded stacks come from exists. An overflow
there would be silent, and the window is bounded by bring-up rather than open
for the life of the system.

With FM-9 corrected, the least-defended modes are **FM-5** (a frame is zeroed
on allocation rather than on free, so contents persist until reuse) and
**FM-8** (no bound on scheduling latency is provable, because no WCET is
claimed — AoU-4).

---

## 6. What an integrator receives

| Artifact | Purpose |
|---|---|
| This manual | assumed requirements, safe state, assumptions of use, failure analysis |
| [ITEM.md](ITEM.md) | exactly what is and is not in the element |
| [SECURITY-TARGET.md](SECURITY-TARGET.md) | the security counterpart, CC EAL5+ |
| [VULNERABILITY-ANALYSIS.md](VULNERABILITY-ANALYSIS.md) | AVA_VAN.4, six residual vulnerabilities |
| [SPECULATION.md](SPECULATION.md) | the side-channel defences behind AoU-11, per architecture, and what they cost |
| [VERIFICATION.md](VERIFICATION.md) | what exercises the element; 74.7% statement coverage on x86-64, 73.7% AArch64, 70.9% ARMv7-A |
| [MEMORY-AND-TIMING.md](MEMORY-AND-TIMING.md) | the determinism arguments behind AoU-4 and AoU-5 |
| [TOOLS.md](TOOLS.md) | tool classification and operational requirements |
| [SOUP.md](SOUP.md) | generated; the element contains none |
| [FINDINGS.md](FINDINGS.md) | every open finding, including the ones this manual exports as assumptions |

**The findings register is shipped deliberately.** An integrator who is told
only what works cannot judge the element. Several assumptions above exist
precisely because a finding is open, and each says which.

---

## 7. What this manual does not make true

It does not make the element certified. Nobody has assessed it (AoU-9).

It does not let an integrator skip their hazard analysis (AoU-1) — it tells
them what to check theirs against.

And it does not raise any of the four target ratings by itself. What it changes
is that [FINDINGS.md](FINDINGS.md) F-20 and F-22 are no longer blocked on a
device that does not exist: the element-level analysis is §5, the element-level
safety argument is §2 to §4, and what remains is the integrator's half, which
is exported rather than missing.
