//! The merge against libxkbcommon's own two-group keymaps.
//!
//! `probe/reference-de-us.txt` and `probe/reference-us-de.txt` are what
//! libxkbcommon 1.13.1 compiled for `kb_layout = de,us` and `us,de`: the real
//! keymap text, and beside it every group, level, keysym and selecting
//! modifier mask of every key. They are committed, so these tests read them
//! on Windows, where there is no libxkbcommon to ask.
//!
//! The comparison is per key and per group, not textual. A keymap says what
//! every level of every group types through three things: the group's bracket
//! list, the type that says which modifiers reach which entry of it, and the
//! virtual modifier bindings that give those modifiers their real bits. The
//! tests below check that the types section, the compat section, the modifier
//! map and every key's per-group list and type are the reference's, which
//! between them decide every one of the reference's `level` records. The
//! records themselves were compared against libxkbcommon by compiling this
//! module's output with `xkb_keymap_new_from_string` on a host that has it;
//! that cannot run here, and this is what stands in for it.

use std::collections::{BTreeMap, BTreeSet};

use super::{Definition, Section, Statement, braces, merged, sections};
use crate::{MAX_GROUPS, generated, layouts};

/// libxkbcommon's `de,us`, with its keymap text and every level of it.
const DE_US: &str = include_str!("../../probe/reference-de-us.txt");

/// libxkbcommon's `us,de`. The other order is a different keymap, not the
/// same one read backwards.
const US_DE: &str = include_str!("../../probe/reference-us-de.txt");

/// The keymap text out of a probe file's `## keymap` section, which runs to
/// the end of the file.
fn reference(probe: &'static str) -> &'static str {
    let (_, rest) = probe.split_once("\n## keymap ").expect("a keymap section");
    let (_, keymap) = rest.split_once('\n').expect("the section's first line");
    keymap
}

/// The layouts a configuration naming `names` asks for.
fn asked(names: &str) -> Vec<&'static generated::Layout> {
    layouts(names, "")
        .into_iter()
        .map(|(layout, wanted)| {
            assert!(wanted, "{names} names a layout this compositor ships");
            layout
        })
        .collect()
}

/// One section of a keymap, by its place in the four.
fn section(keymap: &str, index: usize) -> Section {
    let mut four = sections(keymap).expect("four sections").into_iter();
    four.nth(index).expect("the section")
}

/// The statements of a section, as the lines they are written as.
fn written(keymap: &str, index: usize) -> Vec<Vec<String>> {
    section(keymap, index)
        .statements
        .into_iter()
        .map(|statement| statement.lines)
        .collect()
}

/// How many groups a merged keymap's symbols section writes.
fn groups(keymap: &str) -> usize {
    (1..=MAX_GROUPS)
        .filter(|group| keymap.contains(&format!("\tname[{group}]=")))
        .count()
}

/// Every group of every key, with a key that defines fewer groups than the
/// keymap has wrapped back to its first.
///
/// The wrap is XKB's default `groupsWrap`, and it is why a key written with
/// one group is not a key that is silent in the others: `<ESC>` is written
/// once in a two-group keymap and types `Escape` under both layouts. A
/// comparison that did not wrap would read a one-group key as a difference
/// from a two-group one that says the same thing twice.
fn per_group(keymap: &str, count: usize) -> BTreeMap<String, Vec<Definition>> {
    let mut keys = BTreeMap::new();
    for statement in section(keymap, 3).statements {
        let Some(name) = statement
            .name
            .as_deref()
            .and_then(|name| name.strip_prefix("key "))
        else {
            continue;
        };
        let written = definitions(&statement);
        assert!(!written.is_empty(), "{name} defines no group");
        let all = (0..count)
            .map(|group| {
                written
                    .get(group % written.len())
                    .cloned()
                    .unwrap_or_else(blank)
            })
            .collect();
        let _ = keys.insert(name.to_owned(), all);
    }
    keys
}

