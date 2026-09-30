//! `cargo xtask test-compositor`: the compositor itself, on Ferrix, on a
//! screen.
//!
//! `cargo xtask test-display` boots `src/user/linux/compositor/blank`, which fills the card
//! with one colour: the proof that the path from a program through
//! `/dev/dri/card0`, the kernel's display core and the ring-3 virtio-gpu
//! driver to QEMU's window works at all. This boots the compositor, which
//! goes through the same path with everything above it in place -- the
//! configuration, the layout, the renderer and its own DRM backend with two
//! dumb buffers and a page flip.
//!
//! Two `src/user/linux/compositor/pattern` clients are carried in the initramfs at
//! `/bin/pattern` and started by the compositor's own `exec-once`, so what
//! reaches the screen is two real Wayland clients tiled by the dwindle
//! layout -- the same picture `src/user/linux/compositor/hyprix/tests/two_clients.rs` makes
//! on the host, and compared against the same expected image that
//! `src/user/linux/compositor/render`'s own tests bless.
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
//! `src/user/linux/compositor/render`'s own tests by calling the renderer with rectangles
//! from `src/user/linux/compositor/layout`. The pictures compared here came from two
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

mod boot;
mod desktop;
mod drawing;
mod drawn_here;
mod idle;
mod layout;
mod machine;
mod monitors;
mod picture;
mod pointer;
mod protocols;
mod run;
pub(crate) mod steam_window;
mod user_desktop;

#[cfg(unix)]
pub(crate) use boot::desktop_image;
pub(crate) use boot::{absolute, button_event, client_image, press};
pub(crate) use drawing::test_video;
pub(crate) use run::{board_files, run_compositor};

use crate::args::Args;
use crate::display::{DEVICE_ID, Image, Qmp, free_port, parse_ppm};
use crate::paths::{self, Arch};
use crate::qemu::Watching;
use crate::{Error, Result};
use boot::{build_image, said_on_its_own, say_the_marker, undithered, with_the_transcript};
use drawing::{test_animation, test_decorations, test_gpu, test_terminal};
use drawn_here::{test_caption, test_waybar, test_waybar_volume};
use layout::{test_dispatchers, test_groups, test_plugins, test_rules, test_submap, test_twin};
use machine::{test_desktop, test_driver_restart};
use monitors::{test_edid, test_mode, test_monitors, test_scale, test_transform};
use pointer::{test_cursor, test_pointer};
use protocols::{
    test_bar, test_clipboard, test_lock, test_menu, test_screenshot, test_taskbar, test_typing,
};
use user_desktop::{test_everything_desktop, test_fuzzel, test_fuzzel_user};

/// The compositor's own background: `src/user/linux/compositor/render`'s `Style::BACKGROUND`,
/// which is Hyprland's `misc:background_color` default.
const BACKGROUND: [u8; 3] = [0x11, 0x11, 0x11];

