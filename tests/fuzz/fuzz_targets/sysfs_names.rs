//! Fuzz sysfs's parsers: the name a write to `bind` or `unbind` gives, and
//! the names a lookup is asked for -- PCI slots, device numbers, numbered
//! names.
//!
//! Root writes `bind` and `unbind`, but any process may look up any name in
//! `/sys`, and every one of these runs on a name a program chose. They
//! promise to answer every input with a value or nothing and never to stop.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **A written name is inside what was written**: no NUL, not empty, no
//!    trailing newline dropped twice.
//! 2. **A slot round-trips**: a name `Slot::parse` reads, printed again, is
//!    the same bytes -- so one slot has one name, and a lookup cannot find a
//!    function under a name `ls` never shows.
//! 3. **A device number round-trips** the same way.
//! 4. **A numbered name round-trips** the same way.
//! 5. **A relative link climbs no higher than its directory is deep**, and
//!    names no empty component.

#![no_main]

use ferrix_sysfs::name::{self, Slot};
use ferrix_sysfs::path;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(named) = name::written(data) {
        assert!(!named.is_empty(), "an empty write named something");
        assert!(!named.contains(&0), "a written name holds a NUL");
        assert!(data.starts_with(named), "a written name is not what was written");
    }

    if let Some(slot) = Slot::parse(data) {
        let mut printed = Vec::new();
        slot.name(&mut printed);
        assert_eq!(printed, data, "a slot read back as another name");
    }

    if let Some((major, minor)) = name::parse_dev_number(data) {
        let mut printed = Vec::new();
        name::dev_number(&mut printed, major, minor);
        assert_eq!(printed, data, "a device number read back as another name");
    }

    if let Some(number) = name::numbered(b"card", data) {
        assert_eq!(
            format!("card{number}").as_bytes(),
            data,
            "a numbered name read back as another"
        );
    }

    // Split the input into two paths at the first NUL, each into components
    // at '/', and require the link between them to be well formed.
    let (from, to) = match data.iter().position(|&byte| byte == 0) {
        Some(at) => (&data[..at], &data[at + 1..]),
        None => (data, &[][..]),
    };
    let components = |bytes: &'_ [u8]| -> Vec<Vec<u8>> {
        bytes
            .split(|&byte| byte == b'/')
            .filter(|part| !part.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    };
    let (from, to) = (components(from), components(to));
    let from: Vec<&[u8]> = from.iter().map(Vec::as_slice).collect();
    let to: Vec<&[u8]> = to.iter().map(Vec::as_slice).collect();
    let mut link = Vec::new();
    path::relative(&mut link, &from, &to);
    let parts: Vec<&[u8]> = link.split(|&byte| byte == b'/').collect();
    assert!(parts.iter().all(|part| !part.is_empty()), "an empty component");
    let climbs = parts.iter().filter(|part| **part == b"..").count();
    assert!(climbs <= from.len(), "a link climbs above its mount");
});
