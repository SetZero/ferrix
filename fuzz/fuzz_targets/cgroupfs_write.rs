//! Fuzz cgroupfs's parsers: every write a cgroup interface file takes, and
//! the names `mkdir` may give a cgroup.
//!
//! Any process may write any bytes to a cgroup file it can open, and a
//! delegated subtree's files are opened by an unprivileged user. The parsers
//! promise to answer every input with a value or a refusal and never to
//! stop.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **`strip` is idempotent** and leaves no surrounding whitespace.
//! 2. **`kstrtoint` round-trips**: a number it reads, printed in decimal,
//!    reads back as itself.
//! 3. **A limit round-trips**: one parsed and printed parses back equal.
//! 4. **A subtree change stays inside what the kernel knows**, and enables
//!    and disables no controller at once.
//! 5. **A name `check` accepts is a path component**: no `/`, no newline, not
//!    `.` or `..`, and at most 255 bytes.

#![no_main]

use ferrix_cgroupfs::controllers::{self, Controller, Set};
use ferrix_cgroupfs::{name, render, write};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, text)) = data.split_first() else {
        return;
    };

    let stripped = write::strip(text);
    assert_eq!(write::strip(stripped), stripped, "strip is not idempotent");

    if let Ok(value) = write::kstrtoint(stripped) {
        let printed = value.to_string();
        assert_eq!(
            write::kstrtoint(printed.as_bytes()),
            Ok(value),
            "a number kstrtoint read does not read back"
        );
    }

    if let Ok(limit) = write::parse_limit(text) {
        let mut out = Vec::new();
        render::limit(&mut out, limit);
        assert_eq!(write::parse_limit(&out), Ok(limit), "a limit does not round-trip");
    }

    // The low four bits of the first byte choose which controllers the
    // kernel "has built".
    let known = controllers::ALL
        .iter()
        .enumerate()
        .filter(|(bit, _)| selector & (1 << bit) != 0)
        .fold(Set::EMPTY, |set, (_, controller)| set.with(*controller));
    if let Ok(change) = controllers::parse_change(text, known) {
        assert!(change.enable.is_subset(known), "enabled a controller not built");
        assert!(change.disable.is_subset(known), "disabled a controller not built");
        for controller in [Controller::Cpu, Controller::Io, Controller::Memory, Controller::Pids] {
            assert!(
                !(change.enable.contains(controller) && change.disable.contains(controller)),
                "one write both enabled and disabled {}",
                controller.name()
            );
        }
    }

    let _ = write::parse_procs(text);
    let _ = write::parse_kill(text);
    let _ = write::parse_type(text);
    let _ = write::parse_freeze(text);

    if name::check(text).is_ok() {
        assert!(!text.is_empty() && text.len() <= name::NAME_MAX);
        assert!(text != b"." && text != b"..");
        assert!(!text.iter().any(|&byte| byte == b'/' || byte == b'\n'));
    }
});
