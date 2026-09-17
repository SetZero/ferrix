//! `cargo xtask test-seat`: type into a window on Ferrix, from QEMU's far
//! end.
//!
//! `test-input` proves the path from QEMU's `input-send-event` to a program
//! reading `/dev/input/eventN`. `test-compositor` proves the path from a
//! Wayland client's buffer to the pixels on the card. This is the join:
//! QEMU's key goes into a `virtio-keyboard-pci`, out of the kernel's evdev
//! node, into the compositor's seat, across the Wayland socket as
//! `wl_keyboard.key`, and the client says which key it was and draws
//! something else, which the screen then shows.
//!
//! Four things are required, in order, because each is a different failure:
//!
//! 1. **The client has a keyboard and a keymap.** `wl_seat.capabilities` said
//!    the seat has one, and `wl_keyboard.keymap` handed over a real one --
//!    format 1, `XKB_V1`, with a length. `no_keymap` here would be a client
//!    that can never turn a keycode into a letter.
//! 2. **Focus reached it.** `wl_keyboard.enter`, which is the compositor
//!    saying this window is the one being typed into.
//! 3. **The key arrived, and the screen changed.** The line the client
//!    printed names the evdev code QMP sent, and the screendump taken after
//!    it differs from the one before: the client redrew because of the key,
//!    so the whole path is proven by a picture and not only by a log.
//! 4. **A keybind fired.** `bind = SUPER, Q, killactive` in the carried
//!    configuration, pressed through QMP, and the window closes -- which
//!    means the compositor matched the modifier, ate the key rather than
//!    sending it on, and ran a Hyprland dispatcher.
//!
//! The pointer is checked on the way: an absolute position and a click, and
//! the client reports `enter` and the button.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::display::{DEVICE_ID, Image, Qmp, free_port, mismatches, parse_ppm};
use crate::paths::{self, Arch};
use crate::qemu::Watching;
use crate::{Error, Result};

/// What the compositor prints once it is on a screen.
const MARKER: &str = "hyprix: card0";

/// What it prints instead when it could not start.
const FAILED: &str = "hyprix: failed";

/// Either, so the boot stops at whichever comes.
const EITHER: &str = "hyprix: ";

/// The compositor's own background, `compositor/render`'s `Style::BACKGROUND`.
const BACKGROUND: [u8; 3] = [0x11, 0x11, 0x11];

/// Where the client and the configuration go in the initramfs.
const CLIENT_PATH: &str = "bin/pattern";
const CONFIG_PATH: &str = "etc/hyprland.conf";

/// How wide and tall the cursor the compositor draws is.
///
/// `compositor/render`'s `cursor` module: a 24x24 arrow whose tip is the
/// pointer, drawn down and to the right of it. A screen with the pointer on
/// it is the background plus this, and a check that wants the background
/// alone has to say where the cursor is allowed to be.
const CURSOR: usize = 24;

