/*
 * Ferrix — SysML v2 model, part 0: lifecycle vocabulary.
 *
 * Every other package marks its elements with one of the keywords defined
 * here, so a reader can tell what runs today from what docs/ROADMAP.md still
 * owes. The model is one model; the keywords are the seam between "current"
 * and "future".
 */
package FerrixLifecycle {
    private import ScalarValues::*;

    doc /* Four maturities and the stage that owns an element.
         *
         * #implemented   the code exists and a QEMU boot exercises it on
         *                every architecture it applies to: the boot test
         *                itself, or a named `cargo xtask test-*` gate.
         * #inProgress    the owning stage has started; part of the element runs.
         * #writtenAhead  a `libs/` crate exists and passes its host tests, but
         *                no gate exercises it yet through the kernel or a
         *                program in the image.
         * #planned       only the design exists, in docs/ARCHITECTURE.md.
         *
         * `@stage { number = n; }` names the docs/ROADMAP.md stage that owns
         * the element. An element with no stage is cross-cutting. */

    metadata def implemented;
    metadata def inProgress;
    metadata def writtenAhead;
    metadata def planned;

    metadata def stage {
        doc /* The roadmap stage, 0 to 22, that owns the element. */
        attribute number : Natural;
    }

    metadata def deferred {
        doc /* Work a finished stage explicitly left behind, with the reason
             * the stage gave. Deferred is not the same as planned: the stage
             * that owns it is closed, and the item waits for a machine, a
             * workload or a later stage to make it meaningful. */
        attribute reason : String;
    }

    enum def StageStatus {
        Done;
        InProgress;
        Planned;
    }
}