/// What the compositor prints once it is on a screen, followed by the mode.
pub(crate) const MARKER: &str = "hyprix: card0";

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
/// by `src/user/linux/compositor/render`'s own tests.
const EXPECTED: [(&str, &str); 3] = [
    (
        "tiled",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the focus moved left",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients-focus-left.xrle",
    ),
    (
        "the windows swapped",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients-swapped.xrle",
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
const VDAGENT_PATH: &str = "bin/vdagent";
/// hypridle, and the `loginctl` that reaches it (`src/user/linux/compositor/hypridle`).
const HYPRIDLE_PATH: &str = "bin/hypridle";
const LOGINCTL_PATH: &str = "bin/loginctl";
/// `reboot`, with the word for the firmware busybox's cannot pass; it takes
/// the name, which busybox then does not link.
const REBOOT_PATH: &str = "bin/reboot";
const CONFIG_PATH: &str = "etc/hyprland.conf";

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
    /// `hypridle`, which runs commands when the seat goes idle.
    hypridle: PathBuf,
    /// `loginctl lock-session`, which reaches hypridle's `lock_cmd`.
    loginctl: PathBuf,
    /// `reboot`, which asks the firmware to come back up somewhere.
    reboot: PathBuf,
    /// `vdagent`, which joins the host's clipboard to this one.
    vdagent: PathBuf,
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
            hypridle: build(arch, "compositor-hypridle", "hypridle")?,
            loginctl: build(arch, "compositor-hypridle", "loginctl")?,
            reboot: build(arch, "compositor-reboot", "reboot")?,
            vdagent: build(arch, "compositor-vdagent", "vdagent")?,
        })
    }

    /// The ones the initramfs carries, each with the path it goes at.
    fn carried(&self) -> [(&'static str, &Path); 13] {
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
            (HYPRIDLE_PATH, self.hypridle.as_path()),
            (LOGINCTL_PATH, self.loginctl.as_path()),
            (REBOOT_PATH, self.reboot.as_path()),
            (VDAGENT_PATH, self.vdagent.as_path()),
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
    println!("  building src/user/linux/compositor/{binary} for {target}");
    let program = target_dir.join(target).join("release").join(binary);
    crate::builds::Build::cargo(
        format!("cargo build (src/user/linux/compositor/{binary}) --target {target}"),
        paths::workspace_root().join("src/user/linux/compositor"),
    )
    .args(["build", "--release", "-p", package, "--target", target])
    .env("CARGO_TARGET_DIR", &target_dir)
    .output(&program)
    .run()?;
    Ok(program)
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
    /// `pulsed`, the sound server, as a service of the desktop beside the
    /// compositor, whose clients are told where it listens
    /// (`crate::init::desktop_files`).
    pulsed: Option<Vec<u8>>,
}

impl Carried {
    /// Neither, which is what every judged boot asks for.
    fn none() -> Self {
        Self {
            busybox: None,
            zinc: None,
            ports: Vec::new(),
            pulsed: None,
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
            pulsed: None,
        })
    }
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

/// Every boot `test-compositor` makes, in the order it makes them.
///
/// A table rather than a list of calls so that `--boot <name>` can pick one:
/// each takes minutes under emulation and there are twenty of them, so a
/// change to one is otherwise an hour a try.
type Boot = fn(Arch, &Programs, &Args) -> Result<()>;
const BOOTS: [(&str, Boot); 33] = [
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
    ("edid", test_edid),
    ("desktop", test_desktop),
    ("idle", idle::test_idle),
    ("idle-user", idle::test_idle_user),
    ("caption", test_caption),
    ("waybar", test_waybar),
    ("waybar-volume", test_waybar_volume),
    ("fuzzel", test_fuzzel),
    ("fuzzel-user", test_fuzzel_user),
    ("everything-desktop", test_everything_desktop),
];

/// Whether `--boot` asked for this one.
fn wanted(args: &Args, name: &str) -> bool {
    args.boot
        .as_ref()
        .is_none_or(|asked| name.contains(asked.as_str()))
}

/// The configuration `test-foot` gives the compositor: foot, running
/// `hyprctl version` and holding its window open after it exits.
///
/// `hyprctl version` for `TERMINAL_CONFIG`'s reason: three lines, and a
/// round trip through the control socket on the way. What foot draws of them
/// came through a pseudoterminal ferrousli opened, into a grid laid out in a
/// font fontconfig found and freetype rasterised, into a `wl_shm` buffer
/// libwayland-client handed the compositor. `--log-level=info` makes foot say
/// which font it loaded, which is how the serial port shows it was the
/// image's, and `--log-colorize=never` leaves its lines plain enough to read
/// an error off.
const FOOT_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-foot`.
exec-once = /bin/foot --log-level=info --log-colorize=never --log-no-syslog --hold /bin/hyprctl version
";

/// Who uid 0 is. foot looks its user up for the shell it would start, and
/// says it could not as an error even when it was given a program instead;
/// with the user there, an error from foot is one worth failing on.
const FOOT_PASSWD: &str = "root:x:0:0:root:/:/bin/sh\n";

/// Where fontconfig keeps its cache, which it will not make itself.
const FONTCONFIG_CACHE: [&str; 3] = ["var", "var/cache", "var/cache/fontconfig"];

/// Where the port installs foot, and so where the configuration starts it.
const FOOT_PATH: &str = "bin/foot";

/// What foot says when fontconfig has found the image's font and freetype
/// has opened it: the path of the file it loaded.
const FOOT_FONT: &str = "/usr/share/fonts/dejavu/DejaVuSansMono.ttf";

/// What foot says once it has a grid, which it lays out from the font's
/// metrics: the last thing before its first frame.
const FOOT_GRID: &str = "cell width=";

/// What foot prints ahead of an error of its own, after the padding that
/// lines it up with `info` and `warn`.
const FOOT_ERROR: &str = "err: ";

/// The fewest colours a screen with foot's text on it has.
///
/// A window with nothing drawn in it is a handful: the compositor's
/// background, the border, and foot's own background -- seven, as the
/// development host's probe of a foot with no output records
/// (`src/user/linux/compositor/hyprix/probe/real-client.txt`). Text is antialiased, and
/// every glyph's edges are greys between foot's foreground and background,
/// so three short lines of it are dozens more.
const TEXT_COLOURS: usize = 24;

/// How long foot gets to connect, load its font, start `hyprctl` and draw
/// what it printed, after the compositor is on the screen: a static C
/// program of four megabytes and a font of a third of one, read from the
/// initramfs under emulation.
const FOOT_PATIENCE: Duration = Duration::from_secs(120);

/// How many different colours a screen has, or none when it is not the
/// compositor's: a screen with none of its background is the firmware's
/// console, whose antialiased text has as many colours as a terminal's.
fn colours(screen: &Image) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    let (pixels, _) = screen.pixels.as_chunks::<3>();
    for pixel in pixels {
        let _ = seen.insert(*pixel);
    }
    if seen.contains(&BACKGROUND) {
        seen.len()
    } else {
        0
    }
}

/// `test-foot`: foot, a Wayland terminal nobody here wrote, on the
/// compositor, on Ferrix.
///
/// `docs/CHROME.md` §6's first milestone. The compositor's own tests use
/// clients written against its own crates, which proves the two halves
/// agree, not that the protocol is right; `src/user/linux/compositor/hyprix/probe` runs
/// foot against the compositor on a development host, which proves the
/// protocol and nothing about Ferrix. This is both at once: foot and every
/// library it links -- libwayland-client, libxkbcommon, pixman, freetype,
/// fontconfig, fcft -- built against ferrousli by
/// `src/user/linux/ferrousli/tools/ports/foot`, started by the compositor on the guest,
/// drawing text in a font the image carries.
///
/// No picture is blessed: foot's text is foot's rendering of a font, and an
/// expected image would bless both. What is required is what foot said --
/// the image's font loaded, a grid laid out, no error of its own -- and a
/// screen with antialiased text on it, which a window with nothing drawn in
/// it is not.
pub(crate) fn test_foot(args: &Args) -> Result<()> {
    for arch in args.arches()? {
        if crate::display::target(arch).is_none() {
            println!("  {arch}: no virtio-gpu in QEMU's machine; skipped");
            continue;
        }
        let ports = crate::ports::installed_port(arch, "foot")?;
        if !ports.iter().any(|file| file.path == FOOT_PATH) {
            if arch == Arch::X86_64 {
                return Err(Error::new(format!(
                    "{arch}: foot is not built: `cargo xtask ports` builds it"
                )));
            }
            println!("  {arch}: foot is ported to x86-64 only; skipped");
            continue;
        }
        let programs = Programs::build(arch)?;
        let mut ports = ports;
        ports.push(crate::ports::File {
            path: "etc/passwd".to_owned(),
            mode: 0o644,
            content: crate::ports::Content::Bytes(FOOT_PASSWD.as_bytes().to_vec()),
        });
        for directory in FONTCONFIG_CACHE {
            ports.push(crate::ports::File {
                path: directory.to_owned(),
                mode: 0o755,
                content: crate::ports::Content::Directory,
            });
        }
        let carried = Carried {
            ports,
            ..Carried::none()
        };
        let (image, kernel) =
            build_image(arch, &programs, &undithered(FOOT_CONFIG), carried, args)?;
        let port = free_port()?;
        let mut qemu_args = args.clone();
        qemu_args.display = true;
        qemu_args.qmp_port = Some(port);
        let dump = paths::build_dir(arch).join("foot.ppm");
        let mut said: Vec<String> = Vec::new();
        let mut best: Option<Image> = None;
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
            let deadline = Instant::now() + FOOT_PATIENCE;
            let _ = watching.read_more(deadline, |lines| {
                lines
                    .iter()
                    .any(|line| line.contains(FOOT_GRID) || line.contains(FOOT_ERROR))
            })?;
            // The screen until it carries text, or the time is up: the grid
            // is laid out before `hyprctl` has printed anything, and its
            // lines reach the screen a frame or two later.
            loop {
                qmp.screendump(Some(DEVICE_ID), &dump)?;
                let bytes = std::fs::read(&dump)
                    .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?;
                let screen = parse_ppm(&bytes)?;
                let enough = colours(&screen) >= TEXT_COLOURS;
                if best
                    .as_ref()
                    .is_none_or(|kept| colours(kept) < colours(&screen))
                {
                    best = Some(screen);
                }
                if enough || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
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
        judge_foot(arch, &said, best.as_ref(), &dump)?;
    }
    Ok(())
}

/// The configuration `test-vkgears` gives the compositor: vkgears, printing
/// the Vulkan device it drew on before it starts.
///
/// `MESA_VK_WSI_DEBUG,sw` for `docs/GPU.md` §6.1's reason: until the
/// compositor takes dmabuf, a frame the host's GPU drew is copied into
/// shared memory and handed over as `wl_shm`, which is what Mesa's Wayland
/// code does for a device it is told is a software one.
const VKGEARS_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-vkgears`.
env = MESA_VK_WSI_DEBUG,sw
exec-once = /bin/vkgears -info
";

/// Where the port installs vkgears, and so where the configuration starts it.
pub(super) const VKGEARS_PATH: &str = "bin/vkgears";

/// What vkgears prints of the device it is drawing on, before its first frame.
const VKGEARS_DEVICE: &str = "deviceName    = ";

/// What Mesa's Venus driver calls every device it hands out: the host's own
/// GPU, by its own name, after this.
const VENUS_DEVICE: &str = "Virtio-GPU Venus";

/// What vkgears prints every five seconds of drawing.
const VKGEARS_FRAMES: &str = " frames in ";

/// How long vkgears gets to start, reach the host's GPU through Venus and
/// draw its first five seconds: a static program of a few megabytes read
/// from the initramfs, then a Vulkan instance, device and pipeline made over
/// the render node.
const VKGEARS_PATIENCE: Duration = Duration::from_secs(120);

/// `test-vkgears`: Vulkan's gears on Ferrix, drawn by the host's GPU.
///
/// `docs/GPU.md` §6.1's exit. vkgears is mesa-demos' own, and Mesa's Venus
/// driver is linked into it (`src/user/linux/ferrousli/tools/ports/vkgears`): it opens the
/// render node, makes a Venus context and its rings in host memory mapped
/// through the device's window, compiles nothing -- the SPIR-V goes to the
/// host's Vulkan driver -- and fences each frame on a ring, polling the
/// descriptors the node answers with. The host is Linux with a Venus-built
/// virglrenderer: QEMU's card is `--venus`'s.
///
/// No picture is blessed, and none could be taken: a GL console cannot be
/// dumped (§3.1). What is required is what vkgears said -- that its device is
/// the host's GPU through Venus, and that it drew frames -- and that the
/// kernel did not stop while it did.
pub(crate) fn test_vkgears(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    let ports = crate::ports::installed_port(arch, "vkgears")?;
    if !ports.iter().any(|file| file.path == VKGEARS_PATH) {
        return Err(Error::new(format!(
            "{arch}: vkgears is not built: `cargo xtask ports` builds it"
        )));
    }
    let programs = Programs::build(arch)?;
    let carried = Carried {
        ports,
        ..Carried::none()
    };
    let (image, kernel) = build_image(arch, &programs, &undithered(VKGEARS_CONFIG), carried, args)?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.gl = true;
    qemu_args.venus = true;
    let mut said: Vec<String> = Vec::new();
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
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
        let _ = watching.read_more(Instant::now() + VKGEARS_PATIENCE, |lines| {
            lines
                .iter()
                .any(|line| line.contains(VKGEARS_FRAMES) || line.contains("FERRIX-PANIC"))
        })?;
        said = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    judge_vkgears(arch, &said)
}

/// What [`test_vkgears`] requires of what the guest said.
fn judge_vkgears(arch: Arch, said: &[String]) -> Result<()> {
    let transcript = || {
        said.iter()
            .map(|line| said_on_its_own(line).to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let fail = |why: String| Err(Error::new(format!("{arch}: {why}\n{}", transcript())));
    if let Some(line) = said.iter().find(|line| line.contains("FERRIX-PANIC")) {
        return fail(format!(
            "the kernel stopped while vkgears ran: {}",
            line.trim()
        ));
    }
    let Some(device) = said
        .iter()
        .find_map(|line| said_on_its_own(line).split_once(VKGEARS_DEVICE))
        .map(|(_, name)| name.trim().to_owned())
    else {
        return fail("vkgears never named its Vulkan device".to_owned());
    };
    if !device.starts_with(VENUS_DEVICE) {
        return fail(format!(
            "vkgears drew on `{device}`, which is not the host's GPU through Venus"
        ));
    }
    let Some(frames) = said
        .iter()
        .map(|line| said_on_its_own(line))
        .find(|line| line.contains(VKGEARS_FRAMES))
    else {
        return fail(format!("vkgears found `{device}` and drew no frames"));
    };
    let drawn: u64 = frames
        .split_whitespace()
        .next()
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);
    if drawn == 0 {
        return fail(format!("vkgears drew no frames on `{device}`: `{frames}`"));
    }
    println!("  {arch}: vkgears drew on `{device}`: {}", frames.trim());
    Ok(())
}

/// What [`test_foot`] requires of what the guest said and showed.
fn judge_foot(arch: Arch, said: &[String], screen: Option<&Image>, dump: &Path) -> Result<()> {
    let transcript = || {
        said.iter()
            .map(|line| said_on_its_own(line).to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let fail = |why: String| Err(Error::new(format!("{arch}: {why}\n{}", transcript())));
    if let Some(line) = said.iter().find(|line| line.contains("FERRIX-PANIC")) {
        return fail(format!(
            "the kernel stopped while foot ran: {}",
            line.trim()
        ));
    }
    if let Some(line) = said
        .iter()
        .find(|line| said_on_its_own(line).trim_start().starts_with(FOOT_ERROR))
    {
        return fail(format!("foot said: {}", said_on_its_own(line)));
    }
    if !said.iter().any(|line| line.contains(FOOT_FONT)) {
        return fail(format!("foot never said it loaded {FOOT_FONT}"));
    }
    if !said.iter().any(|line| line.contains(FOOT_GRID)) {
        return fail("foot never laid out its grid".to_owned());
    }
    let Some(screen) = screen else {
        return fail("the boot took no picture".to_owned());
    };
    // The busiest screen, where it can be looked at: what is judged is a
    // count, and a person reading a failure wants the picture.
    let kept = dump.with_file_name("foot-busiest.ppm");
    let mut ppm = format!("P6\n{} {}\n255\n", screen.width, screen.height).into_bytes();
    ppm.extend_from_slice(&screen.pixels);
    std::fs::write(&kept, &ppm)
        .map_err(|error| Error::new(format!("writing {}: {error}", kept.display())))?;
    let found = colours(screen);
    if found < TEXT_COLOURS {
        return fail(format!(
            "the screen has {found} colours, fewer than the {TEXT_COLOURS} of a terminal with \
             text in it; the busiest screen is {}",
            kept.display()
        ));
    }
    println!(
        "  {arch}: foot loaded the image's DejaVu Sans Mono, laid out its grid, and drew \
         `hyprctl version` in {found} colours; the screen is {}",
        kept.display()
    );
    Ok(())
}

/// The page Chrome's window shows: `test-chrome`'s picture, a yellow a
/// screen counts. No spaces, because the compositor splits `exec-once` at
/// them.
const CHROME_WINDOW_PAGE: &str =
    "data:text/html,<body%20style=background:%23fc0><h1>Hello%20from%20Chrome%20on%20Ferrix</h1>";

/// That page's background, `#fc0`, as the screen has it.
const CHROME_YELLOW: [u8; 3] = [0xff, 0xcc, 0x00];

/// The fewest yellow pixels a screen with the page on it has: a tenth of a
/// 1024x768 screen. The window is most of the screen and the page most of
/// the window; the firmware's screen and a window Chrome has not yet drawn
/// into have none.
const CHROME_YELLOW_PIXELS: usize = 78_643;

/// How long Chrome gets from the compositor coming up to its page on the
/// screen: a 294 MB browser and three of its processes, started from a btrfs
/// volume, laying out and drawing in software.
const CHROME_WINDOW_PATIENCE: Duration = Duration::from_secs(240);

/// The configuration `test-chrome-window` gives the compositor.
///
/// Chrome's environment and command are [`crate::chrome::window_command`]'s,
/// which `run-compositor --chrome` starts too; on ferrousli, with its search
/// path.
fn chrome_window_config(ferrousli: bool) -> String {
    format!(
        "# Carried into the initramfs by `cargo xtask test-chrome-window`.\n{}{}exec-once = {}\n",
        crate::chrome::WINDOW_ENV,
        crate::chrome::window_library_path(ferrousli),
        crate::chrome::window_command(CHROME_WINDOW_PAGE)
    )
}

/// What `--interpreter` takes on `run-compositor --chrome` for the volume's
/// own glibc, which ferrousli otherwise stands in for.
const GLIBC: &str = "glibc";

/// What puts the libc [`chrome_libc`] chose under Chrome on `volume`:
/// ferrousli's loader and `libc.so.6`, or the links to the volume's glibc.
fn chrome_libc_files(arch: Arch, volume: &Path, args: &Args) -> Result<Vec<crate::ports::File>> {
    if crate::chrome::on_ferrousli(args) {
        println!("  {arch}: Chrome on ferrousli's loader and libc.so.6");
        crate::chrome::ferrousli_files(arch, volume, crate::chrome::WINDOW_PROGRAM, args)
    } else {
        println!("  {arch}: Chrome on the volume's glibc");
        Ok(crate::rustc::files(crate::chrome::LINKS))
    }
}

/// The libc Chrome runs on: ferrousli's loader and `libc.so.6` unless
/// `--interpreter glibc` asks for the volume's own, the customer's choice of
/// 2026-09-26. An `--interpreter` or `--library` of another is kept.
fn chrome_libc(args: &mut Args) {
    if args.interpreter.as_deref() == Some(GLIBC) {
        args.interpreter = None;
        args.libraries.clear();
    } else if !crate::chrome::on_ferrousli(args) {
        args.interpreter = Some(crate::shell::FERROUSLI.to_owned());
        args.libraries = vec![crate::shell::FERROUSLI.to_owned()];
    }
}

/// How many pixels of a screen are Chrome's page yellow, or none when the
/// screen is not the compositor's.
fn yellow_pixels(screen: &Image) -> usize {
    let (pixels, _) = screen.pixels.as_chunks::<3>();
    if !pixels.contains(&BACKGROUND) {
        return 0;
    }
    pixels
        .iter()
        .filter(|pixel| **pixel == CHROME_YELLOW)
        .count()
}

/// `test-chrome-window`: Google's Chrome in a window on the compositor, on
/// Ferrix.
///
/// The full browser from the volume `tools/common/fetch/fetch-chrome.sh` makes -- the
/// same version `test-chrome` runs headless -- started by the compositor's
/// `exec-once` as a Wayland client, drawing its tabs, its toolbar and a page
/// into a window the compositor tiles. What is required is the page on the
/// screen: its yellow, over a tenth of it, with the compositor's background
/// around the window. The busiest screen is kept, to be looked at.
pub(crate) fn test_chrome_window(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "test-chrome-window runs on x86-64 only: Chrome for Testing publishes linux64 alone",
        ));
    }
    let mut args = args.clone();
    let volume = crate::chrome::volume()?;
    args.data_image = Some(volume.clone());
    if !args.memory_given {
        args.memory = crate::chrome::MEMORY;
    }
    let ferrousli = crate::chrome::on_ferrousli(&args);
    let programs = Programs::build(arch)?;
    let mut ports = if ferrousli {
        println!("  {arch}: Chrome in a window on ferrousli's loader and libc.so.6");
        crate::chrome::ferrousli_files(arch, &volume, crate::chrome::WINDOW_PROGRAM, &args)?
    } else {
        crate::rustc::files(crate::chrome::LINKS)
    };
    ports.extend(crate::chrome::window_files());
    let carried = Carried {
        ports,
        ..Carried::none()
    };
    let (image, kernel) = build_image(
        arch,
        &programs,
        &undithered(&chrome_window_config(ferrousli)),
        carried,
        &args,
    )?;
    let port = free_port()?;
    let mut qemu_args = args;
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("chrome-window.ppm");
    let mut said: Vec<String> = Vec::new();
    let mut best: Option<Image> = None;
    // The emptiest screen after the page was clicked into and typed at.
    let mut after_input: Option<Image> = None;
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
        let deadline = Instant::now() + CHROME_WINDOW_PATIENCE;
        loop {
            qmp.screendump(Some(DEVICE_ID), &dump)?;
            let bytes = std::fs::read(&dump)
                .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?;
            let screen = parse_ppm(&bytes)?;
            let found = yellow_pixels(&screen);
            if best.as_ref().is_none_or(|kept| yellow_pixels(kept) < found) {
                best = Some(screen);
            }
            if found >= CHROME_YELLOW_PIXELS || Instant::now() >= deadline {
                break;
            }
            let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
        }
        // Clicked into and typed at, as a person does: the window must
        // still show its page. It went blank on the desktop when the
        // compositor knew pools by object id, and a pool Chrome made after
        // the click took the id of the one its window was drawn from.
        if best
            .as_ref()
            .is_some_and(|screen| yellow_pixels(screen) >= CHROME_YELLOW_PIXELS)
        {
            after_input = Some(click_and_type(&mut qmp, watching, &dump)?);
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
    judge_chrome_window(arch, &said, best.as_ref(), after_input.as_ref(), &dump)
}

/// The page `test-chrome-audio` opens: a tone of [`CHROME_TONE_HZ`], started
/// as the page loads, which `--autoplay-policy=no-user-gesture-required`
/// lets it do without a click.
const CHROME_TONE_PAGE: &str = "data:text/html,<body%20style=background:%23fc0><h1>tone</h1><script>c=new%20AudioContext();o=c.createOscillator();o.frequency.value=440;g=c.createGain();g.gain.value=0.2;o.connect(g).connect(c.destination);o.start();document.title=c.state</script>";

/// The tone's pitch.
const CHROME_TONE_HZ: f64 = 440.0;

/// Frames of tone the file must hold: a second's worth, at the card's rate.
const CHROME_TONE_FRAMES: usize = 48_000;

/// `test-chrome-audio`: Google's Chrome in a window on the compositor, on
/// Ferrix, playing a page's `AudioContext` through `/dev/snd`.
///
/// The window of `test-chrome-window`, on a page that plays 440 Hz as it
/// loads, with a virtio-snd card whose far end is QEMU's `wav` backend
/// (`docs/AUDIO.md` §4). Chrome's audio service opens alsa-lib's `default`,
/// which is `plug` over the card, and writes through the ALSA ioctls to the
/// kernel's audio core; what the device consumes is in the file. What is
/// required is a second of it that is not silence, with the tone's pitch:
/// its zero crossings, counted over what was played, give the frequency.
///
/// # Errors
///
/// When the volume is missing, the boot fails, or the file holds no second
/// of a 440 Hz tone.
pub(crate) fn test_chrome_audio(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "test-chrome-audio runs on x86-64 only: Chrome for Testing publishes linux64 alone",
        ));
    }
    let mut args = args.clone();
    let volume = crate::chrome::pulse_volume()?;
    args.data_image = Some(volume.clone());
    if !args.memory_given {
        args.memory = crate::chrome::MEMORY;
    }
    // On the libc `run-compositor --chrome` gives Chrome: a client of pulsed
    // loads libpulse on it, which needs what glibc has and ferrousli may
    // not (`backtrace_symbols`, missed on 2026-09-27 by a gate on glibc).
    chrome_libc(&mut args);
    let ferrousli = crate::chrome::on_ferrousli(&args);
    let programs = Programs::build(arch)?;
    let mut ports = chrome_libc_files(arch, &volume, &args)?;
    ports.extend(crate::chrome::window_files());
    let pulsed = crate::audio::build_media(arch, "media-pulsed", "pulsed")?;
    let carried = Carried {
        ports,
        pulsed: Some(
            std::fs::read(&pulsed)
                .map_err(|error| Error::new(format!("{}: {error}", pulsed.display())))?,
        ),
        ..Carried::none()
    };
    let config = format!(
        "# Carried into the initramfs by `cargo xtask test-chrome-audio`.\n{}{}exec-once = {}\n",
        crate::chrome::WINDOW_ENV,
        crate::chrome::window_library_path(ferrousli),
        crate::chrome::window_command(CHROME_TONE_PAGE)
    );
    let (image, kernel) = build_image(arch, &programs, &undithered(&config), carried, &args)?;
    let wav = paths::build_dir(arch).join("chrome-audio.wav");
    if wav.exists() {
        std::fs::remove_file(&wav)
            .map_err(|error| Error::new(format!("{}: {error}", wav.display())))?;
    }
    let mut qemu_args = args;
    qemu_args.display = true;
    qemu_args.audio = Some(format!("wav:{}", wav.display()));
    let mut heard = None;
    let mut said: Vec<String> = Vec::new();
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let deadline = Instant::now() + CHROME_WINDOW_PATIENCE;
        loop {
            if let Some(tone) = std::fs::read(&wav).ok().and_then(|bytes| tone_in(&bytes)) {
                heard = Some(tone);
                if tone.0 >= CHROME_TONE_FRAMES {
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
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
    // Through the sound server, not around it: Chrome takes Pulse once
    // libpulse loads and the server answers, and falls back to ALSA
    // without a word otherwise (docs/AUDIO.md, U2d).
    let Some(stream) = said.iter().find(|line| line.contains(PULSED_STREAM)) else {
        return Err(Error::new(format!(
            "{arch}: pulsed never said `{PULSED_STREAM}`: Chrome's sound did not go through \
             the sound server"
        )));
    };
    println!("  {arch}: {}", stream.trim());
    match heard {
        Some((frames, hz)) if frames >= CHROME_TONE_FRAMES => {
            if (hz - CHROME_TONE_HZ).abs() > CHROME_TONE_HZ * 0.02 {
                return Err(Error::new(format!(
                    "{arch}: Chrome played {frames} frames, but at {hz:.1} Hz, not \
                     {CHROME_TONE_HZ} Hz; {}",
                    wav.display()
                )));
            }
            println!(
                "  {arch}: Chrome played {:.2} s of a {hz:.1} Hz tone through pulsed and \
                 /dev/snd",
                frames as f64 / 48_000.0
            );
            Ok(())
        }
        Some((frames, _)) => Err(Error::new(format!(
            "{arch}: Chrome played only {frames} frames that were not silence; {}",
            wav.display()
        ))),
        None => Err(Error::new(format!(
            "{arch}: nothing Chrome played reached the card's file {}",
            wav.display()
        ))),
    }
}

/// What `pulsed` says of each stream a client makes.
const PULSED_STREAM: &str = "pulsed: a stream: ";

/// The frames of a WAV file's S16 stereo data that are not silence, and the
/// pitch of their left channel by its zero crossings, or `None` when there
/// is no data yet.
fn tone_in(wav: &[u8]) -> Option<(usize, f64)> {
    let data = wav.windows(4).position(|window| window == b"data")?;
    let (whole, _) = wav.get(data + 8..)?.as_chunks::<4>();
    let samples: Vec<i16> = whole
        .iter()
        .map(|&[a, b, _, _]| i16::from_le_bytes([a, b]))
        .collect();
    let start = samples
        .iter()
        .position(|sample| sample.unsigned_abs() > 64)?;
    let end = samples
        .iter()
        .rposition(|sample| sample.unsigned_abs() > 64)?
        + 1;
    let heard = samples.get(start..end)?;
    let crossings = heard
        .windows(2)
        .filter(|pair| matches!(pair, [a, b] if (*a < 0) != (*b < 0)))
        .count();
    let seconds = heard.len() as f64 / 48_000.0;
    Some((heard.len(), crossings as f64 / 2.0 / seconds.max(1e-9)))
}

/// Click into Chrome's page and type three letters, as a person does, and
/// answer the emptiest of the screens taken after the click, after the
/// typing and five seconds later.
fn click_and_type(qmp: &mut Qmp, watching: &mut Watching<'_>, dump: &Path) -> Result<Image> {
    let click = |down: bool| {
        format!("{{\"type\":\"btn\",\"data\":{{\"down\":{down},\"button\":\"left\"}}}}")
    };
    let mut emptiest: Option<Image> = None;
    let mut look = |qmp: &mut Qmp| -> Result<()> {
        qmp.screendump(Some(DEVICE_ID), dump)?;
        let bytes = std::fs::read(dump)
            .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?;
        let screen = parse_ppm(&bytes)?;
        if emptiest
            .as_ref()
            .is_none_or(|kept| yellow_pixels(&screen) < yellow_pixels(kept))
        {
            emptiest = Some(screen);
        }
        Ok(())
    };
    qmp.input_send_event(&[absolute("x", 16384), absolute("y", 20000)])?;
    std::thread::sleep(Duration::from_millis(500));
    qmp.input_send_event(&[click(true)])?;
    std::thread::sleep(Duration::from_millis(100));
    qmp.input_send_event(&[click(false)])?;
    std::thread::sleep(Duration::from_millis(500));
    look(qmp)?;
    for name in ["a", "b", "c"] {
        press(qmp, &[name])?;
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_secs(2));
    look(qmp)?;
    let _ = watching.read_more(Instant::now() + Duration::from_secs(5), |_| false)?;
    look(qmp)?;
    emptiest.ok_or_else(|| Error::new("no screen was taken after the click"))
}

/// The page `bench-chrome` opens: a box turning for ever, which Chrome
/// draws sixty times a second if it can, and a thousand lines to scroll
/// through and point at. No spaces, for [`CHROME_WINDOW_PAGE`]'s reason.
const BENCH_PAGE: &str = "data:text/html,<style>@keyframes%20t{to{transform:rotate(360deg)}}\
%23t{width:120px;height:120px;background:%23c30;animation:t%202s%20linear%20infinite}\
p:hover{background:%23fff}</style><body%20style=background:%23fc0;font-family:sans-serif>\
<div%20id=t></div><script>for(let%20i=0;i<1000;i++)document.body.insertAdjacentHTML(\
'beforeend','<p>Line%20'+i+'%20of%20the%20benchmark,%20long%20enough%20to%20wrap%20a%20little\
%20and%20be%20pointed%20at.</p>')</script>";

/// What the guest runs beside Chrome for `bench-chrome`: it waits for the
/// browser to be up and settled, then says what every process has used and
/// how much memory is taken at the start and after each phase the host
/// drives. Phases are ten seconds on both sides of the wire.
const BENCH_SCRIPT: &str = r#"PATH=/bin
i=0
while [ $i -lt 240 ]; do
  n=0
  for c in /proc/[0-9]*/comm; do
    read -r x < $c 2>/dev/null && [ "$x" = chrome ] && n=$((n+1))
  done
  [ $n -ge 4 ] && break
  sleep 1
  i=$((i+1))
done
sleep 15
snap() {
  for s in /proc/[0-9]*/stat; do
    read -r x < $s 2>/dev/null && echo "bench: $1 proc $x"
  done
  read -r x < /proc/stat
  echo "bench: $1 $x"
  while read -r k v u; do
    case $k in MemTotal:|MemAvailable:|Shmem:) echo "bench: $1 mem $k $v";; esac
  done < /proc/meminfo
}
echo "bench: start"
snap start
for p in idle scroll hover; do
  sleep 10
  snap $p
done
echo "bench: end"
"#;

/// The phases `bench-chrome` drives, in order, after the one it starts at.
const BENCH_PHASES: [&str; 3] = ["idle", "scroll", "hover"];

/// `cargo xtask bench-chrome`: Chrome in a window on the compositor, driven
/// for thirty seconds -- left alone with its animation, scrolled, pointed
/// at -- and what that cost: each phase's processor time by who spent it,
/// the compositor's frames and their time, and the memory taken at the end.
///
/// A number to hold a change to, not a gate: it fails only when the boot
/// never got as far as measuring.
///
/// # Errors
///
/// When the image cannot be built, QEMU cannot be run, or the guest never
/// says what it measured.
pub(crate) fn bench_chrome(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "bench-chrome runs on x86-64 only, as Chrome does",
        ));
    }
    let mut args = args.clone();
    args.data_image = Some(crate::chrome::volume()?);
    if !args.memory_given {
        args.memory = crate::chrome::MEMORY;
    }
    let busybox = gates_busybox(arch).ok_or_else(|| {
        Error::new("bench-chrome needs ~/.local/share/ferrix/busybox/x86_64/bin/busybox.static")
    })?;
    let programs = Programs::build(arch)?;
    let mut ports = crate::rustc::files(crate::chrome::LINKS);
    ports.extend(crate::chrome::window_files());
    ports.push(crate::ports::File {
        path: "etc/bench.sh".to_owned(),
        mode: 0o644,
        content: crate::ports::Content::Bytes(BENCH_SCRIPT.as_bytes().to_vec()),
    });
    let carried = Carried {
        busybox: Some(PathBuf::from(busybox)),
        ports,
        ..Carried::none()
    };
    let config = format!(
        "# Carried into the initramfs by `cargo xtask bench-chrome`.\n{}exec-once = {}\nexec-once = /bin/busybox sh /etc/bench.sh\n",
        crate::chrome::WINDOW_ENV,
        crate::chrome::window_command(BENCH_PAGE)
    );
    let (image, kernel) = build_image(arch, &programs, &undithered(&config), carried, &args)?;
    let port = free_port()?;
    let mut qemu_args = args;
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let mut said: Vec<String> = Vec::new();
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        let started = watching.read_more(Instant::now() + CHROME_WINDOW_PATIENCE, |lines| {
            lines.iter().any(|line| line.contains("bench: start"))
        })?;
        if !started {
            return Err(with_the_transcript(
                &Error::new(format!("{arch}: the guest never started measuring")),
                watching,
            ));
        }
        drive_bench(&mut qmp, watching)?;
        let _ = watching.read_more(Instant::now() + Duration::from_secs(30), |lines| {
            lines.iter().any(|line| line.contains("bench: end"))
        })?;
        said = watching.after().to_vec();
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    let report = bench_report(&said)?;
    println!("{report}");
    Ok(())
}

