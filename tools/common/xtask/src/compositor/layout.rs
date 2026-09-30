//! The boots about where windows go: the first boot's three states, window
//! groups, a plugin's dispatcher, window rules, a submap, and a client that
//! closes one of its own two windows.
//!
//! Each presses its keybinds through QEMU's keyboard, holds every state to a
//! picture the renderer blesses, and then holds `hyprctl` and the event
//! socket to what they must have said about it: the picture proves the
//! layout, and the transcript what a bar would have been told.

use super::{EXPECTED, Programs, Wanted, boot_and_dump, said_on_its_own};
use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result};

/// The two pictures the group boot requires: the windows tiled, then both
/// of them in one slot with the one moved in drawn.
const GROUPED_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "two windows in one slot, the one moved into the group drawn",
        "src/user/linux/compositor/render/tests/data/grouped-two-clients.xrle",
    ),
];

/// The keybind the group boot presses between its two pictures.
const GROUP_BINDS: [(&str, &[&str]); 1] = [("SUPER G", &["meta_l", "g"])];

/// The configuration the fourth boot is given: Hyprland's window groups.
///
/// One keybind runs the three dispatchers, through `hyprctl --batch` over
/// the control socket, because that is the shortest honest proof that both
/// the batch and the group dispatchers work from inside the guest: the
/// picture afterwards is the group's, and `hyprctl clients` names the group.
pub(super) const GROUP_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/hyprctl subscribe
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
bind = SUPER, G, exec, /bin/hyprctl --batch dispatch togglegroup ; \
dispatch movefocus l ; dispatch moveintogroup r
bind = SUPER, C, exec, /bin/hyprctl clients
bind = SUPER, W, exec, /bin/hyprctl activewindow
";

/// The two pictures the plugin boot requires: the windows tiled, then
/// exchanged by a dispatcher the plugin added.
const PLUGIN_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the windows swapped by the plugin's own dispatcher",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
    ),
];

/// The keybind the plugin boot presses between its two pictures: a
/// dispatcher no part of the compositor knows, which the plugin added.
const PLUGIN_BINDS: [(&str, &[&str]); 1] = [("SUPER P", &["meta_l", "p"])];

/// The configuration the seventh boot is given: a plugin, and a keybind
/// naming the dispatcher it adds.
pub(super) const PLUGIN_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
plugin = /bin/plug
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
bind = SUPER, P, swapthem
bind = SUPER, C, exec, /bin/hyprctl --batch plugin list ; clients
bind = SUPER, W, exec, /bin/hyprctl activewindow
";

/// The picture a window rule makes, which the tenth boot requires.
const RULED_EXPECTED: [(&str, &str); 1] = [(
    "a window floating where a rule put it, each drawn as its own rules say",
    "src/user/linux/compositor/render/tests/data/ruled-two-clients.xrle",
)];

/// The configuration the tenth boot is given: the same two clients, with
/// rules that float one of them at a size and a place, and give each of
/// them something of its own to be drawn with.
///
/// The form is Hyprland 0.56's: fields with a name and a value, and
/// `match:` in front of the ones the window must be.
const RULED_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
windowrule = float, match:title ^(two)$
windowrule = size 400 300, match:title ^(two)$
windowrule = move 200 150, match:title ^(two)$
windowrule = opacity 0.6, match:title ^(two)$
windowrule = rounding 12, match:title ^(one)$
windowrule = no_shadow, match:title ^(one)$
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
";

/// The one picture the twin boot requires.
///
/// There is no "before" picture and no keybind: the client opens its second
/// window and destroys it on its own clock, so a picture taken at a
/// particular moment would be a race. `settle` takes screendumps until the
/// screen is the one expected, which is exactly the claim -- the screen
/// *becomes* one window -- and the transcript says the second window was
/// really there and really went.
const TWIN_EXPECTED: [(&str, &str); 1] = [(
    "one window left, after the client that owned two destroyed one of them",
    "src/user/linux/compositor/render/tests/data/one-client-alone.xrle",
)];

/// The configuration the twentieth boot is given: one client with two
/// windows.
///
/// The kept window is the gradient because that is the window
/// `src/user/linux/compositor/render` blesses alone; `--twin` draws the other pattern in
/// the second one, so the screen while both are up is plainly two windows.
const TWIN_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern gradient two --twin
";

