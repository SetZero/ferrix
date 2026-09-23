//! `test-selfhost`: stage 20's first exit. Ferrix's x86-64 image is built on
//! Ferrix, by the same `cargo xtask build` a person runs on a Linux host, and
//! the image it made boots.
//!
//! # The volume
//!
//! One btrfs volume per run, made here and attached writable. It has no
//! `ferrix-root` label, so the kernel mounts it at `/data`. It holds:
//!
//! * the toolchain tree `scripts/fetch-rustc-sysroot.sh` keeps beside its own
//!   image: rustc and Cargo, the standard libraries for the host,
//!   `x86_64-unknown-none` and `x86_64-unknown-uefi`, and Debian's glibc and
//!   gcc driver. Hard-linked into the staging directory rather than copied;
//! * `src/`, every file git tracks in this checkout, as it is in the work
//!   tree, so an uncommitted change is built there as it would be here;
//! * `vendor/`, the workspace's crates.io dependencies from `cargo vendor
//!   --offline`, and a Cargo home whose configuration points at them, since
//!   nothing in the guest's build goes to the network;
//! * [`ROOM`] for the target directory and the image, sparse on the host.
//!
//! The initramfs is zinc and [`rustc::LINKS`], as `test-rustc`'s is: the
//! absolute paths glibc and gcc name, into the volume.
//!
//! # The build
//!
//! zinc runs [`script`]: `cargo xtask build --arch x86_64`, which compiles
//! xtask for the guest, a glibc program like rustc; xtask then has Cargo
//! compile the loader, the kernel and the native programs, and writes the FAT
//! image, as it does on any host. When the shell exits the guest powers off,
//! and the kernel commits `/data` on the way, so this waits for QEMU to exit
//! by itself rather than killing it ([`qemu::watch_to_power_off`]).
//!
//! # The judgement
//!
//! The script's own status and the line xtask prints for the image it wrote;
//! then `btrfs check` over the volume, since Ferrix wrote a gigabyte to it;
//! then `btrfs restore` takes the image and the kernel ELF out, and the image
//! must pass `test-boot`'s boot test, the kernel resolving its own
//! backtraces. It is not compared with an image built here: the paths
//! compiled into it -- `/data/src`, and the vendored crates' -- differ, and
//! so would the bytes.
//!
//! # Where it runs
//!
//! On a Linux host: the volume is made with `mkfs.btrfs --rootdir` and read
//! back with `btrfs restore`, and the tree it starts from is made by a script
//! that needs `dpkg-deb`. x86-64 only, because the tree holds x86-64
//! binaries.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::{Error, Result, btrfs_check, cargo, fat, initramfs, native, qemu, rustc, zinc};

/// Where the guest's Cargo home, target directory and source are, on the
/// volume.
const CARGO_HOME: &str = "/data/cargo-home";
const TARGET: &str = "/data/target";
const SOURCE: &str = "/data/src";

/// The Cargo configuration the guest builds with: crates.io is the vendored
/// directory, and Cargo is told it has no network, so a crate missing from
/// the directory fails the build by name rather than as a DNS error.
const CARGO_CONFIG: &str = r#"[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "/data/vendor"

[net]
offline = true
"#;

/// Space on the volume beyond what the staging directory holds. A debug
/// build's target directory is about 1 GiB and the image 256 MiB; btrfs
/// keeps its metadata twice.
const ROOM: u64 = 8 << 30;

/// Guest memory unless `--memory` says otherwise. btrfs file pages stay in
/// memory once read or written, and the build reads the toolchain's 350 MiB
/// of libraries and writes about 1.3 GiB.
const MEMORY: u32 = 8192;

/// Seconds to wait unless `--timeout` says otherwise, for the build boot.
const TIMEOUT: u64 = 3600;

/// The status the script exits with when the build succeeded.
const STATUS: i32 = 20;

/// What the guest's xtask prints when it has written the image.
const BUILT: &str = "built /data/src/build/x86_64/ferrix.img";

