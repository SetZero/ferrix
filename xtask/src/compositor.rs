//! `cargo xtask test-compositor`: the compositor itself, on Ferrix, on a
//! screen.
//!
//! `cargo xtask test-display` boots `compositor/blank`, which fills the card
//! with one colour: the proof that the path from a program through
//! `/dev/dri/card0`, the kernel's display core and the ring-3 virtio-gpu
//! driver to QEMU's window works at all. This boots the compositor, which
//! goes through the same path with everything above it in place -- the
//! configuration, the layout, the renderer and its own DRM backend with two
//! dumb buffers and a page flip.
//!
//! Two `compositor/pattern` clients are carried in the initramfs at
//! `/bin/pattern` and started by the compositor's own `exec-once`, so what
//! reaches the screen is two real Wayland clients tiled by the dwindle
//! layout -- the same picture `compositor/hyprix/tests/two_clients.rs` makes
//! on the host, and compared against the same expected image that
//! `compositor/render`'s own tests bless.
//!
//! # Three pictures, and two keybinds between them
//!
//! `docs/ROADMAP.md` stage 18's exit criterion asks for more than one
//! picture: the windows tiled, then a keybind sent through QEMU moving the
//! focus, then another swapping them, with each state required from a
//! screendump. So the configuration carried in the initramfs holds the two
//! `exec-once` lines *and* two binds, and this presses them through QMP's
//! `input-send-event` -- the same way a person would press them, through a
//! `virtio-keyboard-pci`, the kernel's evdev node and the compositor's seat.
//!
//! Each of the three states has an expected image of its own, blessed by
//! `compositor/render`'s own tests by calling the renderer with rectangles
//! from `compositor/layout`. The pictures compared here came from two
//! programs talking Wayland to a server that worked the same rectangles out
//! from their requests, so a difference between the two paths is a real one.
//!
//! # The two sockets
//!
//! `hyprctl` is carried in the initramfs beside the clients, because
//! Hyprland's own is not on Ferrix's image. The configuration's first
//! `exec-once` subscribes to `.socket2.sock` and prints every line, which is
//! what a bar does, so the transcript holds the compositor's whole event
//! stream: the monitor, the workspace, each window arriving, and the focus
//! moving as the keybinds are pressed. Two more binds ask `.socket.sock` for
//! `clients` and `activewindow`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::display::{DEVICE_ID, Image, Qmp, free_port, mismatches, parse_ppm};
use crate::paths::{self, Arch};
use crate::qemu::Watching;
use crate::{Error, Result};

/// The compositor's own background: `compositor/render`'s `Style::BACKGROUND`,
/// which is Hyprland's `misc:background_color` default.
const BACKGROUND: [u8; 3] = [0x11, 0x11, 0x11];

/// What the compositor prints once it is on a screen, followed by the mode.
const MARKER: &str = "hyprix: card0";

/// What it prints instead when it could not start.
const FAILED: &str = "hyprix: failed";

/// Either, so the boot stops at whichever comes.
const EITHER: &str = "hyprix: ";

/// How long to let the screen settle after the marker: the clients have to
/// connect, be configured, draw and commit, and the flip is queued after
/// that. `test-display` allows four seconds for one program's fill.
///
/// Half a minute because of the slowest case, which is a window left alone
/// when its neighbour's client went: it has to be told its new size, draw a
/// buffer at it and commit, and only then is a frame drawn and flipped --
/// four round trips at a second and a half a frame under emulation. A
/// screen that is already right costs none of it: the wait ends the moment
/// the picture matches.
const SETTLE: Duration = Duration::from_secs(30);

/// The three pictures, in the order the keybinds make them. Each is blessed
/// by `compositor/render`'s own tests.
const EXPECTED: [(&str, &str); 3] = [
    (
        "tiled",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the focus moved left",
        "compositor/render/tests/data/dwindle-two-clients-focus-left.xrle",
    ),
    (
        "the windows swapped",
        "compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
    ),
];

/// Where the clients, the control program, the plugin and the configuration
/// go in the initramfs, which is what the compositor's `exec-once`, its
/// `plugin` line and the binds name.
const CLIENT_PATH: &str = "bin/pattern";
const CTL_PATH: &str = "bin/hyprctl";
const PLUG_PATH: &str = "bin/plug";
const TERM_PATH: &str = "bin/term";
const CLIP_PATH: &str = "bin/clip";
const LSWT_PATH: &str = "bin/lswt";
const SHOT_PATH: &str = "bin/shot";
const LOCK_PATH: &str = "bin/lock";
const VKBD_PATH: &str = "bin/vkbd";
const CONFIG_PATH: &str = "etc/hyprland.conf";

/// Where `run-compositor` puts the wallpaper it carries.
const WALLPAPER_PATH: &str = "etc/wallpaper.fxwall";

/// Where it puts one that moves, which is a different file and a different
/// flag rather than the same name holding either: a boot that carried the
/// wrong one would say nothing until the screen was grey.
const MOVIE_PATH: &str = "etc/wallpaper.ivf";

/// The instance the control socket is under, which `hyprctl` finds by
/// looking when `HYPRLAND_INSTANCE_SIGNATURE` is not set.
const INSTANCE: &str = "ferrix";

/// The picture the mode boot requires: the two windows on a screen of the
/// size its `monitor =` line asked for.
const MODE_EXPECTED: [(&str, &str); 1] = [(
    "tiled on a 1920x1080 screen, which is the mode the configuration asked for",
    "compositor/render/tests/data/dwindle-two-clients-1920x1080.xrle",
)];

/// The configuration the mode boot is given. The card prefers 1024x768, as
/// every judged boot's does, and this asks for a size it only lists.
const MODE_CONFIG: &str = "# Carried into the initramfs by `cargo xtask test-compositor`.
monitor = , 1920x1080@60, auto, 1
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
";

/// The pictures the transform boot requires, one a boot: the two windows on
/// a monitor stood on its edge, as the connector's buffer holds them --
/// which is what QEMU's screendump reads, since QEMU's window is the
/// connector and knows nothing of how the monitor stands.
const TRANSFORM_EXPECTED: [(u32, (&str, &str)); 2] = [
    (
        1,
        (
            "tiled on a monitor turned clockwise onto its edge, the picture turned \
             counter-clockwise into the buffer",
            "compositor/render/tests/data/dwindle-two-clients-transform-1.xrle",
        ),
    ),
    (
        3,
        (
            "tiled on a monitor turned counter-clockwise onto its edge, the picture turned \
             clockwise into the buffer",
            "compositor/render/tests/data/dwindle-two-clients-transform-3.xrle",
        ),
    ),
];

/// The configuration the transform boot is given for `transform`: the
/// monitor turned, `hyprctl monitors` asked once so the transcript says what
/// it reports, and the two windows.
fn transform_config(transform: u32) -> String {
    format!(
        "# Carried into the initramfs by `cargo xtask test-compositor --boot transform`.
monitor = , preferred, auto, 1, transform, {transform}
exec-once = /bin/hyprctl monitors
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
"
    )
}

/// The picture a bar and two windows make, which the second boot requires.
const BAR_EXPECTED: (&str, &str) = (
    "a bar across the top with the windows under it",
    "compositor/render/tests/data/layer-bar-two-clients.xrle",
);

/// The picture Hyprland's two window decorations make, which the third boot
/// requires.
const DECORATED_EXPECTED: (&str, &str) = (
    "corners cut, a shadow under each window, and the unfocused one dimmed",
    "compositor/render/tests/data/decorated-two-clients.xrle",
);

/// The configuration the third boot is given: the same two windows, with
/// `decoration:rounding` and `decoration:inactive_opacity` set.
const DECORATED_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
# The same settings `compositor/render`'s `decorated_style` blesses the
# picture with; a line changed here and not there is a picture that cannot
# match.
decoration:rounding = 12
decoration:inactive_opacity = 0.6
decoration:shadow:range = 12
decoration:shadow:render_power = 2
decoration:dim_inactive = 1
decoration:dim_strength = 0.4
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
";

/// The two pictures the group boot requires: the windows tiled, then both
/// of them in one slot with the one moved in drawn.
const GROUPED_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "two windows in one slot, the one moved into the group drawn",
        "compositor/render/tests/data/grouped-two-clients.xrle",
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
const GROUP_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/hyprctl subscribe
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = SUPER, G, exec, /bin/hyprctl --batch dispatch togglegroup ; \
dispatch movefocus l ; dispatch moveintogroup r
bind = SUPER, C, exec, /bin/hyprctl clients
bind = SUPER, W, exec, /bin/hyprctl activewindow
";

/// The two pictures the two-monitor boot requires, one a screen: the
/// windows tiled on the first, then the gradient alone on the second with
/// the checkerboard alone on the first.
const MONITOR_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the checkerboard alone on the first monitor",
        "compositor/render/tests/data/two-monitors-left.xrle",
    ),
];

/// What the second screen must show once the keybind has been pressed.
const MONITOR_OTHERS: [(&str, &str); 1] = [(
    "the gradient alone on the second monitor",
    "compositor/render/tests/data/two-monitors-right.xrle",
)];

/// The keybind the two-monitor boot presses between its two pictures.
const MONITOR_BINDS: [(&str, &[&str]); 1] = [("SUPER M", &["meta_l", "m"])];

/// The configuration the fifth boot is given: two monitors, and a keybind
/// that sends the focused window to the second one.
const MONITOR_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/hyprctl subscribe
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = SUPER, M, movewindow, mon:1
# Both answers from one press, because the presses are the ones every boot
# makes: the monitors, and the clients whose last line is what the wait for
# the answers looks for.
bind = SUPER, C, exec, /bin/hyprctl --batch monitors ; clients
bind = SUPER, W, exec, /bin/hyprctl activewindow
";

/// The picture a scaled monitor makes, which the sixth boot requires.
const SCALED_EXPECTED: (&str, &str) = (
    "every logical pixel drawn as two on a monitor at scale 2",
    "compositor/render/tests/data/scaled-two-clients.xrle",
);

/// The configuration the sixth boot is given: one monitor at scale 2, where
/// the windows tile in 512x384 logical pixels and are drawn as 1024x768.
///
/// The clients read `wl_output.scale` and send buffers twice the size, which
/// is what a client on a scaled monitor does and what the expected image is
/// blessed with.
const SCALED_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
monitor = , preferred, auto, 2
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
";

/// The two pictures the plugin boot requires: the windows tiled, then
/// exchanged by a dispatcher the plugin added.
const PLUGIN_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the windows swapped by the plugin's own dispatcher",
        "compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
    ),
];

/// The keybind the plugin boot presses between its two pictures: a
/// dispatcher no part of the compositor knows, which the plugin added.
const PLUGIN_BINDS: [(&str, &[&str]); 1] = [("SUPER P", &["meta_l", "p"])];

/// The configuration the seventh boot is given: a plugin, and a keybind
/// naming the dispatcher it adds.
const PLUGIN_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
plugin = /bin/plug
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = SUPER, P, swapthem
bind = SUPER, C, exec, /bin/hyprctl --batch plugin list ; clients
bind = SUPER, W, exec, /bin/hyprctl activewindow
";

/// The picture the animated boot starts from: the same decorations the third
/// boot requires, since the slide is watched with them on.
const ANIMATED_EXPECTED: [(&str, &str); 1] = [(
    "corners cut, a shadow under each window, and the unfocused one dimmed",
    "compositor/render/tests/data/decorated-two-clients.xrle",
)];

/// Where the slide ends: the same two windows, exchanged.
const ANIMATED_MOVING: Moving<'static> = Moving {
    what: "a window sliding to the other side with its decorations on",
    path: "compositor/render/tests/data/decorated-two-clients-swapped.xrle",
    keys: &["meta_l", "a"],
};

/// How long a frame may take on the guest, in microseconds.
///
/// Not the bound the renderer's software fallback has -- that one is stated
/// and checked where it means something, by `compositor/render`'s own
/// release-build test, at 250 ms for a frame with every effect on. This is
/// the same frame under QEMU's `tcg`, which emulates every instruction and
/// is tens of times slower than the processor it is emulating: the numbers
/// the guest reports are around 1.5 seconds a frame, and what this catches
/// is a compositor that stopped drawing or a frame that became minutes
/// rather than seconds.
///
/// Twice that on ARMv7-A, whose frames under `tcg` take about twice
/// AArch64's in the same runs (1.3 to 2 s against 0.6 to 0.8 s on a loaded
/// nazuna, 2026-09-23), and whose slowest reached 5.25 s there with the
/// host's load average near 11: a bound the 64-bit machines keep a margin
/// under would fail it on load alone.
fn frame_bound(arch: Arch) -> u128 {
    match arch {
        Arch::Armv7a => 10_000_000,
        Arch::X86_64 | Arch::AArch64 => 5_000_000,
    }
}

/// The configuration the eighth boot is given: the decorations of the third,
/// with the animations on and a keybind that sends a window to the other
/// side so that its slide can be watched.
///
/// Two seconds for the slide rather than Hyprland's 0.8, because what is
/// watched here is a sequence of screendumps and a screendump of a
/// virtio-gpu is not a fast thing: a longer slide is the same curve with
/// more points on it.
const ANIMATED_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
decoration:rounding = 12
decoration:inactive_opacity = 0.6
decoration:shadow:range = 12
decoration:shadow:render_power = 2
decoration:dim_inactive = 1
decoration:dim_strength = 0.4
animation = windows, 1, 20, default
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = SUPER, A, exec, /bin/hyprctl --batch dispatch movefocus l ; \
dispatch movewindow r
";

/// The picture a window rule makes, which the tenth boot requires.
const RULED_EXPECTED: [(&str, &str); 1] = [(
    "a window floating where a rule put it, each drawn as its own rules say",
    "compositor/render/tests/data/ruled-two-clients.xrle",
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
exec-once = /bin/pattern gradient two
";

/// The two pictures the pointer boot requires: the windows, and the same
/// windows with the pointer on them.
///
/// The pointer is not drawn until it has moved, because until then the
/// compositor has only its own guess at where the mouse is -- the middle of
/// the screen -- and an arrow drawn at a guess is worse than none. So the
/// first picture is every other boot's, and the second is the one the
/// movement makes.
const POINTER_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled, with no pointer drawn because none has moved",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the pointer over the windows, its tip where the mouse was put",
        "compositor/render/tests/data/pointer-on-two-clients.xrle",
    ),
];

/// Where the pointer is put, in QMP's 0..0x7FFF across the screen.
///
/// `compositor/render` blesses the picture with the arrow's tip at
/// (700, 300) on a 1024x768 screen, and these are those two as a fraction
/// of the axis QEMU's virtio tablet reports.
///
/// Inside the window that already has the focus, because
/// `input:follow_mouse` is on: a pointer moved into the other one would
/// take the focus with it and draw the active border somewhere else, which
/// is a different picture and not the one this boot is about.
const POINTER_AT: (i32, i32) = (22401, 12800);

/// How many places the pointer is swept through on its way to
/// [`POINTER_AT`], and how long after each the next is sent.
///
/// A hand moving a mouse, as a guest sees it: a few hundred reports a
/// second, each a few pixels on from the last. The boot used to put the
/// pointer down once, and a compositor that drew a whole frame for every
/// report -- each ending in the whole framebuffer sent to the host -- passed
/// it and stuttered under a hand. So the pointer is swept first, and two
/// things are required of the sweep. The picture at the end is the one a
/// single movement makes, to the pixel: no frame of the four hundred left
/// an arrow behind on the host's copy of the screen, which is what sending
/// only what changed would do if what changed were worked out wrong. And
/// the compositor drew at the screen's rate and not the mouse's:
/// [`MOST_FRAMES`].
const SWEEP: (u32, Duration) = (400, Duration::from_millis(3));