/// The six pictures the submap boot requires, and the five keys between
/// them.
///
/// `L` is bound in the submap and nowhere else, so the only picture that
/// changes is the one after it is pressed *inside* the map. The pictures
/// before and after it say that entering a submap and leaving one move no
/// window, which is what a mode is.
const SUBMAP_EXPECTED: [(&str, &str); 6] = [
    (
        "tiled",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "still tiled, now inside the submap",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "still tiled, with the submap naming itself",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the windows swapped by a key bound only in the submap",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
    ),
    (
        "still swapped, with the submap left",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
    ),
    (
        "still swapped, with the global map naming itself",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
    ),
];

/// The keys the submap boot presses between its pictures.
///
/// `C` is asked twice, and each time after the map it names has been in
/// force for a whole picture: `hyprctl` is a program the compositor starts,
/// and under emulation it takes long enough to connect that asking on one
/// key and changing the map on the next would be a race rather than a test.
const SUBMAP_BINDS: [(&str, &[&str]); 5] = [
    ("SUPER R, which enters the submap", &["meta_l", "r"]),
    ("C, which asks which map is in force", &["c"]),
    ("L, which is bound only in the submap", &["l"]),
    ("Escape, the universal bind that leaves it", &["esc"]),
    ("C again, now that the submap has been left", &["c"]),
];

/// The configuration the twelfth boot is given: a submap.
///
/// `C` is bound in both maps to the same thing -- asking which map is in
/// force -- so the transcript says `resize` and then `default` from one key.
/// `L` is bound only in the submap, so it is the key that proves the gating:
/// pressed inside the map it swaps the windows, and it is bound to the same
/// two dispatchers the plugin boot sends, so the picture it makes is the one
/// already blessed for a swap. `Escape` carries the `u` flag, which is how
/// the bind that leaves a submap is written once.
const SUBMAP_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/hyprctl subscribe
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
bind = SUPER, R, submap, resize
bind = , C, exec, /bin/hyprctl submap
submap = resize
bind = , L, exec, /bin/hyprctl --batch dispatch movefocus l ; dispatch movewindow r
bind = , C, exec, /bin/hyprctl submap
bindu = , Escape, submap, reset
submap = reset
";

/// The configuration carried into the initramfs.
///
/// The two clients are `exec-once` rather than `--exec`, because that is what
/// stage 18's exit criterion says and because it is what a person's
/// `hyprland.conf` holds. The two binds are the ones this test presses.
pub(super) const CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/hyprctl subscribe
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
bind = SUPER, L, movefocus, l
bind = SUPER SHIFT, L, movewindow, r
bind = SUPER, C, exec, /bin/hyprctl clients
bind = SUPER, W, exec, /bin/hyprctl activewindow
";

/// The keys each bind is, as QMP's `qcode` names them.
///
/// QEMU's names for the modifiers are not the keysyms': the left shift is
/// `shift` and the right one `shift_r`, and `shift_l` is not a value it
/// takes.
const BINDS: [(&str, &[&str]); 2] = [
    ("SUPER L", &["meta_l", "l"]),
    ("SUPER SHIFT L", &["meta_l", "shift", "l"]),
];

/// A tenth boot: a `windowrule` that floats a window somewhere.
///
/// Nothing is pressed: the rules are in the configuration and the
/// compositor applies them as the windows map, so what is required is the
/// picture they make.
pub(super) fn test_rules(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        RULED_CONFIG,
        &Wanted {
            states: &RULED_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &[],
        },
        &[],
        args,
    )?;
    let Some(screen) = screens.first() else {
        return Err(Error::new(format!("{arch}: the rule boot took no picture")));
    };
    if !said.iter().any(|line| line.contains("window rules")) {
        return Err(Error::new(format!(
            "{arch}: the compositor never said it had read the rules"
        )));
    }
    println!(
        "  {arch}: a window rule floated a window at the size and place it names, every one of \
         {} pixels",
        screen.width * screen.height
    );
    Ok(())
}

/// The first boot: the three states stage 18's exit asks for, and what
/// `hyprctl` and the event socket said while they were reached.
pub(super) fn test_dispatchers(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        CONFIG,
        &Wanted {
            states: &EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &[],
        },
        &BINDS,
        args,
    )?;
    if screens.len() != EXPECTED.len() {
        return Err(Error::new(format!(
            "{arch}: {} of {} states were reached",
            screens.len(),
            EXPECTED.len()
        )));
    }
    // The three pictures must be three pictures. A compositor that
    // ignored both keybinds would pass every comparison above if the
    // expected images happened to be the same file, and this is the
    // check that says they are not.
    for (one, other) in [(0, 1), (1, 2), (0, 2)] {
        let (Some(first), Some(second)) = (screens.get(one), screens.get(other)) else {
            continue;
        };
        if first.pixels == second.pixels {
            return Err(Error::new(format!(
                "{arch}: states {one} and {other} are the same picture"
            )));
        }
    }

    the_sockets_said(arch, &said)?;
    Ok(())
}