/// The groups a key statement writes, in order, however many it writes.
///
/// The test's own reader rather than the module's [`super::definition`],
/// which by design refuses anything but a single-group key: this one has to
/// read the merged text and libxkbcommon's, both of which write several.
fn definitions(statement: &Statement) -> Vec<Definition> {
    let mut shared = None;
    let mut kinds: BTreeMap<usize, String> = BTreeMap::new();
    let mut symbols: BTreeMap<usize, String> = BTreeMap::new();
    for line in &statement.lines {
        let line = line.trim();
        let quoted = || {
            let (_, rest) = line.split_once('"').expect("a type's name");
            let (name, _) = rest.split_once('"').expect("a closing quote");
            format!("\"{name}\"")
        };
        let list = |text: &str| {
            let (_, rest) = text.split_once('[').expect("a bracket list");
            let (inside, _) = rest.rsplit_once(']').expect("a closing bracket");
            inside.trim().to_owned()
        };
        let index = |text: &str| {
            let (_, rest) = text.split_once('[').expect("a group index");
            let (group, _) = rest.split_once(']').expect("a closing bracket");
            group.parse::<usize>().expect("a number") - 1
        };
        if let Some(rest) = line.strip_prefix("type") {
            if rest.starts_with('[') {
                let _ = kinds.insert(index(line), quoted());
            } else {
                shared = Some(quoted());
            }
        } else if line.starts_with("symbols") {
            let (before, after) = line.split_once('=').expect("an assignment");
            let _ = symbols.insert(index(before), list(after));
        } else if let Some((_, rest)) = line.split_once('{') {
            // `key <AE01> {	[ 0x31, 0x21 ] };`, the whole key on one line.
            if rest.contains('[') {
                let _ = symbols.insert(0, list(rest));
            }
        }
    }
    (0..symbols.len())
        .map(|group| Definition {
            kind: kinds.get(&group).cloned().or_else(|| shared.clone()),
            symbols: symbols.get(&group).cloned().unwrap_or_else(String::new),
        })
        .collect()
}

/// A group that types nothing, which a key of the shipped keymaps never has
/// and which the wrap above therefore never reaches.
fn blank() -> Definition {
    Definition {
        kind: None,
        symbols: String::new(),
    }
}

/// The keys whose group *g* the merged keymap and the reference write
/// differently, split by what differs: the keysyms, or only the type.
fn differences(mine: &str, reference: &str, count: usize) -> (BTreeSet<String>, BTreeSet<String>) {
    let (mine, theirs) = (per_group(mine, count), per_group(reference, count));
    assert_eq!(
        mine.keys().collect::<Vec<_>>(),
        theirs.keys().collect::<Vec<_>>(),
        "the merged keymap and libxkbcommon's define different keys"
    );
    let mut symbols = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for (name, ours) in &mine {
        let theirs = theirs.get(name).expect("the same keys");
        for (ours, theirs) in ours.iter().zip(theirs) {
            if ours.symbols != theirs.symbols {
                let _ = symbols.insert(name.clone());
            } else if ours.kind != theirs.kind {
                let _ = kinds.insert(name.clone());
            }
        }
    }
    (symbols, kinds)
}

/// A keymap libxkbcommon printed is a better keymap than one assembled from
/// it, so one layout must come back exactly as it was committed -- not
/// re-rendered, not reordered, not even reindented.
#[test]
fn one_layout_is_its_own_keymap_unchanged() {
    for layout in &generated::LAYOUTS {
        assert_eq!(merged(&[layout]), layout.keymap, "{}", layout.described());
    }
    // And so is the layout a configuration that says nothing asks for.
    assert_eq!(merged(&asked("")), crate::KEYMAP);
    assert_eq!(merged(&[]), crate::KEYMAP);
}