/// `test-xwindow`'s boot: yserver's volume at `/data`, or with
/// `--everything` the volume `run-compositor --everything` attaches, with
/// yserver merged into it. Not that desktop's 3D card: QMP's `screendump`
/// has no surface to read from `egl-headless`.
fn xwindow_args(args: &Args) -> Result<Args> {
    let mut args = args.clone();
    args.gl = false;
    args.data_image = Some(if args.everything {
        crate::everything::volume()?
    } else {
        crate::yserver::volume()?
    });
    if !args.memory_given {
        args.memory = crate::yserver::MEMORY;
    }
    Ok(args)
}

/// `cargo xtask test-xwindow`: yserver as a client of the compositor, from
/// the volume `tools/common/fetch/fetch-yserver.sh` makes, and `xdpyinfo` and
/// `xev` against it (docs/YSERVER.md, Y2 to Y4): the root window must be
/// the compositor's screen, xev's window one of the compositor's, by its
/// title and class and on the screen, and the pointer, a click, the wheel
/// and keys put in through QEMU must reach xev as X events; later cases
/// check windows, menus and the clipboard both ways (Y5, Y6). yserver is
/// started as `run-compositor --everything` starts it (Y7), and with
/// `--everything` the gate attaches that desktop's merged volume.
///
/// # Errors
///
/// When the volume is missing, the image cannot be built, QEMU cannot be
/// run, the root window is not the screen's size, xev's window is not the
/// compositor's, or xev did not report the input.
pub(crate) fn test_xwindow(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "test-xwindow runs on x86-64 only, as yserver's volume does",
        ));
    }
    let args = xwindow_args(args)?;
    let busybox = gates_busybox(arch).ok_or_else(|| {
        Error::new("test-xwindow needs ~/.local/share/ferrix/busybox/x86_64/bin/busybox.static")
    })?;
    let programs = Programs::build(arch)?;
    let mut ports = crate::yserver::desktop_files(&[]);
    ports.push(crate::ports::File {
        path: crate::yserver::XWINDOW_PATH.to_owned(),
        mode: 0o644,
        content: crate::ports::Content::Bytes(crate::yserver::XWINDOW_SCRIPT.as_bytes().to_vec()),
    });
    let carried = Carried {
        busybox: Some(PathBuf::from(busybox)),
        ports,
        ..Carried::none()
    };
    // yserver is started as the `--everything` desktop starts it; its input
    // module says each cursor it gives the compositor at debug.
    let config = format!(
        "# Carried into the initramfs by `cargo xtask test-xwindow`.\n{}\
         env = RUST_LOG,info,yserver::wayland::input=debug\n{}exec-once = /bin/busybox sh /{}\n",
        crate::chrome::WINDOW_ENV,
        crate::yserver::desktop_config(),
        crate::yserver::XWINDOW_PATH
    );
    let (image, kernel) = build_image(arch, &programs, &undithered(&config), carried, &args)?;
    let port = free_port()?;
    let mut qemu_args = args;
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("xwindow.ppm");
    let mut said: Vec<String> = Vec::new();
    let mut screen: Option<Image> = None;
    let mut menu: (Option<Image>, Option<Image>) = (None, None);
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        let waiting = |marker: &'static str| {
            move |lines: &[String]| {
                lines
                    .iter()
                    .any(|line| line.contains(marker) || line.contains(crate::yserver::XWINDOW_END))
            }
        };
        let _ = watching.read_more(
            Instant::now() + CHROME_WINDOW_PATIENCE,
            waiting(crate::yserver::XWINDOW_INPUT),
        )?;
        // xev is running: the screen until its window is on it, or the time
        // is up; then xev's input, while the script waits for it.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut found = None;
        while found.is_none() {
            qmp.screendump(Some(DEVICE_ID), &dump)?;
            let bytes = std::fs::read(&dump)
                .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?;
            let shown = parse_ppm(&bytes)?;
            found = crate::yserver::find_xev(&shown).map(|at| (at, (shown.width, shown.height)));
            screen = Some(shown);
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if let Some((at, size)) = found {
            crate::yserver::drive_xev(&mut qmp, watching, at, size)?;
        }
        menu = crate::yserver::watch_menu(&mut qmp, watching, &dump)?;
        let ended = watching.read_more(
            Instant::now() + CHROME_WINDOW_PATIENCE,
            waiting(crate::yserver::XWINDOW_END),
        )?;
        said = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        if ended {
            Ok(())
        } else {
            Err(with_the_transcript(
                &Error::new(format!("{arch}: the xwindow script never ended")),
                watching,
            ))
        }
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    crate::yserver::judge_xwindow(arch, &said)?;
    crate::yserver::judge_xev(arch, &said, screen.as_ref(), &dump)?;
    crate::yserver::judge_xev_input(arch, &said)?;
    crate::yserver::judge_windows(arch, &said, screen.as_ref())?;
    crate::yserver::judge_menu(arch, &said, (menu.0.as_ref(), menu.1.as_ref()))?;
    crate::yserver::judge_clipboard(arch, &said)
}