/// A twentieth boot: one client with two windows, one of which it closes.
///
/// Every other boot in this table gives each window a client of its own, so
/// the only way one has ever gone is with its connection. This one is the
/// other way: the client stays and destroys one of its two
/// `xdg_toplevel`s, which until 2026-09-18 left the layout tiling a window
/// that was not there. The host test in
/// `src/user/linux/compositor/hyprix/tests/two_clients.rs` makes the same claim against
/// the compositor in a process; this makes it on Ferrix, on the card.
pub(super) fn test_twin(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        TWIN_CONFIG,
        &Wanted {
            states: &TWIN_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["destroyed its second window"],
        },
        &[],
        args,
    )?;
    let Some(last) = screens.last() else {
        return Err(Error::new(format!("{arch}: the twin boot took no picture")));
    };
    // The picture alone would also be what a client that never managed to
    // open its second window drew, and that is a different thing entirely:
    // these two lines are what say the screen went from two windows to one
    // rather than never having had two.
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    for wanted in [
        "pattern: two second window",
        "pattern: two destroyed its second window",
    ] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: the client never said `{wanted}`, so it did not have two windows to \
                 close one of"
            )));
        }
    }
    println!(
        "  {arch}: a client opened a second window and destroyed it, and every one of {} pixels \
         is the renderer's own picture of the window it kept",
        last.width * last.height
    );
    Ok(())
}

/// A twelfth boot: a submap, which is Hyprland's modal keybinding.
///
/// The one key that changes a picture here is `L`, and it changes one only
/// while the submap is entered: bound in the map and nowhere else, it does
/// nothing before `SUPER R` and nothing after `Escape`. `C` is bound in both
/// maps to `hyprctl submap`, so the same key names the map it is in, and the
/// event socket says when each was entered and left.
pub(super) fn test_submap(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        SUBMAP_CONFIG,
        &Wanted {
            states: &SUBMAP_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["submap>>resize", "hyprix: the global keymap", "default"],
        },
        &SUBMAP_BINDS,
        args,
    )?;
    if screens.len() != SUBMAP_EXPECTED.len() {
        return Err(Error::new(format!(
            "{arch}: {} of {} pictures were taken",
            screens.len(),
            SUBMAP_EXPECTED.len()
        )));
    }
    // The tiled pictures and the swapped ones must not be the same picture,
    // or every comparison above would pass on a compositor that ignored
    // every key.
    if let (Some(before), Some(after)) = (screens.first(), screens.get(3))
        && before.pixels == after.pixels
    {
        return Err(Error::new(format!(
            "{arch}: the key bound in the submap changed nothing"
        )));
    }
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    // One key, two answers: the map it was pressed in. The whole line, not
    // a substring, because `resize` is in the configuration's own text and a
    // substring would match a diagnostic quoting it.
    let said_alone = |wanted: &str| said.iter().any(|line| said_on_its_own(line) == wanted);
    for wanted in ["default", "resize"] {
        if !said_alone(wanted) {
            return Err(Error::new(format!(
                "{arch}: `hyprctl submap` never printed `{wanted}` on its own line"
            )));
        }
    }
    // And the socket announced both, the leaving with an empty payload.
    if !has("submap>>resize") {
        return Err(Error::new(format!(
            "{arch}: nothing on the event socket said the submap was entered"
        )));
    }
    if !said_alone("submap>>") {
        return Err(Error::new(format!(
            "{arch}: nothing on the event socket said the submap was left"
        )));
    }
    println!(
        "  {arch}: one key did nothing in the global map and swapped the windows in the submap, \
         and `hyprctl submap` named each map from inside the guest"
    );
    Ok(())
}

/// A seventh boot: a plugin, and a keybind naming a dispatcher it added.
///
/// Nothing in the compositor knows `swapthem`: the layout refuses it, and it
/// reaches the plugin because the plugin registered it. What the plugin asks
/// for in return is what the screen then shows, which is the whole of the
/// extension point.
pub(super) fn test_plugins(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        PLUGIN_CONFIG,
        &Wanted {
            states: &PLUGIN_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &[],
        },
        &PLUGIN_BINDS,
        args,
    )?;
    let (Some(tiled), Some(swapped)) = (screens.first(), screens.get(1)) else {
        return Err(Error::new(format!(
            "{arch}: the plugin boot took no pictures"
        )));
    };
    if tiled.pixels == swapped.pixels {
        return Err(Error::new(format!(
            "{arch}: the plugin's dispatcher changed nothing"
        )));
    }
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    for wanted in [
        // The plugin said what it is, and the compositor lists it.
        "plug: loaded, swapthem added",
        "Plugin swap by ferrix:",
        "Dispatchers: swapthem",
        // And it was handed the dispatcher the keybind pressed.
        "plug: dispatched swapthem",
    ] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: the plugin boot did not say `{wanted}`"
            )));
        }
    }
    println!(
        "  {arch}: a plugin added `swapthem`, was handed the keybind's dispatch, and swapped the \
         windows with it"
    );
    Ok(())
}