/// The configuration the compositor is given: one keybind, and a client.
///
/// `killactive` is the bind because its effect is visible from outside --
/// the window goes -- and because it is the one every Hyprland
/// configuration has.
const CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-seat`.
bind = SUPER, Q, killactive
";

/// The evdev code of `a`, and of the left `Super`, from
/// `linux/input-event-codes.h`. What the client prints is the code the
/// compositor sent, which is evdev's.
const KEY_A: u32 = 30;
const BTN_LEFT: u32 = 272;

/// How long to wait for each thing, which crosses a virtqueue, a device node,
/// a socket and a page flip in an emulated machine.
const PATIENCE: Duration = Duration::from_secs(25);

/// How long to let a frame settle: a client has to draw and commit, the
/// compositor has to compose, and the flip is queued after that.
const SETTLE: Duration = Duration::from_secs(4);

/// Build one of the compositor's programs for `arch`, and say where it is.
fn build(arch: Arch, package: &str, binary: &str) -> Result<PathBuf> {
    let target = crate::display::target(arch).ok_or_else(|| {
        Error::new(format!(
            "{arch} has no virtio-gpu in QEMU; the seat test runs on x86_64 and aarch64"
        ))
    })?;
    let target_dir = paths::target_dir().join("compositor").join("seat");
    println!("  building compositor/{binary} for {target}");
    let mut command = std::process::Command::new(crate::cargo::cargo());
    let _ = command
        .current_dir(paths::workspace_root().join("compositor"))
        .args(["build", "--release", "-p", package, "--target", target])
        .env("CARGO_TARGET_DIR", &target_dir);
    crate::cargo::run(command, &format!("cargo build (compositor/{binary})"))?;
    Ok(target_dir.join(target).join("release").join(binary))
}

/// What one boot of the compositor saw.
struct Seen {
    /// Every line the guest printed.
    lines: Vec<String>,
    /// The screen before the key, after it, and after the keybind.
    screens: Vec<Image>,
}

/// Wait for `wanted` among the lines the guest has printed, or the ones it
/// prints next.
///
/// The lines before the marker are searched too: the boot stops at the first
/// `hyprix:` line, which is the seat's, so the lines this test waits for
/// mostly come afterwards -- but not all of them do, and a test that missed
/// one that had already arrived would wait for a line that will never come
/// again.
fn wait_for(watching: &mut Watching<'_>, wanted: &str, arch: Arch) -> Result<()> {
    if watching.lines().iter().any(|line| line.contains(wanted)) {
        return Ok(());
    }
    let found = watching.read_more(Instant::now() + PATIENCE, |lines| {
        lines.iter().any(|line| line.contains(wanted))
    })?;
    if found {
        return Ok(());
    }
    Err(Error::new(format!(
        "{arch}: `{wanted}` was never printed, within {}s of it being asked for",
        PATIENCE.as_secs()
    )))
}

/// A screendump, parsed.
fn screen(qmp: &mut Qmp, dump: &Path) -> Result<Image> {
    qmp.screendump(Some(DEVICE_ID), dump)?;
    let bytes = std::fs::read(dump)
        .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?;
    parse_ppm(&bytes)
}

/// Boot the compositor with a client and a keybind, and type into it.
fn boot_and_type(arch: Arch, program: &Path, client: &Path, args: &Args) -> Result<Seen> {
    let loader = crate::cargo::build_loader(arch, args.release)?;
    // One argument a line: `Options::unshell` says why.
    let script = format!("--config\n/{CONFIG_PATH}\n--exec\n/{CLIENT_PATH} checkerboard one");
    let kernel = crate::cargo::build_kernel_with_init(arch, args.release, program, &script)?;
    let natives = crate::native::build(arch, args.release)?;
    let bytes = std::fs::read(client)
        .map_err(|error| Error::new(format!("reading {}: {error}", client.display())))?;
    let carried = [
        crate::ports::File {
            path: CLIENT_PATH.to_owned(),
            mode: 0o755,
            content: crate::ports::Content::Bytes(bytes),
        },
        crate::ports::File {
            path: CONFIG_PATH.to_owned(),
            mode: 0o644,
            content: crate::ports::Content::Bytes(CONFIG.as_bytes().to_vec()),
        },
    ];
    let initramfs = crate::initramfs::build(None, &natives, None, &carried)?;
    let image = crate::fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;

    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("seat.ppm");

    let mut seen = None;
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        if let Some(line) = watching
            .lines()
            .iter()
            .rev()
            .find(|line| line.contains(FAILED))
        {
            return Err(Error::new(format!("{arch}: {}", line.trim())));
        }
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;

        // The boot stops at the first `hyprix:` line, which is the seat
        // saying what it opened; the screen is up a little later.
        wait_for(watching, MARKER, arch)?;

        // 1 and 2: the client has a keymap, and focus reached it.
        wait_for(watching, "pattern: keymap format 1", arch)?;
        wait_for(watching, "pattern: keyboard enter", arch)?;
        // The first frame, once the client has drawn into it.
        std::thread::sleep(SETTLE);
        let before = screen(&mut qmp, &dump)?;

        // The pointer, on the way past: somewhere inside the only window,
        // then a click.
        qmp.input_send_event(&[absolute("x", 0x4000), absolute("y", 0x4000)])?;
        wait_for(watching, "pattern: pointer enter", arch)?;
        qmp.input_send_event(&[button("left", true)])?;
        wait_for(
            watching,
            &format!("pattern: button {BTN_LEFT} state 1"),
            arch,
        )?;
        qmp.input_send_event(&[button("left", false)])?;

        // 3: the key, and the screen it changed.
        qmp.input_send_event(&[key("a", true)])?;
        wait_for(watching, &format!("pattern: key {KEY_A} state 1"), arch)?;
        qmp.input_send_event(&[key("a", false)])?;
        std::thread::sleep(SETTLE);
        let after = screen(&mut qmp, &dump)?;

        // 4: the keybind, and the window it closed.
        qmp.input_send_event(&[key("meta_l", true)])?;
        qmp.input_send_event(&[key("q", true)])?;
        // Which window, and not merely that one was asked to close: the
        // client says so -- `the compositor asked the Checkerboard window
        // called one to close` -- because a keybind that closed the wrong
        // window would otherwise pass this.
        wait_for(
            watching,
            "the compositor asked the Checkerboard window called one to close",
            arch,
        )?;
        qmp.input_send_event(&[key("q", false)])?;
        qmp.input_send_event(&[key("meta_l", false)])?;
        std::thread::sleep(SETTLE);
        let closed = screen(&mut qmp, &dump)?;

        seen = Some(Seen {
            lines: watching
                .lines()
                .iter()
                .chain(watching.after())
                .cloned()
                .collect(),
            screens: vec![before, after, closed],
        });
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    seen.ok_or_else(|| {
        Error::new(format!(
            "{arch}: the compositor never printed `{MARKER}` within {}s",
            args.timeout
        ))
    })
}