/// The input of [`BENCH_PHASES`], ten seconds each: nothing, the wheel
/// turned down and back up, and the pointer swept across the page.
fn drive_bench(qmp: &mut Qmp, watching: &mut Watching<'_>) -> Result<()> {
    let wheel = |button: &str, down: bool| {
        format!("{{\"type\":\"btn\",\"data\":{{\"down\":{down},\"button\":\"{button}\"}}}}")
    };
    let phase = Duration::from_secs(10);
    // Left alone: only the animation draws.
    let _ = watching.read_more(Instant::now() + phase, |_| false)?;
    // Scrolled: a notch every 50 ms, down for five seconds and back up.
    qmp.input_send_event(&[absolute("x", 16384), absolute("y", 16384)])?;
    let began = Instant::now();
    let mut turned = 0u32;
    while began.elapsed() < phase {
        let button = if turned % 200 < 100 {
            "wheel-down"
        } else {
            "wheel-up"
        };
        qmp.input_send_event(&[wheel(button, true)])?;
        qmp.input_send_event(&[wheel(button, false)])?;
        turned += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    // Pointed at: across the page and back, a move every 16 ms.
    let began = Instant::now();
    let mut step = 0i32;
    while began.elapsed() < phase {
        let across = (step % 100 - 50).abs();
        let x = 4000 + across * 500;
        let y = 6000 + (step % 37) * 600;
        qmp.input_send_event(&[absolute("x", x), absolute("y", y)])?;
        step += 1;
        std::thread::sleep(Duration::from_millis(16));
    }
    Ok(())
}

/// One process as `/proc/<pid>/stat` said it in a `bench:` line: its
/// name, processor time in clock ticks and resident pages.
fn bench_process(stat: &str) -> Option<(u32, String, u64, u64)> {
    let pid = stat.split_whitespace().next()?.parse().ok()?;
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_owned();
    let rest: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    // After the name: state is field 3, utime 14, stime 15 and rss 24.
    let field = |number: usize| rest.get(number - 3)?.parse::<u64>().ok();
    Some((pid, name, field(14)? + field(15)?, field(24)?))
}

/// What one snapshot of the guest held.
#[derive(Default)]
struct BenchSnap {
    /// Each process's name and ticks, by pid.
    ticks: std::collections::BTreeMap<u32, (String, u64)>,
    /// Resident pages by process name, and how many processes had it.
    resident: std::collections::BTreeMap<String, (u64, u32)>,
    /// `/proc/stat`'s busy and idle ticks, summed over the processors.
    busy: u64,
    idle: u64,
    /// `/proc/meminfo`, in KiB, by key.
    memory: std::collections::BTreeMap<String, u64>,
    /// The reports the compositor had made before it, the frames they
    /// counted, and the microseconds those frames took.
    frames: (u64, u64, u64),
}

impl BenchSnap {
    /// Take in one `bench:` line's worth, after its tag.
    fn take(&mut self, what: &str) {
        if let Some(stat) = what.strip_prefix("proc ") {
            if let Some((pid, name, ticks, pages)) = bench_process(stat) {
                let entry = self.resident.entry(name.clone()).or_default();
                entry.0 += pages;
                entry.1 += 1;
                let _ = self.ticks.insert(pid, (name, ticks));
            }
        } else if let Some(memory) = what.strip_prefix("mem ") {
            let mut words = memory.split_whitespace();
            if let (Some(key), Some(value)) = (words.next(), words.next()) {
                let _ = self.memory.insert(
                    key.trim_end_matches(':').to_owned(),
                    value.parse().unwrap_or(0),
                );
            }
        } else if let Some(cpu) = what.strip_prefix("cpu ") {
            let ticks: Vec<u64> = cpu
                .split_whitespace()
                .filter_map(|word| word.parse().ok())
                .collect();
            self.busy = ticks.iter().take(3).sum();
            self.idle = ticks.get(3).copied().unwrap_or(0);
        }
    }
}

/// The guest's snapshots, by tag in the order taken, from its `bench:` lines
/// and the compositor's frame reports between them.
fn bench_snapshots(said: &[String]) -> Vec<(String, BenchSnap)> {
    let mut snaps: Vec<(String, BenchSnap)> = Vec::new();
    let (mut reports, mut counted, mut spent) = (0u64, 0u64, 0u64);
    for line in said {
        let line = said_on_its_own(line);
        if let Some(rest) = line.strip_prefix("hyprix: frames ") {
            let words: Vec<&str> = rest.split_whitespace().collect();
            // "N slowest of the last M X us, all of them Y us (...".
            if let (Some(m), Some(y)) = (words.get(5), words.get(11)) {
                reports += 1;
                counted += m.parse::<u64>().unwrap_or(0);
                spent += y.parse::<u64>().unwrap_or(0);
            }
            continue;
        }
        let Some((tag, what)) = line
            .strip_prefix("bench: ")
            .and_then(|rest| rest.split_once(' '))
        else {
            continue;
        };
        if snaps.last().is_none_or(|(last, _)| last != tag) {
            let snap = BenchSnap {
                frames: (reports, counted, spent),
                ..BenchSnap::default()
            };
            snaps.push((tag.to_owned(), snap));
        }
        if let Some((_, snap)) = snaps.last_mut() {
            snap.take(what);
        }
    }
    snaps
}

/// One phase's row: how busy the machine was, the compositor's reports in
/// it, the frames they counted and their time, and the processor time each
/// process name spent.
///
/// The frames per second are the frames per report, not the frames over
/// ten seconds. The compositor reports at the first frame a second or more
/// after its last report, so a phase holds whole reports only: ten of them,
/// or nine when the phase's ten seconds and a little began just after one.
/// Counted over ten seconds, the nine read as sixty frames lost, a second
/// of nothing drawn that nobody saw. A report that covers a real stall
/// counts fewer frames for its second, so the rate still shows it.
fn bench_row(tag: &str, before: &BenchSnap, after: &BenchSnap) -> String {
    let mut by: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    for (pid, (name, ticks)) in &after.ticks {
        let was = before.ticks.get(pid).map_or(0, |(_, ticks)| *ticks);
        *by.entry(name.as_str()).or_default() += ticks.saturating_sub(was);
    }
    let mut by: Vec<(&str, u64)> = by.into_iter().filter(|&(_, ticks)| ticks >= 10).collect();
    by.sort_by_key(|&(_, ticks)| std::cmp::Reverse(ticks));
    let busy = after.busy.saturating_sub(before.busy);
    let idle = after.idle.saturating_sub(before.idle);
    let busy_share = if busy + idle == 0 {
        0.0
    } else {
        100.0 * busy as f64 / (busy + idle) as f64
    };
    let reports = after.frames.0.saturating_sub(before.frames.0);
    let drawn = after.frames.1.saturating_sub(before.frames.1);
    let took = after.frames.2.saturating_sub(before.frames.2);
    let rate = if reports == 0 {
        0.0
    } else {
        drawn as f64 / reports as f64
    };
    let per_frame = if drawn == 0 {
        0.0
    } else {
        took as f64 / drawn as f64 / 1000.0
    };
    // A tick is a hundredth of a second and a phase ten seconds, so a
    // name's ticks in a phase over ten are its percent of one processor.
    let spent: Vec<String> = by
        .iter()
        .map(|(name, ticks)| format!("{name} {}%", ticks / 10))
        .collect();
    format!(
        "bench-chrome: {tag:<7} {busy_share:>5.1} {reports:>7} {drawn:>6} {rate:>5.1} {per_frame:>9.2}  {}\n",
        spent.join(", ")
    )
}

/// `bench-chrome`'s table, from the guest's `bench:` lines and the
/// compositor's frame reports between them.
fn bench_report(said: &[String]) -> Result<String> {
    use std::fmt::Write as _;
    let snaps = bench_snapshots(said);
    if snaps.len() < BENCH_PHASES.len() + 1 {
        return Err(Error::new(format!(
            "the guest said {} of the {} snapshots bench-chrome takes",
            snaps.len(),
            BENCH_PHASES.len() + 1
        )));
    }
    let mut out = String::from(
        "bench-chrome: phase   busy% reports frames   fps  ms/frame  processor time by process name\n",
    );
    for pair in snaps.windows(2) {
        if let [(_, before), (tag, after)] = pair {
            out.push_str(&bench_row(tag, before, after));
        }
    }
    if let Some((_, last)) = snaps.last() {
        let total = last.memory.get("MemTotal").copied().unwrap_or(0);
        let available = last.memory.get("MemAvailable").copied().unwrap_or(0);
        let _ = writeln!(
            out,
            "bench-chrome: memory used {} MiB of {} MiB, shmem {} MiB",
            total.saturating_sub(available) / 1024,
            total / 1024,
            last.memory.get("Shmem").copied().unwrap_or(0) / 1024
        );
        for (name, (pages, count)) in &last.resident {
            if *pages * 4 >= 8 * 1024 {
                let _ = writeln!(
                    out,
                    "bench-chrome: resident {name} {} MiB in {count} processes",
                    pages * 4 / 1024
                );
            }
        }
    }
    Ok(out)
}

/// Where `bench-chrome-video`'s page and video are in the guest.
const VIDEO_DIRECTORY: &str = "usr/share/ferrix/bench";

/// The page `bench-chrome-video` opens: the video, playing with its sound
/// and looping, and once a second a console line with where it is and the
/// frames it has shown and dropped, which Chrome's `--enable-logging=stderr`
/// puts on the serial line.
const VIDEO_PAGE: &str = r"<!doctype html>
<body style='margin:0;background:#000'>
<video id=v src=video.webm autoplay loop style='width:100%'></video>
<script>
const v = document.getElementById('v');
setInterval(() => {
  const q = v.getVideoPlaybackQuality();
  console.log('bench-video: ' + v.currentTime.toFixed(3) + ' ' + q.totalVideoFrames + ' ' +
    q.droppedVideoFrames + ' ' + v.readyState + ' ' + (v.paused ? 'paused' : 'playing'));
}, 1000);
</script>
";

/// The pitch of the video's sound: one tone, so that anything else in what
/// the card played is a fault.
const VIDEO_TONE_HZ: f64 = 440.0;

/// What the guest runs beside Chrome for `bench-chrome-video`: the snapshots
/// of [`BENCH_SCRIPT`], after each of three ten-second phases of playing.
const VIDEO_BENCH_SCRIPT: &str = r#"PATH=/bin
i=0
while [ $i -lt 240 ]; do
  n=0
  for c in /proc/[0-9]*/comm; do
    read -r x < $c 2>/dev/null && [ "$x" = chrome ] && n=$((n+1))
  done
  [ $n -ge 4 ] && break
  sleep 1
  i=$((i+1))
done
sleep 15
snap() {
  for s in /proc/[0-9]*/stat; do
    read -r x < $s 2>/dev/null && echo "bench: $1 proc $x"
  done
  read -r x < /proc/stat
  echo "bench: $1 $x"
  while read -r k v u; do
    case $k in MemTotal:|MemAvailable:|Shmem:) echo "bench: $1 mem $k $v";; esac
  done < /proc/meminfo
}
echo "bench: start"
snap start
for p in play1 play2 play3; do
  sleep 10
  snap $p