/// Build Ferrix's x86-64 image on Ferrix, take it out of the volume, and
/// boot it.
///
/// # Errors
///
/// When the host cannot make the volume, the build boot fails or its script
/// does not finish, the volume is not a clean btrfs, or the image the guest
/// built does not pass the boot test.
pub(crate) fn test_selfhost(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "test-selfhost runs on x86-64 only: the toolchain tree holds x86-64 binaries",
        ));
    }
    if !cfg!(target_os = "linux") {
        return Err(Error::new(
            "test-selfhost runs on a Linux host: it makes its volume with mkfs.btrfs and reads \
             it back with btrfs restore",
        ));
    }
    let checker = btrfs_check::Checker::required()?;
    let tree = rustc::tree()?;
    let work = paths::build_dir(arch).join("selfhost");
    let volume = stage(&tree, &work)?;

    let mut build = args.clone();
    build.data_image = Some(volume.clone());
    build.data_image_kept = true;
    if !build.memory_given {
        build.memory = MEMORY;
    }
    if !build.timeout_given {
        build.timeout = TIMEOUT;
    }

    let shell =
        zinc::built(arch)?.ok_or_else(|| Error::new("zinc could not be built for x86-64"))?;
    println!("  {arch}: building an image whose shell builds Ferrix");
    let loader = cargo::build_loader(arch, args.release)?;
    let script = match std::env::var_os("FERRIX_SELFHOST_SCRIPT") {
        Some(path) => std::fs::read_to_string(path)?,
        None => script(args.release),
    };
    let kernel = cargo::build_kernel_with_init(arch, args.release, &shell, &script)?;
    let natives = native::build(arch, args.release)?;
    let bytes = std::fs::read(&shell)
        .map_err(|error| Error::new(format!("reading {}: {error}", shell.display())))?;
    let archive = initramfs::build(None, &natives, Some(&bytes), &rustc::files(rustc::LINKS))?;
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, None)?;

    println!(
        "  {arch}: building Ferrix on Ferrix with {} MiB and {} processors (timeout {}s)",
        build.memory, build.smp, build.timeout
    );
    let lines = qemu::watch_to_power_off(arch, &image, &kernel, &build, crate::shell::EXITED);
    // The boot test below writes the serial log again.
    let log = paths::build_dir(arch).join("serial.log");
    let kept = work.join("build-serial.log");
    let _ = std::fs::copy(&log, &kept);
    judge(arch, &lines?)?;
    println!("  {arch}: the build boot's serial output is in {}", kept.display());

    checker.run(&volume, arch)?;
    let (built, built_kernel) = restore(&volume, &work, args.release)?;
    println!("  {arch}: booting the image Ferrix built");
    qemu::test_boot(arch, &built, &built_kernel, args)?;
    println!("  {arch}: Ferrix built its own image, and it booted");
    Ok(())
}

/// The script zinc runs: Cargo's version first, so a failure says whether
/// the toolchain started at all.
fn script(release: bool) -> String {
    let profile = if release { " --release" } else { "" };
    format!(
        "export PATH=/data/rust/bin:/data/usr/bin:/bin\n\
         export HOME=/data/home CARGO_HOME={CARGO_HOME} CARGO_TARGET_DIR={TARGET}\n\
         cd {SOURCE}\n\
         cargo -V || exit 3\n\
         cargo xtask build --arch x86_64{profile} || exit 4\n\
         exit {STATUS}\n"
    )
}

/// Whether the transcript is a build that wrote its image and a script that
/// got to its end.
fn judge(arch: Arch, lines: &[String]) -> Result<()> {
    let after_boot = lines
        .iter()
        .position(|line| line.contains(qemu::SUCCESS_MARKER))
        .and_then(|at| lines.get(at + 1..))
        .unwrap_or_default();
    let exited = after_boot
        .iter()
        .find_map(|line| line.trim().strip_prefix(crate::shell::EXITED))
        .map(str::trim);
    let built = after_boot.iter().any(|line| line.trim_end() == BUILT);
    match exited {
        Some(status) if status == STATUS.to_string() && built => {
            println!("  {arch}: cargo xtask build finished on Ferrix");
            Ok(())
        }
        Some("3") => Err(Error::new(format!("{arch}: `cargo -V` failed"))),
        Some("4") => Err(Error::new(format!(
            "{arch}: Cargo ran, and `cargo xtask build` failed"
        ))),
        Some(status) => Err(Error::new(format!(
            "{arch}: the script exited with {status}; image line {built}"
        ))),
        None => Err(Error::new(format!("{arch}: the shell never exited"))),
    }
}

