//! The tables against the probe's own output, and the state against what
//! libxkbcommon's state machine did when the probe asked it.

use super::{KEYMAP, XKB_OFFSET, code_of, generated, key, layout};

/// The evdev codes the tests name, from `linux/input-event-codes.h`.
const KEY_ESC: u16 = 1;
const KEY_Q: u16 = 16;
const KEY_ENTER: u16 = 28;
const KEY_LEFTCTRL: u16 = 29;
const KEY_A: u16 = 30;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTALT: u16 = 56;
const KEY_CAPSLOCK: u16 = 58;
const KEY_NUMLOCK: u16 = 69;
const KEY_LEFTMETA: u16 = 125;
const KEY_Y: u16 = 21;
const KEY_Z: u16 = 44;

/// The keymap is the text a client compiles, so the one thing that must be
/// true of it is that it is a whole `xkb_keymap` block and nothing else.
#[test]
fn the_keymap_is_a_whole_one() {
    assert!(KEYMAP.starts_with("xkb_keymap {"));
    assert!(KEYMAP.trim_end().ends_with("};"));
    for section in [
        "xkb_keycodes",
        "xkb_types",
        "xkb_compatibility",
        "xkb_symbols",
    ] {
        assert!(KEYMAP.contains(section), "the keymap has no {section}");
    }
    // The client is told the length including a terminating NUL, which the
    // text itself must not already hold.
    assert!(!KEYMAP.contains('\0'));
}

/// The keymap numbers its keys from eight, and Wayland's keycodes are
/// evdev's; a compositor that sent the keymap's numbers would type the letter
/// eight keys along.
#[test]
fn the_keymap_numbers_keys_eight_above_evdev() {
    assert_eq!(XKB_OFFSET, 8);
    assert!(KEYMAP.contains("minimum = 8;"));
    let q = key(KEY_Q).expect("the keymap has Q");
    assert_eq!(q.name, "AD01");
    assert!(KEYMAP.contains(&format!(
        "<{}> = {};",
        q.name,
        u32::from(q.code) + XKB_OFFSET
    )));
}

#[test]
fn a_key_is_found_by_its_code_and_by_its_keysyms_name() {
    let q = key(KEY_Q).expect("Q");
    assert_eq!(q.plain(), Some("q"));
    assert_eq!(q.shifted(), Some("Q"));
    assert_eq!(code_of("q"), Some(KEY_Q));
    assert_eq!(code_of("Q"), Some(KEY_Q));
    assert_eq!(code_of("Return"), Some(KEY_ENTER));
    assert_eq!(code_of("escape"), Some(KEY_ESC));
    assert_eq!(code_of("no such key"), None);
}

/// Every layout's table is searched by code, which only works if it is
/// sorted; a table out of order would answer `None` for a key that is in it.
#[test]
fn the_table_is_in_order_of_the_code_and_has_no_duplicate() {
    for layout in &generated::LAYOUTS {
        let codes: Vec<u16> = layout.keys.iter().map(|key| key.code).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(codes, sorted, "{}", layout.described());
        assert!(
            codes.len() > 100,
            "{}: only {} keys",
            layout.described(),
            codes.len()
        );
    }
}

/// The bits are the keymap's, and the keymap declares them in the order
/// `Shift`, `Lock`, `Control`, `Mod1`..`Mod5`.
#[test]
fn a_modifiers_bit_is_the_one_the_keymap_gives_it() {
    assert_eq!(generated::SHIFT, 1 << 0);
    assert_eq!(generated::LOCK, 1 << 1);
    assert_eq!(generated::CONTROL, 1 << 2);
    assert_eq!(generated::MOD1, 1 << 3);
    assert_eq!(generated::MOD2, 1 << 4);
    assert_eq!(generated::MOD4, 1 << 6);
    for layout in &generated::LAYOUTS {
        assert!(
            layout.source.contains("rules evdev"),
            "{}",
            layout.described()
        );
    }
}

