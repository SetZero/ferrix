# How this roadmap works

`docs/ARCHITECTURE.md` says what is being built. This says in what order, and
how each stage knows it is finished.

Two rules govern the ordering:

1. **Every stage ends in something that runs.** Not "the VM subsystem
   compiles" — a QEMU boot that demonstrates the new capability and stays in CI
   forever after. A stage with no observable exit criterion is a stage nobody
   can tell is broken.
2. **Nothing is stubbed that a later stage has to unpick.** A fixed-size process
   table, an in-memory-only filesystem or a cooperative scheduler would each
   save a week now and cost a rewrite later, because the goal at the end needs
   the real version of all three.

Sizes are order-of-magnitude, in the sense of "a weekend / a week / a month /
longer". This is a long program of work: stages 1–8 are a conventional kernel
bring-up, 9–14 are the parts this design chose to do properly, 15–16 are the
goal, and 17–19 are the goal after it: a Hyprland-shaped Wayland compositor,
written in Rust, running on Ferrix (decided 2026-09-13). From that date
sizes for new work are story points, measured into time only after the fact
-- and since 2026-09-18, at the customer's word, measured *forward* as well.
The status table after *Where it stands* records the pointed remainder and
its current state. A dated forecast is made only from a fresh velocity count;
estimates are arithmetic, not promises.
