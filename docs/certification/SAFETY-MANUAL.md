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
| Size | 49,401 lines of product code, 38,959 of it in `core` |
| Scope | memory protection, scheduling, capability objects, trap and syscall entry, IOMMU, SMP, device enumeration |
| Not in scope | VFS, btrfs, network stack, Linux personality, ring-3 drivers — 44,181 lines of uncertified load |
| Reference configuration | x86-64, AArch64, ARMv7-A; release profile; rustc 1.97.1; zero Cargo features |

The boundary is enforced on every build by `scripts/check-item-boundary.py`, so
what this manual describes and what ships cannot drift apart silently.

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
a failed invariant, an allocation failure (§4, AoU-5), or an unhandled kernel
fault. The report names the condition and carries a catalogued explanation.

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

### AoU-5 — allocation failure is fatal
The element allocates dynamically at 225 sites and has no recoverable
out-of-memory path: a failed allocation reaches the safe state of §3. This is
not a defect that will be fixed in the reference configuration —
`#[alloc_error_handler]`, `Box::try_new` and `Arc::try_new` are unstable in
Rust and the element uses no unstable features. The integrator shall provision
memory so that exhaustion does not occur in normal operation, and shall treat
exhaustion as a transition to the safe state. (Finding F-23.)

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
toolchain, enabling a Cargo feature, or moving a file between rings changes
what is claimed. The boundary gate makes the last of these visible; the other
two are the integrator's to control.

### AoU-9 — no independent assessment has been performed
No accredited laboratory, notified body or independent assessor has examined
this element. Every analysis in `docs/certification` was produced by the same
process that wrote the code. **This is the assumption most likely to be
unacceptable to an integrator**, and it is stated first among the residuals for
that reason. (Finding F-27.)

### AoU-10 — no field history
The element has no operational history. Proven-in-use and prior-use credit
(IEC 61508 route 2s, EN 50716's equivalent) are unavailable. (Finding F-30.)

---

## 5. Element failure analysis

The hazard analysis the element *can* do: not what harm the system causes —
that is AoU-1 — but how the element itself can fail to deliver §2. This is the
safety counterpart to [VULNERABILITY-ANALYSIS.md](VULNERABILITY-ANALYSIS.md),
which asks the same questions with an attacker rather than a fault as the
cause.

| Id | Failure mode | Effect at the element boundary | Detection | Mitigation | Residual |
|---|---|---|---|---|---|
| FM-1 | Separation lost: one partition reaches another's memory | ASR-1 violated silently | boot-time sweep; SMEP/SMAP/PAN fault on the wrong access | per-process tables; hardware backstop | ARMv7-A has no backstop (AoU-6) |
| FM-2 | A mapping becomes writable and executable | ASR-2 violated | every boot sweeps all mappings and fails | enforced at map time | detection is per boot, not continuous |
| FM-3 | A capability is honoured that was never granted | ASR-3 violated | 135 refusal assertions | per-process handle tables, unforgeable | none identified |
| FM-4 | A device writes outside its granted region | ASR-4 violated, arbitrary corruption | IOMMU fault | VT-d / SMMUv3 domains | no IOMMU on ARMv7-A (AoU-6) |
| FM-5 | A frame is reused without being cleared | ASR-5 violated, data disclosure | none at runtime | zeroed on allocation | zeroing is on allocation, not free (V-04) |
| FM-6 | The element continues in a corrupt state | any ASR may be violated silently | invariant checks | safe state on detection | detection is not exhaustive |
| FM-7 | A partition exhausts memory | safe state entered, service lost | allocation failure | job quotas | unquota'd paths exist (V-05, AoU-5) |
| FM-8 | A partition is starved of processor time | ASR-8 violated | none | EEVDF eligibility, EDF admission | no WCET, so no bound is provable (AoU-4) |
| FM-9 | Kernel stack overflow | page fault at the instruction that overflowed | **guard page below every kernel stack**, and a boot check that the guard is unmapped | `vmap` reserves an unmapped page on each side of every allocation; no recursion in the element | the loader-provided boot stack is not guarded (early boot only) |

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
| [VULNERABILITY-ANALYSIS.md](VULNERABILITY-ANALYSIS.md) | AVA_VAN.4, five residual vulnerabilities |
| [VERIFICATION.md](VERIFICATION.md) | what exercises the element; 81.9% statement coverage |
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