/// The most frames one of the compositor's frame reports may count.
///
/// A report is printed by the first frame a second or more after the last,
/// so at sixty frames a second it counts sixty or so however long the quiet
/// before it was. Three hundred reports a second drawn one for one count
/// three hundred.
const MOST_FRAMES: u32 = 75;

/// Where the pointer is at `step` of the sweep, in QMP's 0..0x7FFF.
///
/// Inside the focused window for the reason [`POINTER_AT`] gives, down and
/// back up its whole height so that it crosses the half of the gradient that
/// can be seen through, which is the half with a blur behind it.
fn swept(step: u32) -> (i32, i32) {
    let (steps, _) = SWEEP;
    let along = i32::try_from(step).unwrap_or(0);
    let of = i32::try_from(steps).unwrap_or(1).max(1);
    // x from 56% to 94% of the screen, y a triangle wave from 10% to 90%.
    let x = 18_350 + (12_450 * along) / of;
    let wave = (along * 4) % (of * 2);
    let up = if wave < of { wave } else { of * 2 - wave };
    let y = 3_277 + (26_213 * up) / of;
    (x, y)
}

/// The configuration the seventeenth boot is given: the two windows, and
/// the arrow drawn into the frame, which is what a screendump can see -- a
/// pointer on the card's cursor plane is the `cursor` boot's to judge.
const POINTER_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
cursor:no_hardware_cursors = 1
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
";

/// The picture a window with a menu on it makes, which the sixteenth boot
/// requires.
///
/// Nothing is pressed: the client asks for the popup as soon as its window
/// has drawn, which is when a toolkit would, so what is required is the
/// picture it makes.
const MENU_EXPECTED: [(&str, &str); 1] = [(
    "a menu over the window it hangs off, where the positioner puts it",
    "compositor/render/tests/data/menu-on-a-window.xrle",
)];

/// The configuration the sixteenth boot is given: a window with a menu and
/// a window without one.
///
/// `--menu 200` is what `compositor/render` blesses the picture with, and a
/// number changed here and not there is a picture that cannot match.
const MENU_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern checkerboard one --menu 200
exec-once = /bin/pattern gradient two
";

/// The three pictures the lock boot requires, and the two keys between them.
///
/// The windows, then the lock over them, then the windows again: what
/// `ext-session-lock-v1` is for is that the middle one shows *nothing* of
/// the first, and what says the lock let go is that the third is the first
/// again.
const LOCK_EXPECTED: [(&str, &str); 3] = [
    (
        "tiled",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the lock's own surface over the whole screen, and no window on it",
        "compositor/render/tests/data/locked-screen.xrle",
    ),
    (
        "the windows again, once the lock let go",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
];

/// The keys the lock boot presses. The second is the one that must do
/// nothing: `K` is bound, and a bind that is not `bindl` does not fire while
/// the session is locked.
///
/// No modifier, for the reason `TASKBAR_BINDS` gives.
const LOCK_BINDS: [(&str, &[&str]); 2] = [
    ("L, which locks the screen for four seconds", &["l"]),
    (
        "K, a bind that must not fire while the screen is locked",
        &["k"],
    ),
];

/// The configuration the fifteenth boot is given: two windows and a key
/// that locks the screen.
const LOCK_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = , L, exec, /bin/lock 4
bind = , K, exec, /bin/lswt close one
";

/// The two pictures the screenshot boot requires, and the key between them.
///
/// A screenshot changes nothing on the screen, so both are the tiled pair:
/// what the boot is *for* is the digest the guest prints, which must be the
/// digest of the picture `compositor/render` blesses. The second picture is
/// there so that a compositor which had stopped drawing would still be
/// caught.
const SHOT_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "still tiled, with a screenshot taken of it",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
];

/// The key the screenshot boot presses.
///
/// No modifier, for the reason `TASKBAR_BINDS` gives: a modifier reaches the
/// focused client, and this client draws something else when it is sent a
/// key.
const SHOT_BINDS: [(&str, &[&str]); 1] = [("S, which takes a screenshot", &["s"])];

/// The configuration the fourteenth boot is given: two windows and a key
/// that screenshots them.
const SHOT_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = , S, exec, /bin/shot
";

/// The two pictures the taskbar boot requires, and the two keys between
/// them.
///
/// Nothing draws the taskbar: `zwlr_foreign_toplevel_management_v1` is a
/// list and not a surface, and what it does is visible only when a window
/// acts on it. The first key lists the windows, which changes no picture;
/// the second asks one of them to close, which changes the picture to the
/// one window that is left.
const TASKBAR_EXPECTED: [(&str, &str); 3] = [
    (
        "tiled",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "still tiled, with a taskbar having listed both windows",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "one window left, closed from outside it by the taskbar",
        "compositor/render/tests/data/one-client-alone.xrle",
    ),
];

/// The keys the taskbar boot presses between its pictures.
///
/// No modifier, which matters here and nowhere else. A bind consumes its
/// own key but never the modifier held with it, so `SUPER B` reaches the
/// focused client as a `meta` press -- and `compositor/pattern` draws the
/// *other* pattern for every key it is sent, on purpose, so that a key
/// shows on the screen. It redraws only when it is configured, so in every
/// other boot the change never reaches a frame; this is the one boot that
/// presses a key and then resizes a window, and with `SUPER` in front of
/// them the window that was left drew a checkerboard.
const TASKBAR_BINDS: [(&str, &[&str]); 2] = [
    ("B, which lists the windows", &["b"]),
    ("K, which closes one of them", &["k"]),
];

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
    "compositor/render/tests/data/one-client-alone.xrle",
)];

/// The configuration the twentieth boot is given: one client with two
/// windows.
///
/// The kept window is the gradient because that is the window
/// `compositor/render` blesses alone; `--twin` draws the other pattern in
/// the second one, so the screen while both are up is plainly two windows.
const TWIN_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern gradient two --twin
";

/// The configuration the thirteenth boot is given: two windows, and a
/// program that reads them through the foreign-toplevel protocol.
const TASKBAR_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = , B, exec, /bin/lswt
bind = , K, exec, /bin/lswt close one
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
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "still tiled, now inside the submap",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "still tiled, with the submap naming itself",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the windows swapped by a key bound only in the submap",
        "compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
    ),
    (
        "still swapped, with the submap left",
        "compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
    ),
    (
        "still swapped, with the global map naming itself",
        "compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
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
exec-once = /bin/pattern gradient two
bind = SUPER, R, submap, resize
bind = , C, exec, /bin/hyprctl submap
submap = resize
bind = , L, exec, /bin/hyprctl --batch dispatch movefocus l ; dispatch movewindow r
bind = , C, exec, /bin/hyprctl submap
bindu = , Escape, submap, reset
submap = reset
";

/// What the clipboard boot copies, and the picture it makes while it does.
///
/// The two windows are the ordinary tiled pair: the clipboard has nothing to
/// draw, and the picture is there so that a boot which copied and pasted on
/// a compositor that had stopped drawing would still be caught.
const CLIPBOARD_TEXT: &str = "the clipboard went through the compositor";
const CLIPBOARD_EXPECTED: [(&str, &str); 1] = [(
    "the windows tiled while one program copied and another pasted",
    "compositor/render/tests/data/dwindle-two-clients.xrle",
)];

/// The configuration the eleventh boot is given: two windows, a program
/// that copies and a program that pastes.
///
/// Neither `clip` has a window: a Wayland clipboard needs a connection and
/// nothing else. The copy is started first, but the order does not matter --
/// a paste waits for the compositor to say what the selection holds, which
/// is what every `wl-paste` does.
fn clipboard_config() -> String {
    format!(
        "# Carried into the initramfs by `cargo xtask test-compositor`.\n\
         exec-once = /bin/pattern checkerboard one\n\
         exec-once = /bin/pattern gradient two\n\
         exec-once = /bin/clip copy {CLIPBOARD_TEXT}\n\
         exec-once = /bin/clip paste\n\
         exec-once = /bin/clip --primary copy {PRIMARY_TEXT}\n\
         exec-once = /bin/clip --primary paste\n"
    )
}

/// What the same boot puts on the *primary* selection, which is what a
/// middle click pastes.
///
/// Different text from the clipboard's, because the point of having both is
/// that they are two: a compositor that answered a primary paste from the
/// clipboard would pass with the same string and fail with this one.
const PRIMARY_TEXT: &str = "the primary selection is the other one";

/// The picture a terminal makes, which the ninth boot requires.
const TERMINAL_EXPECTED: [(&str, &str); 1] = [(
    "a terminal with a program's output in it",
    "compositor/render/tests/data/terminal-hyprctl-version.xrle",
)];

/// The configuration the ninth boot is given: a terminal, and nothing else.
///
/// `docs/ROADMAP.md` stage 18's exit asks for a terminal on the compositor.
/// The program it runs is `hyprctl version`, which prints three lines and
/// stops: short enough to be an expected image, and a round trip through
/// the control socket on the way, so what is on the screen came from the
/// compositor through a pseudoterminal and back.
const TERMINAL_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/term /bin/hyprctl version
";

/// The configuration the second boot is given: a bar through
/// `zwlr_layer_shell_v1`, and the same two windows.
///
/// A second boot rather than a fourth picture, because a bar changes every
/// picture: the three states above are the stage's exit criterion and are
/// compared against images blessed without one.
const BAR_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern checkerboard bar --bar 30
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = , A, exec, /bin/hyprctl --batch binds ; devices ; layers ; cursorpos ; locked
";

/// The keys the bar boot presses: one, which asks for everything `hyprctl`
/// answers about that is not a window.
///
/// No modifier, for the reason `TASKBAR_BINDS` gives.
const BAR_BINDS: [(&str, &[&str]); 1] = [(
    "A, which asks hyprctl for the binds, the devices and the layers",
    &["a"],
)];

/// The bar boot's two pictures, which are the same one: asking `hyprctl`
/// about the compositor must change nothing on the screen.
const BAR_PICTURES: [(&str, &str); 2] = [BAR_EXPECTED, BAR_EXPECTED];

/// The configuration carried into the initramfs.
///
/// The two clients are `exec-once` rather than `--exec`, because that is what
/// stage 18's exit criterion says and because it is what a person's
/// `hyprland.conf` holds. The two binds are the ones this test presses.
const CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/hyprctl subscribe
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = SUPER, L, movefocus, l
bind = SUPER SHIFT, L, movewindow, r
bind = SUPER, C, exec, /bin/hyprctl clients
bind = SUPER, W, exec, /bin/hyprctl activewindow
";

/// The two binds that ask the compositor about itself, pressed after the
/// three pictures so that what they print describes the last of them.
const ASKED: [(&str, &[&str]); 2] = [("SUPER C", &["meta_l", "c"]), ("SUPER W", &["meta_l", "w"])];

/// The keys each bind is, as QMP's `qcode` names them.
///
/// QEMU's names for the modifiers are not the keysyms': the left shift is
/// `shift` and the right one `shift_r`, and `shift_l` is not a value it
/// takes.
const BINDS: [(&str, &[&str]); 2] = [
    ("SUPER L", &["meta_l", "l"]),
    ("SUPER SHIFT L", &["meta_l", "shift", "l"]),
];

/// Every program a boot carries: the compositor the kernel starts as init,
/// and the ones the initramfs holds for it to `exec`.
///
/// One value rather than a parameter apiece, because each boot below passes
/// the whole set through unchanged and a program added for one boot would
/// otherwise be a new argument in every signature between here and
/// `build_image`.
#[derive(Clone, Debug)]
struct Programs {
    /// The compositor itself.
    hyprix: PathBuf,
    /// The test client that draws a pattern in a window.
    client: PathBuf,
    /// `hyprctl`.
    ctl: PathBuf,
    /// The plugin.
    plug: PathBuf,
    /// The terminal emulator.
    term: PathBuf,
    /// `clip`, the copy-and-paste program.
    clip: PathBuf,
    /// `lswt`, which lists the windows as a taskbar does.
    lswt: PathBuf,
    /// `shot`, which takes a screenshot as `grim` does.
    shot: PathBuf,
    /// `lock`, which locks the screen as `hyprlock` does.
    lock: PathBuf,
    /// `vkbd`, which types as `wtype` does.
    vkbd: PathBuf,
}

impl Programs {
    /// Build them all for `arch`.
    fn build(arch: Arch) -> Result<Self> {
        Ok(Self {
            hyprix: build(arch, "hyprix", "hyprix")?,
            client: build(arch, "compositor-pattern", "pattern")?,
            ctl: build(arch, "compositor-ctl", "hyprctl")?,
            plug: build(arch, "compositor-plug", "plug")?,
            term: build(arch, "compositor-term", "term")?,
            clip: build(arch, "compositor-clip", "clip")?,
            lswt: build(arch, "compositor-lswt", "lswt")?,
            shot: build(arch, "compositor-shot", "shot")?,
            lock: build(arch, "compositor-lock", "lock")?,
            vkbd: build(arch, "compositor-vkbd", "vkbd")?,
        })
    }

    /// The ones the initramfs carries, each with the path it goes at.
    fn carried(&self) -> [(&'static str, &Path); 9] {
        [
            (CLIENT_PATH, self.client.as_path()),
            (CTL_PATH, self.ctl.as_path()),
            (PLUG_PATH, self.plug.as_path()),
            (TERM_PATH, self.term.as_path()),
            (CLIP_PATH, self.clip.as_path()),
            (LSWT_PATH, self.lswt.as_path()),
            (SHOT_PATH, self.shot.as_path()),
            (LOCK_PATH, self.lock.as_path()),
            (VKBD_PATH, self.vkbd.as_path()),
        ]
    }
}

/// Build one of the compositor's programs for `arch`, and say where it is.
fn build(arch: Arch, package: &str, binary: &str) -> Result<PathBuf> {
    let target = crate::display::target(arch).ok_or_else(|| {
        Error::new(format!(
            "{arch} has no virtio-gpu in QEMU; the compositor test runs on x86_64 and aarch64"
        ))
    })?;
    let target_dir = paths::target_dir().join("compositor").join("hyprix");
    println!("  building compositor/{binary} for {target}");
    let program = target_dir.join(target).join("release").join(binary);
    crate::builds::Build::cargo(
        format!("cargo build (compositor/{binary}) --target {target}"),
        paths::workspace_root().join("compositor"),
    )
    .args(["build", "--release", "-p", package, "--target", target])
    .env("CARGO_TARGET_DIR", &target_dir)
    .output(&program)
    .run()?;
    Ok(program)
}

/// Boot the compositor, and take a screendump of each state in turn.
///
/// The compositor is the kernel's init program; the client and the
/// configuration are files in the initramfs, since the kernel embeds one
/// program and unpacks the rest. The arguments reach the compositor through
/// the init script, which `Options::parse` reads as its own command line.
/// What a boot must show.
///
/// The states are the first screen's, one to a keybind; `others` is what
/// every screen after the first must show once the last state has been
/// reached, which is how a second monitor is judged.
#[derive(Clone, Copy, Debug)]
struct Wanted<'a> {
    /// The pictures the first screen must show, in order.
    states: &'a [(&'a str, &'a str)],
    /// What each further screen must show at the end.
    others: &'a [(&'a str, &'a str)],
    /// A window sliding: the keybind that starts it and the picture it ends
    /// in, with every distinct screendump along the way kept.
    moving: Option<Moving<'a>>,
    /// Where to put the pointer, in QMP's own 0..0x7FFF coordinates, before
    /// the second picture is judged.
    ///
    /// Once and before that one picture, because that is the whole of what a
    /// pointer test needs: the first picture is the screen with no pointer
    /// on it and the second is the same screen with one, and a compositor
    /// that drew it in the wrong place fails the comparison.
    pointer: Option<(i32, i32)>,
    /// Lines the boot is waited for before its transcript is taken.
    ///
    /// A picture can be right before the guest has finished saying what it
    /// did -- a program that prints when it exits has not exited yet -- and
    /// a boot that only drained for a moment would judge a transcript that
    /// was merely early. What is required of the lines is still the
    /// caller's; this only says which ones are worth waiting for.
    awaiting: &'a [&'a str],
}

