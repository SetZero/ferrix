/*
 * Ferrix — SysML v2 model, part 13: the certified item's requirements.
 *
 * The high-level requirements of the certified item (the `core` and `item`
 * rings of docs/certification/ITEM.md): what each subsystem promises at its
 * interface, decomposed from the Security Target's objectives
 * (docs/certification/SECURITY-TARGET.md §4.1 and §8.2) and the safety
 * manual's assumed safety requirements (SAFETY-MANUAL.md §2). The low-level
 * requirements, one per unit of code, go in one file per subsystem after this
 * one. docs/certification/IMPLEMENTATION.md W-8 is the design, and
 * scripts/check/check-traceability.py the gate that reads these and writes
 * docs/certification/TRACEABILITY.md.
 */
package FerrixItemRequirements {
    private import ScalarValues::*;
    private import FerrixStructure::Kernel;

    requirement def ItemRequirement {
        doc /* A requirement of the certified item. Unlike the goal and the
             * design rules of part 1, which say why the system is shaped as it
             * is, each of these says what the item shall do in terms a test
             * can pass or fail, and names the requirement above it that it
             * serves. The gate refuses one without all three. */
        subject item : Kernel;
        attribute statement : String {
            doc /* One sentence with "shall", about behaviour a program, a
                 * device or the boot log can observe. */
        }
        attribute criterion : String {
            doc /* The pass/fail condition a check tests, counted where there
                 * is something to count. */
        }
        attribute parent : String[1..*] {
            doc /* The ids of the level above: for a high-level requirement a
                 * Security Target objective (O.*), an assumed safety
                 * requirement (ASR-*) or a goal requirement (G.*); for a
                 * low-level one, a high-level requirement (H.*). */
        }
    }

    requirement def ItemHighLevel :> ItemRequirement {
        doc /* What one subsystem of the item promises at its interface, with
             * an id `H.<AREA>.<n>`. The areas follow the families of
             * functional requirement the item implements. */
    }

    requirement def ItemLowLevel :> ItemRequirement {
        doc /* What one unit of the item's code does, with an id
             * `L.<module>.<n>`, and the unit named so that a renamed function
             * breaks the trace loudly. */
        attribute unit : String[1..*] {
            doc /* `path::function` or `path::Type::method`, relative to
                 * kernel/src: `mm::zero_frame`. */
        }
    }
}