/// `input:kb_layout` and `input:kb_variant` pick a keymap, and a layout this
/// compositor does not ship says so rather than quietly typing English.
///
/// The whole point of the keyword. A person with a German keyboard writes
/// `kb_layout = de` and expects `y` where the key says `y`; a compositor
/// that took the setting and sent the US keymap anyway would swap `y` and
/// `z` on them and give no reason.
#[test]
fn a_configuration_picks_the_keymap_it_names() {
    let described = |name: &str, variant: &str| {
        let (layout, exact) = layout(name, variant);
        (layout.described(), exact)
    };
    assert_eq!(
        described("", ""),
        ("us".to_owned(), true),
        "Hyprland's default"
    );
    assert_eq!(described("us", ""), ("us".to_owned(), true));
    assert_eq!(described("de", ""), ("de".to_owned(), true));
    assert_eq!(
        described("de", "nodeadkeys"),
        ("de, nodeadkeys".to_owned(), true)
    );
    // A variant that is not shipped falls back to the same layout's plain
    // form: a German keyboard with the wrong dead keys is far closer to
    // right than an American one.
    assert_eq!(described("de", "neo"), ("de".to_owned(), false));
    // And a layout that is not shipped falls back to `us`, and says so.
    assert_eq!(described("ru", ""), ("us".to_owned(), false));

    // The letters are the keyboard's, which is the only thing that matters.
    let (german, _) = layout("de", "nodeadkeys");
    let (american, _) = layout("us", "");
    assert_eq!(german.key(KEY_Y).and_then(|key| key.plain()), Some("z"));
    assert_eq!(american.key(KEY_Y).and_then(|key| key.plain()), Some("y"));
    assert_eq!(german.code_of("z"), Some(KEY_Y));
    assert_eq!(american.code_of("z"), Some(KEY_Z));
    assert!(
        german.keymap.contains("nodeadkeys") || german.keymap.len() > 10_000,
        "the German keymap is a real one"
    );
}

/// What libxkbcommon's state machine said when the probe pressed each key.
#[test]
fn a_modifier_key_holds_what_libxkbcommon_said_it_holds() {
    let held = |code| key(code).expect("a key").held;
    assert_eq!(held(KEY_LEFTSHIFT), generated::SHIFT);
    assert_eq!(held(KEY_LEFTCTRL), generated::CONTROL);
    assert_eq!(held(KEY_LEFTALT), generated::MOD1);
    assert_eq!(held(KEY_LEFTMETA), generated::MOD4);
    assert_eq!(held(KEY_A), 0);

    let locked = |code| key(code).expect("a key").locked;
    assert_eq!(locked(KEY_CAPSLOCK), generated::LOCK);
    assert_eq!(locked(KEY_NUMLOCK), generated::MOD2);
    assert_eq!(locked(KEY_LEFTSHIFT), 0);
}

#[test]
fn holding_a_modifier_shows_in_the_masks_and_releasing_it_stops() {
    let mut keyboard = super::Keyboard::new();
    assert_eq!(keyboard.modifiers().depressed, 0);

    assert!(keyboard.key(KEY_LEFTSHIFT, true));
    assert!(keyboard.key(KEY_LEFTCTRL, true));
    assert_eq!(
        keyboard.modifiers().depressed,
        generated::SHIFT | generated::CONTROL
    );
    assert_eq!(keyboard.pressed(), [KEY_LEFTSHIFT, KEY_LEFTCTRL]);

    assert!(keyboard.key(KEY_LEFTSHIFT, false));
    assert_eq!(keyboard.modifiers().depressed, generated::CONTROL);
    assert!(keyboard.is_held(KEY_LEFTCTRL));
}

/// evdev repeats a held key as value 2, which arrives here as another press.
/// `wl_keyboard.key` has no repeat -- the client repeats for itself from
/// `repeat_info` -- so a repeat must change nothing and say so.
#[test]
fn a_repeat_of_a_held_key_changes_nothing() {
    let mut keyboard = super::Keyboard::new();
    assert!(keyboard.key(KEY_A, true));
    assert!(!keyboard.key(KEY_A, true));
    assert_eq!(keyboard.pressed(), [KEY_A]);
    assert!(keyboard.key(KEY_A, false));
    assert!(!keyboard.key(KEY_A, false));
    assert!(keyboard.pressed().is_empty());
}

#[test]
fn a_lock_toggles_on_the_press_and_survives_the_release() {
    let mut keyboard = super::Keyboard::new();
    let _ = keyboard.key(KEY_CAPSLOCK, true);
    assert_eq!(keyboard.modifiers().locked, generated::LOCK);
    let _ = keyboard.key(KEY_CAPSLOCK, false);
    assert_eq!(keyboard.modifiers().locked, generated::LOCK);

    let _ = keyboard.key(KEY_CAPSLOCK, true);
    let _ = keyboard.key(KEY_CAPSLOCK, false);
    assert_eq!(keyboard.modifiers().locked, 0);

    // Two locks are two bits, not one.
    let _ = keyboard.key(KEY_CAPSLOCK, true);
    let _ = keyboard.key(KEY_NUMLOCK, true);
    assert_eq!(
        keyboard.modifiers().locked,
        generated::LOCK | generated::MOD2
    );
}

