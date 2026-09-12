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
        doc /* Stages 0 to 4 on three architectures. */
        expose FerrixStructure::**;
        expose FerrixBoot::**;
        expose FerrixMemory::**;
        expose FerrixScheduling::**;
        expose FerrixDrivers::**;
        expose FerrixAssurance::**;
        filter @implemented;
    }

    view writtenAhead : TreeView {
        doc /* Crates that exist and are host-tested but that the kernel does
             * not call yet. */
        expose FerrixStructure::Workspace::**;
        expose FerrixStorage::**;
        expose FerrixScheduling::**;
        filter @writtenAhead;
    }

    view futureDesign : TreeView {
        doc /* Stages 5 to 17. */
        expose FerrixStructure::**;
        expose FerrixMemory::**;
        expose FerrixScheduling::**;
        expose FerrixObjects::**;
        expose FerrixIsolation::**;
        expose FerrixDrivers::**;
        expose FerrixStorage::**;
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