/// What the first boot's `hyprctl` and event socket must have said.
///
/// `hyprctl clients` names both windows and `hyprctl activewindow` names the
/// focused one, which after the swap is the checkerboard: the same window
/// the third picture draws the active border round. The subscriber started
/// before the clients did, so it was told the monitor and the workspace as
/// well as each window, and the focus moving as each keybind was pressed.
fn the_sockets_said(arch: Arch, said: &[String]) -> Result<()> {
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    for wanted in [
        // Both windows, from `clients`.
        "title: one",
        "title: two",
        "class: rocks.magical.pattern",
        // And both were told who draws their title bar, which is the only
        // way to see it: a compositor that never answers and one that
        // answers `server_side` look the same on the screen until a toolkit
        // draws a title bar of its own.
        "pattern: one decorations server_side",
        "pattern: two decorations server_side",
        // The focused one, from `activewindow`, on the workspace it is on.
        "workspace: 1 (1)",
    ] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: `hyprctl` over the control socket did not say `{wanted}`"
            )));
        }
    }
    println!("  {arch}: `hyprctl clients` and `hyprctl activewindow` answered on the guest");

    for wanted in [
        "monitoradded>>",
        "createworkspacev2>>1,1",
        "openwindow>>",
        "activewindow>>rocks.magical.pattern,one",
        "activewindow>>rocks.magical.pattern,two",
    ] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: nothing on the event socket said `{wanted}`"
            )));
        }
    }
    let focus_changes = said
        .iter()
        .filter(|line| line.contains("activewindowv2>>"))
        .count();
    if focus_changes < 3 {
        return Err(Error::new(format!(
            "{arch}: the focus moved twice by keybind and the socket said so {focus_changes} \
             times in all"
        )));
    }
    println!(
        "  {arch}: a subscriber on the event socket was told {focus_changes} focus changes and \
         every window"
    );
    Ok(())
}

/// A fourth boot: Hyprland's window groups, made by one keybind that batches
/// three dispatchers through the control socket.
///
/// Two pictures rather than one, because a group that changed nothing would
/// match the tiled image and pass: the second must be the group's, and the
/// two must differ.
pub(super) fn test_groups(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        GROUP_CONFIG,
        &Wanted {
            states: &GROUPED_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &[],
        },
        &GROUP_BINDS,
        args,
    )?;
    let (Some(tiled), Some(grouped)) = (screens.first(), screens.get(1)) else {
        return Err(Error::new(format!(
            "{arch}: the group boot took no pictures"
        )));
    };
    if tiled.pixels == grouped.pixels {
        return Err(Error::new(format!(
            "{arch}: the group is the same picture as the tiling"
        )));
    }
    println!(
        "  {arch}: a group drew one of its two members in the slot they share, every one of {} \
         pixels",
        grouped.width * grouped.height
    );
    group_was_said(arch, &said)
}

/// What the group boot's `hyprctl` and event socket must have said.
///
/// The picture proves the layout; these prove what a bar is told about it --
/// the `grouped` field of `hyprctl clients`, and the two events Hyprland
/// posts when a group is made and joined.
fn group_was_said(arch: Arch, said: &[String]) -> Result<()> {
    let grouped = said
        .iter()
        .filter_map(|line| line.split("grouped: ").nth(1))
        .map(str::trim)
        .find(|value| value.contains(','))
        .map(str::to_owned);
    let Some(grouped) = grouped else {
        return Err(Error::new(format!(
            "{arch}: `hyprctl clients` named no window's group"
        )));
    };
    println!("  {arch}: `hyprctl clients` says grouped: {grouped}");
    for wanted in ["togglegroup>>1,", "moveintogroup>>"] {
        if !said.iter().any(|line| line.contains(wanted)) {
            return Err(Error::new(format!(
                "{arch}: nothing on the event socket said `{wanted}`"
            )));
        }
    }
    println!("  {arch}: the event socket said the group was made and joined");
    Ok(())
}
