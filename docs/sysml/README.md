# The SysML v2 model

`docs/ARCHITECTURE.md` says what is being built and `docs/ROADMAP.md` in what
order. This directory says the same thing as a model: one set of SysML v2
packages, in the textual notation, covering both what runs today and what the
roadmap still owes, with every element marked as one or the other.

The prose documents remain the source of truth. The model is a structured
index over them — requirements with stable ids, parts with the connections
between them, the boot sequence as an action flow, the scheduling-domain mode
switch as a state machine, the gates as verification cases — for the reader
who wants the shape of the system without the argument for it, and for
tooling that can check a model for consistency where it cannot check prose.

## Files

| File | Package | What it holds |
|---|---|---|
| `00-lifecycle.sysml` | `FerrixLifecycle` | The keywords every other element carries: `#implemented`, `#inProgress`, `#writtenAhead`, `#planned`, `@deferred { reason = "..."; }`, and `@stage { number = n; }`. |
| `01-requirements.sysml` | `FerrixRequirements` | The goal read as a specification (`G`, `G.1`–`G.8`), the design rules (`P.1`–`P.17`, including the commit conventions), and the promises deliberately not made (`N.1`–`N.3`). |
| `02-structure.sysml` | `FerrixStructure` | The machine context, the loader, the kernel and its parts, the userland, the architecture facade with its three variants, and the workspace crates with their dependency edges. |
| `03-boot.sysml` | `FerrixBoot` | The hand-off structure, both address layouts, the loader's sequence, the kernel's bring-up through stage 4 with every self-check, and the trap path. |
| `04-memory.sysml` | `FerrixMemory` | Frames, heap, paging, the vmap arena and the VMA map (today); VMOs, process address spaces, copy-on-write and reclaim (ahead). |
| `05-scheduling.sysml` | `FerrixScheduling` | Locks, interrupts, time and SMP (today); tasks, the class stack, scheduling domains and the mode-switch state machine (ahead). |
| `06-objects.sysml` | `FerrixObjects` | The nine kernel objects, clone's sharing set, the Linux ABI layer and the native ABI. |
| `07-isolation.sysml` | `FerrixIsolation` | Namespaces, cgroups v2, seccomp, credentials. |
| `08-drivers.sysml` | `FerrixDrivers` | Table access and MMIO (today); device nodes, IOMMU domains, `devmgr`, driver processes, the shared ring and the bootstrap sequence (ahead). |
| `09-storage.sysml` | `FerrixStorage` | Block core, VFS, page cache, the small filesystems, btrfs in three stages. |
| `10-roadmap.sysml` | `FerrixRoadmap` | Stages 0–17 and the ARMv7-A port as requirements with status and exit criteria, their ordering, what satisfies each, and the boot tests that verify the done ones. |
| `11-assurance.sysml` | `FerrixAssurance` | Every gate `cargo xtask check` and CI run, the commit-authorship check, the assembly budget, what each layer's tests can reach, and the verification later stages owe. |
| `12-views.sysml` | `FerrixViews` | Views filtering the one model into current, written-ahead, future and deferred. |

## Reading it

Every definition and usage carries a maturity keyword:

* `#implemented` — the code exists and the QEMU boot test exercises it.
* `#inProgress` — the owning stage has started; part of the element runs.
* `#writtenAhead` — a `libs/` crate exists and passes host tests, but the
  kernel does not call it yet.
* `#planned` — the design exists in `docs/ARCHITECTURE.md` and nothing else.
* `@deferred { reason = "..."; }` — a finished stage explicitly left it behind; the attribute
  carries the stage's reason.

`@stage { number = n; }` names the roadmap stage that owns the element. The
short names in angle brackets — `<'G.4'>`, `<'P.6'>`, `<'S12'>` — are stable
ids the packages cite across files.

Hexadecimal addresses are `String` attributes because the notation has no
hexadecimal literal; the two layouts in `03-boot.sysml` are the same
constants `libs/bootinfo` checks at compile time.

## Tooling

The files are plain SysML v2 textual notation and load together as one model
in any implementation of the language. No SysML v2 tool is part of this
tree's toolchain and none runs in CI, so the model is not a gate;
`scripts/check-line-endings.py` covers it like any other text file.

There is no Debian or Ubuntu package for a SysML v2 parser. What does exist:

* **`sysmlpy`** (PyPI, pure Python) is what this model was checked with.
  Every file parses; the `analyze` pass reports false positives on valid
  notation it does not implement — wildcard imports across packages, port
  conjugation (`~Port`), enum literals in expressions, sequence literals, and
  `satisfy ... by a.b` feature chains — so only `parse` is meaningful here.

  ```
  python3 -m venv .venv && .venv/bin/pip install sysmlpy
  for f in docs/sysml/*.sysml; do .venv/bin/sysmlpy parse "$f" >/dev/null || echo "FAIL $f"; done
  ```

* **`syside`** (PyPI, Sensmetry's Automator) is a full implementation but
  needs a paid licence key; the free Syside Editor is a VS Code extension.
* The **pilot implementation** (Systems-Modeling on GitHub) ships a Jupyter
  kernel that needs Java 21 and is installed from its release zip with
  `python3 install.py`.

## Keeping it current

The model changes when the documents it indexes change, in the same commit:

* a stage finishing flips its requirement in `10-roadmap.sysml` to `Done`,
  moves its parts from `#planned` to `#implemented`, and records anything the
  stage deferred with the reason the roadmap gives;
* a new `libs/` crate is a new `#writtenAhead` part in
  `FerrixStructure::Workspace`, with its dependency edge;
* a new architecture is a fourth variant of `ArchLayer` and a fourth boot
  test, and nothing else — which is the point of the facade.
