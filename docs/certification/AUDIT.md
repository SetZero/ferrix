# Audit: a minimal record of security-relevant events

Design only, for finding F-21b: *"there is no FAU family at all ... an
evaluator would press on whether a TOE that cannot record a
security-relevant event can claim EAL5."* Nothing here is built. It says what
the TOE would record, where, who reads it, what it costs, and how the build
would prove it, so the claim can be sized and argued before any code.

The TOE is an isolation kernel, and identity is the personality's
(SECURITY-TARGET §9.1). This keeps that split. The item records what *it*
decides: a capability refused, authority handed over, a process ended from
outside, the configuration it booted in. Who a subject is, in uid terms, is
added by the personality through a hook, as the item already takes a process
loader and a file reader from it.

## 1. What is recorded (FAU_GEN.1)

Each record is one event the TSF decided. The events are the decisions
already made at one choke point each, so recording one adds a call and
changes nothing about the decision.

| Class | Event | Where it is decided |
|---|---|---|
| `REFUSED` | A handle's rights did not cover a call: `ACCESS_DENIED` from the rights check | `syscall/native.rs`, where every native call asks for rights (`job_in`, `device_in`, the table's `get_with`) |
| `REFUSED` | A rights request that would widen a handle | `object` table, `Requested::resolve` returning `None` |
| `REFUSED` | A limit refused a charge: tasks, memory, objects at a job's limit | `object/quota.rs`, the one refusal path per resource |
| `GRANTED` | A job handle with `MANAGE` or `SET_LIMIT` given for a cgroup | `fs/cgroupfs.rs` `job_for_cgroup` (a load-ring caller; recorded through the item's call) |
| `GRANTED` | A native process made, with its creator's ids (P0) | `syscall/native.rs` `process_create` |
| `GRANTED` | `devmgr` started through the starter; the starter given to pid 1 | `devmgr.rs` `devmgr_start`, `init.rs` `next_bootstrap` |
| `GRANTED` | A device's control channel made for a driver (block, net, display, render, input, sound, log) | each core's `*_control_create`, through `native::serve` |
| `ENDED` | A process or job ended from outside: a job's kill, the scoped OOM kill, `cgroup.kill` | `object/job.rs` `kill`, `kill_members`; `object/oom.rs` |
| `DEVICE` | A device quiesced; a DMA fault the IOMMU reported for a device | `native::quiesce`; `iommu` fault handlers |
| `CHANGED` | TSF data changed by a call that succeeded: a job's limit set (`job_set_limit`), a cgroup's limit file written | `syscall/native.rs` `job_set_limit`; cgroupfs limit writes |
| `SYSTEM` | The audit function's own start-up, with the ring sizes; and its shutdown, the last record before a power action | `audit::start` at bring-up; `power` |
| `SYSTEM` | The boot's configuration: `--mitigations`, the KASLR state (randomised or not, and why), `ferrix.devmgr=kernel\|init`, and **`ferrix.checks=skip`** -- a boot that skipped its self-tests leaves a record of it -- and each self-check stage's verdict | bring-up in `main.rs` |
| `SYSTEM` | The root switch and pid 1's re-root; a power action | `fs::root_disk`; `power` |

Not recorded: successful calls in general (a mapping, a message), because
their volume would make the record useless and they are no decision against
anyone. What is recorded of successes is authority handed over (`GRANTED`)
and TSF data changed (`CHANGED`), which FAU_GEN.1 expects whatever else is
left out.

**The safe state.** A panic halts the TSF (O.FAILSAFE) and cannot write a
record anyone reads. Its FX code and explanation survive in the console's
kernel log ring and on the serial line, and on the Pixel 7 in the `ramoops`
record read back after the watchdog's reset. That is the audit trail for
that one event, and the design says so rather than claiming a record. Linux-personality decisions -- `setuid`, `EPERM` from credentials,
`execve` of a set-id file -- are outside the TOE; the personality may submit
them through the same interface (§4), marked as its own.

## 2. What a record holds (FAU_GEN.1, FAU_GEN.2, FPT_STM.1)

A fixed 64-byte record, so the store never allocates:

| Field | Bytes | |
|---|---|---|
| sequence number | 8 | per boot, gapless: a missing number is a lost record |
| time | 8 | monotonic nanoseconds since boot (`timer::now_nanos`), the TSF's own clock; wall time is the reader's to add |
| class and event | 4 | §1 |
| outcome and status | 4 | the native status or errno answered |
| subject | 12 | pid and job id, which the TSF attests; and, in a separate field flagged *personality-supplied*, the uid the personality's hook gives (`u32::MAX` when it gives none) |
| object | 16 | the object's kind and identity: a handle's object kind and job id, a device's PCI location, a resource |
| detail | 12 | the rights asked and held, or the limit and the use |

FAU_GEN.2 (associating an event with a user) is claimed, refined, for the
TOE's own subjects: the process and its job, which the TSF attests. The uid
is personality-supplied data carried beside them and marked as such, under
the OE.AUTH and A.AUTH pair `docs/AUTH.md` added; no record presents it as
the TSF's identity of the subject.

## 3. Where it is kept (FAU_STG)

In the TSF, two rings, allocated at bring-up before the boot marker as the
kernel log's buffer is, so nothing on a recording path allocates:

* **The high-value ring**, 512 records (32 KiB): `GRANTED`, `CHANGED`,
  `ENDED`, `DEVICE` and `SYSTEM`. They are rare, and `REFUSED` traffic can
  never evict one.
* **The refusal ring**, 4096 records (256 KiB): `REFUSED`, with per-job
  fairness. Past 64 records from one job in a second, that job's further
  refusals fold into one *suppressed n* record for it, written when its
  second ends. A job that provokes refusals by the thousand -- varying the
  object, which defeats a fold by identical event -- flushes at most its own
  older refusals, never another job's, and never a grant.

Each ring's lock is an `IrqSpinLock` (`crate::sync`): interrupts masked while
it is held, for a 64-byte copy and two counters. An IOMMU fault is recorded
from its interrupt handler, and an OOM kill can be decided with the
allocator's locks held, so a lock that left interrupts on could be taken by
a handler on the processor already holding it. Nothing under it blocks or
allocates, so the F-23 and FX-0503 rules hold on every path that records.

When a ring is full its oldest record is overwritten and its lost counter
counts it; the reader sees the gap in sequence numbers and the count. This
is FAU_STG.1 (protected storage: nothing but the TSF writes it). It is **not**
FAU_STG.3 or FAU_STG.4, and deliberately. Refusing new events when full
(FAU_STG.4's drop-newest) lets an attacker fill the store first and then act
unrecorded; stopping the TSF when full would make audit a denial-of-service
lever (T.EXHAUST). Overwrite keeps the newest, and the two rings and the
fairness rule keep a refusal flood from reaching what matters.

Persistence is outside the TOE: a reader writes records to disk.

## 4. Who reads it (FAU_SAR.1, FAU_SAR.2)

A capability, as the log core's reader and L12's starter are:

* The kernel gives pid 1 one `Object::Audit` handle on its K2 channel (`FXAU`,
  after the hello), with `READ` and nothing else -- not duplicable, not sent.
  Only the holder can read, which is FAU_SAR.2.
* `audit_read(audit, ring, buffer, from_sequence)` copies whole records of
  one ring from the given sequence number, and answers the next number, that
  ring's lost count since the reader's last call, and the boot's audit id: a
  random 128-bit number drawn at start-up and recorded in the start-up
  record, so that records of different boots can never be spliced into one
  sequence on disk. It never blocks; `object_wait_async` on the handle
  signals `READABLE` when records are waiting.
* Init runs a reader as a unit (`audit.service`), handing it the handle over
  its bootstrap channel, which writes the records to
  `/var/log/audit/<boot>.bin` on the root volume and to its journal. It is a
  uid of its own, not root, once AUTH phase 2 has uids for services.
* A boot without init has no reader, and the ring keeps the last 4096 events,
  readable from a crash dump.

The personality's hook to submit its own events (`audit::submit`, load-ring
callable, marked `PERSONALITY`) is the one write path from outside the TSF,
and a record it submits can never claim a TSF class.

## 5. What it costs

* Memory: 288 KiB fixed (the two rings), all three architectures.
* Time: on the recorded paths only, which are refusals and grants, not the
  hot path of a call that succeeds. One uncontended spinlock and a 64-byte
  store, estimated at 50 to 150 ns under KVM, and measured on the boot's own
  check before the claim is made (MEMORY-AND-TIMING.md gains a row).
* Code: an `audit` module in the item, about 400 lines; one `record` call at
  each of about 25 sites; the `Audit` object kind and `audit_read` in the
  native ABI; init's reader, about 200 lines.
* Points: about 15 -- the two rings, the fairness rule and the records (4),
  the ~30 call sites with their negative controls (6), the capability and
  `audit_read` (2), init's reader and the disk file (2), the ST and the
  documents (1).

## 6. How the build would prove it

A boot check, `audit_check`, provokes one event of every class from a check
process -- an `ACCESS_DENIED`, a widened rights request, a limit, a grant, a
kill, a quiesce -- and reads each back through an `Audit` handle with the
right subject, object, outcome and a gapless sequence. It fills the ring and
requires the lost counter and the gap, and reads with a handle lacking `READ`
to be refused. It floods the refusal ring from one check job and requires
another job's refusal and every grant made before the flood to be still
readable, and the flooding job's *suppressed n* record. It requires the
start-up record, the configuration records, and the same audit id in every
answer.

The record a `ferrix.checks=skip` boot writes cannot be checked from inside
that boot, which runs no in-kernel check. It is verified from outside: an
xtask gate boots with `ferrix.checks=skip` and reads the record through
`audit_read` -- from init's `audit.service` on the console, or a small native
program the image carries -- and asserts it is there, naming the option. Negative controls: drop the `record` call at one site, and the
check names the missing event; widen `Object::Audit` to `TRANSFER`, and the
check that it cannot be written into a channel fails. `test-init` requires
`audit.service` to have written the boot's refusals to the volume.

## 7. What the ST would then claim, and still not

Claimed: FAU_GEN.1 (the events of §1, at the TSF's decisions, with audit
start-up and shutdown), FAU_GEN.2 refined to the TSF's own subjects (process
and job; the uid carried beside them as personality data), FAU_SAR.1 and
FAU_SAR.2 (the capability), FAU_STG.1 (only the TSF writes the rings), and
FPT_STM.1 for the timestamps. Not claimed: FAU_STG.3 and FAU_STG.4 (overflow
overwrites rather than stopping, by design), FAU_SEL (no selection: every
§1 event is always recorded), FAU_SAA (no analysis in the TOE), and the
integrity of records once written to disk, which is the environment's
(OE.AUDIT_STORE, a new objective for the environment).

F-21b would then read: audit claimed for the TSF's own decisions; FIA still
the personality's, which is the isolation kernel's position and the reason no
OS protection profile is claimed.
