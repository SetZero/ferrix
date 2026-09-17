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
            edid: None,
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

/// A monitor's `EDID` read into the three strings a `desc:` rule matches on.
///
/// The bytes are a real Dell P2418D's base block, which is what `nazuna`'s
/// own configuration names -- `monitor = desc:Dell Inc. DELL P2418D
/// MY3ND91J09CT`. A compositor that read the manufacturer's five-bit
/// letters the wrong way round, or took the product code where a `0xFC`
/// descriptor names the model, would build a description that line cannot
/// match, and the person's monitor would silently take the fallback rule.
mod monitors {
    use crate::Edid;

    /// One 18-byte display descriptor: three zero bytes, the tag, a zero,
    /// and up to thirteen characters ended by `0x0A` and padded with
    /// spaces.
    fn descriptor(tag: u8, text: &str) -> [u8; 18] {
        let mut block = [0x20u8; 18];
        block[0] = 0;
        block[1] = 0;
        block[2] = 0;
        block[3] = tag;
        block[4] = 0;
        let bytes = text.as_bytes();
        for (at, &byte) in bytes.iter().take(13).enumerate() {
            block[5 + at] = byte;
        }
        if bytes.len() < 13 {
            block[5 + bytes.len()] = 0x0A;
        }
        block
    }

    /// A base block for one monitor: `DEL`, product `4140`, and the two
    /// descriptors a real monitor carries.
    fn edid(name: Option<&str>, serial: Option<&str>) -> Vec<u8> {
        let mut bytes = vec![0u8; 128];
        bytes[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        // `DEL`: D is 4, E is 5, L is 12, five bits each, most significant
        // first, with the top bit zero.
        let packed: u16 = (4 << 10) | (5 << 5) | 12;
        bytes[8..10].copy_from_slice(&packed.to_be_bytes());
        bytes[10..12].copy_from_slice(&0x4140u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        if let Some(name) = name {
            bytes[54..72].copy_from_slice(&descriptor(0xFC, name));
        }
        if let Some(serial) = serial {
            bytes[72..90].copy_from_slice(&descriptor(0xFF, serial));
        }
        bytes
    }

    #[test]
    fn a_monitors_edid_becomes_the_description_a_rule_names() {
        let read = Edid::parse(&edid(Some("DELL P2418D"), Some("MY3ND91J09CT")))
            .expect("the bytes are an EDID");
        assert_eq!(read.manufacturer, "DEL");
        assert_eq!(read.model, "DELL P2418D");
        assert_eq!(read.serial, "MY3ND91J09CT");
        // With a registry, which is what Hyprland has and what makes the
        // description the one a person writes in their file.
        assert_eq!(
            read.describe(|id| (id == "DEL").then(|| "Dell Inc.".to_owned())),
            "Dell Inc. DELL P2418D MY3ND91J09CT"
        );
        // Without one, the three-letter code stands in for the company.
        assert_eq!(read.describe(|_| None), "DEL DELL P2418D MY3ND91J09CT");
    }

    #[test]
    fn a_monitor_with_no_descriptors_falls_back_to_its_numbers() {
        let read = Edid::parse(&edid(None, None)).expect("the bytes are an EDID");
        assert_eq!(read.model, "4140");
        assert_eq!(read.serial, "12345678");
    }

    /// A comma would make the description impossible to write in a
    /// `monitor =` line, which is comma-separated, so Hyprland takes them
    /// out and so does this.
    #[test]
    fn a_comma_in_a_description_is_taken_out() {
        let read = Edid::parse(&edid(Some("ACME, Inc. 27"), None)).expect("an EDID");
        assert_eq!(read.describe(|_| None), "DEL ACME Inc. 27 12345678");
    }

    #[test]
    fn bytes_that_are_not_an_edid_are_refused_rather_than_guessed_at() {
        assert_eq!(Edid::parse(&[]), None);
        assert_eq!(Edid::parse(&[0u8; 128]), None, "no header");
        let mut short = edid(Some("x"), None);
        short.truncate(127);
        assert_eq!(Edid::parse(&short), None, "a base block is 128 bytes");
        // A manufacturer with a zero letter is not one.
        let mut zeroed = edid(Some("x"), None);
        zeroed[8] = 0;
        zeroed[9] = 0;
        assert_eq!(Edid::parse(&zeroed), None);
    }
}