/// Latched modifiers are not kept, and the crate says so; a test that did
/// not pin it would let one appear by accident. A keyboard with one layout
/// reports group zero, which is every keyboard until a configuration names a
/// second layout.
#[test]
fn nothing_is_ever_latched_and_one_layout_is_group_zero() {
    let mut keyboard = super::Keyboard::new();
    let _ = keyboard.key(KEY_LEFTSHIFT, true);
    let _ = keyboard.key(KEY_CAPSLOCK, true);
    let modifiers = keyboard.modifiers();
    assert_eq!(modifiers.latched, 0);
    assert_eq!(modifiers.group, 0);

    keyboard.clear();
    assert_eq!(keyboard.modifiers(), super::Modifiers::default());
}

/// The group cycles and wraps, which is what `hyprctl switchxkblayout next`
/// and `prev` do. Hyprland asks for the index *after* the last one and lets
/// libxkbcommon's modulus bring it back, so a state that refused an
/// out-of-range index would stop `next` cycling.
#[test]
fn the_layout_group_cycles_and_wraps_both_ways() {
    let mut keyboard = super::Keyboard::new();
    keyboard.set_groups(3);
    assert_eq!(keyboard.groups(), 3);
    assert_eq!(keyboard.group(), 0);

    assert!(keyboard.next_group());
    assert_eq!(keyboard.group(), 1);
    assert!(keyboard.next_group());
    assert_eq!(keyboard.group(), 2);
    // Off the end, back to the first.
    assert!(keyboard.next_group());
    assert_eq!(keyboard.group(), 0);
    // And backwards off the start, to the last.
    assert!(keyboard.previous_group());
    assert_eq!(keyboard.group(), 2);

    // An index beyond the last is brought back by modulus, as an effective
    // layout out of range is in libxkbcommon.
    assert!(keyboard.set_group(4));
    assert_eq!(keyboard.group(), 1);
    // And a set to what is already in force changed nothing, which is what
    // stops a redundant `wl_keyboard.modifiers` going out.
    assert!(!keyboard.set_group(1));
}

/// One layout is one group: `next` on it stays put and reports no change, so
/// a keyboard nobody configured a second layout for sends no modifiers event
/// when a key bound to the switch is pressed.
#[test]
fn a_keyboard_with_one_layout_has_nowhere_to_switch() {
    let mut keyboard = super::Keyboard::new();
    assert_eq!(keyboard.groups(), 1);
    assert!(!keyboard.next_group());
    assert!(!keyboard.previous_group());
    assert_eq!(keyboard.group(), 0);
}

/// A keymap change that leaves fewer layouts than the group in force brings
/// the group inside the new keymap rather than reporting one no client could
/// look up.
#[test]
fn shrinking_the_keymap_brings_the_group_inside_it() {
    let mut keyboard = super::Keyboard::new();
    keyboard.set_groups(4);
    assert!(keyboard.set_group(3));
    keyboard.set_groups(2);
    assert_eq!(keyboard.groups(), 2);
    assert_eq!(keyboard.group(), 1);
    assert_eq!(keyboard.modifiers().group, 1);

    // Zero layouts is not a keymap a client could be told about.
    keyboard.set_groups(0);
    assert_eq!(keyboard.groups(), 1);
    assert_eq!(keyboard.group(), 0);
}

/// The group survives losing the keys: it is what the configuration and the
/// switch said, not a finger's doing.
#[test]
fn clearing_the_keys_keeps_the_layout() {
    let mut keyboard = super::Keyboard::new();
    keyboard.set_groups(2);
    assert!(keyboard.next_group());
    let _ = keyboard.key(KEY_LEFTSHIFT, true);
    keyboard.clear();
    assert_eq!(keyboard.group(), 1);
    assert_eq!(keyboard.modifiers().depressed, 0);
}

