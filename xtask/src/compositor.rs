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
//! With no client connected the screen is the compositor's background, and
//! that is what this requires. The two-client picture is
//! `compositor/hyprix/tests/two_clients.rs` on the host, and stays there
//! until the initramfs can carry a program beside init: the kernel embeds one
//! today (`kernel/build.rs`), so the compositor has nothing to start.

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

/// How long to let the screen settle after the marker, as `test-display`
/// allows: the flip is queued and the screendump is the host's own read.
const SETTLE: Duration = Duration::from_secs(4);

/// Build `hyprix` for `arch`, and say where it is.
fn build(arch: Arch) -> Result<PathBuf> {
    let target = crate::display::target(arch).ok_or_else(|| {
        Error::new(format!(
            "{arch} has no virtio-gpu in QEMU; the compositor test runs on x86_64 and aarch64"
        ))
    })?;
    let target_dir = paths::target_dir().join("compositor").join("hyprix");
    println!("  building compositor/hyprix for {target}");
    let mut command = Command::new(crate::cargo::cargo());
    let _ = command
        .current_dir(paths::workspace_root().join("compositor"))
        .args(["build", "--release", "-p", "hyprix", "--target", target])
        .env("CARGO_TARGET_DIR", &target_dir);
    crate::cargo::run(command, "cargo build (compositor/hyprix)")?;
    Ok(target_dir.join(target).join("release").join("hyprix"))
}

/// Boot the compositor and take a screendump once it says it is on the card.
fn boot_and_dump(arch: Arch, program: &Path, args: &Args) -> Result<Image> {
    let loader = crate::cargo::build_loader(arch, args.release)?;
    let kernel = crate::cargo::build_kernel_with_init(arch, args.release, program, "")?;
    let natives = crate::native::build(arch, args.release)?;
    let image = crate::fat::write_image(arch, &loader, &kernel, &natives, None)?;

    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("compositor.ppm");
    let mut taken = None;
    let hook = |lines: &[String]| -> Result<()> {
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
        let settle = Instant::now() + SETTLE;
        loop {
            let bytes = {
                qmp.screendump(Some(DEVICE_ID), &dump)?;
                std::fs::read(&dump)
                    .map_err(|error| Error::new(format!("reading {}: {error}", dump.display())))?
            };
            let screen = parse_ppm(&bytes)?;
            let clean = mismatches(&screen, BACKGROUND, 0).1 == 0;
            if clean || Instant::now() >= settle {
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
        let program = build(arch)?;
        let screen = boot_and_dump(arch, &program, args)?;
        let (found, count) = mismatches(&screen, BACKGROUND, 8);
        if count != 0 {
            return Err(Error::new(format!(
                "{arch}: {count} of {} pixels are not the compositor's background \
                 0x111111; the first: {found:?}",
                screen.width * screen.height
            )));
        }
        println!(
            "  {arch}: every one of {} pixels is the compositor's background",
            screen.width * screen.height
        );
    }
    Ok(())
}
