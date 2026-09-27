# Brief: draft ARMv7-A / arm_common low-level requirements (W-8, file 24)

You are drafting part of `docs/sysml/24-armv7a-requirements.sysml` for the Ferrix kernel.
Repository worktree (read-only for you): /home/sebastian/Documents/projects/os/ferrix/.claude/worktrees/w8-armv7a
**Do not edit, build, or run anything in the repository.** Read only. Write your output
to the file named in your task, under the scratchpad directory
/tmp/claude-1000/-home-sebastian-Documents-projects-os-ferrix/77c352c8-8127-4625-aa6e-de880e138ac0/scratchpad/w8/

## Read first (the format and its rules)
- `docs/sysml/13-item-requirements.sysml`: defines `ItemLowLevel` (statement, criterion,
  parent, unit) and holds every high-level `H.*` requirement you may use as `parent`.
- `docs/sysml/14-object-requirements.sysml` header comment: the pilot's rules.
- The model to copy most closely is the AArch64 file, which is on a branch, not main:
  `git show e4-w8:docs/sysml/19-aarch64-requirements.sysml` (run `git show` read-only).
  Where ARMv7-A has the same behaviour as AArch64, use the **same parent H.* id** 19 uses.
  Also useful: `docs/sysml/18-x86-64-requirements.sysml` (x86-64's version).

## The rules (from the pilot, strictly)
1. One requirement per behaviour a check can pass or fail. A requirement's `unit` names
   **every** function that carries the behaviour (a tuple of strings when several).
2. A requirement is no broader than ONE check can prove. A `/// Verifies:` tag credits the
   whole requirement, so only propose a verifying check when that one check function
   asserts the requirement's whole criterion. Otherwise the requirement goes to the
   baseline (needs a check); say why.
3. The statement says no more than the criterion tests. The criterion is concrete and
   countable (e.g. "15 frames ... classify as ...", "the `machine` line").
4. An accessor (one statement or expression, no branch, no `unsafe`) needs no requirement;
   the gate already classified those. You only have to cover the units listed for you.
5. Check code living in a product file (a function whose only purpose is a check) is not
   given a requirement; list it separately with a one-sentence reason, in the style of
   `git show e4-w8:scripts/data/traceability-units.json` "check_code_in_product".
6. Unit paths are module paths from kernel/src, e.g. `arch::armv7a::trap::classify`,
   methods as `arch::armv7a::Irq::disable`, `arch::arm_common::gicv2::init`.
7. Ids: use placeholders `L.armv7a.X<k>` (your area letter X, k = 1..n). I renumber.
8. SysML string style: statements "... shall ...", wrapped at about 80 columns inside the
   string, as in 18/19. Criteria name where a check shows it (the boot line, e.g. the
   `machine` line) or, if no check, say what a check would have to show.

## Checks that exist for ARMv7-A (read them)
- `kernel/src/arch/armv7a/check.rs`: check_trap_decoding (15 frames: classify + fault_signal,
  came_from_user), check_frames_render, check_chosen (already `Verifies: L.console.41`),
  check_cache_maintenance (flush_for_device), check_masking (gicv2 idle lines, mask/unmask,
  read back), check_refusals (special ids 1020..1023, GICv2m frame at 0 / past SPIs, doorbell
  a page). Driven by `check()`, printing the `machine` line.
- `kernel/src/arch/armv7a/trap/check.rs`: real USR programs: udf -> SIGILL, bkpt -> SIGTRAP,
  misaligned ldm -> SIGBUS, and a handler installed without SA_RESTORER (signal frame's
  return sequence) exiting 55.
- `kernel/src/arch/armv7a/speculation.rs` contains a `check` function (check code in a product
  file; AArch64 moved theirs to speculation/check.rs).
- Generic checks elsewhere may exercise arch code (sched/check.rs, smp/check.rs,
  syscall checks, signal checks, user/check.rs, timer checks). A generic check verifies an
  arch requirement only if it asserts that requirement's whole criterion on ARMv7-A. Search
  with grep; when unsure, baseline it and say which check nearly does.

## Output (one markdown file)
1. `## Requirements`: a ```sysml block with one `package <Name> { doc /* file: what */ ... }`
   per logical area, containing the `requirement <'L.armv7a.X1'> camelName : ItemLowLevel {...}`
   entries. Every unit in your list must appear in exactly one requirement's `unit`, or in
   section 3.
2. `## Verification`: a table: id | verifying check function (path::fn) or "baseline" | why.
3. `## Check code in product files`: unit | reason (one sentence).
4. `## Notes`: anything I must decide (a unit that looks mis-scoped, a parent that does not
   fit, a behaviour that seems wrong in the code - report possible bugs, don't fix them).

Be accurate to the code: read each function before writing its requirement. Prefer fewer,
well-cut requirements (AArch64 covered ~150 functions with 48).