/// `kb_layout = de,us` is one keymap with two groups, which is the whole
/// point: the client compiles it once and a layout switch afterwards is an
/// index in `wl_keyboard.modifiers`.
#[test]
fn two_layouts_are_one_keymap_with_two_groups() {
    let keymap = merged(&asked("de,us"));
    assert!(keymap.starts_with("xkb_keymap {"));
    assert!(keymap.ends_with("};\n\n};\n"));
    assert!(!keymap.contains('\0'));
    assert_eq!(groups(&keymap), 2);
    assert!(keymap.contains("\tname[1]=\"German\";\n\tname[2]=\"English (US)\";\n"));
    // A key the two layouts write differently carries one list a group.
    assert!(keymap.contains(
        "\tkey <AD01> {\n\t\tsymbols[1]= [ 0x71, 0x51, 0x40, 0x7d9 ],\n\t\tsymbols[2]= [ 0x71, 0x51 ]\n\t};"
    ));
    // A key they agree about is written once, and XKB's group wrapping gives
    // the second group the same meaning.
    assert!(keymap.contains("\tkey <ESC> {\t[ 0xff1b ] };"));
}

/// The order of `kb_layout` decides which layout is group 0, so `de,us` and
/// `us,de` are different keymaps -- the reason both references exist.
#[test]
fn the_group_order_follows_the_argument_order() {
    let (forwards, backwards) = (merged(&asked("de,us")), merged(&asked("us,de")));
    assert_ne!(forwards, backwards);
    assert!(forwards.contains("\tname[1]=\"German\";\n\tname[2]=\"English (US)\";\n"));
    assert!(backwards.contains("\tname[1]=\"English (US)\";\n\tname[2]=\"German\";\n"));
    // `AD01` is `q` in both, but German reaches `@` and `ł` above it; which
    // group has them is what the order decides.
    assert!(forwards.contains("\t\tsymbols[1]= [ 0x71, 0x51, 0x40, 0x7d9 ],"));
    assert!(backwards.contains("\t\tsymbols[2]= [ 0x71, 0x51, 0x40, 0x7d9 ]"));
}

/// Four layouts are XKB's limit, and a configuration that names more gets the
/// first four rather than a keymap libxkbcommon would refuse to compile.
#[test]
fn four_layouts_are_four_groups_and_a_fifth_is_dropped() {
    let four = merged(&asked("de,us,fr,gb"));
    assert_eq!(groups(&four), 4);
    assert!(four.contains("\t\tsymbols[4]= [ 0x71, 0x51, 0x40, 0x7d9 ]"));
    let five = asked("de,us,fr,gb")
        .into_iter()
        .chain(asked("de"))
        .collect::<Vec<_>>();
    assert_eq!(five.len(), MAX_GROUPS + 1);
    assert_eq!(merged(&five), four);
}

/// The merged text is a keymap's four sections in a keymap's order, because
/// that is all a client's libxkbcommon will accept.
#[test]
fn the_merged_keymap_is_the_four_sections() {
    let keymap = merged(&asked("de,us"));
    let four = sections(&keymap).expect("four sections");
    for (section, expected) in four.iter().zip(super::SECTIONS) {
        assert!(section.head.starts_with(expected), "{}", section.head);
        assert!(!section.statements.is_empty(), "{expected} is empty");
    }
    assert_eq!(keymap.lines().map(braces).sum::<isize>(), 0);
    // The sections are the same four for every combination, in the same
    // order, and every one of them parses.
    for names in ["us,de", "de,us,fr,gb", "fr,gb", "gb,us,de"] {
        let keymap = merged(&asked(names));
        assert!(sections(&keymap).is_some(), "{names} is not four sections");
    }
    // A variant is a layout of its own here, and merges like one.
    let variant = layouts("de,us", "nodeadkeys,")
        .into_iter()
        .map(|(layout, _)| layout)
        .collect::<Vec<_>>();
    let keymap = merged(&variant);
    assert!(sections(&keymap).is_some());
    assert_eq!(groups(&keymap), 2);
    assert!(keymap.starts_with("xkb_keymap {"));
}