/// A window on its way somewhere: what starts it, and where it stops.
#[derive(Clone, Copy, Debug)]
struct Moving<'a> {
    /// What the sequence shows, for the line the test prints.
    what: &'a str,
    /// The expected image it must end in.
    path: &'a str,
    /// The keys that start it, as QMP's `qcode` names them.
    keys: &'a [&'a str],
}

impl Wanted<'_> {
    /// How many virtio-gpu devices the boot needs: one a screen.
    fn screens(&self) -> u32 {
        u32::try_from(self.others.len())
            .unwrap_or(0)
            .saturating_add(1)
    }
}

/// `config` with the blur's dither turned off, which is what every gate boot
/// is given.
///
/// `decoration:blur:noise` is 0.0117 in Hyprland and here, and the dither is
/// drawn. But the pictures a boot is judged against are
/// `compositor/render`'s expected images, and those are blessed without it
/// (`Style::undithered` says why: a dither is the one thing a run-length
/// encoded image cannot hold). `compositor/hyprix/tests/two_clients.rs`
/// tells the compositor under test the same thing for the same reason.
///
/// Without this a boot fails on a dither and nothing else: every pixel that
/// differs is inside the translucent half of the gradient client and is one
/// step up in each of its three channels, which is what a dither seen
/// through a window a quarter transparent rounds to. How many of them there
/// are depends on what is behind the window, so a change to the blur moves
/// the count and looks like the cause.
///
/// `run-compositor` does not come this way, and draws the dither.
fn undithered(config: &str) -> String {
    format!("decoration:blur:noise = 0\n{config}")
}

fn boot_and_dump(
    arch: Arch,
    programs: &Programs,
    config: &str,
    wanted: &Wanted<'_>,
    binds: &[(&str, &[&str])],
    args: &Args,
) -> Result<(Vec<Image>, Vec<String>)> {
    let (image, kernel) = build_image(arch, programs, &undithered(config), Carried::none(), args)?;

    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    qemu_args.screens = wanted.screens();
    let dump = paths::build_dir(arch).join("compositor.ppm");
    let mut taken = Vec::new();
    let mut said = Vec::new();
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        if let Some(line) = watching
            .lines()
            .iter()
            .rev()
            .find(|line| line.contains(FAILED))
        {
            return Err(Error::new(format!("{arch}: {}", line.trim())));
        }
        // The boot stops at the first `hyprix:` line, which is the seat
        // saying what it opened; the screen is up a little later.
        let up = watching.read_more(Instant::now() + SETTLE, |lines| {
            lines.iter().any(|line| line.contains(MARKER))
        })?;
        if !up && !watching.lines().iter().any(|line| line.contains(MARKER)) {
            return Err(Error::new(format!(
                "{arch}: the compositor never printed `{MARKER}`"
            )));
        }
        say_the_marker(watching, arch);

        for (index, (what, path)) in wanted.states.iter().enumerate() {
            ask_for_state(&mut qmp, arch, index, binds, wanted.pointer)?;
            let want = expected(path)?;
            let screen = settle(&mut qmp, &dump, &want)?;
            let (found, count) = differences(&screen, &want);
            if count != 0 {
                // What the guest said, which is where the reason usually
                // is: a keybind that did not fire, a client that died, a
                // program that could not start.
                let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
                return Err(with_the_transcript(
                    &unexpected(arch, what, &screen, found, count),
                    watching,
                ));
            }
            println!(
                "  {arch}: {what}, every one of {} pixels as the renderer draws them",
                screen.width * screen.height
            );
            taken.push(screen);
        }

        // A window sliding: the keybind, then every distinct picture until
        // the one it ends in. What is required of them is the caller's --
        // how many there were, and what the last is -- because a frame
        // part-way through a slide is drawn at whatever time it was drawn
        // and cannot be an expected image.
        if let Some(moving) = wanted.moving {
            press(&mut qmp, moving.keys)?;
            println!("  {arch}: pressed the keys that start {}", moving.what);
            let mut kept = follow(&mut qmp, &dump, &expected(moving.path)?)?;
            println!(
                "  {arch}: {}: {} pictures, the last of them the one it ends in",
                moving.what,
                kept.len()
            );
            taken.append(&mut kept);
            // The compositor says how long its frames took while it draws,
            // and a boot with no keybinds to press asks the serial port for
            // nothing else: read what it said, so the caller can judge it.
            let _ = watching.read_more(Instant::now() + SETTLE, |lines| {
                lines
                    .iter()
                    .any(|line| line.contains("slowest of the last"))
            })?;
        }

        // The screens after the first, once the states have been reached:
        // each is a device of its own in QEMU, and a screendump names it.
        for (index, (what, path)) in wanted.others.iter().enumerate() {
            let which = u32::try_from(index).unwrap_or(0).saturating_add(1);
            let want = expected(path)?;
            let screen = settle_on(&mut qmp, &crate::display::device_id(which), &dump, &want)?;
            let (found, count) = differences(&screen, &want);
            if count != 0 {
                return Err(unexpected(arch, what, &screen, found, count));
            }
            println!(
                "  {arch}: screen {which}: {what}, every one of {} pixels as the renderer draws \
                 them",
                screen.width * screen.height
            );
            taken.push(screen);
        }

        if !binds.is_empty() {
            ask_the_sockets(&mut qmp, watching, arch)?;
        }
        wait_for(watching, wanted.awaiting)?;
        // Whatever else the guest said by now, so that what is checked
        // against the transcript is what the boot actually printed rather
        // than what had been read when the last picture matched.
        let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
        said = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    // A machine that stopped is not a boot that passed, whatever its screen
    // showed first. The pictures are judged as they are taken, and a kernel
    // that panics a moment after the last one matched had a right picture on
    // a dead machine: three boots did exactly that and said they had passed,
    // the night the compositor first drew on several threads, before a
    // fourth stopped early enough to spoil its picture.
    if let Some(line) = said.iter().find(|line| line.contains("FERRIX-PANIC")) {
        return Err(Error::new(format!(
            "{arch}: the kernel stopped while the compositor ran: {}",
            line.trim()
        )));
    }
    Ok((taken, said))
}

/// Read the guest until every line in `awaiting` has been said, or the time
/// is up.
///
/// Giving up quietly is right: what is required of the lines is the caller's
/// to say, and a failure there prints the whole transcript, which is more
/// use than "the wait timed out".
fn wait_for(watching: &mut Watching<'_>, awaiting: &[&str]) -> Result<()> {
    if awaiting.is_empty() {
        return Ok(());
    }
    let _ = watching.read_more(Instant::now() + SETTLE, |lines| {
        awaiting
            .iter()
            .all(|want| lines.iter().any(|line| line.contains(want)))
    })?;
    Ok(())
}

/// Build the bootable image for one boot: the compositor as init, the
/// client, `hyprctl` and the plugin in the initramfs, and the configuration
/// beside them.
fn build_image(
    arch: Arch,
    programs: &Programs,
    config: &str,
    carried_too: Carried,
    args: &Args,
) -> Result<(PathBuf, PathBuf)> {
    let (loader, kernel, initramfs) = build_parts(arch, programs, config, carried_too, args)?;
    // The kernel as well as the image: the watcher symbolises a panic's
    // addresses out of it.
    let image = crate::fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
    Ok((image, kernel))
}

/// What [`build_image`] puts in an image, which is also what `flash` copies
/// onto a card: the loader, the kernel with the compositor as init, and the
/// initramfs.
fn build_parts(
    arch: Arch,
    programs: &Programs,
    config: &str,
    carried_too: Carried,
    args: &Args,
) -> Result<(PathBuf, PathBuf, Vec<u8>)> {
    let loader = crate::cargo::build_loader(arch, args.release)?;
    // One argument a line: a script has no quoting, and `Options::unshell`
    // says so. `--instance` is what puts the control socket where `hyprctl`
    // looks for it.
    let script = format!("--config\n/{CONFIG_PATH}\n--instance\n{INSTANCE}");
    let kernel =
        crate::cargo::build_kernel_with_init(arch, args.release, &programs.hyprix, &script)?;
    let natives = crate::native::build(arch, args.release)?;
    let read = |path: &Path| -> Result<Vec<u8>> {
        std::fs::read(path)
            .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
    };
    let mut carried = Vec::new();
    for (path, program) in programs.carried() {
        carried.push(crate::ports::File {
            path: path.to_owned(),
            mode: 0o755,
            content: crate::ports::Content::Bytes(read(program)?),
        });
    }
    carried.push(crate::ports::File {
        path: CONFIG_PATH.to_owned(),
        mode: 0o644,
        content: crate::ports::Content::Bytes(config.as_bytes().to_vec()),
    });
    // The busybox, zinc and the ported programs, when a caller asked for
    // them. A gate boot asks for none and its archive is the bytes it always
    // was; `run-compositor` asks for all three, because a shell whose every
    // command answers `command not found` is not one anybody can use. The
    // busybox and zinc go in through the same slots `build` and `run` use, so
    // the applet links, `/etc/passwd` and `/bin/zsh` come with them.
    carried.extend(carried_too.ports);
    let initramfs = crate::initramfs::build(
        carried_too.busybox.as_deref(),
        &natives,
        carried_too.zinc.as_deref(),
        &carried,
    )?;
    Ok((loader, kernel, initramfs))
}

/// Print the line the compositor said when its screen came up.
fn say_the_marker(watching: &Watching<'_>, arch: Arch) {
    let marker = watching
        .lines()
        .iter()
        .chain(watching.after())
        .rev()
        .find(|line| line.contains(MARKER))
        .map_or("", |line| line.trim());
    println!("  {arch}: {}", marker.trim_start_matches("| ").trim());
}

/// Ask the compositor about itself, from inside the guest.
///
/// `hyprctl` is carried in the initramfs and a keybind `exec`s it, because
/// Hyprland's own is not on Ferrix's image and `exec` is how a person starts
/// anything.
fn ask_the_sockets(qmp: &mut Qmp, watching: &mut Watching<'_>, arch: Arch) -> Result<()> {
    for (name, keys) in ASKED {
        press(qmp, keys)?;
        println!("  {arch}: pressed {name}");
    }
    // The answers are whole when the second command's last line is in.
    let _ = watching.read_more(Instant::now() + SETTLE, |lines| {
        lines.iter().any(|line| line.contains("workspace: 1"))
            && lines.iter().filter(|line| line.contains("title: ")).count() >= 3
    })?;
    Ok(())
}

/// Do whatever the state at `index` is reached by: the keybind before it,
/// and the pointer movement if the boot asked for one.
///
/// Each keybind but the first is sent after the picture before it has
/// settled, so that a state is never judged before the compositor has been
/// asked to make it.
fn ask_for_state(
    qmp: &mut Qmp,
    arch: Arch,
    index: usize,
    binds: &[(&str, &[&str])],
    pointer: Option<(i32, i32)>,
) -> Result<()> {
    if let Some((name, keys)) = binds.get(index.wrapping_sub(1)) {
        press(qmp, keys)?;
        println!("  {arch}: pressed {name}");
    }
    if index == 1
        && let Some((x, y)) = pointer
    {
        let (steps, every) = SWEEP;
        for step in 0..steps {
            let (x, y) = swept(step);
            qmp.input_send_event(&[absolute("x", x), absolute("y", y)])?;
            std::thread::sleep(every);
        }
        qmp.input_send_event(&[absolute("x", x), absolute("y", y)])?;
        println!("  {arch}: swept the pointer through {steps} places and put it down");
    }
    Ok(())
}

/// One axis of an absolute pointer movement, as QMP takes it.
///
/// The value is 0..0x7FFF across the screen, which is what QEMU's virtio
/// tablet reports and what the compositor's seat turns back into pixels.
fn absolute(axis: &str, value: i32) -> String {
    format!(
        "{{\"type\":\"abs\",\"data\":{{\"axis\":{},\"value\":{value}}}}}",
        crate::display::json_string(axis)
    )
}

/// Press and release `keys` in order, as a hand does: the modifiers first,
/// the key last, and everything let go in reverse.
fn press(qmp: &mut Qmp, keys: &[&str]) -> Result<()> {
    for name in keys {
        qmp.input_send_event(&[key(name, true)])?;
    }
    for name in keys.iter().rev() {
        qmp.input_send_event(&[key(name, false)])?;
    }
    Ok(())
}

/// `cargo xtask test-video`: boot a wallpaper that moves, and require the
/// screen to show its frames in turn.
///
/// The video is a four-frame AV1 test pattern checked into the repository,
/// so the gate needs no `ffmpeg` on the machine that runs it. What is being
/// tested is the whole path: the format, the client that decodes and plays
/// it, the layer surface it plays on, and the compositor drawing frame after
/// frame of it.
///
/// Nothing else is started, so the wallpaper is the whole screen.
///
/// # Errors
///
/// A guest whose screen never showed both frames.
pub(crate) fn test_video(args: &Args) -> Result<()> {
    for arch in args.arches()? {
        if crate::display::target(arch).is_none() {
            println!("  {arch}: no virtio-gpu in QEMU's machine; skipped");
            continue;
        }
        let programs = Programs::build(arch)?;
        video_boot(arch, &programs, args)?;
    }
    Ok(())
}

/// The boot `test_video` judges.
fn video_boot(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let size = args.size.unwrap_or(crate::wallpaper::SCREEN);
    let video = crate::wallpaper::fixture();
    println!(
        "  {arch}: a four-frame AV1 video, scaled to {}x{}, {} KiB",
        size.0,
        size.1,
        video.len() / 1024
    );
    let config = format!(
        "# Written into the initramfs by `cargo xtask test-video`.\n\
         monitor = , {}x{}@60, auto, 1\n\
         exec-once = /{CLIENT_PATH} --video /{MOVIE_PATH}\n",
        size.0, size.1
    );
    let mut carried = Carried::none();
    carried.ports.push(crate::ports::File {
        path: MOVIE_PATH.to_owned(),
        mode: 0o644,
        content: crate::ports::Content::Bytes(video),
    });
    let mut qemu_args = args.clone();
    qemu_args.size = Some(size);
    let (image, kernel) = build_image(arch, programs, &config, carried, &qemu_args)?;

    let port = free_port()?;
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("video.ppm");
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        let seen =
            both_frames(&mut qmp, &dump).map_err(|error| with_the_transcript(&error, watching))?;
        println!(
            "  {arch}: the screen showed two AV1 frames of the video, {seen} screendumps apart"
        );
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, MARKER, hook)?;
    Ok(())
}