done
echo "bench: end"
"#;

/// The video `bench-chrome-video` plays: `FERRIX_BENCH_VIDEO` when it names
/// one -- a clip fetched from a video site, say -- or else one made here with
/// the host's `ffmpeg`, once, and kept: forty seconds of a moving test
/// picture at 1280x720 and 30 frames a second in VP9, as a video site sends
/// to a window of that size, with a 440 Hz tone in Opus, stereo at 48 kHz.
fn bench_video() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FERRIX_BENCH_VIDEO") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| Error::new("HOME is not set"))?;
    let directory = PathBuf::from(home).join(".local/share/ferrix/bench-video");
    let video = directory.join("tone-720p30-vp9-opus.webm");
    if video.is_file() {
        return Ok(video);
    }
    std::fs::create_dir_all(&directory)
        .map_err(|error| Error::new(format!("{}: {error}", directory.display())))?;
    let partial = directory.join("partial.webm");
    let status = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.5*sin(2*PI*440*t):s=48000:c=stereo",
        ])
        .args([
            "-t",
            "40",
            "-c:v",
            "libvpx-vp9",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
        ])
        .args([
            "-b:v", "1500k", "-g", "60", "-c:a", "libopus", "-b:a", "128k",
        ])
        .arg(&partial)
        .status()
        .map_err(|error| Error::new(format!("running ffmpeg: {error}")))?;
    if !status.success() {
        return Err(Error::new(format!(
            "ffmpeg could not make {}",
            video.display()
        )));
    }
    std::fs::rename(&partial, &video)
        .map_err(|error| Error::new(format!("{}: {error}", video.display())))?;
    Ok(video)
}