/// A keymap that is not the shape this module reads costs the merge, not the
/// keyboard: a compositor whose clients cannot compile the keymap is one
/// where nothing can be typed, so the first layout's own text is sent
/// instead.
#[test]
fn an_unreadable_keymap_falls_back_to_the_first_layout() {
    static BROKEN: generated::Layout = generated::Layout {
        name: "broken",
        variant: "",
        source: "this test",
        label: "Broken",
        keymap: "not a keymap",
        keys: &[],
    };
    let us = asked("us");
    let first = us.first().copied().expect("us");
    assert_eq!(merged(&[&BROKEN, first]), "not a keymap");
    assert_eq!(merged(&[first, &BROKEN]), first.keymap);
}

/// Every layout is merged against the same keys, which is what lets the
/// merged keymap keep the first layout's `xkb_keycodes` and its order of
/// keys. A shipped keymap that named a key the others do not would need the
/// keycodes united too, and this test is where that would show up.
#[test]
fn the_shipped_keymaps_define_the_same_keys_in_the_same_order() {
    let names = |keymap: &str| {
        section(keymap, 3)
            .statements
            .into_iter()
            .filter_map(|statement| statement.name)
            .filter(|name| name.starts_with("key "))
            .collect::<Vec<_>>()
    };
    let first = generated::LAYOUTS.first().expect("a layout");
    let expected = names(first.keymap);
    assert_eq!(expected.len(), 458);
    for layout in &generated::LAYOUTS {
        assert_eq!(names(layout.keymap), expected, "{}", layout.described());
    }
}

/// `xkb_keycodes` and `xkb_types` come out as libxkbcommon's own, and the
/// modifier map with them.
///
/// The types are the section a concatenation would break: `us` alone
/// declares ten types and `de` thirteen, because libxkbcommon prints only
/// the ones its keys use, and a German group needs `de`'s
/// `FOUR_LEVEL_SEMIALPHABETIC` to reach `AltGr`. United by name and in the
/// `complete` file's own order, both merges come out with exactly the types
/// libxkbcommon printed, in its order.
#[test]
fn the_keycodes_the_types_and_the_modifier_map_are_libxkbcommons_own() {
    for (names, probe) in [("de,us", DE_US), ("us,de", US_DE)] {
        let (mine, theirs) = (merged(&asked(names)), reference(probe));
        for (index, called) in [(0, "xkb_keycodes"), (1, "xkb_types")] {
            assert_eq!(
                written(&mine, index),
                written(theirs, index),
                "{names}: {called} is not libxkbcommon's"
            );
            assert_eq!(
                section(&mine, index).head,
                section(theirs, index).head,
                "{names}: {called}'s name is not libxkbcommon's"
            );
        }
        let modifier_map = |keymap: &str| {
            section(keymap, 3)
                .statements
                .into_iter()
                .filter(|statement| {
                    statement
                        .name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("modifier_map "))
                })
                .map(|statement| statement.lines)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            modifier_map(&mine),
            modifier_map(theirs),
            "{names}: the modifier map is not libxkbcommon's"
        );
    }
}

/// `xkb_compatibility` comes out as libxkbcommon's plus interprets that fire
/// for nothing.
///
/// libxkbcommon prints only the interprets some key of the keymap uses, and
/// the union cannot know which those are, so it keeps a few more. The two it
/// keeps are named here because "a superset" is only harmless as long as
/// somebody has read what is in it: `Alt_R`'s interpret on `de,us`, where no
/// key types `Alt_R` at all, and a less specific second interpret for
/// `ISO_Level3_Shift` on `us,de` whose action is the same
/// `SetMods(modifiers=LevelThree)` as the one libxkbcommon kept.
#[test]
fn the_compat_section_is_libxkbcommons_own_plus_interprets_nothing_uses() {
    let extra = |names: &str, probe: &'static str| {
        let (mine, theirs) = (merged(&asked(names)), reference(probe));
        let kept = written(theirs, 2);
        let mine = written(&mine, 2);
        for statement in &kept {
            assert!(
                mine.contains(statement),
                "{names}: the merge dropped {statement:?}"
            );
        }
        mine.into_iter()
            .filter(|statement| !kept.contains(statement))
            .flatten()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        extra("de,us", DE_US),
        [
            "\tinterpret 0xffea+AnyOf(all) {",
            "\t\tvirtualModifier= Alt;",
            "\t\taction= SetMods(modifiers=modMapMods,clearLocks);",
            "\t};",
        ]
    );
    assert!(!merged(&asked("de,us")).contains("[ 0xffea"));
    assert_eq!(
        extra("us,de", US_DE),
        [
            "\tinterpret 0xfe03+AnyOfOrNone(all) {",
            "\t\taction= SetMods(modifiers=LevelThree,clearLocks);",
            "\t};",
        ]
    );
}

