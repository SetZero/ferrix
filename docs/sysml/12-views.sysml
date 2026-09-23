/*
 * Ferrix — SysML v2 model, part 12: views.
 *
 * How to read the one model as two: what runs today, and what the roadmap
 * still owes. The filters key on the lifecycle keywords every element
 * carries.
 */
package FerrixViews {
    private import FerrixLifecycle::*;

    viewpoint def MaintainerViewpoint {
        doc /* Someone changing kernel/ or libs/ who wants to see the parts
             * that exist, their boot-time sequence, and the gates that guard
             * them. */
    }

    viewpoint def PlannerViewpoint {
        doc /* Someone starting a stage who wants the design it has to
             * satisfy, the exit criterion it has to meet, and the parts the
             * stage is allocated to. */
    }

    view def TreeView;

    view currentDesign : TreeView {
        doc /* What runs: stages 0 to 12, networking and stages 16 to 18 are
             * done, and parts of dynamic linking and stages 15 and 19 run.
             * Stages 0 to 12 are in the boot test on all three
             * architectures. */
        expose FerrixStructure::**;
        expose FerrixBoot::**;
        expose FerrixMemory::**;
        expose FerrixScheduling::**;
        expose FerrixObjects::**;
        expose FerrixIsolation::**;
        expose FerrixDrivers::**;
        expose FerrixStorage::**;
        expose FerrixAssurance::**;
        filter @implemented;
    }

    view writtenAhead : TreeView {
        doc /* Crates and types that exist and are host-tested but that
             * nothing running calls yet. */
        expose FerrixStructure::Workspace::**;
        expose FerrixStorage::**;
        expose FerrixScheduling::**;
        filter @writtenAhead;
    }

    view futureDesign : TreeView {
        doc /* What the roadmap still owes: stage 13's isolation, stage 14's
             * real-time domains, stage 15's init, and the pieces finished
             * stages name as not yet built. */
        expose FerrixStructure::**;
        expose FerrixMemory::**;
        expose FerrixScheduling::**;
        expose FerrixObjects::**;
        expose FerrixIsolation::**;
        expose FerrixDrivers::**;
        expose FerrixStorage::**;
        expose FerrixAssurance::**;
        filter @planned;
    }

    view deferredWork : TreeView {
        doc /* What finished stages explicitly left behind, with reasons. */
        expose FerrixStructure::**;
        expose FerrixMemory::**;
        expose FerrixScheduling::**;
        expose FerrixRoadmap::**;
        filter @deferred;
    }

    view roadmap : TreeView {
        expose FerrixRoadmap::**;
        expose FerrixRequirements::**;
    }
}