/// One `InputEvent` of QMP's `input-send-event`, as its JSON.
fn key(name: &str, down: bool) -> String {
    format!(
        "{{\"type\":\"key\",\"data\":{{\"down\":{down},\"key\":\
         {{\"type\":\"qcode\",\"data\":{}}}}}}}",
        crate::display::json_string(name)
    )
}

fn button(name: &str, down: bool) -> String {
    format!(
        "{{\"type\":\"btn\",\"data\":{{\"down\":{down},\"button\":{}}}}}",
        crate::display::json_string(name)
    )
}

fn absolute(axis: &str, value: i32) -> String {
    format!(
        "{{\"type\":\"abs\",\"data\":{{\"axis\":{},\"value\":{value}}}}}",
        crate::display::json_string(axis)
    )
}

/// How many pixels of `left` and `right` differ.
///
/// A screen that changed size is not comparable at all, which is
/// [`usize::MAX`]: the caller only asks whether anything changed, and a
/// different mode is not the answer it wanted.
fn differences(left: &Image, right: &Image) -> usize {
    if left.width != right.width || left.height != right.height {
        return usize::MAX;
    }
    left.pixels
        .chunks(3)
        .zip(right.pixels.chunks(3))
        .filter(|(one, other)| one != other)
        .count()
}

