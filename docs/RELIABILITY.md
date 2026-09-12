# Reliability

The quality gates in this tree are ported from the Starling workspace. This
document records what each one is for, and what changed in the port — because a
kernel can hold some of those gates more strictly than a userspace server, and
one of them not at all.

## The one gate that could not be inherited

Starling sets `unsafe_code = "deny"` workspace-wide and means it. A kernel
cannot: writing a page table entry, storing to an MMIO register and moving a CPU
system register *are* the program. Denying unsafe would be denying the project.

So the burden moved from the crate to the site. Unsafe is not forbidden here; it
is made expensive:

| Mechanism | What it requires |
|---|---|
| `clippy::undocumented_unsafe_blocks` (deny) | A `// SAFETY:` comment on every `unsafe` block |
| `clippy::missing_safety_doc` (deny) | A `# Safety` section on every `unsafe fn`, stating the caller's contract |
| `clippy::multiple_unsafe_ops_per_block` (deny) | One unsafe operation per block, so a `SAFETY:` comment makes one claim rather than covering a paragraph of them |
| `unsafe_op_in_unsafe_fn` (deny, and the edition-2024 default) | An `unsafe fn` body is not an unchecked region |
| `scripts/check-unsafe-audit.py` | The same rules again, in CI, so a clippy release that softens a nursery lint cannot silently retire them |

The script also prints the unsafe-block count per crate on every run. That
number is not a gate — it is meant to be visible in a diff, so that unsafe
growing is something somebody noticed rather than something that happened.

## Panics

A panic in a userspace server is a 500 and a restart. In a kernel it is the
machine: there is no supervisor above us, and with `panic = "abort"` there is no
unwinding either. So the panicking constructs are denied in production code —
`panic!`, `unreachable!`, `unwrap`, `expect`, `indexing_slicing`,
`string_slice`, `panic_in_result_fn` — and the exemptions have to be argued:

```rust
#[expect(clippy::indexing_slicing, reason = "AUDIT: len checked at :212")]
```

Two rules, enforced by `scripts/check-panic-audit.py`:

1. The reason begins `AUDIT:`, so it reads as an argument in the diff that adds
   it rather than as a way past the lint.
2. Exemptions use `#[expect]`, never `#[allow]`. `expect` fails the build once
   the lint stops firing, so a site refactored into safety loses its exemption
   instead of accumulating a stale one.

`.clippy.toml` exempts test bodies, where a failed assertion *should* panic with
a clear message.

### `panic!` in the kernel

The kernel crate is the one exemption from the `panic` lint, and it takes it
once, at the crate root. A self-check that fails in `kmain`, or a processor
that never answers an interrupt, has nowhere to return an error to: the only
thing left to do is say what happened and stop. Before this exemption every such
site printed its own `FERRIX-PANIC` line and called `arch::halt()` by hand; now
it is a `panic!` with a sentence, and `kernel/src/panic.rs` writes the report:

```text
FERRIX-PANIC stage 2 self-check failed: the heap lost an allocation
  at        kernel/src/main.rs:742:5
  on        the boot processor, before any other was started
  stopped   this processor halts here; nothing will recover it
  trace     #0  0xffffffff80002428  __rustc::rust_begin_unwind+0x1b4
  trace     #1  0xffffffff800074fc  core::panicking::panic_fmt+0x28
  trace     #2  0xffffffff80002e54  ferrix_kernel::memory_check+0x20
  trace     #3  0xffffffff80003268  ferrix_kernel::kmain+0x414
  trace     #4  0xffffffff800000fc  _start+0xfc
```

The marker and the message share the first line, because that is the line the
boot test judges on. What follows is context, which `xtask` keeps reading for a
moment so that the log holds the whole report.

The report masks interrupts on its own processor and then asks every other one
to stop, through the inter-processor interrupt they already answer. Otherwise
the machine keeps running around a failure it cannot survive, and the other
processors' output lands in the middle of the report.

