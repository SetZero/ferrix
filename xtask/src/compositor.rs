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
const SETTLE: Duration = Duration::from_secs(10);

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

/// Where the clients, the control program and the configuration go in the
/// initramfs, which is what the compositor's `exec-once` and the binds name.
const CLIENT_PATH: &str = "bin/pattern";
const CTL_PATH: &str = "bin/hyprctl";
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
}

impl Wanted<'_> {
    /// How many virtio-gpu devices the boot needs: one a screen.
    fn screens(&self) -> u32 {
        u32::try_from(self.others.len())
            .unwrap_or(0)
            .saturating_add(1)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "a boot is a program, its clients, a configuration and what to require of it"
)]
fn boot_and_dump(
    arch: Arch,
    program: &Path,
    client: &Path,
    ctl: &Path,
    config: &str,
    wanted: &Wanted<'_>,
    binds: &[(&str, &[&str])],
    args: &Args,
) -> Result<(Vec<Image>, Vec<String>)> {
    let loader = crate::cargo::build_loader(arch, args.release)?;
    // One argument a line: a script has no quoting, and `Options::unshell`
    // says so. `--instance` is what puts the control socket where `hyprctl`
    // looks for it.
    let script = format!("--config\n/{CONFIG_PATH}\n--instance\n{INSTANCE}");
    let kernel = crate::cargo::build_kernel_with_init(arch, args.release, program, &script)?;
    let natives = crate::native::build(arch, args.release)?;
    let read = |path: &Path| -> Result<Vec<u8>> {
        std::fs::read(path)
            .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
    };
    let carried = [
        crate::ports::File {
            path: CLIENT_PATH,
            mode: 0o755,
            bytes: read(client)?,
        },
        crate::ports::File {
            path: CTL_PATH,
            mode: 0o755,
            bytes: read(ctl)?,
        },
        crate::ports::File {
            path: CONFIG_PATH,
            mode: 0o644,
            bytes: config.as_bytes().to_vec(),
        },
    ];
    let initramfs = crate::initramfs::build(None, &natives, None, &carried)?;
    let image = crate::fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;

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
                return Err(unexpected(arch, what, &screen, found, count));
            }
            println!(
                "  {arch}: {what}, every one of {} pixels as the renderer draws them",
                screen.width * screen.height
            );
            taken.push(screen);
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
        let program = build(arch, "hyprix", "hyprix")?;
        let client = build(arch, "compositor-pattern", "pattern")?;
        let ctl = build(arch, "compositor-ctl", "hyprctl")?;
        let (screens, said) = boot_and_dump(
            arch,
            &program,
            &client,
            &ctl,
            CONFIG,
            &Wanted {
                states: &EXPECTED,
                others: &[],
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

        // Two more boots, each with a configuration of its own: a bar
        // through `zwlr_layer_shell_v1`, and Hyprland's two window
        // decorations. Each is a boot rather than another picture in the
        // first, because both change every picture and the three states
        // above are the stage's exit criterion.
        for (what, config, wanted) in [
            (
                "a bar reserved its strip and the windows tiled under it",
                BAR_CONFIG,
                BAR_EXPECTED,
            ),
            (
                "rounded corners, a shadow, a dimmed window and a blurred background",
                DECORATED_CONFIG,
                DECORATED_EXPECTED,
            ),
            (
                "a monitor at scale 2, tiling in logical pixels and drawing in the screen's own",
                SCALED_CONFIG,
                SCALED_EXPECTED,
            ),
        ] {
            let (screens, _) = boot_and_dump(
                arch,
                &program,
                &client,
                &ctl,
                config,
                &Wanted {
                    states: &[wanted],
                    others: &[],
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
        }

        test_groups(arch, &program, &client, &ctl, args)?;
        test_monitors(arch, &program, &client, &ctl, args)?;
    }
    Ok(())
}

/// A fifth boot: two monitors, which on QEMU are two virtio-gpu devices and
/// so two cards in the guest.
///
/// The keybind sends the focused window to the second monitor, and each
/// screen is then required to be the picture `compositor/render`'s own tests
/// bless for it: one window each, neither monitor drawing the other's.
fn test_monitors(arch: Arch, program: &Path, client: &Path, ctl: &Path, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        program,
        client,
        ctl,
        MONITOR_CONFIG,
        &Wanted {
            states: &MONITOR_EXPECTED,
            others: &MONITOR_OTHERS,
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
fn test_groups(arch: Arch, program: &Path, client: &Path, ctl: &Path, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        program,
        client,
        ctl,
        GROUP_CONFIG,
        &Wanted {
            states: &GROUPED_EXPECTED,
            others: &[],
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