/// Take screendumps until the moving AV1 clip produced two distinct, non-flat
/// screens, and say how many dumps that took.
fn both_frames(qmp: &mut Qmp, dump: &Path) -> Result<usize> {
    let deadline = Instant::now() + SETTLE;
    let mut first: Option<Vec<u8>> = None;
    let mut distinct = false;
    let mut dumps = 0_usize;
    let mut last;
    loop {
        qmp.screendump(Some(DEVICE_ID), dump)?;
        let bytes = std::fs::read(dump)
            .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?;
        let screen = parse_ppm(&bytes)?;
        dumps = dumps.saturating_add(1);
        let pixels = screen.width.saturating_mul(screen.height);
        let varied = screen
            .pixels
            .chunks_exact(3)
            .any(|pixel| pixel != screen.pixels.get(..3).unwrap_or(&[]));
        last = format!(
            "{}x{}, {pixels} pixels, {}",
            screen.width,
            screen.height,
            if varied {
                "a non-flat frame"
            } else {
                "a flat frame"
            }
        );
        if varied && pixels > 0 {
            match &first {
                Some(previous) => distinct |= previous != &screen.pixels,
                None => first = Some(screen.pixels),
            }
        }
        if distinct {
            return Ok(dumps);
        }
        if Instant::now() >= deadline {
            return Err(Error::new(format!(
                "the screen never showed two AV1 frames of the video in {}s: {} dumps, the last {last}",
                SETTLE.as_secs(),
                dumps
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// One `key` event of QMP's `input-send-event`, as its JSON.
fn key(name: &str, down: bool) -> String {
    format!(
        "{{\"type\":\"key\",\"data\":{{\"down\":{down},\"key\":\
         {{\"type\":\"qcode\",\"data\":{}}}}}}}",
        crate::display::json_string(name)
    )
}

/// Take screendumps as fast as QEMU gives them until one is `want` or the
/// time is up, keeping every picture that differs from the one before it.
///
/// This is how a slide is watched: a window part-way along its curve is at a
/// place no expected image can hold, so what a test can require is that
/// there were several of them and that the last is where it was going.
fn follow(qmp: &mut Qmp, dump: &Path, want: &[u8]) -> Result<Vec<Image>> {
    let mut kept: Vec<Image> = Vec::new();
    let deadline = Instant::now() + SETTLE;
    loop {
        qmp.screendump(Some(DEVICE_ID), dump)?;
        let bytes = std::fs::read(dump)
            .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?;
        let screen = parse_ppm(&bytes)?;
        let fresh = kept.last().is_none_or(|last| last.pixels != screen.pixels);
        let done = differences(&screen, want).1 == 0;
        if fresh {
            kept.push(screen);
        }
        if done || Instant::now() >= deadline {
            return Ok(kept);
        }
    }
}

/// Take screendumps until one is `want`, or until the time is up.
///
/// A client has to draw and commit and the compositor has to compose and
/// flip, and each step is a round trip; the last dump taken is the one
/// judged, so a state that never arrives is reported as the picture it
/// stopped at rather than as a timeout.
fn settle(qmp: &mut Qmp, dump: &Path, want: &[u8]) -> Result<Image> {
    settle_on(qmp, DEVICE_ID, dump, want)
}

/// The same, of one named device: a machine with two screens has a
/// virtio-gpu each, and a screendump names which.
fn settle_on(qmp: &mut Qmp, device: &str, dump: &Path, want: &[u8]) -> Result<Image> {
    let deadline = Instant::now() + SETTLE;
    loop {
        qmp.screendump(Some(device), dump)?;
        let bytes = std::fs::read(dump)
            .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?;
        let screen = parse_ppm(&bytes)?;
        if differences(&screen, want).1 == 0 || Instant::now() >= deadline {
            return Ok(screen);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// An error with the last of what the guest said after it.
///
/// A picture that is not the one expected says nothing about why; the lines
/// the compositor and its clients printed usually do.
fn with_the_transcript(error: &Error, watching: &Watching<'_>) -> Error {
    let every: Vec<&String> = watching.lines().iter().chain(watching.after()).collect();
    let said: Vec<String> = every
        .iter()
        .skip(every.len().saturating_sub(20))
        .map(|line| line.trim().to_owned())
        .collect();
    Error::new(format!(
        "{error}\n  the guest's last lines:\n    {}",
        said.join("\n    ")
    ))
}

/// Why a state is not the picture it should be.
///
/// A screen that is all background is the clients never having drawn, which
/// is a different failure from a wrong picture and sends whoever reads it
/// somewhere else.
fn unexpected(
    arch: Arch,
    what: &str,
    screen: &Image,
    found: Option<(usize, usize)>,
    count: usize,
) -> Error {
    let blank = mismatches(screen, BACKGROUND, 0).1 == 0;
    let why = if blank {
        "the screen is the compositor's background: no client drew"
    } else {
        "the picture is not the one the renderer's own tests bless"
    };
    Error::new(format!(
        "{arch}: with {what}, {why}; {count} of {} pixels differ, the first: {found:?}",
        screen.width * screen.height
    ))
}

/// `test-compositor` on each architecture that has a virtio-gpu.
///
/// # Errors
///
/// A screen that is not the compositor's background, or a compositor that
/// never reached one.
pub(crate) fn test_compositor(args: &Args) -> Result<()> {
    for arch in args.arches()? {
        if crate::display::target(arch).is_none() {
            println!("  {arch}: no virtio-gpu in QEMU's machine; skipped");
            continue;
        }
        let programs = Programs::build(arch)?;
        // `--gl` puts a GPU behind the card, and then the compositor draws
        // on it. Every boot below judges QEMU's screendump, which cannot
        // read such a card's console (`docs/GPU.md` §3.1), so a GPU boot is
        // one of its own, judged from inside the guest.
        if args.gl {
            test_gpu(arch, &programs, args)?;
            continue;
        }
        for (name, boot) in BOOTS {
            if wanted(args, name) {
                boot(arch, &programs, args)?;
            }
        }
    }
    Ok(())
}

/// Where the GPU boot's expected image is on the guest.
const GPU_EXPECTED_PATH: &str = "etc/expected.xrle";

/// The configuration the GPU boot is given: the decorated pair -- corners,
/// shadows, an opacity and a dim, which between them are every shader but
/// the blur's -- and a key that holds a screenshot to the expected image.
///
/// The settings are `DECORATED_CONFIG`'s, for its reason: the picture is
/// `compositor/render`'s `decorated_style`.
const GPU_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor --gl`.
decoration:rounding = 12
decoration:inactive_opacity = 0.6
decoration:shadow:range = 12
decoration:shadow:render_power = 2
decoration:dim_inactive = 1
decoration:dim_strength = 0.4
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = , S, exec, /bin/shot 0 /etc/expected.xrle
";

/// What the compositor says when a screen's frames are drawn on the GPU,
/// and what it says when they stop being.
const ON_THE_GPU: &str = "frames are drawn on the GPU";
const IN_SOFTWARE: &str = "drawing in software";

/// How long the GPU boot keeps asking for a screenshot that matches: the
/// clients have to connect, draw and be tiled first, and a picture taken
/// before they have is a true picture of something else.
const GPU_PATIENCE: Duration = Duration::from_secs(90);

/// `test-compositor --gl`: the compositor drawing on the GPU, in the guest.
///
/// The frame is drawn by `compositor/render`'s GPU painter through
/// `/dev/dri/renderD128` -- the kernel's render node, the ring-3 driver,
/// virtio-gpu's 3D commands and the host's virglrenderer -- and what is
/// required is that it is the picture the software renderer blesses.
///
/// QEMU cannot be asked: `screendump` reads a surface and a GL console has
/// none. So the guest judges itself. `/bin/shot` takes a screenshot through
/// `zwlr_screencopy_v1`, reads the expected image off its own filesystem,
/// and says how many channels are more than a step from it; a GPU's frame
/// is the same picture and not the same bytes, which is why the verdict
/// crosses the serial port and a digest does not. Two more things are
/// required, because a screenshot that matches proves the picture and not
/// who drew it: the compositor must say it draws on the GPU, and must not
/// say it fell back.
fn test_gpu(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let expected_image = std::fs::read(paths::workspace_root().join(DECORATED_EXPECTED.1))
        .map_err(|error| Error::new(format!("{}: {error}", DECORATED_EXPECTED.1)))?;
    let carried = Carried {
        ports: vec![crate::ports::File {
            path: GPU_EXPECTED_PATH.to_owned(),
            mode: 0o644,
            content: crate::ports::Content::Bytes(expected_image),
        }],
        ..Carried::none()
    };
    let (image, kernel) = build_image(arch, programs, &undithered(GPU_CONFIG), carried, args)?;
    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let mut said: Vec<String> = Vec::new();
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        let up = watching.read_more(Instant::now() + SETTLE, |lines| {
            lines
                .iter()
                .any(|line| line.contains(MARKER) || line.contains(FAILED))
        })?;
        if !up {
            return Err(with_the_transcript(
                &Error::new(format!("{arch}: the compositor never printed `{MARKER}`")),
                watching,
            ));
        }
        say_the_marker(watching, arch);
        // Ask until the picture is the expected one. Each press is one more
        // `shot:` line; the last of them is what is judged.
        let deadline = Instant::now() + GPU_PATIENCE;
        loop {
            press(&mut qmp, &["s"])?;
            // `shot: ` begins its own line and ends the compositor's
            // `started /bin/shot`: only the first is an answer.
            let answers = |lines: &[String]| {
                lines
                    .iter()
                    .filter(|line| said_on_its_own(line).starts_with("shot: "))
                    .count()
            };
            // What `read_more` hands its closure is what was said after the
            // boot's first line, so that is what is counted before too.
            let before = answers(watching.after());
            let _ = watching.read_more(Instant::now() + Duration::from_secs(20), |lines| {
                answers(lines) > before
            })?;
            let matched = watching
                .lines()
                .iter()
                .chain(watching.after())
                .rev()
                .map(|line| said_on_its_own(line))
                .find(|line| line.starts_with("shot: "))
                .is_some_and(|line| line.contains(": 0 channels more than"));
            if matched || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
        said = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    judge_gpu(arch, &said)
}

/// What [`test_gpu`] requires of what the guest said.
fn judge_gpu(arch: Arch, said: &[String]) -> Result<()> {
    let transcript = || {
        said.iter()
            .map(|line| said_on_its_own(line).to_owned())
            .filter(|line| line.starts_with("hyprix: ") || line.starts_with("shot: "))
            .collect::<Vec<_>>()
            .join("\n    ")
    };
    if let Some(line) = said.iter().find(|line| line.contains("FERRIX-PANIC")) {
        return Err(Error::new(format!(
            "{arch}: the kernel stopped while the compositor ran: {}",
            line.trim()
        )));
    }
    if !said.iter().any(|line| line.contains(ON_THE_GPU)) {
        return Err(Error::new(format!(
            "{arch}: the compositor did not say it draws on the GPU:\n    {}",
            transcript()
        )));
    }
    if let Some(line) = said.iter().find(|line| line.contains(IN_SOFTWARE)) {
        return Err(Error::new(format!(
            "{arch}: the compositor gave the GPU up: {}",
            line.trim()
        )));
    }
    let Some(verdict) = said
        .iter()
        .rev()
        .map(|line| said_on_its_own(line))
        .find(|line| line.starts_with("shot: "))
    else {
        return Err(Error::new(format!(
            "{arch}: the guest took no screenshot:\n    {}",
            transcript()
        )));
    };
    if !verdict.contains(": 0 channels more than") {
        return Err(Error::new(format!(
            "{arch}: the GPU's frame is not the expected image: `{verdict}`\n    {}",
            transcript()
        )));
    }
    println!("  {arch}: the compositor draws on the GPU, and the guest's own screenshot says:");
    println!("  {arch}:   {verdict}");
    Ok(())
}

/// The configuration `run-compositor` writes when none was given: a desktop
/// somebody can drive.
///
/// The gate's `CONFIG` is written for a screendump -- two clients, and binds
/// a test presses -- and a person sitting in front of the window wants the
/// rest of what a keyboard is for. So this one opens a terminal as its first
/// `exec-once`: the boot ends at a shell prompt rather than at a picture,
/// which is what somebody who asked to watch the compositor asked for.
///
/// Every dispatcher named here is one `compositor/layout` has. `SUPER+P`
/// starts a `compositor/pattern` client, which is how the tiling a gate boot
/// shows is reached from a configuration that starts none.
const RUN_CONFIG: &str = "# Written into the initramfs by `cargo xtask run-compositor`.
# `--config <PATH>` carries a real `hyprland.conf` instead of this one.
# A terminal first, because a screen somebody is watching is one they want to
# type into: `/bin/term` runs a program on a pseudoterminal and `/bin/zinc`
# is the shell, with the busybox applets the image carries beside it. The
# pattern clients a gate boot tiles are a keybind away rather than started
# here: somebody who opened a desktop wants a shell, not a test pattern.
#
# `decoration:blur:enabled` is already Hyprland's own default, but the blur
# it draws is only what shows through a translucent window -- an opaque one
# covers it completely -- so the terminal needs an opacity below 1 for its
# own default to be visible at all.
windowrule = opacity 0.88, match:class ^(rocks\\.magical\\.term)$
exec-once = /bin/term /bin/zinc
bind = SUPER, RETURN, exec, /bin/term /bin/zinc
bind = SUPER, P, exec, /bin/pattern gradient another
bind = SUPER, Q, killactive
bind = SUPER, F, fullscreen
bind = SUPER, V, togglefloating
bind = SUPER, L, movefocus, l
bind = SUPER, H, movefocus, r
bind = SUPER SHIFT, L, movewindow, r
bind = SUPER SHIFT, H, movewindow, l
bind = SUPER, 1, workspace, 1
bind = SUPER, 2, workspace, 2
bind = SUPER SHIFT, 1, movetoworkspace, 1
bind = SUPER SHIFT, 2, movetoworkspace, 2
bind = SUPER, C, exec, /bin/hyprctl clients
bind = SUPER, W, exec, /bin/hyprctl activewindow
";

/// `config` with the interface brought up, when the boot has a network.
///
/// `--net` puts a virtio-net device on the bus and xtask's own gateway behind
/// it, and that is all it does: the guest has a device and no address, so a
/// name does not resolve and `ping` answers `bad address` for want of a
/// route rather than for want of a resolver. Every boot check that uses the
/// network runs `udhcpc` first, from its init script; a watched boot has no
/// init script, so the same command goes in as an `exec-once`. It is the
/// busybox applet, so it needs the busybox this boot carries: without one
/// there is nothing to run and the line would be a diagnostic at boot rather
/// than an address.
///
/// The address, the route and `/etc/resolv.conf` all come from the lease.
/// What answers the names is the gateway's own resolver at `10.0.2.3`, which
/// forwards what it does not serve to this host's.
fn with_network(config: String, args: &Args) -> String {
    if !args.net {
        return config;
    }
    let mut config = config;
    if !config.ends_with('\n') {
        config.push('\n');
    }
    config.push_str("# Appended by `cargo xtask run-compositor --net`:\n");
    config.push_str(&format!("exec-once = /bin/{DHCP}\n"));
    println!("  network: the guest runs `{DHCP}` for its address");
    config
}

/// What brings the interface up, which is what every boot check that uses
/// the network runs before it: busybox's client, the tries and the timeout
/// `initramfs`'s own profile gives it.
const DHCP: &str = "udhcpc -i eth0 -n -q -t 5 -T 2";

/// `config` with the layouts `--layout` and `--variant` asked for.
///
/// Appended rather than substituted, and appended to a person's own file as
/// readily as to [`RUN_CONFIG`]: a later line overrides an earlier one, which
/// is hyprlang's rule and this configuration parser's, so two lines at the
/// end are the whole of it. A `--config` that sets `input:kb_layout` and a
/// `--layout` that disagrees means the flag wins, which is the way round a
/// flag typed now should win over a file written earlier.
///
/// The names are not checked here. The compositor ships the keymaps it has
/// and says which one it gave -- `hyprix: no keymap for kb_layout = ru,
/// kb_variant = ; using us` -- and a second opinion in this tool would be
/// one more place to keep the list.
fn with_layout(config: String, args: &Args) -> String {
    if args.layout.is_none() && args.variant.is_none() {
        return config;
    }
    let mut config = config;
    if !config.ends_with('\n') {
        config.push('\n');
    }
    config.push_str("# Appended by `cargo xtask run-compositor`:\n");
    if let Some(layout) = &args.layout {
        println!("  keyboard: input:kb_layout = {layout}");
        config.push_str(&format!("input:kb_layout = {layout}\n"));
    }
    if let Some(variant) = &args.variant {
        println!("  keyboard: input:kb_variant = {variant}");
        config.push_str(&format!("input:kb_variant = {variant}\n"));
    }
    config
}

/// What a boot's image carries to type into: the busybox whose applets are
/// most of what a person types, zinc, the shell that runs them, and the
/// programs ported onto ferrousli.
///
/// A gate boot carries neither. Its archive is compared byte for byte against
/// what it has always been, and nothing it checks needs `ls`.
///
/// A watched boot carries both, and the terminal bind is the reason: a shell
/// whose every command answers `command not found` is not a terminal anybody
/// can use. That was the first thing tried in the window and it is what this
/// exists to fix.
struct Carried {
    /// The busybox, which goes to `/bin/busybox` with a link for each applet.
    busybox: Option<PathBuf>,
    /// zinc's bytes, which go to `/bin/zinc` with `/bin/zsh` beside them.
    zinc: Option<Vec<u8>>,
    /// What `cargo xtask ports` built -- curl and btop -- when this machine
    /// has them. `build` and `run` put them on every image they make; a
    /// watched boot wants them for the same reason it wants the applets, and
    /// a gate boot's archive names none.
    ports: Vec<crate::ports::File>,
}

impl Carried {
    /// Neither, which is what every judged boot asks for.
    fn none() -> Self {
        Self {
            busybox: None,
            zinc: None,
            ports: Vec::new(),
        }
    }

    /// What a watched boot should carry, from `--init` or from whatever
    /// busybox this machine already has.
    ///
    /// `--init` is the same flag `build`, `run` and `test-shell` take, with
    /// the same meanings: a path, `{arch}` replaced, or `ferrousli` for the
    /// one built against this tree's own library. Given nothing, an installed
    /// busybox is used where there is one and skipped where there is not:
    /// somebody who asked to look at the compositor did not ask to wait for a
    /// busybox to be built, and a screen with no shell is still the screen
    /// they wanted.
    ///
    /// # Errors
    ///
    /// A `--init` that names no file, or a zinc that will not build.
    fn wanted(arch: Arch, args: &Args) -> Result<Self> {
        let asked = crate::optional_program(arch, args)?;
        let busybox = match asked {
            Some(program) => Some(program),
            None => crate::busybox::installed_program(arch),
        };
        match &busybox {
            Some(program) => println!("  busybox {} in /bin", program.display()),
            None => {
                println!("  no busybox: zinc's own builtins are all the shell has");
                println!("    `cargo xtask busybox` builds one, or --init <PATH> names one");
            }
        }
        let ports = crate::ports::installed(arch)?;
        if !ports.is_empty() {
            println!("  {} ported files in /bin and /etc", ports.len());
        }
        Ok(Self {
            busybox,
            zinc: crate::zinc::build(arch)?,
            ports,
        })
    }
}

/// `cargo xtask run-compositor`: the compositor on a screen a person watches.
///
/// The same image `test-compositor` boots -- the compositor as init, its
/// clients and `hyprctl` in the initramfs -- with three differences, each of
/// which is what "watch it" means rather than "judge it":
///
/// * QEMU gets a window, or a VNC server where this host has no way to open
///   one. `window` decides which and says so.
/// * The serial port is this terminal, as `run`'s is, so the compositor's own
///   log is in front of the person watching it and `Ctrl-A x` ends the boot.
/// * The accelerator is `auto`, again as `run`'s is: a screen somebody is
///   looking at wants the hypervisor this host has, where a test wants `tcg`
///   on every host to be the same test.
///
/// The keyboard and the pointer are QEMU's own virtio devices, which the
/// window forwards to -- the same path `test-seat` drives over QMP -- so the
/// binds in [`RUN_CONFIG`] are pressed by pressing them.
///
/// # Errors
///
/// An architecture QEMU has no virtio-gpu for, a `--config` that cannot be
/// read, or a QEMU that cannot show a screen at all.
pub(crate) fn run_compositor(args: &Args) -> Result<()> {
    let arch = args.single_arch()?;
    if crate::display::target(arch).is_none() {
        return Err(Error::new(format!(
            "{arch} has no virtio-gpu in QEMU's machine; the compositor runs on x86_64 and aarch64"
        )));
    }
    let mut args = args.clone();
    crate::rustc::prepare_default(arch, &mut args)?;
    let config = match &args.config {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|error| Error::new(format!("reading {path}: {error}")))?,
        None => RUN_CONFIG.to_owned(),
    };
    // A watched boot has a network unless it was told not to: a person at a
    // screen expects a machine that can fetch something, and finding out
    // that `ping` says `bad address` for want of a device is nobody's
    // idea of a lesson. A boot that is judged still asks for `--net`,
    // because the bus a check enumerates must be the bus it has always
    // enumerated.
    let args = &Args {
        net: !args.no_net,
        ..crate::ssh::checked(&args)
    };
    let programs = Programs::build(arch)?;
    let size = args.size.unwrap_or(crate::wallpaper::SCREEN);
    let (config, carried) = desktop(arch, config, size, args)?;
    let (image, _) = build_image(arch, &programs, &config, carried, args)?;
    // The host's GPU behind the card where it can be had: `window::watched_gl`
    // says when, and why it is the default for a desktop somebody watches.
    let args = crate::window::watched_gl(arch, args)?;
    let args = Args {
        // The card, the keyboard and the tablet: `--display` is what puts a
        // virtio-gpu on the bus, and without one the compositor has no
        // `/dev/dri` to open. `test-compositor` sets it the same way.
        display: true,
        size: Some(size),
        accel: args
            .accel
            .clone()
            .or_else(|| (!args.gdb).then(|| "auto".to_owned())),
        ..args
    };
    println!("  {arch}: the compositor is init; its log is this terminal");
    crate::qemu::run(arch, &image, &args)
}

/// The screen a board's HDMI output runs: the DK1's LTDC scans out 720p60
/// and nothing else (`docs/DISPLAY.md` §6).
const BOARD_SCREEN: (u32, u32) = (1280, 720);

/// What `flash --compositor` puts on a card: the desktop `run-compositor`
/// boots -- the compositor as init, its clients, `hyprctl`, a shell -- as the
/// loader, the kernel and the initramfs `flash` copies.
///
/// No network, since the board has none Ferrix drives, and no wallpaper
/// unless one is named: a picture scaled every frame, let alone a video
/// decoded, is a large share of what a 650 MHz Cortex-A7 has to give.
pub(crate) fn board_files(arch: Arch, args: &Args) -> Result<(PathBuf, PathBuf, Vec<u8>)> {
    if crate::display::target(arch).is_none() {
        return Err(Error::new(format!(
            "the compositor is not built for {arch}"
        )));
    }
    let config = match &args.config {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|error| Error::new(format!("reading {path}: {error}")))?,
        None => RUN_CONFIG.to_owned(),
    };
    // A shell with no `ls` or `mkdir` is what the board's first desktop had:
    // the only busybox `Carried::wanted` finds unasked is ferrousli's, which
    // has no ARM port. So the static one the gates boot is carried, when it
    // is where they keep it and nothing else was named.
    let init = args
        .init
        .clone()
        .or_else(|| std::env::var("FERRIX_INIT").ok())
        .or_else(|| gates_busybox(arch));
    let args = &Args {
        net: false,
        wallpaper: args.wallpaper.clone().or_else(|| Some("none".to_owned())),
        init,
        ..args.clone()
    };
    let programs = Programs::build(arch)?;
    let size = args.size.unwrap_or(BOARD_SCREEN);
    let (config, carried) = desktop(arch, config, size, args)?;
    build_parts(arch, &programs, &config, carried, args)
}

/// The static busybox the gates boot as `--init`, at
/// `~/.local/share/ferrix/busybox/<arch>/bin/busybox.static` (Alpine's
/// `busybox-static`), if this machine has one.
fn gates_busybox(arch: Arch) -> Option<String> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = Path::new(&home)
        .join(".local/share/ferrix/busybox")
        .join(arch.name())
        .join("bin/busybox.static");
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

/// A desktop a person uses, from `config`: the layout and network lines,
/// the ssh server when asked for, the screen's size as a `monitor =` line,
/// and a wallpaper; with the busybox, zinc and ports that ride along.
fn desktop(arch: Arch, config: String, size: (u32, u32), args: &Args) -> Result<(String, Carried)> {
    let config = with_network(with_layout(config, args), args);
    let mut carried = Carried::wanted(arch, args)?;
    carried.ports.extend(crate::rustc::default_links(args));
    let config = crate::ssh::with_server(config, args, &mut carried.ports)?;
    // A wallpaper, from this machine's own and from nowhere else:
    // `crate::wallpaper` says where they come from and why a run never goes
    // looking. It is started before anything the configuration starts, so
    // the first thing on the screen is a desktop and not a grey one; a real
    // `hyprland.conf` starts a wallpaper program the image does not have,
    // so the line is added to one of those too.
    // The screen's size, said twice. To QEMU, as the card's `xres` and
    // `yres`, which is what a screen served over VNC is. And to the
    // compositor, as a `monitor =` line ahead of whatever the configuration
    // says itself: under a *window* the card prefers the window's size,
    // which for a window QEMU has just opened is 640x480 whatever it was
    // told, and the kernel lists the standard sizes beside that one so that
    // such a line can pick. A configuration's own line for the monitor
    // comes later and wins.
    let config = format!("monitor = , {}x{}@60, auto, 1\n{config}", size.0, size.1);
    // A wallpaper that moves is started the way a still one is, and the way
    // `mpvpaper ALL <file>` is started from a Linux desktop's `exec-once`:
    // the difference is the flag, and that the frames were decoded on a
    // machine that has a decoder.
    let config = match crate::wallpaper::file(args, size)? {
        Some(chosen) => {
            // A moving one is started the way `mpvpaper` is on a desktop,
            // down to the words: `-o no-audio` and `ALL` mean here what they
            // mean there, so a line copied either way says the same thing.
            let (path, how) = match chosen {
                crate::wallpaper::Chosen::Still(_) => {
                    (WALLPAPER_PATH, format!("--wallpaper /{WALLPAPER_PATH}"))
                }
                // One word after `-o`, because `exec-once` is split on
                // whitespace here and not by a shell: `hyprix::state` reads
                // it with `split_whitespace`, so a quoted string would
                // arrive as three arguments and two of them with quotes in.
                crate::wallpaper::Chosen::Moving(_) => {
                    (MOVIE_PATH, format!("--video -o no-audio ALL /{MOVIE_PATH}"))
                }
            };
            carried.ports.push(crate::ports::File {
                path: path.to_owned(),
                mode: 0o644,
                content: crate::ports::Content::Bytes(chosen.bytes()),
            });
            format!("exec-once = /{CLIENT_PATH} {how}\n{config}")
        }
        None => config,
    };
    Ok((config, carried))
}

/// A fifth boot: two monitors, which on QEMU are two virtio-gpu devices and
/// so two cards in the guest.
///
/// The keybind sends the focused window to the second monitor, and each
/// screen is then required to be the picture `compositor/render`'s own tests
/// bless for it: one window each, neither monitor drawing the other's.
fn test_monitors(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        MONITOR_CONFIG,
        &Wanted {
            states: &MONITOR_EXPECTED,
            others: &MONITOR_OTHERS,
            moving: None,
            pointer: None,
            awaiting: &[],
        },
        &MONITOR_BINDS,
        args,
    )?;
    if screens.len() != MONITOR_EXPECTED.len() + MONITOR_OTHERS.len() {
        return Err(Error::new(format!(
            "{arch}: {} of {} pictures were taken",
            screens.len(),
            MONITOR_EXPECTED.len() + MONITOR_OTHERS.len()
        )));
    }
    // The two monitors are two pictures: a compositor drawing the same frame
    // on both screens would match one of them and not the other, and this is
    // what says so out loud.
    if let (Some(left), Some(right)) = (screens.get(1), screens.get(2))
        && left.pixels == right.pixels
    {
        return Err(Error::new(format!(
            "{arch}: both monitors show the same picture"
        )));
    }
    monitors_were_said(arch, &said)
}

/// A tenth boot: a `windowrule` that floats a window somewhere.
///
/// Nothing is pressed: the rules are in the configuration and the
/// compositor applies them as the windows map, so what is required is the
/// picture they make.
fn test_rules(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
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

/// What the guest printed on a transcript line, without whatever the
/// watcher put in front of it.
///
/// The lines the watcher keeps are the guest's own; the timestamp and the
/// `|` are added when one is printed. Both forms are stripped here, because
/// a check that matched a whole line in one and not the other would pass or
/// fail on where the line came from rather than on what it said.
fn said_on_its_own(line: &str) -> &str {
    line.split_once("| ").map_or(line, |(_, rest)| rest).trim()
}

/// Every boot `test-compositor` makes, in the order it makes them.
///
/// A table rather than a list of calls so that `--boot <name>` can pick one:
/// each takes minutes under emulation and there are twenty of them, so a
/// change to one is otherwise an hour a try.
type Boot = fn(Arch, &Programs, &Args) -> Result<()>;
const BOOTS: [(&str, Boot); 23] = [
    ("restart", test_driver_restart),
    ("dispatchers", test_dispatchers),
    ("bar", test_bar),
    ("decorations", test_decorations),
    ("scale", test_scale),
    ("groups", test_groups),
    ("monitors", test_monitors),
    ("plugins", test_plugins),
    ("animation", test_animation),
    ("terminal", test_terminal),
    ("rules", test_rules),
    ("clipboard", test_clipboard),
    ("submap", test_submap),
    ("taskbar", test_taskbar),
    ("twin", test_twin),
    ("screenshot", test_screenshot),
    ("lock", test_lock),
    ("menu", test_menu),
    ("pointer", test_pointer),
    ("cursor", test_cursor),
    ("mode", test_mode),
    ("transform", test_transform),
    ("typing", test_typing),
];

/// Where the restart boot's script is on the guest.
const KILL_GPU_PATH: &str = "etc/killgpu";

/// The script the restart boot runs from `exec-once`, twice over: wait until
/// the compositor holds `card0`, kill the display driver, and wait for devmgr
/// to have started another. zinc's, since a gate boot carries no busybox and
/// `exec-once` runs one program with no shell.
///
/// A card is open to one program at a time, so an open that fails on a card
/// that is there says the compositor has it: the second kill waits for that,
/// or it would land while the compositor was still looking for the first
/// card and the gate would see one loss where it asked for two.
const KILL_GPU: &str = r#"gpu() {
  for p in /proc/[0-9]*; do
    read n < $p/comm 2>/dev/null
    if [[ $n == gpu ]]; then echo ${p#/proc/}; return; fi
  done
}
held() { [[ -e /dev/dri/card0 ]] && ! { : 3<> /dev/dri/card0 } 2>/dev/null }
for round in 1 2; do
  until held; do :; done
  killed=$(gpu)
  echo "killgpu: killing gpu $killed"
  kill -9 $killed
  until [[ -n $(gpu) && $(gpu) != $killed ]]; do :; done
done
"#;

/// The configuration the restart boot is given: the first boot's two
/// windows, so the screen after the second return can be held to the
/// picture that boot blesses, and the script.
const RESTART_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor --boot restart`.
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
exec-once = /bin/zinc /etc/killgpu
";

/// What the compositor says when its card goes, and when it has it back.
const CARD_WENT: &str = "the card went away";
const CARD_BACK: &str = "the card is back";

/// How long the restart boot waits for both deaths and both returns.
const RESTART_PATIENCE: Duration = Duration::from_secs(120);

/// The twenty-first boot: the display driver killed under the compositor,
/// twice (`docs/DEVMGR.md` §4).
///
/// Killing `gpu` once took the whole machine down: the card answered
/// `ENODEV`, the compositor ended on it, and it was init. Now devmgr starts
/// the driver again, the kernel publishes the card as `card0` again, and the
/// compositor waits for it rather than ending. What is required, in the
/// guest's own words: the script killed the driver twice, devmgr said it
/// started it again twice, the compositor saw its card go and had it back
/// after the second kill (see [`back_after_the_last_kill`]), and it neither
/// failed nor ended. And on the screen: after that return, every pixel of
/// the first boot's tiled picture, drawn on the third driver's card.
fn test_driver_restart(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let carried = Carried {
        zinc: crate::zinc::build(arch)?,
        ports: vec![crate::ports::File {
            path: KILL_GPU_PATH.to_owned(),
            mode: 0o755,
            content: crate::ports::Content::Bytes(KILL_GPU.as_bytes().to_vec()),
        }],
        ..Carried::none()
    };
    if carried.zinc.is_none() {
        println!("  {arch}: zinc is not built here, so nothing can kill the driver; skipped");
        return Ok(());
    }
    let (image, kernel) = build_image(arch, programs, &undithered(RESTART_CONFIG), carried, args)?;
    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("compositor.ppm");
    let (what, path) = EXPECTED[0];
    let want = expected(path)?;
    let mut said: Vec<String> = Vec::new();
    let mut screen = None;
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        let ended = |line: &String| {
            line.contains(FAILED)
                || line.contains(crate::shell::EXITED)
                || line.contains(crate::qemu::PANIC_MARKER)
        };
        let _ = watching.read_more(Instant::now() + RESTART_PATIENCE, |lines| {
            lines.iter().any(ended) || back_after_the_last_kill(lines)
        })?;
        if back_after_the_last_kill(watching.after()) {
            screen = Some(settle(&mut qmp, &dump, &want)?);
        }
        // A moment for anything the last return set off to be said.
        let _ = watching.read_more(Instant::now() + Duration::from_secs(3), |_| false)?;
        said = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    judge_restart(arch, &said)?;
    let Some(screen) = screen else {
        return Err(Error::new(format!(
            "{arch}: no picture was taken after the card came back"
        )));
    };
    let (_, wrong) = differences(&screen, &want);
    if wrong != 0 {
        return Err(Error::new(format!(
            "{arch}: after the card came back the screen was not {what} ({path}): \
             {wrong} pixels differ"
        )));
    }
    println!("  {arch}: and the screen is {what} again, every pixel");
    Ok(())
}

/// What the restart boot's script says as it kills the driver.
const KILLING: &str = "killgpu: killing gpu";

/// Whether the compositor had its card back after the script's second kill.
///
/// Not "went and came back twice": the second kill can land while the
/// compositor is still reopening the first restart's card. It holds the card
/// open then, which is all the script can see, but has not yet said so. That
/// reopen fails, the compositor looks again, and the card it has is the
/// third driver's, one loss and one return in its own log. What matters is
/// that it ends with the card, after both kills.
fn back_after_the_last_kill(lines: &[String]) -> bool {
    let kills: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(KILLING))
        .map(|(at, _)| at)
        .collect();
    let Some(&last) = kills.get(1) else {
        return false;
    };
    lines
        .get(last..)
        .unwrap_or_default()
        .iter()
        .any(|line| line.contains(CARD_BACK))
}

/// What [`test_driver_restart`] requires of what the guest said.
fn judge_restart(arch: Arch, said: &[String]) -> Result<()> {
    let count = |want: &str| said.iter().filter(|line| line.contains(want)).count();
    if let Some(line) = said.iter().find(|line| {
        line.contains(FAILED)
            || line.contains(crate::shell::EXITED)
            || line.contains(crate::qemu::PANIC_MARKER)
    }) {
        return Err(Error::new(format!(
            "{arch}: the machine did not survive its display driver being killed: {}",
            line.trim()
        )));
    }
    let wanted = [
        (KILLING, 2, "the script killed the driver twice"),
        (CARD_WENT, 1, "the compositor saw its card go"),
        (
            "was started again and published",
            2,
            "devmgr started the driver again twice",
        ),
    ];
    let mut missing = Vec::new();
    for (want, times, what) in wanted {
        let seen = count(want);
        if seen < times {
            missing.push(format!("{what}: `{want}` {seen} of {times} times"));
        }
    }
    if !back_after_the_last_kill(said) {
        missing.push(format!(
            "the compositor had its card back after the second kill: no `{CARD_BACK}` after it"
        ));
    }
    if !missing.is_empty() {
        return Err(Error::new(format!(
            "{arch}: the display driver was not restarted under the compositor:\n    - {}",
            missing.join("\n    - ")
        )));
    }
    println!(
        "  {arch}: the display driver was killed twice under the compositor, and each time \
         devmgr started it again and the compositor drew on its card again"
    );
    Ok(())
}

/// Whether `--boot` asked for this one.
fn wanted(args: &Args, name: &str) -> bool {
    args.boot
        .as_ref()
        .is_none_or(|asked| name.contains(asked.as_str()))
}

/// The first boot: the three states stage 18's exit asks for, and what
/// `hyprctl` and the event socket said while they were reached.
fn test_dispatchers(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
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

/// The second boot: a bar through `zwlr_layer_shell_v1`, and what
/// `hyprctl` says about everything that is not a window.
///
/// The bar is here because this is the boot that has one: `hyprctl layers`
/// is what a bar reads to find its own surface, and a compositor with no
/// layer surface would answer it with an empty list whatever it did wrong.
/// The binds, the devices, the pointer and the lock are asked for in the
/// same batch, because each is one line and a boot is minutes.
fn test_bar(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        BAR_CONFIG,
        &Wanted {
            states: &BAR_PICTURES,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["Layer level 2 (top)"],
        },
        &BAR_BINDS,
        args,
    )?;
    let Some(screen) = screens.first() else {
        return Err(Error::new(format!("{arch}: the bar boot took no picture")));
    };
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    for wanted in [
        // `binds`: the one this configuration has, with the letters and the
        // fields Hyprland prints.
        "\tdispatcher: exec",
        "\tkey: A",
        // `devices`: the two QEMU publishes, in their groups.
        "Keyboards:",
        "QEMU Virtio Keyboard",
        "\t\t\tmain: yes",
        // `layers`: the bar, on the level and under the namespace it asked
        // for.
        "Layer level 2 (top)",
        "namespace: pattern-bar",
        // `cursorpos`, which starts in the middle of the screen, and
        // `locked`, which without a session lock protocol is never true.
        "512, 384",
        "false",
    ] {
        if !has(wanted) {
            // The last of what the guest said, which is where the reason is:
            // a batch that stopped at the first unknown request prints
            // nothing after it.
            let tail: Vec<&str> = said
                .iter()
                .rev()
                .take(20)
                .rev()
                .map(|line| said_on_its_own(line))
                .collect();
            return Err(Error::new(format!(
                "{arch}: `hyprctl` never said `{wanted}`; the guest's last lines:\n    {}",
                tail.join("\n    ")
            )));
        }
    }
    println!(
        "  {arch}: a bar reserved its strip and the windows tiled under it, every one of {} \
         pixels, and `hyprctl` named its binds, its devices and the bar's own layer from inside \
         the guest",
        screen.width * screen.height
    );
    Ok(())
}

/// The third boot: Hyprland's two window decorations.
fn test_decorations(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    one_picture(
        arch,
        programs,
        args,
        "rounded corners, a shadow, a dimmed window and a blurred background",
        DECORATED_CONFIG,
        DECORATED_EXPECTED,
    )
}

/// The sixth boot: a monitor at `scale = 2`.
fn test_scale(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    one_picture(
        arch,
        programs,
        args,
        "a monitor at scale 2, tiling in logical pixels and drawing in the screen's own",
        SCALED_CONFIG,
        SCALED_EXPECTED,
    )
}

/// A boot with a configuration of its own, one picture and no keys.
///
/// Each is a boot rather than another picture in the first, because each
/// changes every picture and the first boot's three states are the stage's
/// exit criterion.
fn one_picture(
    arch: Arch,
    programs: &Programs,
    args: &Args,
    what: &str,
    config: &str,
    wanted: (&str, &str),
) -> Result<()> {
    let (screens, _) = boot_and_dump(
        arch,
        programs,
        config,
        &Wanted {
            states: &[wanted],
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &[],
        },
        &[],
        args,
    )?;
    let Some(screen) = screens.first() else {
        return Err(Error::new(format!("{arch}: {what}: no screendump")));
    };
    println!(
        "  {arch}: {what}, every one of {} pixels",
        screen.width * screen.height
    );
    Ok(())
}

/// A seventeenth boot: the pointer, drawn.
///
/// A compositor with a mouse and no arrow on the screen is one a person
/// cannot use. This moves the pointer with QMP and requires the screen to
/// become the picture `compositor/render` blesses for the arrow at that
/// point -- and to have been the ordinary tiled pair before it, because the
/// pointer is not drawn until it has moved.
fn test_pointer(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        POINTER_CONFIG,
        &Wanted {
            states: &POINTER_EXPECTED,
            others: &[],
            moving: None,
            pointer: Some(POINTER_AT),
            awaiting: &[],
        },
        &[],
        args,
    )?;
    let (Some(before), Some(after)) = (screens.first(), screens.get(1)) else {
        return Err(Error::new(format!(
            "{arch}: the pointer boot took {} pictures",
            screens.len()
        )));
    };
    if before.pixels == after.pixels {
        return Err(Error::new(format!(
            "{arch}: moving the pointer drew nothing"
        )));
    }
    // The compositor's own count of what the sweep cost: `hyprix: frames 412
    // slowest of the last 58 1312 us`, a second or more apart.
    let counted: Vec<u32> = said
        .iter()
        .filter_map(|line| {
            line.split_once("slowest of the last ")?
                .1
                .split(' ')
                .next()?
                .parse()
                .ok()
        })
        .collect();
    let Some(most) = counted.iter().copied().max() else {
        return Err(Error::new(format!(
            "{arch}: the compositor never said how many frames it drew"
        )));
    };
    // The sweep lasts a second and is drawn: a report that counted a
    // handful is a sweep that was not seen at all.
    if most < 20 {
        return Err(Error::new(format!(
            "{arch}: the most frames a report counted is {most}, so the sweep was not drawn"
        )));
    }
    if most > MOST_FRAMES {
        return Err(Error::new(format!(
            "{arch}: one report counted {most} frames, over {MOST_FRAMES}: the compositor is \
             drawing a frame for every report of the mouse rather than at the screen's rate"
        )));
    }
    println!(
        "  {arch}: the pointer was swept over the windows and put down, drawn at the screen's \
         rate ({most} frames the most in a report) and left where the mouse was put, in every \
         one of {} pixels",
        after.width * after.height
    );
    Ok(())
}

/// The pictures the cursor boot requires: the two windows before the pointer
/// is used, and after it has been swept over them and put down -- the same
/// picture both times, because a pointer on the card's cursor plane is not
/// drawn into the frame at all.
const CURSOR_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled, with no pointer drawn because none has moved",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the pointer swept over the windows and put down, and still not in the frame",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
];

/// The configuration the cursor boot is given: the pointer boot's two
/// windows, with the pointer left to the card's cursor plane, which is
/// Hyprland's default.
const CURSOR_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
";

/// The most frames the compositor may draw while the pointer is swept over
/// the windows and put down. None is owed: a few for whatever the clients
/// do is all that is allowed, where the same sweep drawn into the frame is
/// sixty and more.
const PLANE_FRAMES: u32 = 10;

/// Where the pointer boot's arrow is drawn, in the screen's pixels:
/// [`POINTER_AT`] on a 1024 x 768 screen.
const POINTER_PIXEL: (usize, usize) = (700, 300);

/// The most frames the compositor has said it drew, over every frame report
/// in `lines`: `hyprix: frames 412 slowest of the last 58 ...`.
fn frames_drawn<'a>(lines: impl Iterator<Item = &'a String>) -> u32 {
    lines
        .filter_map(|line| {
            line.split_once("hyprix: frames ")?
                .1
                .split(' ')
                .next()?
                .parse()
                .ok()
        })
        .max()
        .unwrap_or(0)
}

/// The twenty-second boot: the pointer on the card's cursor plane.
///
/// A pointer on a plane is the host's to show over the frame, so a
/// screendump -- which is the frame -- cannot see it. The judge is a VNC
/// viewer asking QEMU for the pointer's shape, which is how QEMU hands a
/// plane to anything that shows the screen, and how a served desktop's
/// pointer comes to have no lag at all (`docs/GPU.md` §3.9). Four things are
/// required: the frame is the two windows and nothing else, before the
/// pointer moved and after it was swept through four hundred places and put
/// down; the sweep cost the compositor no frames to speak of; the
/// compositor says the pointer is on the plane; and the shape the viewer is
/// handed is, pixel for pixel, the arrow the pointer boot's picture has drawn
/// into it, hotspot and all.
fn test_cursor(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (image, kernel) = build_image(
        arch,
        programs,
        &undithered(CURSOR_CONFIG),
        Carried::none(),
        args,
    )?;
    let port = free_port()?;
    // A VNC display is a port above 5900; the loopback's free ports are.
    let vnc = loop {
        let candidate = free_port()?;
        if candidate > 5900 {
            break candidate;
        }
    };
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    qemu_args.judge_vnc = Some(vnc);
    let dump = paths::build_dir(arch).join("compositor.ppm");
    let mut shape = None;
    let mut said = Vec::new();
    let mut before = 0;
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        if let Some(line) = watching
            .lines()
            .iter()
            .rev()
            .find(|line| line.contains(FAILED))
        {
            return Err(Error::new(format!("{arch}: {}", line.trim())));
        }
        let up = watching.read_more(Instant::now() + SETTLE, |lines| {
            lines.iter().any(|line| line.contains(MARKER))
        })?;
        if !up && !watching.lines().iter().any(|line| line.contains(MARKER)) {
            return Err(Error::new(format!(
                "{arch}: the compositor never printed `{MARKER}`"
            )));
        }
        say_the_marker(watching, arch);
        before = cursor_states(watching, &mut qmp, arch, &dump)?;
        let mut viewer =
            crate::vnc::Viewer::connect(vnc, Instant::now() + Duration::from_secs(10))?;
        shape = viewer.cursor(Instant::now() + Duration::from_secs(10))?;
        let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
        said = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    on_the_plane(arch, &said, before, shape)
}

/// [`test_cursor`]'s two pictures, each required to be the two windows and
/// nothing else, with the sweep before the second: what the compositor had
/// drawn before the sweep, by its own count.
fn cursor_states(
    watching: &mut Watching<'_>,
    qmp: &mut Qmp,
    arch: Arch,
    dump: &Path,
) -> Result<u32> {
    let mut before = 0;
    for (index, (what, path)) in CURSOR_EXPECTED.iter().enumerate() {
        if index == 1 {
            let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
            before = frames_drawn(watching.lines().iter().chain(watching.after()));
        }
        ask_for_state(qmp, arch, index, &[], Some(POINTER_AT))?;
        let want = expected(path)?;
        let screen = settle(qmp, dump, &want)?;
        let (found, count) = differences(&screen, &want);
        if count != 0 {
            let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
            return Err(with_the_transcript(
                &unexpected(arch, what, &screen, found, count),
                watching,
            ));
        }
        println!(
            "  {arch}: {what}, every one of {} pixels as the renderer draws them",
            screen.width * screen.height
        );
    }
    Ok(before)
}

/// [`test_cursor`]'s verdict on what the boot said and what the viewer was
/// handed.
fn on_the_plane(
    arch: Arch,
    said: &[String],
    before: u32,
    shape: Option<crate::vnc::Shape>,
) -> Result<()> {
    if let Some(line) = said.iter().find(|line| line.contains("FERRIX-PANIC")) {
        return Err(Error::new(format!(
            "{arch}: the kernel stopped while the compositor ran: {}",
            line.trim()
        )));
    }
    if !said
        .iter()
        .any(|line| line.contains("the pointer is on the card's cursor plane"))
    {
        return Err(Error::new(format!(
            "{arch}: the compositor never put the pointer on the card's cursor plane"
        )));
    }
    let swept = frames_drawn(said.iter()).saturating_sub(before);
    if swept > PLANE_FRAMES {
        return Err(Error::new(format!(
            "{arch}: sweeping the pointer cost {swept} frames, over {PLANE_FRAMES}: the pointer \
             is being drawn into the frame"
        )));
    }
    let Some(shape) = shape else {
        return Err(Error::new(format!(
            "{arch}: a VNC viewer was never handed the pointer's shape"
        )));
    };
    same_arrow(arch, &shape)?;
    println!(
        "  {arch}: the pointer is on the card's cursor plane: {} frames for a sweep of {}, and a \
         viewer handed the {}x{} arrow the frame would have drawn, hotspot ({}, {})",
        swept, SWEEP.0, shape.size.0, shape.size.1, shape.hot.0, shape.hot.1
    );
    Ok(())
}

/// Whether `shape` is the arrow the pointer boot's picture has drawn into it
/// at [`POINTER_PIXEL`]: every pixel the shape shows is that picture's pixel
/// there, and every pixel it does not is the picture without a pointer.
fn same_arrow(arch: Arch, shape: &crate::vnc::Shape) -> Result<()> {
    let plain = expected(POINTER_EXPECTED[0].1)?;
    let pointed = expected(POINTER_EXPECTED[1].1)?;
    const WIDTH: usize = 1024;
    let at = |x: u16, y: u16| -> Option<usize> {
        let x = (POINTER_PIXEL.0 + usize::from(x)).checked_sub(usize::from(shape.hot.0))?;
        let y = (POINTER_PIXEL.1 + usize::from(y)).checked_sub(usize::from(shape.hot.1))?;
        Some((y * WIDTH + x) * 3)
    };
    let mut shown = 0;
    for y in 0..shape.size.1 {
        for x in 0..shape.size.0 {
            let Some(index) = at(x, y) else { continue };
            let (Some(drawn), Some(under)) =
                (pointed.get(index..index + 3), plain.get(index..index + 3))
            else {
                continue;
            };
            if shape.shows(x, y) {
                shown += 1;
                let Some([blue, green, red]) = shape.pixel(x, y) else {
                    continue;
                };
                if drawn != [red, green, blue] {
                    return Err(Error::new(format!(
                        "{arch}: the viewer's pointer has {:?} at ({x}, {y}) where the frame's \
                         arrow has {drawn:?}",
                        [red, green, blue]
                    )));
                }
            } else if drawn != under {
                return Err(Error::new(format!(
                    "{arch}: the viewer's pointer shows nothing at ({x}, {y}), where the frame's \
                     arrow is drawn"
                )));
            }
        }
    }
    if shown == 0 {
        return Err(Error::new(format!(
            "{arch}: the viewer was handed a pointer with nothing in it"
        )));
    }
    Ok(())
}

/// A boot for `monitor = , WIDTHxHEIGHT`: the mode a configuration asks for
/// is the mode the screen is set to.
///
/// Under a window QEMU has just opened a virtio-gpu prefers 640x480,
/// whatever it was started with, so a desktop somebody watches is the size
/// of a postage stamp unless its configuration can say otherwise. The
/// kernel lists the standard sizes beside the preferred one, the compositor
/// takes the one its `monitor =` line names, and what is required here is
/// the whole of that: a picture 1920 by 1080, of windows tiled for a screen
/// that size, from a card that prefers 1024x768. A compositor that set the
/// preferred mode anyway shows a picture of another size, and the
/// comparison says so.
fn test_mode(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        MODE_CONFIG,
        &Wanted {
            states: &MODE_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &[],
        },
        &[],
        args,
    )?;
    let Some(screen) = screens.first() else {
        return Err(Error::new(format!("{arch}: the mode boot took no picture")));
    };
    if !said.iter().any(|line| line.contains("1920x1080")) {
        return Err(Error::new(format!(
            "{arch}: the compositor never said its screen is 1920x1080"
        )));
    }
    println!(
        "  {arch}: the screen took the mode its `monitor =` line asked for, {}x{}, from a card \
         that prefers 1024x768, every one of {} pixels",
        screen.width,
        screen.height,
        screen.width * screen.height
    );
    Ok(())
}

/// A boot for `monitor = , preferred, auto, 1, transform, N`: a monitor
/// stood on its edge, twice -- turned one way, then the other.
///
/// The card's mode stays 1024x768, and QEMU's screendump reads the card: so
/// what is required is the buffer Hyprland would scan out for the same line,
/// the windows tiled on a monitor 768 wide and 1024 tall and the picture
/// turned into the buffer, pixel for pixel as `compositor/render` blesses
/// it. Both quarter turns, because each is the other upside down and a
/// compositor that turned the wrong way would pass one of them by drawing
/// the other; and `hyprctl monitors` from inside the guest has to say
/// `transform: N` beside the connector's own, unturned, mode, which is what
/// Hyprland prints.
fn test_transform(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    for (transform, wanted) in TRANSFORM_EXPECTED {
        let said_transform = format!("transform: {transform}");
        let (screens, said) = boot_and_dump(
            arch,
            programs,
            &transform_config(transform),
            &Wanted {
                states: &[wanted],
                others: &[],
                moving: None,
                pointer: None,
                awaiting: &[said_transform.as_str()],
            },
            &[],
            args,
        )?;
        let Some(screen) = screens.first() else {
            return Err(Error::new(format!(
                "{arch}: the transform {transform} boot took no picture"
            )));
        };
        let told = |wanted: &str| said.iter().any(|line| said_on_its_own(line) == wanted);
        if !told(&said_transform) || !said.iter().any(|line| line.contains("1024x768@")) {
            return Err(Error::new(format!(
                "{arch}: `hyprctl monitors` did not say `{said_transform}` beside the 1024x768 \
                 mode"
            )));
        }
        println!(
            "  {arch}: transform {transform}: `hyprctl monitors` said `{said_transform}` and the \
             screen is the turned picture, every one of {} pixels",
            screen.width * screen.height
        );
    }
    Ok(())
}

/// A sixteenth boot: a menu, through `xdg_popup`.
///
/// Every right-click menu, dropdown and tooltip in every toolkit is an
/// `xdg_popup`, and a client that makes one and is never configured waits
/// for ever -- the menu simply does not appear. This boot is a window that
/// asks for one the moment it has drawn, and what is required is the
/// picture: the popup over the window, at the rectangle
/// `xdg_positioner`'s rules put it, which `compositor/render` blesses by
/// calling those same rules.
fn test_menu(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        MENU_CONFIG,
        &Wanted {
            states: &MENU_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["menu at"],
        },
        &[],
        args,
    )?;
    let Some(screen) = screens.first() else {
        return Err(Error::new(format!("{arch}: the menu boot took no picture")));
    };
    // And the client was told where it was put, which says the configure
    // reached it rather than the picture having come from somewhere else.
    if !said
        .iter()
        .any(|line| line.contains("pattern: one menu at 1,1 200x200"))
    {
        return Err(Error::new(format!(
            "{arch}: the client was never told where its menu is"
        )));
    }
    println!(
        "  {arch}: a window asked for a menu and the compositor placed it, drew it over the \
         window and told the client where it is, in every one of {} pixels",
        screen.width * screen.height
    );
    Ok(())
}

/// A fifteenth boot: the screen lock, through `ext-session-lock-v1`.
///
/// Three pictures: the windows, the lock over them, and the windows again.
/// The middle one is the point -- while the lock is held the compositor
/// draws its surface and *nothing else*, so the screen must be the picture
/// `compositor/render` blesses for a locked screen and not one pixel of
/// either window.
///
/// The second key is the other half. `K` closes a window, and it is pressed
/// while the screen is locked: a bind that is not written `bindl` does not
/// fire then, so both windows have to still be there when the lock lets go.
/// A lock that showed a picture and still let a keybind through would not
/// be one.
fn test_lock(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        LOCK_CONFIG,
        &Wanted {
            states: &LOCK_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["lock: locked"],
        },
        &LOCK_BINDS,
        args,
    )?;
    if screens.len() != LOCK_EXPECTED.len() {
        return Err(Error::new(format!(
            "{arch}: {} of {} pictures were taken",
            screens.len(),
            LOCK_EXPECTED.len()
        )));
    }
    // The locked screen and the tiled one must be two pictures, which a
    // compositor that ignored the lock would fail here rather than at the
    // comparison above.
    if let (Some(before), Some(locked)) = (screens.first(), screens.get(1))
        && before.pixels == locked.pixels
    {
        return Err(Error::new(format!(
            "{arch}: the locked screen is the picture the windows made"
        )));
    }
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    for wanted in [
        "hyprix: the session is locked",
        "hyprix: the lock covers 1 screen(s)",
        "hyprix: the session is unlocked",
        "lock: locked 1 screen(s) and unlocked again",
    ] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: the lock boot did not say `{wanted}`"
            )));
        }
    }
    // And the bind pressed while the screen was locked did not fire: the
    // window it would have closed is still drawn in the third picture,
    // which is the tiled pair.
    if has("lswt: closed") {
        return Err(Error::new(format!(
            "{arch}: a bind fired while the session was locked"
        )));
    }
    println!(
        "  {arch}: a program locked the screen and every one of its {} pixels was the lock's own \
         picture, a keybind pressed while it was locked did nothing, and the windows came back \
         when it let go",
        screens
            .first()
            .map_or(0, |screen| screen.width * screen.height)
    );
    Ok(())
}

