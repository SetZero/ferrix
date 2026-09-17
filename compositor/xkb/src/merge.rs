//! One keymap with several layout groups, assembled from the shipped ones.
//!
//! # Why this exists
//!
//! `input:kb_layout = de,us` is not two keymaps. Hyprland hands the string to
//! libxkbcommon whole (`IKeyboard.cpp:66-73` passes `rules.layout` verbatim),
//! which compiles **one** keymap with two groups; the client is sent that one
//! text, and a layout switch afterwards only changes the group index in
//! `wl_keyboard.modifiers`. Hyprland's seat even dedupes the keymap by
//! content before sending it (`Seat.cpp:377-380`), so a group switch sends no
//! keymap at all. A compositor that answered `de,us` with two separate
//! keymaps and swapped them would be sending a new file on every switch, and
//! every client would have to recompile it and forget its own state; a
//! compositor that sent only the first would be lying about the second.
//!
//! libxkbcommon compiles such a keymap out of the XKB data files, and this
//! compositor has neither: the data is tens of megabytes of a desktop
//! distribution and libxkbcommon is not on a machine running Ferrix, which is
//! why [`crate::generated`] ships keymaps that libxkbcommon printed on a host
//! (`probe/`). So the multi-group keymap has to be assembled here, out of the
//! single-group texts we were given, and [`merged`] is that assembly.
//!
//! # What the assembly is measured against
//!
//! `probe/reference-de-us.txt` and `probe/reference-us-de.txt` are the real
//! thing: libxkbcommon 1.13.1's own `de,us` and `us,de` keymaps, with every
//! key's groups, levels, keysyms and selecting modifier masks printed beside
//! them. The rules below are read off those two files rather than reasoned
//! out from the XKB specification, and `merge/tests.rs` holds the comparison
//! against them. Where we knowingly differ from libxkbcommon, that test says
//! so by name.
//!
//! # What each section needs
//!
//! An XKB keymap text has four sections, and a keymap is not the
//! concatenation of two keymaps in any of them:
//!
//! * `xkb_keycodes` names the keys and numbers them. The five shipped
//!   keymaps agree on all 458 keys and their codes, and differ only in the
//!   aliases the QWERTY and QWERTZ variants set (`<LatY>`, `<LatZ>`) --
//!   which the real `de,us` resolves by keeping the first layout's, aliases
//!   and section name included. So: a union that keeps the first statement
//!   about each name.
//! * `xkb_types` is where a group's shift levels are decided, and this is
//!   the section a concatenation would break outright: `de` declares
//!   `FOUR_LEVEL_ALPHABETIC` and the others and `us` does not, because
//!   libxkbcommon prints only the types its keys use. A German group in a
//!   keymap carrying only `us`'s types would have no type to name for
//!   `AltGr`, so the types are **united**, by name.
//! * `xkb_compatibility` turns keysyms into actions and binds the virtual
//!   modifiers. It is united the same way. The union can be a superset of
//!   what libxkbcommon prints -- it drops interprets no key of the keymap
//!   uses -- and an interpret for a keysym nothing produces fires for
//!   nothing.
//! * `xkb_symbols` is the merge proper: every key carries one bracket list
//!   per group, `symbols[1]` from the first layout and `symbols[2]` from the
//!   second, with the per-group type beside it.
//!
//! # Three things about `xkb_symbols` that make concatenation wrong
//!
//! 1. **`type=` is not `type[1]=`.** A single-layout keymap writes
//!    `type= "FOUR_LEVEL_PLUS_LOCK"` for the key that needs it, which sets
//!    the type of *every* group. Carried into a merged keymap unchanged it
//!    would give the second layout's two symbols a five-level German type,
//!    and `Caps Lock` on an English `-` would type the fifth level of
//!    nothing. Every type the merge keeps is per group.
//! 2. **`modifier_map` is per key, not per group.** A key belongs to
//!    `Mod1` or to nothing regardless of which layout is in force, so the
//!    merged keymap can only carry one `modifier_map` -- the first layout's,
//!    which is what both references carry. It must not be united: `us` maps
//!    `<RALT>` to `Mod1` and `de` does not, and a `de,us` keymap that
//!    claimed `<RALT>` for `Mod1` would bind the `LevelThree` virtual
//!    modifier to `Mod1` as well, because the interpret that binds it reads
//!    the modifier map of whichever key types `ISO_Level3_Shift` at group 1
//!    level 1 (`useModMapMods=level1`). `Alt+q` would then type `@`.
//! 3. **A group whose definition sets no modifier is worse than no group.**
//!    Because of the point above, a key the merged `modifier_map` does not
//!    claim cannot act as a modifier in *any* group, whatever a later layout
//!    says it types. `de,us` is exactly that case: `us` types `Alt_R` on
//!    `<RALT>` and the interpret for `Alt_R` reads `modMapMods`, which for
//!    `<RALT>` is empty under `de`'s modifier map -- so an English group
//!    with its own `<RALT>` would have a dead `AltGr` key, and no other key
//!    reaches `Mod5`. Such a group keeps the first layout's definition,
//!    which is also what libxkbcommon's own `de,us` does.
//!
//! # Where this is known to differ from libxkbcommon
//!
//! libxkbcommon resolves `de,us` to the symbols expression
//! `pc+de+us:2+inet(evdev)`, in which `us` contributes a second group only
//! for the keys `us`'s own symbols file defines -- the 47 of the
//! alphanumeric block. Keys the layout inherits from `pc(pc105)` stay at one
//! group and so mean the same in both groups. A compiled single-group keymap
//! does not say where a key came from, so the merge cannot tell an inherited
//! key from an overridden one, and it gives a second group to every key the
//! two layouts write differently. For `de,us` that is two keys more than
//! libxkbcommon: `<KPDL>` (`KP_Decimal` where libxkbcommon keeps
//! `KP_Separator`) and `<LSGT>` (`brokenbar` at the fourth level where
//! libxkbcommon keeps `dead_belowmacron`). `merge/tests.rs` pins that list,
//! so the day the shipped keymaps grow a way to say which keys a layout owns,
//! the test is where to start. `us,de` is exact.
//!
//! # One thing a keymap text cannot say
//!
//! Compiling libxkbcommon's own printed `de,us` text gives a keymap whose
//! *levels* are the ones it compiled from the rules, but whose
//! `xkb_keymap_key_get_mods_for_level` answers are fewer: 53 of the 1285
//! level records lose an alternative, `Shift+Lock` and `Shift+Mod2` against
//! level 0. The printer leaves out a type's `map[...]` entries that select
//! level 1, because XKB sends a mask no entry names to level 0 anyway -- so
//! nothing is typed differently, and the loss is the printer's and not this
//! module's: the shipped single-layout keymaps carry it already. Anyone
//! comparing an assembled keymap against `probe/reference-*.txt` through
//! libxkbcommon has to compare against the reference's *text recompiled*,
//! not against its records, or they will read 53 of the printer's omissions
//! as their own mistake.