/// Make the volume in `work` from `tree`, this checkout and its vendored
/// crates, and return its path.
fn stage(tree: &Path, work: &Path) -> Result<PathBuf> {
    let stage = work.join("stage");
    if stage.exists() {
        std::fs::remove_dir_all(&stage)?;
    }
    std::fs::create_dir_all(&stage)?;
    println!("  staging the volume in {}", stage.display());
    // Hard links, since the tree is 1.9 GiB and mkfs copies it anyway; a
    // copy where the two are on different filesystems.
    let linked = Command::new("cp")
        .arg("-al")
        .arg(tree.join("."))
        .arg(&stage)
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !linked {
        std::fs::remove_dir_all(&stage)?;
        std::fs::create_dir_all(&stage)?;
        let mut command = Command::new("cp");
        let _ = command.arg("-a").arg(tree.join(".")).arg(&stage);
        cargo::run(command, "copying the toolchain tree")?;
    }
    let copied = copy_sources(&stage.join("src"))?;
    println!("  {copied} tracked files in src/");
    vendor(&stage.join("vendor"))?;
    std::fs::create_dir_all(stage.join("cargo-home"))?;
    std::fs::write(stage.join("cargo-home/config.toml"), CARGO_CONFIG)?;
    std::fs::create_dir_all(stage.join("home"))?;

    let volume = work.join("volume.img");
    let _ = std::fs::remove_file(&volume);
    let size = used(&stage)?.saturating_add(ROOM);
    std::fs::File::create(&volume)?.set_len(size)?;
    let mut command = Command::new("mkfs.btrfs");
    let _ = command.arg("-q").arg("--rootdir").arg(&stage).arg(&volume);
    cargo::run(command, "mkfs.btrfs --rootdir")?;
    println!(
        "  volume {} ({} MiB, sparse)",
        volume.display(),
        size >> 20
    );
    Ok(volume)
}

/// Copy every file git tracks in this checkout, as the work tree has it, into
/// `into`, and say how many.
fn copy_sources(into: &Path) -> Result<usize> {
    let root = paths::workspace_root();
    let listed = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| Error::new(format!("running git ls-files: {error}")))?;
    if !listed.status.success() {
        return Err(Error::new(format!(
            "git ls-files failed in {}: the source is what git tracks there",
            root.display()
        )));
    }
    let mut copied = 0;
    for name in listed.stdout.split(|&byte| byte == 0) {
        if name.is_empty() {
            continue;
        }
        let name = std::str::from_utf8(name)
            .map_err(|_| Error::new("git tracks a path that is not UTF-8"))?;
        let from = root.join(name);
        let to = into.join(name);
        // Deleted in the work tree and not yet committed: absent here too.
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            continue;
        };
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if meta.file_type().is_symlink() {
            link(&std::fs::read_link(&from)?, &to)?;
        } else if meta.is_file() {
            let _ = std::fs::copy(&from, &to)
                .map_err(|error| Error::new(format!("copying {name}: {error}")))?;
        } else {
            return Err(Error::new(format!(
                "git tracks {name}, which is neither a file nor a link"
            )));
        }
        copied += 1;
    }
    Ok(copied)
}

/// A symbolic link at `to` reading `target`.
#[cfg(unix)]
fn link(target: &Path, to: &Path) -> Result<()> {
    Ok(std::os::unix::fs::symlink(target, to)?)
}

/// A symbolic link at `to` reading `target`: never asked for, since
/// [`test_selfhost`] refuses a host that is not Linux first.
#[cfg(not(unix))]
fn link(_target: &Path, to: &Path) -> Result<()> {
    Err(Error::new(format!(
        "{} is a symbolic link, which this host cannot make",
        to.display()
    )))
}