/// A fourteenth boot: a screenshot, through `zwlr_screencopy_v1`.
///
/// The guest takes a picture of its own screen with `/bin/shot` -- which is
/// `grim` without the file format -- and prints its size and a digest of
/// every pixel. What is required is that the digest is the one the expected
/// image has: the screenshot the compositor wrote into a client's shared
/// memory inside the guest is, pixel for pixel, the frame
/// `compositor/render` builds on the host by calling the renderer with
/// rectangles.
///
/// That is a stronger statement than the screendump the other boots make.
/// QEMU's screendump reads the virtio-gpu's scanout; this reads what the
/// compositor handed to a program *through the Wayland protocol*, so a
/// compositor that drew the right thing and answered screencopy with
/// rubbish is caught here and nowhere else.
fn test_screenshot(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        SHOT_CONFIG,
        &Wanted {
            states: &SHOT_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["shot: "],
        },
        &SHOT_BINDS,
        args,
    )?;
    let Some(screen) = screens.first() else {
        return Err(Error::new(format!(
            "{arch}: the screenshot boot took no picture"
        )));
    };
    if screens.len() != SHOT_EXPECTED.len() {
        return Err(Error::new(format!(
            "{arch}: {} of {} pictures were taken",
            screens.len(),
            SHOT_EXPECTED.len()
        )));
    }
    // The picture the guest must have handed the program: the same expected
    // image every screendump above is compared against, and its size is the
    // screen's, since a screendump of another size would already have
    // failed.
    let want = expected(SHOT_EXPECTED[0].1)?;
    let line = format!(
        "shot: {}x{} {:016x}",
        screen.width,
        screen.height,
        fnv1a(&want)
    );
    if !said.iter().any(|said| said_on_its_own(said) == line) {
        let printed = said
            .iter()
            .map(|said| said_on_its_own(said))
            .filter(|said| said.starts_with("shot: "))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::new(format!(
            "{arch}: the guest's screenshot is not the expected image: it said `{printed}` and \
             the image is `{line}`"
        )));
    }
    println!(
        "  {arch}: a program on the guest took a screenshot through `zwlr_screencopy_v1` and \
         every one of its {} pixels is the one the renderer blesses",
        screen.width * screen.height
    );
    Ok(())
}