use crate::{MAX_GROUPS, generated};

/// The keymap text a client is handed for `layouts`, with one group each.
///
/// One layout is its own keymap, returned as it was committed: the common
/// case is not a rewrite, and a text libxkbcommon printed is a better text
/// than anything this module could print for it.
///
/// Several layouts are one keymap whose group *g* is `layouts[g]`, in the
/// order the configuration named them -- `de,us` and `us,de` are different
/// keymaps, and which one a person gets decides what group 0 types. At most
/// [`MAX_GROUPS`] of them, XKB's own limit; a longer list keeps its first
/// four, as libxkbcommon does.
///
/// This never fails. A keymap that is not the shape this module reads --
/// which would mean the committed texts or the printer that made them had
/// changed -- comes back as the first layout's text alone, because one
/// working layout is a keyboard a person can type on and a keymap a client
/// cannot compile is not.
#[must_use]
pub fn merged(layouts: &[&'static generated::Layout]) -> String {
    let layouts = layouts.get(..MAX_GROUPS).unwrap_or(layouts);
    let Some(first) = layouts.first() else {
        return crate::KEYMAP.to_owned();
    };
    if layouts.len() < 2 {
        return first.keymap.to_owned();
    }
    assemble(layouts).unwrap_or_else(|| first.keymap.to_owned())
}

/// The sections a keymap has, in the order libxkbcommon prints them and in
/// the order a keymap must carry them: `xkb_symbols` names the types that
/// `xkb_types` declares.
const SECTIONS: [&str; 4] = [
    "xkb_keycodes",
    "xkb_types",
    "xkb_compatibility",
    "xkb_symbols",
];

/// One top-level statement of a section, as it was written.
///
/// Owned text rather than slices of the keymaps: a keymap is assembled when a
/// configuration is read, once, and 40 kilobytes of copying there buys code
/// that can be read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Statement {
    /// What the statement is *about*: `<AD01>`, `type "KEYPAD"`,
    /// `modifier_map Mod1`. Two layouts that say something about the same
    /// thing name it the same way here, which is what lets a union keep one
    /// of the two. `None` for a blank line, which is about nothing and is
    /// kept only to print the section back the way it was written.
    name: Option<String>,
    /// Its lines, newlines excluded.
    lines: Vec<String>,
}

