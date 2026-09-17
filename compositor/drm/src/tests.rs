//! What can be tested without a card: the naming of the screens a machine's
//! cards add up to.

#[cfg(target_os = "linux")]
mod names {
    use ferrix_linux_abi::drm::ModeInfo;
    use ferrix_linux_abi::layout::Field;

    use crate::{Plan, rename};

    fn plan(name: &str) -> Plan {
        Plan {
            connector: 0,
            crtc: 0,
            crtc_index: 0,
            mode: <ModeInfo as Field>::ZERO,
            name: name.to_owned(),
        }
    }

    fn names(plans: &mut [Plan]) -> Vec<String> {
        rename(plans);
        plans.iter().map(|plan| plan.name.clone()).collect()
    }

    /// Two virtio-gpu cards each call their connector `Virtual-1`; a person
    /// with two screens needs two names, and wlroots numbers each connector
    /// type from one across the whole backend.
    #[test]
    fn each_connector_type_is_numbered_across_every_card() {
        let mut plans = [plan("Virtual-1"), plan("Virtual-1")];
        assert_eq!(names(&mut plans), ["Virtual-1", "Virtual-2"]);

        // Types are counted apart from one another, and a connector's own
        // number is not kept: the order is what decides.
        let mut mixed = [plan("DP-3"), plan("HDMI-A-1"), plan("DP-1")];
        assert_eq!(names(&mut mixed), ["DP-1", "HDMI-A-1", "DP-2"]);

        // One screen keeps the name it always had.
        let mut one = [plan("Virtual-1")];
        assert_eq!(names(&mut one), ["Virtual-1"]);
    }
}