/// The two pictures the typing boot requires, and the key between them.
///
/// Nothing a person does closes a window here. `V` starts a program, and
/// that program types `SUPER Q` through `zwp_virtual_keyboard_v1` -- so the
/// key that fires the bind comes from a *client*, on the Wayland socket, and
/// not from a device at all.
const TYPING_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled",
        "compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "one window left, closed by a key another program typed",
        "compositor/render/tests/data/one-client-alone.xrle",
    ),
];

/// No modifier, for the reason `TASKBAR_BINDS` gives.
const TYPING_BINDS: [(&str, &[&str]); 1] =
    [("V, which starts a program that types SUPER Q", &["v"])];

/// The configuration the eighteenth boot is given.
///
/// `closewindow` takes one of Hyprland's window expressions rather than a
/// direction, so the key the other program types closes the window *named*
/// `one` -- whichever is focused. Two new things in one picture: a client
/// acting as a keyboard, and a dispatcher that picks a window out by title.
const TYPING_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two
bind = , V, exec, /bin/vkbd SUPER Q
bind = SUPER, Q, closewindow, title:^(one)$
";

/// An eighteenth boot: a program that types, through
/// `zwp_virtual_keyboard_v1`.
///
/// `wtype`, `ydotool` and every on-screen keyboard are clients that act as a
/// device: what they report has to reach the seat as a person's input does,
/// keybinds and all. That is the whole point of the protocol and the one
/// thing a test can check from outside -- so this boot presses one key that
/// starts `/bin/vkbd`, and `vkbd` types the chord that fires a bind.
///
/// The bind is `closewindow, title:^(one)$`, which names its window with one
/// of Hyprland's window expressions. So the picture proves two things at
/// once: the key crossed from a client into the seat, and the compositor
/// picked a window out by its title.
fn test_typing(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        TYPING_CONFIG,
        &Wanted {
            states: &TYPING_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["vkbd: typed"],
        },
        &TYPING_BINDS,
        args,
    )?;
    let Some(last) = screens.last() else {
        return Err(Error::new(format!(
            "{arch}: the typing boot took no picture"
        )));
    };
    for wanted in [
        "vkbd: typed SUPER Q",
        // And the window that went is the one the expression named.
        "the compositor asked the Checkerboard window called one to close",
    ] {
        if !said.iter().any(|line| line.contains(wanted)) {
            return Err(Error::new(format!(
                "{arch}: the typing boot never said `{wanted}`"
            )));
        }
    }
    println!(
        "  {arch}: a program typed SUPER Q through `zwp_virtual_keyboard_v1`, the bind fired, \
         and `closewindow title:^(one)$` closed the window it named -- every one of {} pixels \
         the renderer's own picture of the window that is left",
        last.width * last.height
    );
    Ok(())
}

