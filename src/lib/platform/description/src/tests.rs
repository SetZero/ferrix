//! One test per way a machine can describe itself.

use core::cell::Cell;

use super::*;

/// Stand-ins for an opened ACPI table set and an opened device tree.
const TABLES: &str = "tables";
const TREE: &str = "tree";

/// Verifies: `L.discovery.1`
#[test]
fn acpi_only_is_read_by_its_tables() {
    let found = choose(Ok::<_, ()>(TABLES), || Err::<&str, _>("no tree"));
    assert_eq!(found, Description::Acpi(TABLES));
}

/// Verifies: `L.discovery.1`
#[test]
fn acpi_beside_a_tree_is_read_by_its_tables() {
    // The tree is there, and would parse, but is never opened.
    let opened = Cell::new(false);
    let found = choose(Ok::<_, ()>(TABLES), || {
        opened.set(true);
        Ok::<_, ()>(TREE)
    });
    assert_eq!(found, Description::Acpi(TABLES));
    assert!(
        !opened.get(),
        "the tree was opened although the ACPI tables were readable"
    );
}

/// Verifies: `L.discovery.1`
#[test]
fn unreadable_tables_beside_a_tree_are_read_by_the_tree() {
    let opened = Cell::new(false);
    let found = choose(Err::<&str, _>("bad RSDP"), || {
        opened.set(true);
        Ok::<_, ()>(TREE)
    });
    assert_eq!(found, Description::Tree(TREE));
    assert!(opened.get(), "the tree is what the machine is read by");
}

/// Verifies: `L.discovery.1`
#[test]
fn a_tree_only_is_read_by_the_tree() {
    let found = choose(Err::<&str, _>("no RSDP"), || Ok::<_, ()>(TREE));
    assert_eq!(found, Description::Tree(TREE));
}

/// Verifies: `L.discovery.1`
#[test]
fn neither_is_neither() {
    let found = choose(Err::<&str, _>("no RSDP"), || Err::<&str, _>("no tree"));
    assert_eq!(found, Description::<&str, &str>::Neither);
}