/// One section of a keymap.
#[derive(Clone, Debug)]
struct Section {
    /// The opening line, `xkb_types "complete" {`.
    head: String,
    statements: Vec<Statement>,
}

/// What one group of one key types: its bracket list, and the type that says
/// which modifiers reach which of its entries.
///
/// `kind` is `None` where the keymap named no type, which is not "no type":
/// libxkbcommon prints a type only where it is not the canonical one for the
/// symbols, and the canonical one is worked out again by whoever compiles the
/// text. Leaving it out is therefore how a group is written, not an omission.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Definition {
    /// The type's name with its quotes, `"FOUR_LEVEL_PLUS_LOCK"`.
    kind: Option<String>,
    /// The keysyms between the brackets, `0x31, 0x21`.
    symbols: String,
}

/// The whole assembly. `None` where a keymap is not the shape read here.
fn assemble(layouts: &[&'static generated::Layout]) -> Option<String> {
    let parsed = layouts
        .iter()
        .map(|layout| sections(layout.keymap))
        .collect::<Option<Vec<_>>>()?;
    let at = |index: usize| {
        parsed
            .iter()
            .filter_map(|four| four.get(index))
            .collect::<Vec<_>>()
    };
    let keycodes = unite(&at(0))?;
    let types = unite(&at(1))?;
    let compat = unite(&at(2))?;
    // The symbols merge asks the compat section which keysyms take their
    // modifiers from the key's own modifier map, so it is built last.
    let symbols = symbols(&at(3), layouts, &modmap_driven(&compat))?;

    let mut text = String::from("xkb_keymap {\n");
    for section in [&keycodes, &types, &compat, &symbols] {
        text.push_str(&section.head);
        text.push('\n');
        for statement in &section.statements {
            for line in &statement.lines {
                text.push_str(line);
                text.push('\n');
            }
        }
        text.push_str("};\n\n");
    }
    text.push_str("};\n");
    Some(text)
}

/// A keymap's four sections, or `None` if it does not have exactly those four
/// in that order.
fn sections(keymap: &str) -> Option<[Section; 4]> {
    let mut lines = keymap.lines();
    if lines.next()?.trim() != "xkb_keymap {" {
        return None;
    }
    let mut found: Vec<Section> = Vec::new();
    let mut head: Option<&str> = None;
    let mut statements: Vec<Statement> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut depth: isize = 0;
    for line in lines {
        let trimmed = line.trim();
        let Some(open) = head else {
            // Between the sections: the blank line that separates them, and
            // the keymap's own closing brace after the last of them.
            if trimmed.is_empty() || trimmed == "};" {
                continue;
            }
            if !trimmed.starts_with("xkb_") || !trimmed.ends_with('{') {
                return None;
            }
            head = Some(line);
            continue;
        };
        if pending.is_empty() && trimmed == "};" {
            found.push(Section {
                head: open.to_owned(),
                statements: std::mem::take(&mut statements),
            });
            head = None;
            continue;
        }
        if pending.is_empty() {
            name = named(line).map(str::to_owned);
        }
        depth += braces(line);
        pending.push(line.to_owned());
        if depth <= 0 {
            statements.push(Statement {
                name: name.take(),
                lines: std::mem::take(&mut pending),
            });
            depth = 0;
        }
    }
    if head.is_some() || !pending.is_empty() {
        return None;
    }
    let four: [Section; 4] = found.try_into().ok()?;
    for (section, expected) in four.iter().zip(SECTIONS) {
        if !section.head.starts_with(expected) {
            return None;
        }
    }
    Some(four)
}

/// How much deeper the braces on this line go. A quoted brace would fool it;
/// nothing in the shipped keymaps has one, and an indicator called `{` would
/// only cost the merge its fallback.
fn braces(line: &str) -> isize {
    let count = |brace| isize::try_from(line.matches(brace).count()).unwrap_or(0);
    count('{') - count('}')
}

/// What a statement is about, from the line it begins with.
///
/// The left-hand side of an assignment (`<AD01> = 24;`, `name[1]="German";`),
/// or what stands before the brace of a block (`key <AD01> {`,
/// `type "KEYPAD" {`, `modifier_map Mod1 { ... };`), or, failing both, the
/// line itself. The point is only that two layouts saying the same thing
/// agree on it: `alias <LatY> = <AD06>;` and `alias <LatY> = <AB01>;` are two
/// statements about one alias, and a keymap may carry only one of them.
fn named(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some((before, _)) = line.split_once('{') {
        return Some(before.trim_end());
    }
    if let Some((before, _)) = line.split_once('=') {
        return Some(before.trim_end());
    }
    Some(line)
}

/// The sections' statements as one, keeping the first layout's wherever two
/// of them are about the same thing.
///
/// A statement of a later layout that the result has nothing about goes in
/// where it stood relative to the statements it does have, rather than at the
/// end: the sections all come from one `complete` file, so this reproduces
/// that file's order, and `de,us` and `us,de` both come out with the types in
/// libxkbcommon's own order.
fn unite(sections: &[&Section]) -> Option<Section> {
    let (first, rest) = sections.split_first()?;
    let mut statements = first.statements.clone();
    for section in rest {
        let mut at = 0;
        for statement in &section.statements {
            if statement.name.is_none() {
                continue;
            }
            if let Some(found) = statements
                .iter()
                .position(|kept| kept.name == statement.name)
            {
                at = found + 1;
            } else {
                statements.insert(at.min(statements.len()), statement.clone());
                at += 1;
            }
        }
    }
    Some(Section {
        head: first.head.clone(),
        statements,
    })
}

/// The keysyms whose action a keymap takes from the pressed key's own
/// `modifier_map`.
///
/// Read out of the merged compat section rather than listed here: an
/// interpret says which keysym it is for and what it does, and the ones that
/// matter are those whose action reads `modMapMods` --
/// `SetMods(modifiers=modMapMods)` for `Alt_L`, `Alt_R`, `Meta_L`, `Super_L`
/// and `Super_R` in the shipped compat sections. A key the modifier map does
/// not claim, typing one of these, sets nothing at all; `Num_Lock` and
/// `ISO_Level3_Shift` name their modifier in the action instead and work
/// wherever they are put. The catch-all `interpret Any+AnyOf(all)` is skipped
/// because `AnyOf(all)` cannot match a key with an empty modifier map, which
/// is the only case this list is consulted for.
fn modmap_driven(compat: &Section) -> Vec<String> {
    compat
        .statements
        .iter()
        .filter(|statement| {
            statement
                .lines
                .iter()
                .any(|line| line.contains("modMapMods"))
        })
        .filter_map(|statement| {
            let interpret = statement.name.as_deref()?.strip_prefix("interpret ")?;
            let keysym = interpret.split('+').next()?.trim();
            (keysym != "Any").then(|| keysym.to_owned())
        })
        .collect()
}

/// The merged `xkb_symbols`: the first layout's section, with every key that
/// the layouts write differently given one bracket list per group.
fn symbols(
    sections: &[&Section],
    layouts: &[&'static generated::Layout],
    driven: &[String],
) -> Option<Section> {
    let united = unite(sections)?;
    // The modifier map the merged keymap will carry, which is the first
    // layout's: `unite` kept it, and a union of two would misbind the virtual
    // modifiers. See the module's documentation.
    let claimed = claimed(&united);
    let mut statements = Vec::with_capacity(united.statements.len() + layouts.len());
    for statement in &united.statements {
        match statement.name.as_deref() {
            Some(name) if name.starts_with("key ") => {
                statements.push(key(name, sections, layouts.len(), driven, &claimed)?);
            }
            // One name a group, in place of the single layout's one name.
            // These are the layouts' own XKB names ("German",
            // "English (US)"), which is what a client shows a person who
            // asks which layout is in force.
            Some("name[1]") => statements.extend(names(sections, layouts)),
            _ => statements.push(statement.clone()),
        }
    }
    Some(Section {
        head: head(&united.head, layouts),
        statements,
    })
}

/// The keys some `modifier_map` of this section claims for a real modifier.
fn claimed(symbols: &Section) -> Vec<String> {
    let mut keys = Vec::new();
    for statement in &symbols.statements {
        if !statement
            .name
            .as_deref()
            .is_some_and(|name| name.starts_with("modifier_map "))
        {
            continue;
        }
        for line in &statement.lines {
            for token in line.split(',') {
                let Some((_, rest)) = token.split_once('<') else {
                    continue;
                };
                let Some((key, _)) = rest.split_once('>') else {
                    continue;
                };
                keys.push(format!("<{key}>"));
            }
        }
    }
    keys
}

/// One key of the merged keymap: the statement it is written as.
fn key(
    name: &str,
    sections: &[&Section],
    groups: usize,
    driven: &[String],
    claimed: &[String],
) -> Option<Statement> {
    let written = |group: usize| {
        sections
            .get(group)?
            .statements
            .iter()
            .find(|statement| statement.name.as_deref() == Some(name))
    };
    // The definition every group falls back to. XKB sends a group index past
    // a key's last group back to its first (the default `groupsWrap`), so a
    // key a layout says nothing about means in that group what it means in
    // the first -- which is also what the real `de,us` does for every key
    // `us` inherits rather than defines.
    let (kept, first) = (0..groups)
        .find_map(|group| written(group).map(|statement| (statement, definition(statement))))?;
    let first = first?;
    // Whether the merged `modifier_map` claims this key, which decides
    // whether a group of it can act as a modifier at all.
    let bare = name.strip_prefix("key ").unwrap_or(name).trim();
    let unclaimed = !claimed.iter().any(|claim| claim == bare);
    let mut definitions = Vec::with_capacity(groups);
    for group in 0..groups {
        let mine = match written(group) {
            Some(statement) => definition(statement)?,
            None => first.clone(),
        };
        // A definition that could only be inert in the merged keymap is not
        // worth a group of its own; the first layout's stands in its place.
        let inert = unclaimed
            && !mine.symbols.is_empty()
            && mine
                .symbols
                .split(',')
                .all(|keysym| driven.iter().any(|sym| sym == keysym.trim()));
        definitions.push(if inert { first.clone() } else { mine });
    }
    // Every group the same is one group: the text stays the one libxkbcommon
    // printed, short form and all, and XKB's group wrapping gives the other
    // groups the same meaning. The real `de,us` writes `<ESC>` that way.
    if definitions.iter().all(|mine| *mine == first) {
        return Some(kept.clone());
    }
    Some(render(name, &definitions))
}

/// What one group of a key types, read off the statement it is written as.
///
/// `None` for anything this module has not seen a keymap print: a source
/// group other than the first, an `actions[...]` list, a key whose brackets
/// it cannot find. The merge then falls back to the first layout alone rather
/// than guess.
fn definition(statement: &Statement) -> Option<Definition> {
    let mut kind = None;
    let mut symbols = None;
    for line in &statement.lines {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("type") {
            let rest = rest.strip_prefix("[1]").unwrap_or(rest);
            let (_, quoted) = rest.split_once('"')?;
            let (name, _) = quoted.split_once('"')?;
            kind = Some(format!("\"{name}\""));
        } else if let Some(rest) = line.strip_prefix("symbols") {
            let rest = rest.strip_prefix("[1]")?;
            symbols = Some(brackets(rest)?);
        } else if line.starts_with("key ") {
            // The short form, whole on its line: `key <AE01> {	[ 0x31 ] };`.
            let (_, rest) = line.split_once('{')?;
            if rest.contains('[') {
                symbols = Some(brackets(rest)?);
            }
        } else if line != "};" && line != "}" {
            return None;
        }
    }
    Some(Definition {
        kind,
        symbols: symbols?,
    })
}

/// The keysyms between the first `[` of `text` and its last `]`.
fn brackets(text: &str) -> Option<String> {
    let (_, rest) = text.split_once('[')?;
    let (inside, _) = rest.rsplit_once(']')?;
    Some(inside.trim().to_owned())
}

/// A key with a definition of its own for each group, written the way
/// libxkbcommon writes one.
///
/// One `type=` where every group has the same one, as libxkbcommon prints
/// `<KPDL>` on `us,de`; `type[g]=` otherwise, as it prints `<AE11>` on
/// `de,us`. A group whose definition names no type gets none, and whoever
/// compiles the text works out the canonical type for its symbols -- which is
/// what the single-layout keymap it came from relied on too.
fn render(name: &str, definitions: &[Definition]) -> Statement {
    let mut lines = vec![format!("\t{name} {{")];
    let kinds = definitions
        .iter()
        .map(|mine| mine.kind.as_deref())
        .collect::<Vec<_>>();
    let shared = kinds
        .first()
        .copied()
        .flatten()
        .filter(|kind| kinds.iter().all(|other| *other == Some(*kind)));
    if let Some(shared) = shared {
        lines.push(format!("\t\ttype= {shared},"));
    } else {
        for (group, kind) in kinds.iter().enumerate() {
            if let Some(kind) = kind {
                lines.push(format!("\t\ttype[{}]= {kind},", group + 1));
            }
        }
    }
    let last = definitions.len().saturating_sub(1);
    for (group, mine) in definitions.iter().enumerate() {
        let comma = if group == last { "" } else { "," };
        lines.push(format!(
            "\t\tsymbols[{}]= [ {} ]{comma}",
            group + 1,
            mine.symbols
        ));
    }
    lines.push("\t};".to_owned());
    Statement {
        name: Some(name.to_owned()),
        lines,
    }
}

/// One `name[g]` a group, from each layout's own.
fn names(sections: &[&Section], layouts: &[&'static generated::Layout]) -> Vec<Statement> {
    let mut statements = Vec::with_capacity(layouts.len());
    for (group, layout) in layouts.iter().enumerate() {
        let written = sections.get(group).and_then(|section| {
            section
                .statements
                .iter()
                .find(|statement| statement.name.as_deref() == Some("name[1]"))
                .and_then(|statement| statement.lines.first())
                .and_then(|line| line.split_once('='))
                .map(|(_, value)| value.trim().to_owned())
        });
        // A keymap with no name of its own is answered with the name the
        // configuration used, which is at least the layout a person asked
        // for.
        let value = written.unwrap_or_else(|| format!("\"{}\";", layout.name));
        statements.push(Statement {
            name: Some(format!("name[{}]", group + 1)),
            lines: vec![format!("\tname[{}]={value}", group + 1)],
        });
    }
    statements
}

/// The merged `xkb_symbols` opening line.
///
/// libxkbcommon's name for a section is the rules expression it resolved --
/// `pc_de_us_2_inet(evdev)` for `de,us`, which is `pc+de+us:2+inet(evdev)`
/// with its punctuation flattened. We cannot resolve the rules, so the first
/// layout's name keeps the later layouts appended in XKB's own
/// `+layout(variant):group` spelling. Nothing reads it -- a keymap's meaning
/// is in its statements -- but a person reading what a client was sent should
/// see which layouts are in it.
fn head(head: &str, layouts: &[&'static generated::Layout]) -> String {
    let mut extra = String::new();
    for (group, layout) in layouts.iter().enumerate().skip(1) {
        extra.push('+');
        extra.push_str(layout.name);
        if !layout.variant.is_empty() {
            extra.push_str(&format!("({})", layout.variant));
        }
        extra.push_str(&format!(":{}", group + 1));
    }
    match head.rsplit_once('"') {
        Some((before, after)) => format!("{before}{extra}\"{after}"),
        None => head.to_owned(),
    }
}

#[cfg(test)]
mod tests;