/// Every key of `us,de`, group by group, is what libxkbcommon compiled.
///
/// Nothing differs but the two keys where libxkbcommon printed a type our
/// text leaves out: it writes `type= "KEYPAD"` on `<KPDL>` and
/// `type= "ONE_LEVEL"` on `<RALT>` for both groups, where the merge writes
/// the type only for the group whose own keymap named one. The other group's
/// keysyms are `[ KP_Delete, KP_Decimal ]` and `[ Alt_R ]`, for which
/// `KEYPAD` and `ONE_LEVEL` are the canonical types anyway -- which is why
/// `us`'s own keymap does not name them either, and it is libxkbcommon that
/// works them out when a client compiles the text.
#[test]
fn every_key_and_group_of_us_de_is_libxkbcommons_own() {
    let (symbols, kinds) = differences(&merged(&asked("us,de")), reference(US_DE), 2);
    assert_eq!(symbols, BTreeSet::new());
    assert_eq!(
        kinds,
        ["<KPDL>", "<RALT>"]
            .map(str::to_owned)
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
}

/// Every key of `de,us`, group by group, is what libxkbcommon compiled --
/// except the two this merge cannot get right, which are pinned here.
///
/// libxkbcommon resolves `de,us` to `pc+de+us:2+inet(evdev)`, in which `us`
/// gives a second group only to the 47 keys its own symbols file defines;
/// the keys a layout inherits from `pc(pc105)` stay at one group and so mean
/// the same under both layouts. A compiled single-group keymap does not say
/// which keys those were, so the merge gives a second group to every key the
/// two texts write differently, and for `de,us` that is two keys more:
/// `<KPDL>`, where the English group types `KP_Decimal` rather than
/// libxkbcommon's `KP_Separator`, and `<LSGT>`, whose fourth level is
/// `brokenbar` rather than `dead_belowmacron`. Both are what those keys type in
/// `us`'s own keymap, so this is a defensible reading of `de,us` and not a
/// broken one -- but it is not libxkbcommon's, and the day the shipped
/// keymaps can say which keys a layout owns, this test is where to start.
///
/// `<RALT>` is *not* in the list, and that is the merge's one deliberate
/// rule: `us` types `Alt_R` on it, whose interpret takes its modifiers from
/// the key's own `modifier_map`, and the merged map is `de`'s, which does not
/// claim `<RALT>`. An English group with its own `<RALT>` would have a dead
/// `AltGr` -- and no other key of this keymap reaches `Mod5` -- so the first
/// layout's `ISO_Level3_Shift` stands in both groups, which is also what
/// libxkbcommon did.
#[test]
fn every_key_and_group_of_de_us_is_libxkbcommons_own_but_two() {
    let (symbols, kinds) = differences(&merged(&asked("de,us")), reference(DE_US), 2);
    let pinned = |keys: &[&str]| {
        keys.iter()
            .map(|key| (*key).to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(symbols, pinned(&["<KPDL>", "<LSGT>"]));
    // Nothing else: no key of `de,us` is written with the reference's
    // keysyms and another type than the reference's.
    assert_eq!(kinds, pinned(&[]));
    let keymap = merged(&asked("de,us"));
    assert!(
        keymap
            .contains("\tkey <RALT> {\n\t\ttype= \"ONE_LEVEL\",\n\t\tsymbols[1]= [ 0xfe03 ]\n\t};")
    );
}
