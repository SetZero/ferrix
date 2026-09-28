//! `test-yserver`: the X server Steam will draw through, started on Ferrix
//! with no display of its own and asked who it is (stage 22, the yserver
//! feasibility pass).
//!
//! yserver (github.com/joske/yserver, v1.6.0) is built for x86-64 glibc
//! against Debian 13's libraries, and runs here on them from a data volume, as
//! Chrome does. With no DRM card it starts headless and renders through
//! Vulkan on the CPU, Mesa's lavapipe; with no input device it starts only
//! when `YSERVER_ALLOW_NO_INPUT` says so, which a patch of Ferrix's adds.
//! The volume's `yserver/run.sh` starts it on `:1`, runs `xdpyinfo` against
//! it, and prints the server's log.
//!
//! The volume is made outside the tree for now, while the pass decides what
//! a fetch script would pin: `FERRIX_YSERVER_VOLUME` names the image.

use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result, busybox, cargo, fat, initramfs, native, qemu, rustc, shell, zinc};

/// glibc's x86-64 paths and the data files yserver and its libraries name
/// absolutely, each a link into the volume.
const LINKS: &[(&str, &str)] = &[
    ("lib64", "/data/usr/lib64"),
    ("lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("usr/lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("etc/fonts", "/data/etc/fonts"),
    ("usr/share/fonts", "/data/usr/share/fonts"),
    ("usr/share/fontconfig", "/data/usr/share/fontconfig"),
    ("usr/share/vulkan", "/data/usr/share/vulkan"),
    ("usr/share/X11", "/data/usr/share/X11"),
    ("usr/share/drirc.d", "/data/usr/share/drirc.d"),
];

/// The script: the volume's own, whose status says whether `xdpyinfo`
/// reached the server.
const SCRIPT: &str = r#"export PATH=/bin HOME=/tmp
[ -x /data/yserver/run.sh ] || exit 3
busybox sh /data/yserver/run.sh || exit 4
exit 17
"#;

/// What the script exits with when `xdpyinfo` reached the server.
const STATUS: i32 = 17;

/// Memory for the guest: lavapipe and a 130 MiB server.
const MEMORY: u32 = 2048;

/// The volume, from `FERRIX_YSERVER_VOLUME` or where the feasibility pass
/// writes it.
fn volume() -> Result<std::path::PathBuf> {
    let image = match std::env::var_os("FERRIX_YSERVER_VOLUME") {
        Some(image) => std::path::PathBuf::from(image),
        None => crate::paths::volume_directory("yserver-ref")?.join("vol/yserver.img"),
    };
    if !image.is_file() {
        return Err(Error::new(format!("{} is not there", image.display())));
    }
    Ok(image)
}

/// Boot a shell whose script starts yserver and runs `xdpyinfo` against it.
///
/// # Errors
///
/// When the volume is missing, the image cannot be built, the boot fails, or
/// `xdpyinfo` did not reach the server.
pub(crate) fn test_yserver(args: &Args) -> Result<()> {
    let arch = match args.arches()?.as_slice() {
        [Arch::X86_64] => Arch::X86_64,
        _ => return Err(Error::new("test-yserver runs on x86-64")),
    };
    let mut args = args.clone();
    args.data_image = Some(volume()?);
    if !args.memory_given {
        args.memory = MEMORY;
    }

    let shell =
        zinc::built(arch)?.ok_or_else(|| Error::new("zinc could not be built for x86-64"))?;
    println!("  {arch}: building an image whose shell starts yserver from the volume");
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &shell, SCRIPT)?;
    let natives = native::build(arch, args.release)?;
    let bytes = std::fs::read(&shell)
        .map_err(|error| Error::new(format!("reading {}: {error}", shell.display())))?;
    let busybox = busybox::program(arch)?;
    let archive = initramfs::build(Some(&busybox), &natives, Some(&bytes), &rustc::files(LINKS))?;
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, None)?;

    println!(
        "  {arch}: running yserver on Ferrix with {} MiB (timeout {}s)",
        args.memory, args.timeout
    );
    let lines = qemu::watch_then(arch, &image, &kernel, &args, shell::EXITED, |_| Ok(()))?;
    let exited = lines
        .iter()
        .find_map(|line| line.trim().strip_prefix(shell::EXITED))
        .map(str::trim);
    match exited {
        Some(status) if status == STATUS.to_string() => {
            println!("  {arch}: xdpyinfo reached yserver on Ferrix");
            Ok(())
        }
        Some("3") => Err(Error::new(format!("{arch}: /data/yserver/run.sh is not there"))),
        other => Err(Error::new(format!(
            "{arch}: the yserver script ended with {other:?}; its `yserver-feas:` lines say how"
        ))),
    }
}
