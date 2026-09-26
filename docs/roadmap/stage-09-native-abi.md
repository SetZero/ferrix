# Stage 9 — The native ABI: handles, channels, ports, VMOs ✅

Handle tables, `Channel` with handle passing, `Port` event queues, `Interrupt`
objects, `IoMapping`, and `Job`. The syscalls in the `0x1000` range. This is
what stage 10 is written against.

**Done — the ABI written down, and the table under it.** Host-tested, fuzzed
and under Miri, and reached from the kernel by everything below.

* `libs/proto/native-abi` — the numbers, handle values, rights, signals, error names
  and `repr(C)` layouts. One number table on every architecture, in
  `0x1000..=0x1FFF`, held clear of all three Linux tables by a test rather than
  by a comment. No argument is wider than a register — anything that must be
  64 bits on ARMv7-A goes through a pointer — so no native call exists twice
  the way sixteen Linux calls do there. Failures are `errno`, each native
  failure a distinct one, so a musl program making a native call reads an `errno`
  it can name. Rights live on handles and only shrink, decided in one function.
* `libs/kernel/objects` — the handle table and a channel's message queue. A handle is
  a slot and a generation, and a slot is retired rather than let its
  generation wrap, so a closed handle *never* names anything again: the
  `handle_table` fuzz target checks that after every operation, against a
  model. Batch take and insert are all-or-nothing, and a read that does not
  fit takes nothing, because either failure half-done loses handles.

**Done — the first kernel objects, in the boot test on all three
architectures.**

* A handle table on every `Process`, behind a lock of its own, and the native
  dispatch: `dispatch` branches on the range before any Linux table is asked,
  so `arch::decode_syscall` never sees a native number.
* `handle_close`, `handle_duplicate`, `handle_replace`; `channel_create`,
  `channel_write` and `channel_read` with handle passing; `vmo_create`,
  `vmo_read`, `vmo_write`, `vmo_get_size`. A channel write takes the sender's
  handles out of its table only under the peer's queue lock, after the queue
  has accepted the message, so a refused send moves nothing.
* Objects that contain objects never drop them recursively. A closed endpoint
  hands its queue to `object::dispose`, which drops one level at a time, so a
  program that queues endpoints inside endpoints cannot turn a close into a
  kernel stack overflow. Jobs too, since a review found this was true only of
  channels: a child holds its parent, so a job's drop unwinds its chain of
  parents in a loop, and the check frees a chain of ten thousand.
* The self-check builds two processes and drives the handlers through raw
  registers: a message and a VMO handle cross from one table to the other and
  the handle that arrives reads back what the sender wrote; a read that does
  not fit is refused with the sizes and leaves the message queued; rights only
  shrink; a refused send keeps its handles; a full channel says wait; closing
  a reader frees a VMO still queued in it; a send that would leave two
  channels queued in each other is refused; and the frame count says nothing
  leaked.
* `libs/kernel/objects` now keeps every handle value below 2^31. A new handle comes
  back in the register an `errno` does, and on a 32-bit machine a larger value
  reads as negative.
