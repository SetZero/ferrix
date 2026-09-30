//! The live installer's MVP (`docs/INSTALLER.md` §11): `ferrix-install` in
//! the image, and `test-install`, which installs and boots what it wrote.
//!
//! `build --installer` makes the live image: the usual image, whose
//! initramfs also carries `/sbin/ferrix-install` and the empty root volume
//! it writes. Attached to a virtual machine as a virtio disk, it boots as any
//! image does; `ferrix-install /dev/vdX` then puts it on another disk.
//!
//! `test-install`, x86-64:
//!
//! 1. The live image is built, and copied to `build/x86_64/install-live.img`.
//! 2. A boot whose shell runs `ferrix-install --yes --from /dev/vdd /dev/vde`
//!    has the live copy as `vdd` and a blank 2 GiB disk as `vde`.
//! 3. A second boot starts from that disk alone, as a virtio disk, through
//!    OVMF: the kernel must publish its partitions, find `ferrix-root` on
//!    `vdd2`, install the system on it and reach the boot marker.

use std::path::{Path, PathBuf};

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::{Error, Result, cargo, fat, initramfs, native, ports, qemu, zinc};

/// Where the installer goes in the initramfs.
const PROGRAM_PATH: &str = "sbin/ferrix-install";
/// Where the empty root volume goes.
const ROOT_PATH: &str = "usr/share/ferrix/root.img.packed";
/// The empty root volume, packed as `src/lib/fs/btrfs/testdata` keeps it.
const ROOT_PACKED: &[u8] = include_bytes!("../../../../src/lib/fs/btrfs/testdata/root.img.packed");
/// The target disk's size.
const TARGET_BYTES: u64 = 2 << 30;

/// What the install boot's shell runs.
const SCRIPT: &str = "/sbin/ferrix-install --yes --from /dev/vdd /dev/vde\n\
echo \"install: exited $?\"\n\
exit 0\n";

/// What the install boot must print.
const DONE: &str = "ferrix-install: done.";
/// What the installed disk's boot must print.
const ROOTED: &str = "/ is btrfs on vdd2; the system was installed on it";

/// The installer and its root volume, as files for the initramfs, or `None`
/// on an architecture the installer is not built for.
pub(crate) fn files(arch: Arch) -> Result<Option<Vec<ports::File>>> {
    let Some(target) = zinc::target(arch) else {
        println!("  ferrix-install is not built for {} yet", arch.name());
        return Ok(None);
    };
    println!("  building ferrix-install for {target}");
    let target_dir = paths::target_dir().join("installer");
    let program = target_dir
        .join(target)
        .join("release")
        .join("ferrix-install");
    crate::builds::Build::cargo(
        format!("cargo build (ferrix-install) --target {target}"),
        paths::workspace_root().join("src/user/system/linux/installer"),
    )
    .args(["build", "--release", "--target", target])
    .env("CARGO_TARGET_DIR", &target_dir)
    .env("RUSTFLAGS", zinc::RUSTFLAGS)
    .output(&program)
    .run()?;
    let bytes = std::fs::read(&program)
        .map_err(|error| Error::new(format!("reading {}: {error}", program.display())))?;
    Ok(Some(vec![
        ports::File {
            path: PROGRAM_PATH.to_owned(),
            mode: 0o755,
            content: ports::Content::Bytes(bytes),
        },
        ports::File {
            path: ROOT_PATH.to_owned(),
            mode: 0o644,
            content: ports::Content::Bytes(ROOT_PACKED.to_vec()),
        },
    ]))
}

/// `test-install`: see the module documentation.
pub(crate) fn test_install(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "test-install runs on x86-64 only for now: the installed disk boots as a \
             virtio-blk-pci disk through OVMF",
        ));
    }
    let carried = files(arch)?.ok_or_else(|| Error::new("ferrix-install is not built"))?;
    let natives = native::build(arch, args.release)?;
    let loader = cargo::build_loader(arch, args.release)?;
    let initramfs = initramfs::build(None, &natives, None, &carried)?;

    // The live image, as a person would boot it.
    let kernel = cargo::build_kernel(arch, args.release)?;
    let live_built = fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
    let build = paths::workspace_root().join("build").join(arch.name());
    let live = build.join("install-live.img");
    copy(&live_built, &live)?;
    let target = build.join("install-target.img");
    blank(&target)?;

    // The install: the same system, with a shell running the installer.
    let zinc = zinc::built(arch)?.ok_or_else(|| Error::new("zinc is not built for x86_64"))?;
    let scripted = cargo::build_kernel_with_init(arch, args.release, &zinc, SCRIPT)?;
    let image = fat::write_image_with(arch, &loader, &scripted, &initramfs, None)?;
    let mut install = args.clone();
    install.install_disks = Some((live, target.clone()));
    let lines = qemu::watch_lines(arch, &image, &scripted, &install, crate::shell::EXITED)?;
    if !lines.iter().any(|line| line.contains(DONE)) {
        return Err(Error::new(format!(
            "{arch}: the installer did not finish; its lines are in the serial log"
        )));
    }
    println!("  {arch}: installed on {}", target.display());

    // The installed disk, on its own.
    let mut installed = args.clone();
    installed.boot_virtio = true;
    let lines = qemu::test_boot_lines(arch, &target, &kernel, &installed)?;
    if !lines.iter().any(|line| line.contains(ROOTED)) {
        return Err(Error::new(format!(
            "{arch}: the installed disk booted, but did not put / on its root partition \
             (no `{ROOTED}`)"
        )));
    }
    println!("  {arch}: the installed disk booted with / on its root partition");
    Ok(())
}

fn copy(from: &Path, to: &PathBuf) -> Result<()> {
    std::fs::copy(from, to)
        .map(drop)
        .map_err(|error| Error::new(format!("copying to {}: {error}", to.display())))
}

/// A fresh sparse blank disk at `path`.
fn blank(path: &PathBuf) -> Result<()> {
    let file = std::fs::File::create(path)
        .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
    file.set_len(TARGET_BYTES)
        .map_err(|error| Error::new(format!("{}: {error}", path.display())))
}
