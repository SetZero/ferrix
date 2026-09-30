//! evdev, in Rust, with no libinput and no `evdev` crate.
//!
//! `docs/INPUT.md` §2.4 reads out what a compositor's input backend asks a
//! `/dev/input/eventN` node for, from the `evdev` crate's own source. This
//! crate makes exactly those calls, so that Ferrix's evdev nodes are proven
//! against the demand a real backend puts on them rather than against a
//! convenient subset -- and so that the compositor above has a backend to
//! use when it grows a seat.
//!
//! [`Device`] is the open node; [`event_nodes`] is discovery without udev,
//! which is §3.4's rule: read the directory, there is nothing else to ask.
//! The names are [`names`], and they are the part that is host-tested, since
//! a host has no Ferrix node to open.
//!
//! Every call here is a Unix one, so the crate builds anywhere `libc` does;
//! on a machine with no `/dev/input` there is simply nothing to open, which
//! is what [`event_nodes`] answers.

mod device;
pub mod init;
pub mod names;

pub use device::{Description, Device, INPUT_DIR, event_nodes};

/// One line for an event, as `evecho` prints it and a test reads it.
///
/// The type and code are named where they have a name here, and numbers
/// where they do not; the value is a number always, since it is a count, a
/// position or a press.
#[must_use]
pub fn line(node: &str, kind: u16, code: u16, value: i32) -> String {
    format!(
        "evecho: {node} {} {} {value}",
        names::event_type_text(kind),
        names::code_text(kind, code)
    )
}

#[cfg(test)]
mod tests {
    use ferrix_linux_abi::input::{ABS_X, BTN_LEFT, EV_ABS, EV_KEY, EV_SYN, KEY_A, SYN_REPORT};

    use super::{line, names};

    #[test]
    fn an_events_line_names_what_it_can_and_numbers_what_it_cannot() {
        assert_eq!(
            line("event0", EV_KEY, KEY_A, 1),
            "evecho: event0 EV_KEY KEY_A 1"
        );
        assert_eq!(
            line("event0", EV_SYN, SYN_REPORT, 0),
            "evecho: event0 EV_SYN SYN_REPORT 0"
        );
        assert_eq!(
            line("event1", EV_ABS, ABS_X, 16384),
            "evecho: event1 EV_ABS ABS_X 16384"
        );
        assert_eq!(
            line("event1", EV_KEY, BTN_LEFT, 1),
            "evecho: event1 EV_KEY BTN_LEFT 1"
        );
        // A code with no name here is its number, as `evtest` prints one.
        assert_eq!(
            line("event0", EV_KEY, 0x2ff, 0),
            "evecho: event0 EV_KEY 0x02ff 0"
        );
        assert_eq!(line("event0", 0x1e, 0, 0), "evecho: event0 0x1e 0x0000 0");
    }

    /// The same code is a different name under a different type; a table
    /// that ignored the type would say `KEY_ESC` for a relative Y motion.
    #[test]
    fn a_code_is_named_for_its_type() {
        assert_eq!(names::code(EV_KEY, 1), Some("KEY_ESC"));
        assert_eq!(names::code(0x02, 1), Some("REL_Y"));
        assert_eq!(names::code(EV_ABS, 1), Some("ABS_Y"));
    }
}