* **A send that would close a cycle of channels is refused.** Endpoints keep
  each other alive only through their queues, so two endpoints each queued in
  the other would outlive every handle to both. A send carrying an endpoint
  walks from what it carries, through the endpoints queued in each, for the
  end it would land in (`libs/kernel/objects`'s `reaches`), and holds a lock only
  such sends take from the walk to the push, so two sends cannot build the
  loop between them. A walk past 1024 endpoints is refused as too big rather
  than allowed to hold that lock.
* **Signals and waiting.** Every object reports its signals as a level —
  a channel end `READABLE`, `WRITABLE`, `PEER_CLOSED`; a killed job
  `TERMINATED` — and names the queue woken when they may have changed.
  `object_wait_one` looks at the level before it sleeps, so a message that
  arrived before the wait is not lost, and also ends when the waiting process
  is killed rather than sleeping out its deadline. The check wakes a
  two-minute wait with a message written twenty milliseconds later.
* **`Job`.** `job_create` and `job_kill`, with a tree of jobs holding
  processes. A kill walks the tree without recursion, ends every process in
  the job and beneath it through `process::kill`, and marks each job under the
  lock that adding to it takes, so nothing can join a job being killed and
  survive it. The check kills the middle of a three-job tree of programs,
  each blocked in `object_wait_one` on a channel nobody writes to, and
  requires the two beneath to end with 137 and their tasks to stop, the one
  above to keep running, and a program outside every job to finish with its
  own status; then it kills the root. The members were spinning programs at
  first, and on a loaded four-processor ARMv7-A boot a spin of `u32::MAX`
  rounds finished before the kill it was meant to survive — so the check
  failed three boots in four on `main` until they became programs only a kill
  can end.
* **Device handles, `IoMapping` and `Interrupt`**, minted from stage 10's
  device nodes through `Aperture` and `Vector`, types only enumeration can
  construct, so a driver cannot name memory or a line its device does not
  have. `io_mapping_create` refuses a range outside one of the device's
  apertures with `ACCESS_DENIED`, and an aperture that is not whole pages with
  `INVALID_ARGS` rather than rounding it out: QEMU's ARM machines pack
  virtio-mmio transports several to a page, and rounding would hand a driver
  its neighbours' registers. `AddressSpace::map_device` inserts a
  `Backing::Device` region that commits nothing, is shared rather than copied
  across `fork`, and is never executable; its pages arrive on first fault,
  uncached. An `Interrupt` masks its line when it fires and holds `READABLE`
  until `interrupt_ack` unmasks it, through `arch::mask_interrupt`, backed by
  GICv2 on both Arm architectures. The check maps a whole-page aperture and
  requires its first fault to translate to the device's own physical page,
  from a forked child too, refuses one byte past it and a sub-page aperture,
  and on ARMv7-A claims a virtio-mmio vector once, runs the kernel's delivery
  path, waits, acknowledges, and claims the line again after closing. It
  first ran before the device nodes were published, found none, and passed on
  every machine having checked nothing; it now runs after them and fails if a
  machine with an aperture or a vector mapped or held none.

* **Ports.** `port_create`, `port_queue` and `port_wait`, and
  `object_wait_async`, which asks for one packet on a port the next time a
  channel end becomes readable or its peer closes, or a job is killed — at
  once if that is already true. A registration is held by the object it
  watches, under the lock that object takes to change the state it reports,
  so a change cannot land between looking and registering; it fires once and
  is gone. User packets stop at 1024 and are told to wait; a signal packet is
  always queued, since refusing it would lose the event it was waiting for,
  and registrations are capped at 64 per object instead. `WRITABLE` cannot be
  waited for asynchronously: it depends on the peer's queue, whose lock an end
  may not take while holding its own. The queue sits under an interrupt-safe
  lock, so binding an interrupt to a port can queue from a handler. The check
  round-trips a user packet, fires a registration by a message, once and only
  once, fires one at once on a state already true and one on a closing peer,
  fires one on a job kill, refuses a full port, a `WRITABLE` registration, a
  port watched through a port and a registration without `WRITE`, and wakes a
  two-minute `port_wait` by a message a kernel thread writes twenty
  milliseconds later.

* **Interrupts reach ports.** `interrupt_bind` delivers an `Interrupt` to a
  port as packets carrying a key and the time it fired, one each time it goes
  from quiet to pending, so a device that fires twice before its driver
  acknowledges produces one packet. The handler queues it itself, under the
  port's interrupt-safe lock and without allocating, and takes the binding's
  lock before marking the interrupt pending, which serialises it against a
  bind racing on another processor: exactly one packet between them. The
  check, where a machine has a device vector, binds one, refuses a second
  binding, requires two deliveries before acknowledgement to queue one packet
  with its key, kind and time, and one more after acknowledging.
  A line is free the moment its holder's last handle closes, which is what a
  restarted driver needs. It was free only when the last reference to the
  object went, and under load that was later twice over: a delivery preempted
  on another processor still held the object, and a close queued it behind
  another processor's drain of disposed objects, so the check's immediate
  re-claim failed about once in three busy boots (`FX-0901`). A delivery now
  holds only the line's pending mark, binding and waiters, never the claim;
  finding a line and masking it, and giving one up and masking it, are single
  steps under the table's lock, so a stale delivery cannot mask a line its new
  holder unmasked; and a close drops the objects that contain no others where
  it stands. The check re-claims a line with a delivery for its old holder in
  flight, requires that delivery not to reach the new holder, and re-claims it
  again as though another processor were draining.

**The exit criterion is met in the boot test**, on all three architectures.
Two copies of `arch::USER_NATIVE_PROGRAM`, assembled per architecture, run
as user-mode tasks of their own: one creates a VMO, writes a secret into it and
sends a message carrying its handle; the other blocks in `object_wait_one`,
reads the message, reads the secret back through the handle it was sent, and
replies with it; and the first exits 0 only if the reply is the secret. The job
check is the other half: a `job_kill` ends every program in the job and beneath
it, blocked in a native wait or spinning alone on a processor, and none above
it. The exit test found, on four-processor ARMv7-A, that a program blocked in
a system call could resume with another program's user stack pointer; stage
7's `144b0cc` fixed it. The job check found that a kill never reached a
program spinning alone on its processor; `301aec4` fixed that.

**Done — an interrupt wakes its waiter at once.** The handler marks the
interrupt pending and queues its port's packet under interrupt-safe locks,
lets go of them, and wakes whoever waits on the interrupt and on the port.
Since `0cda8de` made a wait queue's lock interrupt-safe, `WaitQueue::wake_all`
may be called from a handler as it is: every lock it takes is taken with
interrupts masked, it allocates nothing, and it leaves the reschedule to the
way out of the interrupt or to an IPI. The device check makes sixteen
deliveries from another thread, eight waited on through the interrupt and
eight through its port, staggered across the five-millisecond recheck period,
and requires a majority of each kind to have ended their wait by waking it:
the wait queue counts a wait whose task a wake took off its list, which a task
the recheck timer woke is still on, however long its processor took to run
it. With the wakes taken out of the delivery, none does. It used to time the
rounds against 2 ms instead, and failed under a busy host or KVM, where a
halted processor can take that long to run again (`FX-0901`); the slowest
time is still printed.

**Done — a port hears that a process has ended.** A handle to a process
names its `Exit`: its status, the signal that ended it, the queue woken when it
ends, and the port registrations waiting for it. It never names the process,
so a handle kept past the end does not keep the address space or anything else
the process owned. `object_wait_async` on the handle queues a `TERMINATED`
packet once the process has ended and closed its handles and descriptors, at
once for one that already has, and the handle's own `TERMINATED` signal asserts
at the same point. So `devmgr` hears of a driver's death only after the
driver's pins have been given back or kept. The object check watches a process through
a port and directly, sees a channel the process held close before the packet
arrives, and sees the process freed while the handle is still open. A program
that dies of its own fault is heard the same way: the kill a user-mode fault
forces ran inside the trap with interrupts masked, where ending a process may
not run, until the reverse map's first boot check to end a child by `SIGSEGV`
stopped the kernel on it. The trap now opens interrupts while it forces the
signal, as the way back to user mode already did, and the object check runs a
program that writes through a null pointer on every architecture and requires
`128 + SIGSEGV`, the `TERMINATED` packet, and the process freed.

**Done — `vmo_map`, and what a DMA pin needs from a VMO.** A VMO maps into
the calling process a whole number of pages at a time, and shared: a write
through the mapping is a write to the VMO, a fork reaches the same pages
rather than copies, and the mapping keeps the object alive once the handle it
was made with is closed. A mapping always reads, writes only when asked and
when the handle carries `WRITE`, and never executes. The region is attached to
the object on the reverse map before it is recorded, so a VMO that takes a page
away forgets it in the mapping space before its frame goes back. `mremap` and
`mprotect` refuse a region `vmo_map` made -- the first would grow the VMO
through the Linux path, and the second, answering `EACCES`, would give a
mapping a right its handle did not carry -- and an object mapped twice counts
once in the resident set. The object check maps a VMO, reads and writes it
both ways, forks the space, closes the handle, and is refused twelve ways.

`Vmo::hold` keeps a page on the frame a device was given for as long as a pin
holds it. Decommitting skips the page, a copy-on-write replace is refused, and
a fork copies it instead of sharing it; a stage 6 self-check walks a held page
through each of those and through nested holds.

**Done — a process makes and starts another.** `process_create(job, image,
name, name_len)` reads an ELF image out of a VMO into kernel memory and loads
it with nothing on its stack. The child's code is therefore ordinary memory of
its own, and no VMO is ever mapped executable. The child is placed, not yet
running, in the job, and the caller gets a handle to it.

`process_start(process, bootstrap)` works in four steps, so that a race
between two starts, or a start and a kill, is harmless, and a start that fails
has nothing to undo:
1. It claims the start, so a second start is refused before it has moved
   anything.
2. It makes the child's task without running it -- the processor it goes to,
   its stack, its thread counted live -- so everything that can fail fails with
   nothing moved.
3. It moves the bootstrap handle into the child's table under both tables'
   locks. A refusal here drops the prepared task, which frees its stack and
   gives the start back.
4. It enters the program with that handle's value in its first argument
   register, which cannot fail.

A process that has already started or ended is `BAD_STATE`, and the bootstrap
then stays with the caller.

The handle holds the process weakly. The one strong reference to a child
nobody started lives in what its handles share, and the start hands it over to
the task. Closing the last handle to an unstarted child kills it, so it ends
with a status and its watchers hear. That drop runs only inside
`object::dispose`, whose callers are all tasks with interrupts on, and it
asserts so.

The object check drives both calls through the native dispatch from a check
process. The child is a real program on every architecture that exits with its
first argument register, so its status is the bootstrap's value as the child
saw it. The check also covers the refusals; a child killed before its start; an
unstarted child's handle dropped with the channel message carrying it; and one
whose only handle is closed, which must be heard and freed.

**Left for later stages**, none of it on the exit criterion's path:

* **No native handle to a Linux mapping's object or a file's VMO until
  `mremap` makes other mappers forget moved pages under its own lock.** The
  VMO reverse map (stage 6, *An object knows who maps it*) tells a moved
  object's other mappers to forget its pages only after `mremap`'s lock is
  released. That is safe only while a private region's object has exactly one
  mapper, which an assertion at `Vmo::attach` and in `mremap`'s adopt path
  checks. `vmo_map` cannot break it: a VMO handle comes only from
  `vmo_create`, and native regions refuse `mremap`.
* **An `EXECUTE` right on a VMO handle**, with the native loader that is its
  first consumer. `vmo_map` refuses an executable mapping for now, by the
  product owner's decision of 2026-09-13; `process_create` reads a program's
  image out of its VMO into anonymous memory, so nothing needs one yet.
* Sub-page apertures, which need each access trapped. (An `Interrupt` on
  x86-64, masked in the device's own MSI-X table, came with stage 10's PCI
  vectors.)
* The calls that act on a process beyond making and starting it.
  `0x1032..=0x1037` is held for them; init's `process_give`,
  `process_bootstrap` and `process_status` took `0x1032..=0x1034` on
  2026-09-26 (`docs/INIT.md` §16).

**Exit:** two user processes exchange messages and a handle over a channel, and
a `Job` kill takes down a process tree.

---