/// `input:kb_layout = de,us` is two groups, and the variants are read
/// alongside the names: the pairing is positional, as XKB's own grammar has
/// it, so a variant for the first layout and none for the second is
/// `nodeadkeys,`.
#[test]
fn a_comma_separated_layout_list_is_a_layout_each() {
    let names = |asked: &str, variants: &str| {
        super::layouts(asked, variants)
            .into_iter()
            .map(|(layout, exact)| (layout.name, layout.variant, exact))
            .collect::<Vec<_>>()
    };
    assert_eq!(names("de,us", ""), [("de", "", true), ("us", "", true)]);
    assert_eq!(
        names("de,us", "nodeadkeys,"),
        [("de", "nodeadkeys", true), ("us", "", true)]
    );
    // A person aligning their configuration file has not changed it.
    assert_eq!(names("de , us", ""), [("de", "", true), ("us", "", true)]);
    // One layout is still a list of one, and an empty list is the default.
    assert_eq!(names("de", ""), [("de", "", true)]);
    assert_eq!(names("", ""), [("us", "", true)]);
}

/// A layout the compositor does not ship is reported as not the one asked
/// for, in a list as on its own: a person whose second group silently became
/// English would have no way to tell.
#[test]
fn a_layout_that_is_not_shipped_says_so_inside_a_list() {
    let asked = super::layouts("de,ru", "");
    assert_eq!(asked.len(), 2);
    assert_eq!((asked[0].0.name, asked[0].1), ("de", true));
    assert_eq!((asked[1].0.name, asked[1].1), ("us", false));
}

/// XKB allows four groups and libxkbcommon refuses the fifth, so a
/// configuration naming more gets the first four rather than a keymap
/// libxkbcommon would reject.
#[test]
fn at_most_four_groups_are_taken() {
    let asked = super::layouts("de,us,fr,gb,us", "");
    assert_eq!(asked.len(), super::MAX_GROUPS);
    let names = asked
        .iter()
        .map(|(layout, _)| layout.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["de", "us", "fr", "gb"]);
}

/// `AltGr` is the third level, and on a German keyboard it is where the
/// characters a shell is written with live. A table that stopped at `Shift`
/// could not reach any of them, which is what this is here to keep.
///
/// The codes are `q`, `8`, the key beside the left shift, and the one right
/// of `ü`; the keysyms are libxkbcommon's own names for what they make.
#[test]
fn altgr_reaches_the_third_level_of_a_german_keyboard() {
    const KEY_8: u16 = 9;
    const KEY_PLUS: u16 = 27;
    const KEY_LSGT: u16 = 86;
    const ALTGR: u32 = generated::MOD5;

    let (german, exact) = layout("de", "");
    assert!(exact);
    let made = |code, modifiers| german.key(code).and_then(|key| key.keysym(modifiers));

    assert_eq!(made(KEY_Q, 0), Some("q"));
    assert_eq!(made(KEY_Q, generated::SHIFT), Some("Q"));
    assert_eq!(made(KEY_Q, ALTGR), Some("at"));

    assert_eq!(made(KEY_8, 0), Some("8"));
    assert_eq!(made(KEY_8, generated::SHIFT), Some("parenleft"));
    assert_eq!(made(KEY_8, ALTGR), Some("bracketleft"));

    assert_eq!(made(KEY_LSGT, ALTGR), Some("bar"));
    assert_eq!(made(KEY_PLUS, ALTGR), Some("asciitilde"));

    // And the characters they are, which is what a terminal writes.
    assert_eq!(super::character("at"), Some('@'));
    assert_eq!(super::character("bar"), Some('|'));
    assert_eq!(super::character("asciitilde"), Some('~'));
    assert_eq!(super::character("bracketleft"), Some('['));
}

/// A modifier the key's type does not care about does not change its level.
///
/// XKB narrows the active modifiers to the ones the key declares before
/// looking a level up, so `Shift` with `Super` held is still the shifted
/// level. A lookup that asked for an exact mask would fall back to the plain
/// level instead and type the wrong character for anybody holding a
/// modifier a bind uses.
#[test]
fn a_modifier_the_key_ignores_does_not_change_its_level() {
    const KEY_8: u16 = 9;
    let (german, _) = layout("de", "");
    let made = |modifiers| german.key(KEY_8).and_then(|key| key.keysym(modifiers));

    assert_eq!(made(generated::SHIFT), Some("parenleft"));
    assert_eq!(made(generated::SHIFT | generated::MOD4), Some("parenleft"));
    assert_eq!(
        made(generated::SHIFT | generated::CONTROL),
        Some("parenleft")
    );
    assert_eq!(made(generated::MOD5 | generated::MOD4), Some("bracketleft"));
    // A combination that selects nothing is the plain level, never nothing.
    assert_eq!(made(generated::MOD3), Some("8"));
    assert_eq!(made(generated::MOD4), Some("8"));
}