The backtrace follows the frame-pointer chain, which is why every kernel target
is built with `force-frame-pointers`. Every step of the walk is checked before
it is trusted. A frame has to be aligned and above the last one. It has to be
within one stack's worth of where the walk began, and on a mapped page, read
from the page tables without taking their lock. A return address has to fall
inside the kernel image. Anything else ends the walk, because a fault inside
the panic handler is a report nobody sees. The kernel prints only addresses. It
carries no symbol table, so `xtask` names each frame from the ELF it booted, in
the terminal and in the log.

The exemption is deliberately narrow. `unwrap`, `expect`, `unreachable!` and
indexing are still denied in the kernel, because each of those panics without
saying why; `panic!` is only allowed because it has to. And nothing in `libs/`
gets it: code that can be a pure function of its input returns its errors, and
the kernel decides which of them are fatal.

The trap path's fatal reports do not go through `panic!`. They print the
architecture's saved registers after the marker, and a panic message has no way
to carry a trap frame.

## Overflow checks are on in release

This inverts Starling's choice, deliberately and for the opposite reason.
Starling turns them off in release because with `panic = "abort"` an overflow
inside a dependency becomes a hard production abort, and for a server aiming at
years of uptime a wrong number beats an outage.

Here the calculus reverses. A wrapped frame number, page count or physical
address does not produce a wrong answer — it produces a write to the wrong
physical page, and the symptom appears in an unrelated subsystem minutes later.
A panic that names the line is strictly better than a machine that limps.

## What the tests can actually reach

Nothing in `kernel/` can be run by `cargo test`. Miri cannot interpret a
privileged instruction, and a fuzzer cannot drive a page-fault handler. That is
the argument for `libs/`:

> Anything expressible as a pure function of bytes is written as one, in
> `libs/`, where `cargo test`, Miri and the fuzzers all reach it.

ELF parsing, cpio unpacking, btrfs item decoding, seccomp BPF evaluation,
page-table index arithmetic, buddy-order maths, the VMA interval tree — all of
it is host-testable by construction, and `scripts/check-crate-layering.sh` keeps
the layering that makes it so.

What is left in `kernel/` is the part that genuinely needs a CPU, and it is
covered by the QEMU boot test instead: every roadmap stage's exit criterion
becomes a boot test and stays in CI forever after.

## Fuzzing

The surfaces that parse bytes somebody else chose are small in a kernel, but
they are the sharp end. An ELF header comes from whatever the user just ran; a
cpio archive from an initramfs assembled elsewhere; a btrfs extent item from a
disk that may be corrupt or hostile. All three are parsed with the MMU on and
nothing above us.

CI replays the committed corpus first and searches second. The replay is what
makes it a regression test: an input that crashed once fails again in seconds,
rather than waiting for the fuzzer to rediscover it inside a timebox.

## The boot test

The gate that answers the question the others cannot. Everything else checks the
source; this one boots it — firmware to loader to kernel — on every
architecture, and fails the build if the machine does not come up. An OS that
compiles and does not boot is not a passing build.

### What booting under an emulator cannot tell you

It runs under `tcg`, QEMU's interpreter, because that is what `CI` has and
because an interpreter is reproducible. The cost is a blind spot with a sharp
edge: `tcg` resolves every access through the page tables as it finds them, so
it has no `TLB` worth the name, and a stale translation cannot be stale in a
cache that does not exist. No amount of booting under it will find a missing
invalidation.

That is not a hypothetical. `arch::flush_tlb` on x86-64 spared global entries —
which is nearly every mapping the kernel makes — for four stages, through a
boot test that passed every time. The first boot under a hardware accelerator
failed three self-checks.

So `--accel auto` exists: it boots on the real processor, with the real `MMU`
and the real `TLB`, using whatever the host offers (`whpx` on Windows, `kvm` on
Linux, `hvf` on macOS) and falling back to `tcg` when there is nothing. It is
not the default, because reproducibility is what a gate is for and because the
guest must match the host's architecture to be accelerated at all. It is what
to run before believing a change to page tables, invalidation or `SMP`.