/// FNV-1a, which is how a whole screen is compared through a serial port.
///
/// The same function `compositor/shot`'s `digest` is, and it has to stay the
/// same: the guest prints the digest of what it was handed and this is what
/// that is compared against. Short enough to print on one line, and simple
/// enough that two copies cannot drift without a test saying so.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A twentieth boot: one client with two windows, one of which it closes.
///
/// Every other boot in this table gives each window a client of its own, so
/// the only way one has ever gone is with its connection. This one is the
/// other way: the client stays and destroys one of its two
/// `xdg_toplevel`s, which until 2026-09-18 left the layout tiling a window
/// that was not there. The host test in
/// `compositor/hyprix/tests/two_clients.rs` makes the same claim against
/// the compositor in a process; this makes it on Ferrix, on the card.
fn test_twin(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
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

/// A thirteenth boot: a taskbar, through
/// `zwlr_foreign_toplevel_management_v1`.
///
/// `/bin/lswt` is what a bar's window list is once the drawing is taken
/// out: it binds the manager, takes the handle the compositor makes for
/// each window, and reads the title, the application id and the states.
/// Then it sends a request back through one of those handles -- `close`, on
/// a window it does not own -- which is what a middle click on a taskbar
/// entry does, and the screen says whether it arrived.
fn test_taskbar(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        TASKBAR_CONFIG,
        &Wanted {
            states: &TASKBAR_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["lswt: left"],
        },
        &TASKBAR_BINDS,
        args,
    )?;
    let Some(last) = screens.last() else {
        return Err(Error::new(format!(
            "{arch}: the taskbar boot took no picture"
        )));
    };
    // Both windows, by application id, title and state: the focused one is
    // marked and the other is not, which is the tick a taskbar draws.
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    for wanted in [
        "lswt: rocks.magical.pattern \"one\" []",
        "lswt: rocks.magical.pattern \"two\" [activated]",
        "lswt: closed \"one\"",
        // And the window that is left is the other one, which says the
        // close reached the window the bar named and not its neighbour.
        "lswt: left \"two\"",
    ] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: the taskbar never said `{wanted}`"
            )));
        }
    }
    println!(
        "  {arch}: a taskbar listed both windows with the focused one marked, and closed the \
         other from outside it, leaving every one of {} pixels the renderer's own picture of \
         one window",
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
fn test_submap(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
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

/// An eleventh boot: the clipboard, between two programs that have no
/// window.
///
/// `/bin/clip copy` offers text as `text/plain;charset=utf-8` and stays
/// alive to answer, as Wayland's clipboard requires: the data lives in the
/// program that copied it and the compositor holds only the promise.
/// `/bin/clip paste` is told what the selection holds, asks for the text on
/// a pipe it makes, and prints what comes back. Nothing but the pipe joins
/// the two processes, and the compositor is what passed it across.
fn test_clipboard(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    // The copying program prints when it leaves, which is a moment after it
    // has answered: the boot is waited for that line rather than drained for
    // a fixed time, which under emulation is a coin toss.
    let asked = format!(
        "clip: copied {} bytes to the clipboard, asked for 1 times",
        CLIPBOARD_TEXT.len()
    );
    let pasted = format!("clip: pasted {CLIPBOARD_TEXT}");
    let asked_primary = format!(
        "clip: copied {} bytes to the primary, asked for 1 times",
        PRIMARY_TEXT.len()
    );
    let pasted_primary = format!("clip: pasted {PRIMARY_TEXT}");
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        &clipboard_config(),
        &Wanted {
            states: &CLIPBOARD_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &[
                asked.as_str(),
                pasted.as_str(),
                asked_primary.as_str(),
                pasted_primary.as_str(),
            ],
        },
        &[],
        args,
    )?;
    let Some(screen) = screens.first() else {
        return Err(Error::new(format!(
            "{arch}: the clipboard boot took no picture"
        )));
    };
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    for wanted in [
        // The compositor took both selections and handed each pipe on.
        "hyprix: the selection is 1 type(s) from client",
        "hyprix: the primary selection is 1 type(s) from client",
        "pasted text/plain;charset=utf-8, on a pipe to whoever copied",
    ] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: the clipboard boot did not say `{wanted}`"
            )));
        }
    }
    // The copying program was asked for its data exactly once, which is the
    // paste and nothing else.
    if !has(&asked) {
        return Err(Error::new(format!(
            "{arch}: the copying program did not say `{asked}`"
        )));
    }
    // And what came out of one program is what went into the other, for
    // each of the two selections.
    for (wanted, text) in [(&asked_primary, PRIMARY_TEXT), (&pasted, CLIPBOARD_TEXT)] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: nothing said `{wanted}` for {text:?}"
            )));
        }
    }
    if !has(&pasted_primary) {
        return Err(Error::new(format!(
            "{arch}: nothing pasted `{PRIMARY_TEXT}` from the primary selection"
        )));
    }
    println!(
        "  {arch}: one program copied {} bytes to the clipboard and {} to the primary selection \
         and two others pasted each back, on pipes the compositor passed between them, with the \
         windows still drawn in every one of {} pixels",
        CLIPBOARD_TEXT.len(),
        PRIMARY_TEXT.len(),
        screen.width * screen.height
    );
    Ok(())
}

