# W-8 file 24 (arch/armv7a + arch/arm_common low-level requirements) — handover

Owner when parked: ferrix-b5 (was ferrix-55b), 2026-09-27 wind-down.
Branch: `w8-armv7a` (WIP commit; never main). Drafts also here, in this directory.

## State
- Nothing of file 24 is written yet as `docs/sysml/24-armv7a-requirements.sysml`.
- Four area drafts exist (80 draft requirements over all 163 unnamed units of
  arch::armv7a and arch::arm_common): draft-trap.md (12, T1–T12), draft-core.md
  (26, C1–C26), draft-common.md (18, A1–A18), draft-facade.md (24, F1–F24).
  Each has: sysml fragments with placeholder ids, a verification table
  (which check could carry `Verifies:`), check-code-in-product lists, notes.
  They were written against main before F-50/F-48 landed; re-read the units
  those changed (gicv2 enable/set_edge_triggered/rmw, signal.rs
  setup_signal_frame/return_address, sigpage) before using them.
- Agreed with ferrix-44 (AArch64 slice, file 19, landed): file 24 claims ALL of
  arch/arm_common (gicv2, pl011, stm32_usart); 19 points at it.
- Consultant (ferrix-20) conditions for 24: requirements state the correct
  behaviour (never the defect); a tag only where one check proves the whole
  criterion; F-50's check (arm_common::gicv2::check::concurrent_enables) gets
  an L requirement under H.IRQ.2 and its tag; F-48's /returning check
  (arch::armv7a::trap::check::run) an L requirement under H.TRAP.9.

## Defects the reading found
- F-50 GICv2 unlocked RMW: LANDED 759a1b4c.
- F-48 ARMv7-A no-SA_RESTORER return: landing/landed (see BACKLOG).
- F-49 psci_system r12 clobber: NOT fixed; see its BACKLOG row.
- Unconfirmed flags still to check (consultant asked to be told before
  numbering): Linux ARMv7 fixes up a misaligned user ldm (alignment.c UM_FIXUP)
  where Ferrix sends SIGBUS/BUS_ADRALN (ABI deviation → state in Linux-ABI
  docs); mask/unmask accept SGIs and lines past GICD_TYPER's count (a finding
  only if the write can leave the window or reach another line's bits); the
  speculation plan is taken from the boot core only (arch/aarch64, F-31: a
  finding if mixed cores need different mitigations, e.g. the Pixel 7).
- Smaller drafter notes: report_trap prints r13–r15 as zeros; timer::init's doc
  says it checks the counter runs but only refuses CNTFRQ=0; switch.rs's doc
  says a ninth register pads, the code uses `sub sp, #4`; console stdout-path
  fallback without `reg` fails with NoConsole instead of falling through.

## Next step
Write 24 in the 18/19 pattern from the drafts (renumber L.armv7a.* and
L.arm_common.* or one flat L.armv7a.*), move arch/armv7a/speculation.rs's
`check` into speculation/check.rs as 19 did so a tag can go on it, add
arch::armv7a and arch::arm_common to traceability-units.json "complete",
list check code there, baseline the rest with reasons, regenerate
TRACEABILITY/model docs, consultant review, land.