/// `test-seat` on each architecture asked for that has a screen.
///
/// # Errors
///
/// A key that never reached a window, a screen that did not change when it
/// did, or a keybind that did not close the window it was bound to close.
pub(crate) fn test_seat(args: &Args) -> Result<()> {
    for arch in args.arches()? {
        if crate::display::target(arch).is_none() {
            println!("  {arch}: no virtio-gpu in QEMU's machine; skipped");
            continue;
        }
        let program = build(arch, "hyprix", "hyprix")?;
        let client = build(arch, "compositor-pattern", "pattern")?;
        let seen = boot_and_type(arch, &program, &client, args)?;

        let has = |wanted: &str| seen.lines.iter().any(|line| line.contains(wanted));
        // The seat announced a keyboard and a pointer, which is 3.
        if !has("pattern: seat capabilities 0x3") {
            return Err(Error::new(format!(
                "{arch}: the seat did not announce both a keyboard and a pointer"
            )));
        }
        // A keymap of a real length: `no_keymap` is format 0 and size 0, and
        // a client given that can never name a key.
        if has("pattern: keymap format 1 size 0") {
            return Err(Error::new(format!(
                "{arch}: the keymap was sent with a length of zero"
            )));
        }

        let [before, after, closed] = seen.screens.as_slice() else {
            return Err(Error::new(format!("{arch}: not three screendumps")));
        };
        let pixels = before.width * before.height;
        let changed = differences(before, after);
        if changed == 0 {
            return Err(Error::new(format!(
                "{arch}: the key reached the client and the screen did not change; \
                 the client redraws on a key, so nothing it drew reached the card"
            )));
        }
        println!(
            "  {arch}: a key from QEMU reached the window and redrew {changed} of {pixels} pixels"
        );

        // The keybind closed the window, so the screen is the compositor's
        // background -- everywhere but the cursor. The pointer was put in
        // the middle of the screen a few steps up, and the compositor draws
        // a 24x24 arrow whose tip is the pointer (`compositor/render`'s
        // `cursor`), down and to the right of it. That is what is left, and
        // a check that asked for the background *everywhere* was written
        // before the cursor was drawn at all.
        let tip = (closed.width / 2, closed.height / 2);
        let (found, wrong) = mismatches(closed, BACKGROUND, usize::MAX);
        let stray = found
            .iter()
            .filter(|(x, y, _)| {
                x.saturating_sub(tip.0) >= CURSOR || y.saturating_sub(tip.1) >= CURSOR
            })
            .take(4)
            .copied()
            .collect::<Vec<_>>();
        if !stray.is_empty() {
            return Err(Error::new(format!(
                "{arch}: `SUPER, Q, killactive` did not clear the screen; {} of {pixels} \
                 pixels are not the background or the cursor at {tip:?}, the first: {stray:?}",
                stray.len()
            )));
        }
        println!(
            "  {arch}: `bind = SUPER, Q, killactive` closed the window and left the background,              with {wrong} pixels of cursor at {tip:?}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CONFIG, absolute, button, differences, key};
    use crate::display::Image;

    #[test]
    fn an_event_is_the_json_qmp_takes() {
        assert_eq!(
            key("meta_l", true),
            "{\"type\":\"key\",\"data\":{\"down\":true,\"key\":{\"type\":\"qcode\",\"data\":\"meta_l\"}}}"
        );
        assert_eq!(
            button("left", true),
            "{\"type\":\"btn\",\"data\":{\"down\":true,\"button\":\"left\"}}"
        );
        assert_eq!(
            absolute("y", 16384),
            "{\"type\":\"abs\",\"data\":{\"axis\":\"y\",\"value\":16384}}"
        );
    }

    /// The bind the test presses has to be in the configuration it carries,
    /// or the test would pass on a compositor that closed the window for
    /// another reason.
    #[test]
    fn the_carried_configuration_holds_the_bind_the_test_presses() {
        assert!(CONFIG.contains("bind = SUPER, Q, killactive"));
    }

    #[test]
    fn two_screens_differ_by_their_pixels_and_another_mode_is_not_comparable() {
        let image = |width: usize, height: usize, fill: u8| Image {
            width,
            height,
            pixels: vec![fill; width * height * 3],
        };
        let plain = image(2, 2, 0);
        assert_eq!(differences(&plain, &plain), 0);
        assert_eq!(differences(&plain, &image(2, 2, 1)), 4);
        let mut one = image(2, 2, 0);
        // One byte of one pixel is one pixel, not three.
        if let Some(byte) = one.pixels.get_mut(1) {
            *byte = 9;
        }
        assert_eq!(differences(&plain, &one), 1);
        assert_eq!(differences(&plain, &image(3, 2, 0)), usize::MAX);
    }
}