/// A ninth boot: a terminal, with a program running in it.
///
/// The whole path at once: the compositor starts `compositor/term`, which
/// opens `/dev/ptmx`, opens the slave, runs a program on it with the slave
/// for its session and its three descriptors, reads what it wrote back
/// through the master, draws it in a grid with its antialiased Hack, and
/// puts that in a `wl_shm` buffer the compositor composes into the frame.
/// Every pixel of that frame is compared against the one
/// `compositor/term`'s own test blesses.
fn test_terminal(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        TERMINAL_CONFIG,
        &Wanted {
            states: &TERMINAL_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &[],
        },
        &[],
        args,
    )?;
    let Some(screen) = screens.first() else {
        return Err(Error::new(format!(
            "{arch}: the terminal boot took no picture"
        )));
    };
    // The terminal said what it drew, which says the pseudoterminal carried
    // the program's output rather than the picture having come from
    // somewhere else.
    if !said.iter().any(|line| line.contains("term: ")) {
        return Err(Error::new(format!(
            "{arch}: the terminal never said anything"
        )));
    }
    println!(
        "  {arch}: a terminal ran a program on a pseudoterminal and drew its output, every one \
         of {} pixels",
        screen.width * screen.height
    );
    Ok(())
}

/// An eighth boot: a window sliding, watched frame by frame.
///
/// `docs/ROADMAP.md` stage 19's exit asks for a sequence of screendumps
/// showing a window moving along the configured curve with rounded corners
/// and blur behind a translucent client, inside the stated frame-time bound
/// under the software fallback. This is that: the decorated picture, a
/// keybind, every distinct picture until the windows have changed places,
/// and the compositor's own frame times from the same boot.
fn test_animation(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        ANIMATED_CONFIG,
        &Wanted {
            states: &ANIMATED_EXPECTED,
            others: &[],
            moving: Some(ANIMATED_MOVING),
            pointer: None,
            awaiting: &[],
        },
        &[],
        args,
    )?;
    // The first is the state it started in; the rest are the slide, the
    // first of which is usually that same state, since a screendump asked
    // for the instant the keys go in is taken before the compositor has
    // drawn anything new.
    let sliding = screens.get(ANIMATED_EXPECTED.len()..).unwrap_or(&[]);
    let (Some(first), Some(last)) = (screens.first(), sliding.last()) else {
        return Err(Error::new(format!("{arch}: no pictures at all")));
    };
    // It arrived: the loop that took them stops at the goal or at the time,
    // and a run that stopped at the time is a window that never got there.
    let want = expected(ANIMATED_MOVING.path)?;
    let (found, count) = differences(last, &want);
    if count != 0 {
        return Err(unexpected(arch, ANIMATED_MOVING.what, last, found, count));
    }
    // And it went through somewhere else on the way: a compositor that drew
    // the goal at once would have the state it started from and the state it
    // ended in and nothing between them, however many dumps were taken.
    let between = sliding
        .iter()
        .filter(|screen| screen.pixels != first.pixels && screen.pixels != last.pixels)
        .count();
    if between == 0 {
        return Err(Error::new(format!(
            "{arch}: the window jumped: {} pictures, none of them between the two states",
            sliding.len()
        )));
    }
    println!(
        "  {arch}: a window slid through {} pictures with its decorations on, {between} of them \
         places neither layout put it, ending in the one the renderer blesses",
        sliding.len()
    );
    frames_were_inside_the_bound(arch, &said)
}

/// What the compositor said about its own frames, against the bound.
fn frames_were_inside_the_bound(arch: Arch, said: &[String]) -> Result<()> {
    let mut slowest = 0u128;
    for line in said {
        // `hyprix: frames <n> slowest of the last <m> <us> us`.
        let Some(rest) = line.split("slowest of the last ").nth(1) else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let (Some(_count), Some(number)) = (words.next(), words.next()) else {
            continue;
        };
        if let Ok(micros) = number.parse::<u128>() {
            slowest = slowest.max(micros);
        }
    }
    if slowest == 0 {
        return Err(Error::new(format!(
            "{arch}: the compositor never said how long its frames took"
        )));
    }
    let bound = frame_bound(arch);
    if slowest > bound {
        return Err(Error::new(format!(
            "{arch}: the slowest frame took {slowest} us, past the {bound} us a frame under \
             emulation is allowed"
        )));
    }
    println!(
        "  {arch}: the slowest frame the guest drew took {slowest} us, under emulation; the \
         renderer's own bound is checked in release by `compositor/render`"
    );
    Ok(())
}

/// A seventh boot: a plugin, and a keybind naming a dispatcher it added.
///
/// Nothing in the compositor knows `swapthem`: the layout refuses it, and it
/// reaches the plugin because the plugin registered it. What the plugin asks
/// for in return is what the screen then shows, which is the whole of the
/// extension point.
fn test_plugins(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
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

/// What the two-monitor boot's `hyprctl` and event socket must have said:
/// two monitors by name, and the window's move between them.
fn monitors_were_said(arch: Arch, said: &[String]) -> Result<()> {
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    for wanted in ["Monitor Virtual-1 (ID 0)", "Monitor Virtual-2 (ID 1)"] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: `hyprctl monitors` did not say `{wanted}`"
            )));
        }
    }
    let added = said
        .iter()
        .filter(|line| line.contains("monitoradded>>"))
        .count();
    if added < 2 {
        return Err(Error::new(format!(
            "{arch}: the event socket announced {added} monitors, not two"
        )));
    }
    if !has("movewindow>>") {
        return Err(Error::new(format!(
            "{arch}: nothing on the event socket said the window moved"
        )));
    }
    println!("  {arch}: `hyprctl monitors` named both screens and the socket announced both");
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
fn test_groups(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
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

/// The expected image, as the `(red, green, blue)` bytes a screendump holds.
///
/// `compositor/render/src/golden.rs` writes the format and says why: a
/// run-length image of `XRGB8888` rows, with a row that repeats the one above
/// written as a single byte.
fn expected(relative: &str) -> Result<Vec<u8>> {
    let path = paths::workspace_root().join(relative);
    let bytes = std::fs::read(&path)
        .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))?;
    let word = |at: usize| -> Result<u32> {
        let slice: [u8; 4] = bytes
            .get(at..at + 4)
            .and_then(|slice| slice.try_into().ok())
            .ok_or_else(|| Error::new(format!("{} ends inside a word", path.display())))?;
        Ok(u32::from_le_bytes(slice))
    };
    if bytes.get(..16) != Some(b"ferrix-xrgb-rle\n") {
        return Err(Error::new(format!(
            "{} is not an expected image",
            path.display()
        )));
    }
    let (width, height) = (word(16)?, word(20)?);
    let mut out = Vec::with_capacity((width * height * 3) as usize);
    let mut at = 24;
    let mut previous: Vec<u8> = Vec::new();
    for _ in 0..height {
        let tag = *bytes
            .get(at)
            .ok_or_else(|| Error::new(format!("{} ends at a row", path.display())))?;
        at += 1;
        if tag == 0 {
            out.extend_from_slice(&previous);
            continue;
        }
        let mut row = Vec::with_capacity((width * 3) as usize);
        while row.len() < (width * 3) as usize {
            let count = u16::from_le_bytes([
                *bytes.get(at).unwrap_or(&0),
                *bytes.get(at + 1).unwrap_or(&0),
            ]);
            let pixel = word(at + 2)?;
            at += 6;
            if count == 0 {
                return Err(Error::new(format!(
                    "{} has a run of no pixels",
                    path.display()
                )));
            }
            for _ in 0..count {
                row.extend_from_slice(&[
                    ((pixel >> 16) & 0xFF) as u8,
                    ((pixel >> 8) & 0xFF) as u8,
                    (pixel & 0xFF) as u8,
                ]);
            }
        }
        out.extend_from_slice(&row);
        previous = row;
    }
    Ok(out)
}

/// Where a screendump and the expected image differ: the first pixel, and how
/// many there were.
fn differences(screen: &Image, want: &[u8]) -> (Option<(usize, usize)>, usize) {
    let pixels = screen.width.saturating_mul(screen.height);
    if screen.pixels.len() != want.len() {
        // A different size is every pixel different, and the first is (0, 0).
        return (Some((0, 0)), pixels);
    }
    let mut count = 0;
    let mut first = None;
    for index in 0..pixels {
        let at = index * 3;
        if screen.pixels.get(at..at + 3) != want.get(at..at + 3) {
            count += 1;
            if first.is_none() {
                first = Some((index % screen.width, index / screen.width));
            }
        }
    }
    (first, count)
}

#[cfg(test)]
mod tests {
    use super::{RUN_CONFIG, back_after_the_last_kill, with_layout};
    use crate::args::Args;

    fn said(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    const KILL: &str = "    6.67 | killgpu: killing gpu 198";
    const WENT: &str =
        "    6.68 | hyprix: Virtual-1: the card went away; waiting for it to come back";
    const BACK: &str = "    7.24 | hyprix: Virtual-1: the card is back; drawing on it again";

    /// The race the full matrix met: the second kill lands mid-reopen, so the
    /// compositor loses and regains its card once, and that is a pass.
    #[test]
    fn one_loss_and_one_return_after_both_kills_is_back() {
        assert!(back_after_the_last_kill(&said(&[KILL, WENT, KILL, BACK])));
        assert!(back_after_the_last_kill(&said(&[
            KILL, WENT, BACK, KILL, WENT, BACK
        ])));
    }

    #[test]
    fn a_return_only_before_the_second_kill_is_not_back() {
        assert!(!back_after_the_last_kill(&said(&[
            KILL, WENT, BACK, KILL, WENT
        ])));
        assert!(!back_after_the_last_kill(&said(&[KILL, WENT, BACK])));
    }

    /// The flags a watched boot takes for its keyboard, appended so that they
    /// beat whatever the configuration said: a later line overrides an
    /// earlier one, which the configuration parser has its own test for.
    #[test]
    fn the_layout_flags_are_appended_to_whatever_configuration_is_used() {
        let asked = |layout: Option<&str>, variant: Option<&str>| {
            with_layout(
                "input:kb_layout = fr\n".to_owned(),
                &Args {
                    layout: layout.map(str::to_owned),
                    variant: variant.map(str::to_owned),
                    ..Args::default()
                },
            )
        };

        // Neither flag leaves the configuration exactly as it was, which is
        // what a person's own file must get.
        assert_eq!(asked(None, None), "input:kb_layout = fr\n");

        // Both, in the order the options are read.
        let both = asked(Some("de,us"), Some("nodeadkeys,"));
        assert!(both.starts_with("input:kb_layout = fr\n"), "{both}");
        assert!(
            both.ends_with("input:kb_layout = de,us\ninput:kb_variant = nodeadkeys,\n"),
            "{both}"
        );

        // A variant on its own is a variant of whatever the file's layout is.
        let variant = asked(None, Some("dvorak"));
        assert!(
            variant.ends_with("input:kb_variant = dvorak\n"),
            "{variant}"
        );
        assert!(!variant.contains("kb_layout = de"), "{variant}");
    }

    /// A configuration that does not end in a newline still gets its own
    /// line: the flag would otherwise land on the end of the last one.
    #[test]
    fn a_configuration_with_no_final_newline_gets_one() {
        let appended = with_layout(
            "bind = SUPER, Q, killactive".to_owned(),
            &Args {
                layout: Some("de".to_owned()),
                ..Args::default()
            },
        );
        assert!(appended.contains("killactive\n"), "{appended}");
        assert!(appended.ends_with("input:kb_layout = de\n"), "{appended}");
    }

    /// The configuration a watched boot writes when nothing else was named
    /// starts a terminal, because a screen somebody is watching is one they
    /// want to type into.
    #[test]
    fn the_default_configuration_opens_a_terminal() {
        assert!(RUN_CONFIG.contains("exec-once = /bin/term /bin/zinc"));
        assert!(RUN_CONFIG.contains("bind = SUPER, RETURN, exec, /bin/term /bin/zinc"));
    }
}