/// What `bench-chrome-video`'s image carries beside the compositor: the
/// volume's links, or ferrousli's in glibc's place, the fonts, the guest's
/// script, and the page with its video.
fn bench_video_files(
    arch: Arch,
    volume: &Path,
    ferrousli: bool,
    args: &Args,
    video_bytes: Vec<u8>,
) -> Result<Vec<crate::ports::File>> {
    let mut ports = if ferrousli {
        crate::chrome::ferrousli_files(arch, volume, crate::chrome::WINDOW_PROGRAM, args)?
    } else {
        crate::rustc::files(crate::chrome::LINKS)
    };
    ports.extend(crate::chrome::window_files());
    for (name, bytes) in [
        ("etc/bench.sh", VIDEO_BENCH_SCRIPT.as_bytes().to_vec()),
        (
            &format!("{VIDEO_DIRECTORY}/video.html"),
            VIDEO_PAGE.as_bytes().to_vec(),
        ),
        (&format!("{VIDEO_DIRECTORY}/video.webm"), video_bytes),
    ] {
        ports.push(crate::ports::File {
            path: name.to_owned(),
            mode: 0o644,
            content: crate::ports::Content::Bytes(bytes),
        });
    }
    Ok(ports)
}

/// `cargo xtask bench-chrome-video`: Chrome in a window on the compositor
/// playing a video with its sound through the virtio-snd card, as a person
/// watching a video site does, for thirty seconds, and what that cost and
/// how well it went: each phase's processor time by who spent it, the
/// compositor's frames, the frames the video showed and dropped, the
/// underruns the kernel counted, and the card's sound as QEMU's `wav`
/// backend took it -- which the device consumes at the host's pace, so a
/// second in which it was given less than a second of sound shows as a
/// short second, and a gap Chrome filled with silence shows as silence in
/// the tone.
///
/// A number to hold a change to, not a gate: it fails only when the boot
/// never got as far as measuring.
///
/// # Errors
///
/// When the video cannot be made, the image cannot be built, QEMU cannot be
/// run, or the guest never says what it measured.
pub(crate) fn bench_chrome_video(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "bench-chrome-video runs on x86-64 only, as Chrome does",
        ));
    }
    let video = bench_video()?;
    let video_bytes = std::fs::read(&video)
        .map_err(|error| Error::new(format!("{}: {error}", video.display())))?;
    let mut args = args.clone();
    let volume = crate::chrome::volume()?;
    args.data_image = Some(volume.clone());
    if !args.memory_given {
        args.memory = crate::chrome::MEMORY;
    }
    let busybox = gates_busybox(arch).ok_or_else(|| {
        Error::new(
            "bench-chrome-video needs ~/.local/share/ferrix/busybox/x86_64/bin/busybox.static",
        )
    })?;
    let programs = Programs::build(arch)?;
    // The C library is named, never a default another command's may change:
    // glibc, the volume's own, unless `--interpreter` or `--library` asks for
    // ferrousli, as `test-chrome-window` takes them. Two runs compared are
    // then the same browser on the same library.
    let ferrousli = crate::chrome::on_ferrousli(&args);
    println!(
        "  {arch}: Chrome on {}",
        if ferrousli {
            "ferrousli's loader and libc.so.6"
        } else {
            "the volume's glibc"
        }
    );
    let ports = bench_video_files(arch, &volume, ferrousli, &args, video_bytes)?;
    let carried = Carried {
        busybox: Some(PathBuf::from(busybox)),
        ports,
        ..Carried::none()
    };
    let page = format!("file:///{VIDEO_DIRECTORY}/video.html");
    let config = format!(
        "# Carried into the initramfs by `cargo xtask bench-chrome-video`.\n{}{}exec-once = {}\nexec-once = /bin/busybox sh /etc/bench.sh\n",
        crate::chrome::WINDOW_ENV,
        crate::chrome::window_library_path(ferrousli),
        crate::chrome::window_command(&page)
    );
    let (image, kernel) = build_image(arch, &programs, &undithered(&config), carried, &args)?;
    let wav = paths::build_dir(arch).join("bench-chrome-video.wav");
    if wav.exists() {
        std::fs::remove_file(&wav)
            .map_err(|error| Error::new(format!("{}: {error}", wav.display())))?;
    }
    let mut qemu_args = args;
    qemu_args.display = true;
    // Through QEMU's mixing engine at the card's own rate, as a desktop's
    // sound server is, not `wav:PATH`'s engine-less file: with the engine
    // off, the `wav` backend runs at its own default of 44100 Hz whatever
    // the stream's rate, so the device would take 48 kHz frames 8% slow.
    if qemu_args.audio.is_none() {
        qemu_args.audio = Some(format!("wav,path={}", wav.display()));
    }
    let mut said: Vec<String> = Vec::new();
    // The card's file's length each half second the phases ran, host time.
    let mut sizes: Vec<(Instant, u64)> = Vec::new();
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let started = watching.read_more(Instant::now() + CHROME_WINDOW_PATIENCE, |lines| {
            lines.iter().any(|line| line.contains("bench: start"))
        })?;
        if !started {
            return Err(with_the_transcript(
                &Error::new(format!("{arch}: the guest never started measuring")),
                watching,
            ));
        }
        let ended = |lines: &[String]| lines.iter().any(|line| line.contains("bench: end"));
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let length = std::fs::metadata(&wav).map_or(0, |metadata| metadata.len());
            sizes.push((Instant::now(), length));
            if watching.read_more(Instant::now() + Duration::from_millis(500), ended)? {
                break;
            }
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
    let mut report = bench_video_report(&said)?;
    report.push_str(&bench_video_sound(&wav, &sizes));
    println!("{report}");
    Ok(())
}