/// `cargo vendor` the workspace's crates.io dependencies into `into`, from
/// Cargo's own cache: no network here either.
fn vendor(into: &Path) -> Result<()> {
    let status = Command::new(cargo::cargo())
        .current_dir(paths::workspace_root())
        .args(["vendor", "--locked", "--offline", "--quiet"])
        .arg(into)
        .stdout(Stdio::null())
        .status()
        .map_err(|error| Error::new(format!("could not run cargo vendor: {error}")))?;
    if !status.success() {
        return Err(Error::new(
            "cargo vendor --offline failed: the crates are taken from Cargo's cache, which \
             `cargo fetch` fills once, with the network",
        ));
    }
    Ok(())
}

/// The bytes `du` says `path` holds, each hard-linked file once.
fn used(path: &Path) -> Result<u64> {
    let output = Command::new("du")
        .arg("-sb")
        .arg(path)
        .output()
        .map_err(|error| Error::new(format!("running du: {error}")))?;
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .and_then(|bytes| bytes.parse().ok())
        .ok_or_else(|| Error::new(format!("du could not size {}", path.display())))
}

/// Take the image and the kernel ELF the guest built out of `volume`, into
/// `work`, and return their paths.
fn restore(volume: &Path, work: &Path, release: bool) -> Result<(PathBuf, PathBuf)> {
    let out = work.join("out");
    if out.exists() {
        std::fs::remove_dir_all(&out)?;
    }
    std::fs::create_dir_all(&out)?;
    let profile = if release { "release" } else { "debug" };
    // `btrfs restore` matches each directory on the way down as well, so the
    // expression names every one.
    let wanted = format!(
        "^/(|src(|/build(|/x86_64(|/ferrix\\.img)))|target(|/x86_64-unknown-none(|/{profile}(|/ferrix-kernel))))$"
    );
    let mut command = Command::new("btrfs");
    let _ = command
        .args(["restore", "--path-regex", &wanted])
        .arg(volume)
        .arg(&out)
        .stdout(Stdio::null());
    cargo::run(command, "btrfs restore")?;
    let image = out.join("src/build/x86_64/ferrix.img");
    let kernel = out.join(format!("target/x86_64-unknown-none/{profile}/ferrix-kernel"));
    for file in [&image, &kernel] {
        if !file.is_file() {
            return Err(Error::new(format!(
                "btrfs restore did not give {}: the guest said it built the image, and the \
                 volume does not have it",
                file.display()
            )));
        }
    }
    println!(
        "  took the guest's image out of the volume: {} MiB, kernel {} KiB",
        std::fs::metadata(&image)?.len() >> 20,
        std::fs::metadata(&kernel)?.len() >> 10
    );
    Ok((image, kernel))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn a_build_that_wrote_its_image_passes() {
        let lines = transcript(&[
            qemu::SUCCESS_MARKER,
            "cargo 1.97.1 (5ee8cd0dd 2026-07-09)",
            BUILT,
            "  init     the shell exited with 20",
        ]);
        assert!(judge(Arch::X86_64, &lines).is_ok());
    }

    #[test]
    fn the_image_line_must_come_after_the_boot() {
        let lines = transcript(&[
            BUILT,
            qemu::SUCCESS_MARKER,
            "  init     the shell exited with 20",
        ]);
        assert!(judge(Arch::X86_64, &lines).is_err());
    }

    #[test]
    fn each_failing_step_is_named() {
        for (status, words) in [("3", "cargo -V"), ("4", "cargo xtask build")] {
            let exit = format!("  init     the shell exited with {status}");
            let lines = transcript(&[qemu::SUCCESS_MARKER, &exit]);
            let error = judge(Arch::X86_64, &lines).unwrap_err().to_string();
            assert!(error.contains(words), "{error}");
        }
    }

    #[test]
    fn the_script_builds_the_profile_asked_for() {
        assert!(script(false).contains("cargo xtask build --arch x86_64 ||"));
        assert!(script(true).contains("cargo xtask build --arch x86_64 --release ||"));
        assert!(script(false).contains(&format!("exit {STATUS}")));
    }

    #[test]
    fn the_image_line_is_the_one_xtask_prints() {
        // `main.rs`'s `build` prints "built <image>", and the image is
        // `paths::build_dir`'s `ferrix.img` under the guest's source.
        assert_eq!(BUILT, format!("built {SOURCE}/build/x86_64/ferrix.img"));
    }
}
