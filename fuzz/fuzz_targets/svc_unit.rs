//! Fuzz the service manager's unit-file parser and loader (`libs/svc`).
//!
//! A unit file is written by whoever administers the machine, and a
//! generator writes more at every boot; init loads them all as pid 1, where
//! a panic is the machine. The parser promises to answer every input with a
//! unit, or a reason it did not load, and warnings, and never to stop.
//!
//! The input is a selector byte, then a unit file, then optionally a NUL and
//! a drop-in for it. The selector's low three bits choose the unit's kind,
//! so every kind's keys are reached, and the next bit whether it is loaded
//! as an instance of a template.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **Loading is a function**: the same directories load the same unit.
//! 2. **Warnings point into the files**: every one names a file that was
//!    read, at a line that file has.
//! 3. **A parsed file writes back**: its assignments, written one per line
//!    under their sections, parse to the same assignments, whenever no key
//!    starts with `[` and no value ends in a backslash (the two things one
//!    line of the syntax cannot say).
//! 4. **Words re-quote**: words split from the text, each written back in
//!    double quotes with `"` and `\` escaped, split to the same words.
//! 5. **A name that parses is its parts**: prefix, `@instance` and suffix put
//!    back together are the name.
//! 6. **Path escaping reverses**: a path that escapes unescapes to itself,
//!    normalized.

#![no_main]

use std::sync::Arc;

use ferrix_svc::ini;
use ferrix_svc::name::{UnitType, escape_path, path_components, unescape};
use ferrix_svc::source::{Entry, Layer, Source};
use ferrix_svc::{UnitName, Warnings, value};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let (unit_file, drop_in) = match rest.iter().position(|&byte| byte == 0) {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };

    load(selector, unit_file, drop_in);
    round_trip(unit_file);

    if let Ok(text) = std::str::from_utf8(unit_file) {
        words(text);
        name(text);
        path(text);
    }
});

/// Properties 1 and 2.
fn load(selector: u8, unit_file: &[u8], drop_in: Option<&[u8]>) {
    let kind = UnitType::ALL[usize::from(selector & 7) % UnitType::ALL.len()];
    // Slices have no templates, so a templated slice is loaded plain.
    let templated = selector & 8 != 0 && kind != UnitType::Slice;
    let (file_name, load_name) = if templated {
        (format!("fuzz@.{}", kind.suffix()), format!("fuzz@x.{}", kind.suffix()))
    } else {
        let name = format!("fuzz.{}", kind.suffix());
        (name.clone(), name)
    };
    let mut source = Source::new();
    source
        .add(Layer::Image, &file_name, Entry::File(unit_file.to_vec()))
        .expect("a unit name is a unit path");
    if let Some(drop_in) = drop_in {
        source
            .add(Layer::Admin, &format!("{file_name}.d/fuzz.conf"), Entry::File(drop_in.to_vec()))
            .expect("a drop-in is a unit path");
    }
    let first = source.load(&load_name);
    let second = source.load(&load_name);
    assert_eq!(first, second, "loading the same directories twice differs");

    let Ok(unit) = first else {
        return;
    };
    for warning in unit.warnings.list() {
        let bytes = if warning.file.ends_with("fuzz.conf") {
            drop_in.unwrap_or_default()
        } else {
            assert!(warning.file.ends_with(file_name.as_str()), "a warning names {}", warning.file);
            unit_file
        };
        let lines = bytes.split(|&byte| byte == b'\n').count();
        assert!(
            usize::try_from(warning.line).is_ok_and(|line| line <= lines),
            "warning at line {} of a {lines}-line file: {}",
            warning.line,
            warning.message
        );
    }
}

/// Property 3.
fn round_trip(bytes: &[u8]) {
    let file: Arc<str> = Arc::from("fuzz");
    let mut warnings = Warnings::new();
    let Ok(document) = ini::parse(&file, bytes, &mut warnings) else {
        return;
    };
    let writable = document.sections.values().all(|section| {
        !section.name.contains([']', '\n'])
            && section.assignments.iter().all(|assignment| {
                !assignment.key.starts_with('[')
                    && !assignment.value.ends_with('\\')
                    && !assignment.key.contains('=')
            })
    });
    if !writable {
        return;
    }
    let mut text = String::new();
    for section in document.sections.values() {
        text.push_str(&format!("[{}]\n", section.name));
        for assignment in &section.assignments {
            text.push_str(&format!("{}={}\n", assignment.key, assignment.value));
        }
    }
    let again = ini::parse(&file, text.as_bytes(), &mut Warnings::new()).expect("written back");
    let pairs = |document: &ini::Document| -> Vec<(String, String, String)> {
        document
            .sections
            .values()
            .flat_map(|section| {
                section.assignments.iter().map(move |assignment| {
                    (section.name.clone(), assignment.key.clone(), assignment.value.clone())
                })
            })
            .collect()
    };
    assert_eq!(pairs(&document), pairs(&again), "a file does not write back:\n{text}");
}

/// Property 4.
fn words(text: &str) {
    let Ok(words) = value::words(text) else {
        return;
    };
    let quoted: Vec<String> = words
        .iter()
        .map(|word| {
            let mut out = String::from("\"");
            for c in word.text.chars() {
                if c == '"' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
            out
        })
        .collect();
    let again = value::words(&quoted.join(" ")).expect("quoted words split");
    let texts = |list: &[value::Word]| list.iter().map(|w| w.text.clone()).collect::<Vec<_>>();
    assert_eq!(texts(&words), texts(&again), "words do not re-quote");
}

/// Property 5.
fn name(text: &str) {
    let Ok(name) = UnitName::parse(text) else {
        return;
    };
    let mut rebuilt = String::from(name.prefix());
    if let Some(instance) = name.instance() {
        rebuilt.push('@');
        rebuilt.push_str(instance);
    }
    rebuilt.push('.');
    rebuilt.push_str(name.unit_type().suffix());
    assert_eq!(rebuilt, name.as_str(), "a name is not its parts");
    if let Some(template) = name.template() {
        assert_eq!(
            template.instantiate(name.instance().unwrap_or_default()).as_ref(),
            Ok(&name),
            "an instance is not its template instantiated"
        );
    }
}

/// Property 6.
fn path(text: &str) {
    let Ok(escaped) = escape_path(text) else {
        return;
    };
    let components = path_components(text).expect("a path that escaped has components");
    let normalized = format!("/{}", components.join("/"));
    let back = if escaped == "-" {
        String::from("/")
    } else {
        format!("/{}", unescape(&escaped))
    };
    assert_eq!(back, normalized, "escaping {text:?} does not reverse");
}