/// `bench-chrome-video`'s table: [`bench_row`] for each phase, then the
/// video's frames shown and dropped in each second the page reported, and
/// the kernel's underrun lines.
fn bench_video_report(said: &[String]) -> Result<String> {
    use std::fmt::Write as _;
    let snaps = bench_snapshots(said);
    if snaps.len() < 4 {
        return Err(Error::new(format!(
            "the guest said {} of the 4 snapshots bench-chrome-video takes",
            snaps.len()
        )));
    }
    let mut out = String::from(
        "bench-chrome: phase   busy% reports frames   fps  ms/frame  processor time by process name\n",
    );
    for pair in snaps.windows(2) {
        if let [(_, before), (tag, after)] = pair {
            out.push_str(&bench_row(tag, before, after));
        }
    }
    // "bench-video: <time> <shown> <dropped> <readyState> <playing>".
    let mut last: Option<(f64, u64, u64)> = None;
    let (mut seconds, mut slow, mut dropped_seconds) = (0u32, 0u32, 0u32);
    let mut worst: Option<f64> = None;
    let mut totals = (0u64, 0u64);
    for line in said {
        let Some(rest) = line.split_once("bench-video: ").map(|(_, rest)| rest) else {
            continue;
        };
        let words: Vec<&str> = rest.trim_end_matches('"').split_whitespace().collect();
        let (Some(time), Some(shown), Some(dropped)) = (
            words.first().and_then(|word| word.parse::<f64>().ok()),
            words.get(1).and_then(|word| word.parse::<u64>().ok()),
            words.get(2).and_then(|word| word.parse::<u64>().ok()),
        ) else {
            continue;
        };
        if let Some((was_time, was_shown, was_dropped)) = last {
            // A loop back to the start counts from zero again.
            let advanced = if time >= was_time {
                time - was_time
            } else {
                time
            };
            seconds += 1;
            worst = Some(worst.map_or(advanced, |least| least.min(advanced)));
            if advanced < 0.9 {
                slow += 1;
            }
            if dropped > was_dropped {
                dropped_seconds += 1;
            }
            totals.0 += shown.saturating_sub(was_shown);
            totals.1 += dropped.saturating_sub(was_dropped);
        }
        last = Some((time, shown, dropped));
    }
    let _ = writeln!(
        out,
        "bench-video: {seconds} seconds reported, {slow} advanced under 0.9 s (least {:.2} s), \
         {} frames shown, {} dropped, in {dropped_seconds} seconds",
        worst.unwrap_or(0.0),
        totals.0,
        totals.1
    );
    let underruns: Vec<&str> = said
        .iter()
        .map(|line| said_on_its_own(line))
        .filter(|line| line.contains("underrun"))
        .collect();
    let _ = writeln!(out, "bench-video: {} underrun lines", underruns.len());
    for line in underruns.iter().rev().take(3).rev() {
        let _ = writeln!(out, "bench-video:   {line}");
    }
    Ok(out)
}

