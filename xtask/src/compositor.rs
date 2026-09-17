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
use std::process::Command;
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
const CONFIG_PATH: &str = "etc/hyprland.conf";

/// The instance the control socket is under, which `hyprctl` finds by
/// looking when `HYPRLAND_INSTANCE_SIGNATURE` is not set.
const INSTANCE: &str = "ferrix";

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
const FRAME_BOUND: u128 = 5_000_000;

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
         exec-once = /bin/clip paste\n"
    )
}

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
";

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
        })
    }

    /// The ones the initramfs carries, each with the path it goes at.
    fn carried(&self) -> [(&'static str, &Path); 6] {
        [
            (CLIENT_PATH, self.client.as_path()),
            (CTL_PATH, self.ctl.as_path()),
            (PLUG_PATH, self.plug.as_path()),
            (TERM_PATH, self.term.as_path()),
            (CLIP_PATH, self.clip.as_path()),
            (LSWT_PATH, self.lswt.as_path()),
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
    let mut command = Command::new(crate::cargo::cargo());
    let _ = command
        .current_dir(paths::workspace_root().join("compositor"))
        .args(["build", "--release", "-p", package, "--target", target])
        .env("CARGO_TARGET_DIR", &target_dir);
    crate::cargo::run(command, &format!("cargo build (compositor/{binary})"))?;
    Ok(target_dir.join(target).join("release").join(binary))
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

fn boot_and_dump(
    arch: Arch,
    programs: &Programs,
    config: &str,
    wanted: &Wanted<'_>,
    binds: &[(&str, &[&str])],
    args: &Args,
) -> Result<(Vec<Image>, Vec<String>)> {
    let (image, kernel) = build_image(arch, programs, config, args)?;

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
            // Each keybind but the first is sent after the picture before it
            // has settled, so that a state is never judged before the
            // compositor has been asked to make it.
            if let Some((name, keys)) = binds.get(index.wrapping_sub(1)) {
                press(&mut qmp, keys)?;
                println!("  {arch}: pressed {name}");
            }
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
    args: &Args,
) -> Result<(PathBuf, PathBuf)> {
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
            path,
            mode: 0o755,
            bytes: read(program)?,
        });
    }
    carried.push(crate::ports::File {
        path: CONFIG_PATH,
        mode: 0o644,
        bytes: config.as_bytes().to_vec(),
    });
    let initramfs = crate::initramfs::build(None, &natives, None, &carried)?;
    // The kernel as well as the image: the watcher symbolises a panic's
    // addresses out of it.
    let image = crate::fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
    Ok((image, kernel))
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
        for (name, boot) in BOOTS {
            if wanted(args, name) {
                boot(arch, &programs, args)?;
            }
        }
    }
    Ok(())
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
/// each takes minutes under emulation and there are fourteen of them, so a
/// change to one is otherwise an hour a try.
type Boot = fn(Arch, &Programs, &Args) -> Result<()>;
const BOOTS: [(&str, Boot); 13] = [
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
];

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

/// The second boot: a bar through `zwlr_layer_shell_v1`.
fn test_bar(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    one_picture(
        arch,
        programs,
        args,
        "a bar reserved its strip and the windows tiled under it",
        BAR_CONFIG,
        BAR_EXPECTED,
    )
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
        "clip: copied {} bytes, asked for 1 times",
        CLIPBOARD_TEXT.len()
    );
    let pasted = format!("clip: pasted {CLIPBOARD_TEXT}");
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        &clipboard_config(),
        &Wanted {
            states: &CLIPBOARD_EXPECTED,
            others: &[],
            moving: None,
            awaiting: &[asked.as_str(), pasted.as_str()],
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
        // The compositor took the selection and handed the pipe on.
        "hyprix: the selection is 1 type(s) from client",
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
    // And what came out of one program is what went into the other.
    if !has(&pasted) {
        return Err(Error::new(format!(
            "{arch}: nothing pasted `{CLIPBOARD_TEXT}`"
        )));
    }
    println!(
        "  {arch}: one program copied {} bytes and another pasted them back, on a pipe the \
         compositor passed between them, with the windows still drawn in every one of {} pixels",
        CLIPBOARD_TEXT.len(),
        screen.width * screen.height
    );
    Ok(())
}

/// A ninth boot: a terminal, with a program running in it.
///
/// The whole path at once: the compositor starts `compositor/term`, which
/// opens `/dev/ptmx`, opens the slave, runs a program on it with the slave
/// for its session and its three descriptors, reads what it wrote back
/// through the master, draws it in a grid with `libs/fbtext`'s font, and
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
    if slowest > FRAME_BOUND {
        return Err(Error::new(format!(
            "{arch}: the slowest frame took {slowest} us, past the {FRAME_BOUND} us a frame under \
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