/// Every key of every shipped layout answers something at every level it
/// declares, and `level` never points past the levels there are. A generated
/// table that lost a level to a parsing slip would show up here rather than
/// as a key that types nothing.
#[test]
fn every_level_of_every_layout_is_reachable_by_its_own_mask() {
    for layout in &generated::LAYOUTS {
        for key in layout.keys {
            for (index, level) in key.levels.iter().enumerate() {
                for mask in level.masks {
                    assert_eq!(
                        key.level(*mask),
                        index,
                        "{}: {} at {mask:#x} should be level {index}",
                        layout.name,
                        key.name
                    );
                }
            }
            // A mask no level names is the plain level.
            assert!(key.level(u32::MAX) < key.levels.len().max(1));
        }
    }
}

/// The levels beyond the second are not searched for a bind, because
/// Hyprland does not search them either: it resolves a bind against a state
/// with no modifiers applied, so no bind of its reaches a third level. A
/// person who wants such a key writes `code:NN`.
#[test]
fn a_bind_does_not_resolve_to_a_third_level_keysym() {
    let (german, _) = layout("de", "");
    // `at` is AltGr+q on this keymap, and naming it resolves nothing.
    assert_eq!(german.code_of("at"), None);
    // While the two levels a bind may name resolve as they always did.
    assert_eq!(german.code_of("q"), Some(KEY_Q));
    assert_eq!(german.code_of("Q"), Some(KEY_Q));
}

/// A client reads the layouts out of the keymap it was handed, which is how
/// the built-in terminal follows `input:kb_layout` instead of always reading
/// the first table. `name[1]="German"` is libxkbcommon's own line.
#[test]
fn a_keymaps_own_text_names_its_layouts() {
    let (german, _) = layout("de", "");
    let named = super::groups_of(german.keymap);
    assert_eq!(named.len(), 1);
    assert_eq!((named[0].name, named[0].variant), ("de", ""));

    let (american, _) = layout("us", "");
    let named = super::groups_of(american.keymap);
    assert_eq!(named.len(), 1);
    assert_eq!(named[0].name, "us");

    // Group order is the numbering in the text, not the order of the lines.
    let two = "xkb_symbols \"x\" {\n\tname[2]=\"English (US)\";\n\tname[1]=\"German\";\n};\n";
    let named = super::groups_of(two);
    assert_eq!(
        named.iter().map(|layout| layout.name).collect::<Vec<_>>(),
        ["de", "us"]
    );

    // A name this compositor does not ship is skipped, not guessed at, and
    // a keymap that names none leaves the caller its own default.
    assert!(super::groups_of("\tname[1]=\"Klingon\";\n").is_empty());
    assert!(super::groups_of("\tlevel_name[1]= \"Any\";\n").is_empty());
    assert!(super::groups_of("").is_empty());
}

/// What a held key does to the modifiers is the layout's: right Alt is
/// `AltGr` (`Mod5`) on a German keyboard and `Alt` (`Mod1`) on an American
/// one. The keyboard once read every key from the default keymap, told
/// clients `Alt` while a German keyboard held `AltGr`, and `AltGr` and `+`
/// typed `+`.
#[test]
fn right_alt_is_the_modifier_the_layout_in_force_makes_it() {
    const KEY_RIGHTALT: u16 = 100;
    const KEY_PLUS: u16 = 27;
    const MOD1: u32 = 0x8;

    let (german, _) = layout("de", "");
    let (american, _) = layout("us", "");
    let mut keyboard = super::Keyboard::new();
    keyboard.set_layouts(vec![german, american]);
    assert!(keyboard.is_modifier(KEY_RIGHTALT));

    assert!(keyboard.key(KEY_RIGHTALT, true));
    let held = keyboard.modifiers().depressed;
    assert_eq!(held, generated::MOD5, "AltGr on de");
    // Which is the level `~` is on, as the terminal will look it up.
    let made = german.key(KEY_PLUS).and_then(|key| key.keysym(held));
    assert_eq!(made, Some("asciitilde"));

    // The second group is `us`, where the same key is Alt.
    assert!(keyboard.next_group());
    assert_eq!(keyboard.modifiers().depressed, MOD1, "Alt on us");
    assert!(keyboard.key(KEY_RIGHTALT, false));
    assert_eq!(keyboard.modifiers().depressed, 0);
}