/// What the card played: the frames per host second over the phases, the
/// seconds that were short of 48000 by more than a period, and the silences
/// inside the tone -- runs of 2 ms or more where the tone's samples were
/// near nothing, which is what Chrome writes when its renderer is late.
fn bench_video_sound(wav: &Path, sizes: &[(Instant, u64)]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let Ok(bytes) = std::fs::read(wav) else {
        let _ = writeln!(out, "bench-video: no sound file at {}", wav.display());
        return out;
    };
    // Frames the device took in each whole host second, from the lengths
    // taken each half second: the header is fixed, so differences are data.
    let mut rates: Vec<u64> = Vec::new();
    let mut from = sizes.first().copied();
    for &(when, length) in sizes {
        let Some((was, was_length)) = from else {
            break;
        };
        if when.duration_since(was) >= Duration::from_secs(1) {
            let frames = length.saturating_sub(was_length) / 4;
            let per_second = frames as f64 / when.duration_since(was).as_secs_f64();
            rates.push(per_second as u64);
            from = Some((when, length));
        }
    }
    // The first and last seconds may hold the start and the stop.
    let inner = rates.get(1..rates.len().saturating_sub(1)).unwrap_or(&[]);
    let short = inner.iter().filter(|&&rate| rate < 48_000 - 960).count();
    let least = inner.iter().min().copied().unwrap_or(0);
    let _ = writeln!(
        out,
        "bench-video: the card took {:?} frames a second; {short} of {} inner seconds short by \
         more than a period (least {least})",
        rates,
        inner.len()
    );
    let Some(data) = bytes.windows(4).position(|window| window == b"data") else {
        return out;
    };
    let (whole, _) = bytes.get(data + 8..).unwrap_or(&[]).as_chunks::<4>();
    let left: Vec<i16> = whole
        .iter()
        .map(|&[a, b, _, _]| i16::from_le_bytes([a, b]))
        .collect();
    let loud = |sample: &i16| sample.unsigned_abs() > 256;
    let (Some(start), Some(end)) = (left.iter().position(loud), left.iter().rposition(loud)) else {
        let _ = writeln!(out, "bench-video: the card played nothing but silence");
        return out;
    };
    let tone = left.get(start..=end).unwrap_or(&[]);
    let mut gaps: Vec<(usize, usize)> = Vec::new();
    let mut quiet_from = None;
    for (index, sample) in tone.iter().enumerate() {
        match (loud(sample), quiet_from) {
            (false, None) => quiet_from = Some(index),
            (true, Some(began)) => {
                if index - began >= 96 {
                    gaps.push((began, index - began));
                }
                quiet_from = None;
            }
            _ => {}
        }
    }
    // A jump inside the tone: a sample far from where a sine of this pitch
    // continuing the last two would be, which a lost or repeated stretch
    // makes and a whole tone never does.
    let turn = 2.0 * (2.0 * std::f64::consts::PI * VIDEO_TONE_HZ / 48_000.0).cos();
    let jumps = tone
        .array_windows::<3>()
        .filter(|&&[a, b, c]| {
            let [a, b, c] = [f64::from(a), f64::from(b), f64::from(c)];
            (c - (turn * b - a)).abs() > 2000.0
        })
        .count();
    let silent: usize = gaps.iter().map(|&(_, frames)| frames).sum();
    let _ = writeln!(
        out,
        "bench-video: {:.2} s of tone, {} silences of 2 ms or more ({:.0} ms in all), {jumps} jumps",
        tone.len() as f64 / 48_000.0,
        gaps.len(),
        silent as f64 / 48.0
    );
    for &(at, frames) in gaps.iter().take(10) {
        let _ = writeln!(
            out,
            "bench-video:   silence at {:.3} s for {:.1} ms",
            at as f64 / 48_000.0,
            frames as f64 / 48.0
        );
    }
    let _ = writeln!(out, "bench-video: the sound is {}", wav.display());
    out
}

/// What [`test_chrome_window`] requires of what the guest said and showed.
fn judge_chrome_window(
    arch: Arch,
    said: &[String],
    screen: Option<&Image>,
    after_input: Option<&Image>,
    dump: &Path,
) -> Result<()> {
    let transcript = || {
        said.iter()
            .map(|line| said_on_its_own(line).to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let fail = |why: String| Err(Error::new(format!("{arch}: {why}\n{}", transcript())));
    if let Some(line) = said.iter().find(|line| line.contains("FERRIX-PANIC")) {
        return fail(format!(
            "the kernel stopped while Chrome ran: {}",
            line.trim()
        ));
    }
    let Some(screen) = screen else {
        return fail("the boot took no picture".to_owned());
    };
    let kept = dump.with_file_name("chrome-window-busiest.ppm");
    let mut ppm = format!("P6\n{} {}\n255\n", screen.width, screen.height).into_bytes();
    ppm.extend_from_slice(&screen.pixels);
    std::fs::write(&kept, &ppm)
        .map_err(|error| Error::new(format!("writing {}: {error}", kept.display())))?;
    let found = yellow_pixels(screen);
    if found < CHROME_YELLOW_PIXELS {
        return fail(format!(
            "the screen has {found} pixels of the page's yellow, fewer than the \
             {CHROME_YELLOW_PIXELS} of Chrome's window with it drawn; the busiest screen is {}",
            kept.display()
        ));
    }
    let Some(after) = after_input else {
        return fail("the page was never clicked into and typed at".to_owned());
    };
    let after_found = yellow_pixels(after);
    if after_found < CHROME_YELLOW_PIXELS {
        let blank = dump.with_file_name("chrome-window-after-input.ppm");
        let mut ppm = format!("P6\n{} {}\n255\n", after.width, after.height).into_bytes();
        ppm.extend_from_slice(&after.pixels);
        std::fs::write(&blank, &ppm)
            .map_err(|error| Error::new(format!("writing {}: {error}", blank.display())))?;
        return fail(format!(
            "clicked into and typed at, the window went blank: {after_found} pixels of the \
             page's yellow at the emptiest, from {found}; the screen is {}",
            blank.display()
        ));
    }
    println!(
        "  {arch}: Chrome drew its window and the page on the compositor, {found} pixels of \
         the page's yellow, and still {after_found} after it was clicked into and typed at; \
         the screen is {}",
        kept.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    /// `bench-chrome`'s table from the guest's snapshots: each phase's
    /// processor time by name, from the ticks each process added, and the
    /// frames the compositor reported between two snapshots.
    #[test]
    fn the_chrome_bench_reads_its_snapshots() {
        let stat = |pid: u32, name: &str, ticks: u64| {
            format!("{pid} ({name}) S 1 1 1 0 -1 0 0 0 0 0 {ticks} 0 0 0 20 0 1 0 0 4096 256")
        };
        let mut said = Vec::new();
        for (tag, chrome, gpu, busy) in [("start", 100, 10, 1000), ("idle", 150, 30, 1100)] {
            said.push(format!(
                "  1.00 | bench: {tag} proc {}",
                stat(7, "chrome", chrome)
            ));
            said.push(format!(
                "  1.00 | bench: {tag} proc {}",
                stat(8, "gpu", gpu)
            ));
            said.push(format!(
                "  1.00 | bench: {tag} cpu  {busy} 0 0 {busy} 0 0 0"
            ));
            said.push(format!("  1.00 | bench: {tag} mem MemTotal: 4096000 kB"));
            said.push(format!(
                "  1.00 | bench: {tag} mem MemAvailable: 3072000 kB"
            ));
            if tag == "start" {
                said.push(
                    "  1.00 | hyprix: frames 9 slowest of the last 60 9000 us, all of them 300000 us (x)"
                        .to_owned(),
                );
            }
        }
        for tag in ["scroll", "hover"] {
            said.push(format!("  1.00 | bench: {tag} cpu  1100 0 0 1100 0 0 0"));
            said.push(format!("  1.00 | bench: {tag} mem MemTotal: 4096000 kB"));
            said.push(format!(
                "  1.00 | bench: {tag} mem MemAvailable: 3072000 kB"
            ));
        }
        let report = super::bench_report(&said).unwrap();
        assert!(
            report.contains(
                "bench-chrome: idle     50.0       1     60  60.0      5.00  chrome 5%, gpu 2%"
            ),
            "{report}"
        );
        assert!(
            report.contains("memory used 1000 MiB of 4000 MiB"),
            "{report}"
        );
    }
}
