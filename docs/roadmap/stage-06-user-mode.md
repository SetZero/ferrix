# Stage 6 — User mode ✅

`AddressSpace`, VMOs, the VMA interval tree, demand paging, copy-on-write, the
ELF loader, and the ring-3/EL0 transition. The first user process is a
hand-written static binary that makes one syscall.

**Exit:** a boot test that runs a user binary which writes to fd 1 and exits,
with a page fault serviced along the way.

**Exit criterion met, and in the boot test on all three architectures.** Each
architecture boots a program at user privilege — through the ELF loader, a
startup stack built by `src/lib/kernel/ustack`, and its own way down: `sysretq` to ring 3
on x86-64, `eret` to EL0 on AArch64, `rfeia` to USR on ARMv7-A. The program is a
few dozen bytes of that architecture's machine code calling `write(1, …)` and
`exit_group(42)`, and the boot log carries both:

    hello from EL0
    usermode a program ran in user mode and exited with 42

The page fault is asserted rather than assumed: the check counts resolved faults
either side of the program and fails if none was serviced. Each architecture
services exactly one today — the text page, faulted back in after the loader
narrows its permissions — which is incidental rather than designed, and the
check says so, because a later change that narrows permissions in place would
remove that fault and the criterion's evidence with it. A program that stores
well below its stack pointer, into a page nothing has touched, is the sturdier
version.

x86-64 is also booted under KVM (`--accel kvm`), where the host processor checks
what `tcg` lets through, and that is how a missing RPL in `STAR` was found.
`SYSRET` forces RPL 3 into the CS it computes but loads SS as written, so a
program ran on an RPL-0 SS until its first exception, and the `iretq` back to
ring 3 refused it with `#GP(0x20)`. The default boot test cannot see that class,
so anything that touches ring 3 wants a KVM boot before it is called done.

**Done:**

* **The objects.** `Vmo` is a sparse page list, committed on first touch, so a
  reservation costs nothing until it is written — which is what makes a large
  `mmap` cheap and is measured rather than asserted: 2048 pages reserved, seven
  committed. `AddressSpace` is the `src/lib/kernel/vma` interval tree used for the first
  time as what it was written for, with a lock of its own rather than a global
  one, so two processes faulting at once contend for nothing. Anonymous memory
  carries an identity, because `MAP_SHARED|MAP_ANONYMOUS`, futexes resolving to
  one wait queue and `/proc/self/maps` all need to name the object behind a
  mapping.
* **Demand paging**, generalised from stage 3's fixed window: the fault handler
  finds the region, asks its object for the page, and installs it with that
  region's permissions.
* **The processor walks it.** `arch::install_user_root` puts a root the kernel
  built into `CR3` or `TTBR0`. The three architectures disagree about what a
  switch even is — one `CR3` write, whose own side effect drops the non-global
  entries, against writing `TTBR0`, clearing `EPD0` to re-enable a translation
  regime, and invalidating by `ASID`. No `ASID`s or `PCID`s are allocated: a
  full invalidation of user entries on switch is the correct baseline, and
  eliding it later makes the switch faster rather than unpicking anything.
* **fork and copy-on-write**, copying no memory at all. Each private object is
  cloned page list and all, with a reference taken on every committed frame, so
  a write on either side copies that page into its own object. A page with one
  holder left is let through uncopied — the refcount is what makes that
  decision, which is also why a second write to an already-copied page
  terminates.
* **Tasks carry address spaces.** `Task` holds an `Option<Arc<AddressSpace>>`
  and `sched::choose_next` swaps roots under the run queue lock, comparing by
  pointer so two threads of one process cost nothing. A kernel thread gets the
  user half switched off rather than left installed.
* **Into user mode.** `arch::enter_user` zeroes every general register and
  drops privilege from the program's own task, and does not return: a program
  leaves user mode through a trap or a system call, and for good through
  `exit_group`, which ends its task. The entry stack is the part that bites, and
  it is the task's own kernel stack on every architecture — `gs:8` and `RSP0` set
  on each switch on x86-64, and on the Arm pair simply the stack pointer at the
  return, since at EL1 `sp` *is* `SP_EL1` and ARMv7-A stores every exception from
  USR on the SVC stack. The first version parked registers where the first
  fault then pushed its frame, which x86-64 shipped and fixed. On the two Arm
  architectures `svc` arrives through the trap vector, so `Trap::SystemCall` asks
  the architecture through `arch::system_call`; x86-64's `SYSCALL` has an entry
  of its own. Interrupts are open in user mode — see stage 7, *Programs are
  scheduled tasks*.
* **The permissions the map states are the ones enforced.** A read of a region
  that permits nothing is refused rather than served a fresh zero page — the
  guard-page case, which nothing notices when it is wrong, because the mapping
  is more permissive than the map rather than less.
