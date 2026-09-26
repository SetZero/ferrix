//! Reading a section through a table of its keys.
//!
//! Each kind's section, and `[Unit]` and `[Install]`, is a table of key
//! names and the function that applies one assignment of that key. Applying
//! the assignments in order is what makes a later scalar win, a list grow,
//! and an empty assignment clear a list, each by what its function does.
//! A key the table lacks is a warning in systemd's words; one starting with
//! `X-` is the author's own and is ignored in silence, as systemd ignores it.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Display;

use crate::ini::{Assignment, Section};
use crate::value::{self, Span};
use crate::{UnitName, Warnings};

/// What applies one assignment of a key to `T`.
pub(crate) type Setter<T> = fn(&mut T, &Assignment, &mut Warnings);

/// The setter `table` has for `key`.
pub(crate) fn find<T>(table: &[(&str, Setter<T>)], key: &str) -> Option<Setter<T>> {
    table
        .iter()
        .find(|(known, _)| *known == key)
        .map(|&(_, setter)| setter)
}

/// Apply every assignment in `section` to `target` through `keys`.
pub(crate) fn apply<T>(
    section: &Section,
    keys: &[(&str, Setter<T>)],
    target: &mut T,
    warnings: &mut Warnings,
) {
    for assignment in &section.assignments {
        match find(keys, &assignment.key) {
            Some(setter) => setter(target, assignment, warnings),
            None => unknown(&section.name, assignment, warnings),
        }
    }
}

/// Warn about a key nothing knows, unless it is an `X-` key.
pub(crate) fn unknown(section: &str, assignment: &Assignment, warnings: &mut Warnings) {
    if assignment.key.starts_with("X-") {
        return;
    }
    warnings.at(
        assignment,
        format!(
            "Unknown key name '{}' in section '{section}', ignoring.",
            assignment.key
        ),
    );
}

/// Warn that a value did not parse.
pub(crate) fn invalid(assignment: &Assignment, warnings: &mut Warnings, why: impl Display) {
    warnings.at(
        assignment,
        format!(
            "Failed to parse {}= value '{}' ({why}), ignoring.",
            assignment.key, assignment.value
        ),
    );
}

/// Parse with `parse`, warning and returning `None` if it fails.
pub(crate) fn parsed<V, E: Display>(
    assignment: &Assignment,
    warnings: &mut Warnings,
    parse: impl FnOnce(&str) -> Result<V, E>,
) -> Option<V> {
    match parse(&assignment.value) {
        Ok(value) => Some(value),
        Err(error) => {
            invalid(assignment, warnings, error);
            None
        }
    }
}

/// A boolean key.
pub(crate) fn boolean(assignment: &Assignment, warnings: &mut Warnings) -> Option<bool> {
    parsed(assignment, warnings, value::boolean)
}

/// A time span in seconds by default.
pub(crate) fn span(assignment: &Assignment, warnings: &mut Warnings) -> Option<Span> {
    parsed(assignment, warnings, value::seconds)
}

/// A string key; empty means unset.
pub(crate) fn string(assignment: &Assignment) -> Option<String> {
    (!assignment.value.is_empty()).then(|| assignment.value.clone())
}

/// A path key: absolute and normalized, or empty for unset.
pub(crate) fn path(assignment: &Assignment, warnings: &mut Warnings) -> Option<Option<String>> {
    if assignment.value.is_empty() {
        return Some(None);
    }
    if value::is_absolute_path(&assignment.value) {
        Some(Some(assignment.value.clone()))
    } else {
        invalid(assignment, warnings, "not an absolute path");
        None
    }
}

/// Add the unit names of a space-separated list to `list`; an empty value
/// clears it when `clears`, and does nothing otherwise, as systemd's
/// dependency keys cannot be reset.
pub(crate) fn names(
    list: &mut Vec<UnitName>,
    assignment: &Assignment,
    warnings: &mut Warnings,
    clears: bool,
) {
    if assignment.value.is_empty() {
        if clears {
            list.clear();
        }
        return;
    }
    for word in assignment
        .value
        .split([' ', '\t'])
        .filter(|w| !w.is_empty())
    {
        match UnitName::parse(word) {
            Ok(name) if !list.contains(&name) => list.push(name),
            Ok(_) => {}
            Err(error) => warnings.at(
                assignment,
                format!(
                    "Failed to add dependency on '{word}' in {}= ({error}), ignoring.",
                    assignment.key
                ),
            ),
        }
    }
}

/// Add the space-separated words of a value to `list`; an empty value
/// clears it.
pub(crate) fn words(list: &mut Vec<String>, assignment: &Assignment) {
    if assignment.value.is_empty() {
        list.clear();
        return;
    }
    list.extend(
        assignment
            .value
            .split([' ', '\t'])
            .filter(|word| !word.is_empty())
            .map(str::to_owned),
    );
}
