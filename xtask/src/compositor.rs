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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::display::{DEVICE_ID, Image, Qmp, free_port, mismatches, parse_ppm};
use crate::paths::{self, Arch};
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

/// The picture two clients make, which `compositor/render`'s own tests bless
/// and `compositor/hyprix/tests/two_clients.rs` compares against on the host.
const EXPECTED: &str = "compositor/render/tests/data/dwindle-two-clients.xrle";

/// Where the clients go in the initramfs, which is what the compositor's
/// `exec-once` names.
const CLIENT_PATH: &str = "bin/pattern";

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

/// Boot the compositor and take a screendump once it says it is on the card.
///
/// The compositor is the kernel's init program, and the client is a file in
/// the initramfs: the kernel embeds one program and unpacks the rest, so
/// `/bin/pattern` is where the compositor's `exec-once` finds it. The
/// arguments reach the compositor through the init script, which
/// `Options::parse` reads as its own command line.
fn boot_and_dump(arch: Arch, program: &Path, client: &Path, args: &Args) -> Result<Image> {
    let loader = crate::cargo::build_loader(arch, args.release)?;
    // One argument a line: a script has no quoting, and each `--exec` value
    // is a command line with spaces in it. `Options::unshell` says so.
    let script =
        format!("--exec\n/{CLIENT_PATH} checkerboard one\n--exec\n/{CLIENT_PATH} gradient two");
    let kernel = crate::cargo::build_kernel_with_init(arch, args.release, program, &script)?;
    let natives = crate::native::build(arch, args.release)?;
    let bytes = std::fs::read(client)
        .map_err(|error| Error::new(format!("reading {}: {error}", client.display())))?;
    let carried = [crate::ports::File {
        path: CLIENT_PATH,
        mode: 0o755,
        bytes,
    }];
    let initramfs = crate::initramfs::build(None, &natives, None, &carried)?;
    let image = crate::fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;

    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("compositor.ppm");
    let mut taken = None;
    let hook = |watching: &mut crate::qemu::Watching<'_>| -> Result<()> {
        let lines = watching.lines();
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        if let Some(line) = lines.iter().rev().find(|line| line.contains(FAILED)) {
            return Err(Error::new(format!("{arch}: {}", line.trim())));
        }
        let marker = lines
            .iter()
            .rev()
            .find(|line| line.contains(MARKER))
            .map_or("", |line| line.trim());
        println!("  {arch}: {}", marker.trim_start_matches("| ").trim());
        // Read until the screen is the picture, or until the time is up: the
        // clients have to connect, be configured, draw and commit, and each
        // step is a round trip through the socket.
        let want = expected()?;
        let settle = Instant::now() + SETTLE;
        loop {
            let bytes = {
                qmp.screendump(Some(DEVICE_ID), &dump)?;
                std::fs::read(&dump)
                    .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?
            };
            let screen = parse_ppm(&bytes)?;
            let done = differences(&screen, &want).1 == 0;
            if done || Instant::now() >= settle {
                taken = Some(screen);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    taken.ok_or_else(|| {
        Error::new(format!(
            "{arch}: the compositor never printed `{MARKER}` within {}s",
            args.timeout
        ))
    })
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
        let screen = boot_and_dump(arch, &program, &client, args)?;
        let want = expected()?;
        let (found, count) = differences(&screen, &want);
        if count != 0 {
            // A screen that is all background is the clients never having
            // drawn, which is a different failure from a wrong picture.
            let blank = mismatches(&screen, BACKGROUND, 0).1 == 0;
            let why = if blank {
                "the screen is the compositor's background: no client drew"
            } else {
                "the picture is not the one the renderer's own tests bless"
            };
            return Err(Error::new(format!(
                "{arch}: {why}; {count} of {} pixels differ, the first: {found:?}",
                screen.width * screen.height
            )));
        }
        println!(
            "  {arch}: two clients tiled, every one of {} pixels as the renderer draws them",
            screen.width * screen.height
        );
    }
    Ok(())
}

/// The expected image, as the `(red, green, blue)` bytes a screendump holds.
///
/// `compositor/render/src/golden.rs` writes the format and says why: a
/// run-length image of `XRGB8888` rows, with a row that repeats the one above
/// written as a single byte.
fn expected() -> Result<Vec<u8>> {
    let path = paths::workspace_root().join(EXPECTED);
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