* **Frame counts wait for the reaper, as a condition.** Every count in the
  self-check is taken by `sched::wait_until_reaper_quiet`: no task exited and
  not yet dropped by its reaper, nothing on the zombie list, no free in flight.
  The time-based settle before it let a task that had exited, but was not yet
  reaped, move the count ("running tasks in address spaces leaked frames").
  What every window counts is `mm::FrameWindow`'s: free frames and the heap's
  slab pages together. A slab page a size class takes or gives back inside a
  window moves one frame between the two and changes nothing. Before that, the
  first touch of a class failed stage 6's region check about one boot in five
  on a branch that grew the address space, and a slab drained elsewhere failed
  stage 8's path check under a slow boot, each by one frame that was not a
  leak. Large heap allocations still count. A small-object leak does not, by
  construction, so teardown of one is proven with a `Weak` that must fail to
  upgrade. A window that fails prints what it held at open and now, the heap's
  live bytes, and the frames through each route (allocated, freed, released,
  heap taken, heap returned, user and kernel tables), so a failure names its
  way in. A boot check holds a committed page and a large buffer across a
  window, and requires the window to count both.
* **An object knows who maps it, and a shootdown reaches only who may cache
  it.** Every address space that names a VMO is in that object's mapper list.
  So a page the object takes away is taken out of every space that maps it
  before its frame goes back. That covers a decommit, a replace, a move, and
  the copy a write makes of a page `fork` left shared. It happens in three
  phases: out of the page list under the object's lock; translations down in
  each space, with no object lock held, and one shootdown to the union of
  their processors; only then the frame. A shootdown names its pages and goes
  only to the processors in the space's set. A processor more than one
  generation behind, or a request with more pages than its slots, flushes
  everything. An object backing a private region has exactly one mapper,
  checked at every attach and before `mremap` moves its pages (FX-0005), which
  is what lets that move tell nobody else. `mprotect` shoots down what it takes
  down. The check poisons a page two processes on two processors have both
  touched, takes it away, and requires that neither reads the poison from user
  mode. It holds a page for a device across a decommit and a replace. Then it
  has a protect end the child's next write with `SIGSEGV`:

  ```
    rmap     2 frames taken from two processes on processors 0 and 1 and poisoned, never reached from user mode; a held page kept its mappings; scoped shootdowns 3 sent to 3 processors and 5 needing no interrupt, against 2 global; 0 frames leaked
  ```

  On the Arm pair a scoped shootdown needs no interrupt: `tlbi` with the `is`
  suffix reaches every processor. So the line counts none sent there.

**Decisions worth not reversing silently.** No `ASID`s or `PCID`s: a full
invalidation of user entries on switch is the correct baseline, and eliding it
later makes the switch faster rather than unpicking anything. On the Arm pair it
is still an invalidation *by* `ASID` — every space is `ASID` zero — because that
is the only form that drops user entries and keeps the kernel's global ones. A
mapping change is flushed by page, on the processors in the space's set, and a
switch never broadcasts. A frame is released only after every space its object
names has let go of it and the shootdown has returned. Anonymous memory names a
VMO, so shared anonymous mappings, futex keys and `/proc/self/maps` have an
object to name. One lock per address space, never a global one.

**Left for later, none of it on stage 6's path:**

* A check that types at the console on the Arm architectures. Every
  architecture receives now — the Arm UART drivers were write-only, so a
  program reading fd 0 there waited forever, until the PL011 and the STM32
  USART gained receive, by interrupt into a ring, as x86-64's 16550 now does
  too. Since stage 15, `cargo xtask test-jobs` types a session at the console
  on x86-64; on AArch64 and ARMv7-A input is still exercised only by hand.
  The ring itself is checked at boot.

**What the boot test could not see, now seen from user mode.** A
copy-on-write fault replaces a live read-only translation with a writable one,
and leaving the stale entry makes the retrying instruction fault forever — a
hang with no message; `protect` takes translations down and, until the reverse
map's scoped shootdown, left a processor's cached copy more permissive than the
map. No kernel-side check could see either. One writes from the kernel, through
the direct map or with the space installed, and a stale entry is only certain to
matter for a write made from user mode, on the processor that cached it. Stage
7's check now runs three hand-assembled programs per architecture, each the
witness this paragraph used to say nothing ran. A program writes a page, narrows
it to `PROT_READ`, writes again and must die of `SIGSEGV`; it is pinned to
another processor, so no switch between the two writes drops the entry by
accident, and without the shootdown it wrote through and exited with 1 on every
architecture, under `tcg` and under KVM alike; the reverse map's own check walks
the same narrowing from the kernel side, as its fourth step. A forked parent and
child, ordered
by a pipe, each write a page the other still shares copy-on-write, and neither
sees the other's write (61). A child's writes to `MAP_SHARED` anonymous pages,
one of them first touched by the child, reach its parent, and its `MAP_PRIVATE`
write does not (62). The last two have no failure shown without what they check:
a deliberate break in the fault path is caught by stage 6's kernel-side check
before stage 7 runs.
Relatedly: a check that reads or writes user memory through the direct map tests
no permission bit at all. To test the tables, ask `translate_in`, or install the
space and use the address.

---

