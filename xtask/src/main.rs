//! Host-side driver for building, imaging and booting Ferrix.
//!
//! ```text
//! cargo xtask build     --arch x86_64 [--release]
//! cargo xtask run       --arch x86_64 [--release] [--gdb] [--smp N] [--memory M]
//!                       [--accel auto|tcg|whpx|kvm|hvf]
//! cargo xtask test-boot --arch x86_64 [--release] [--timeout SECONDS]
//! cargo xtask test-shell --arch all --init PATH/{arch}/busybox [--timeout SECONDS]
//! cargo xtask test-vfs  --arch all --init PATH/{arch}/busybox [--timeout SECONDS]
//! cargo xtask check     [--fast]
//! cargo xtask flash     [--arch armv7a] [--to MOUNT]
//! cargo xtask watch-serial            [--port DEVICE] [--timeout SECONDS]
//! cargo xtask deploy    [--arch armv7a] [--to MOUNT] [--port DEVICE]
//! ```
//!
//! `build` compiles the loader and the kernel for their two different targets
//! and writes a bootable FAT32 image. `test-boot` boots that image under QEMU,
//! watches the serial port, and fails if the kernel does not report success —
//! which is the only test in this repository that can tell us the thing runs.

// AUDIT: this is a command-line build tool. Its output *is* stdout and stderr,
// and routing it through a logging facade would make `cargo xtask build` read
// like a service rather than like a build.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "AUDIT: xtask is a CLI build tool; the terminal is its interface"
)]

mod args;
mod cargo;
mod check;
mod fat;
mod flash;
mod initramfs;
mod paths;
mod pe;
mod qemu;
mod serial;
mod shell;
mod symbolize;
mod vfs;

use std::path::PathBuf;
use std::process::ExitCode;

use args::Args;
use paths::Arch;

/// What went wrong, in a form that can be printed and turned into an exit code.
#[derive(Debug)]
pub(crate) struct Error {
    /// Human-readable description, already contextualised.
    message: String,
}

impl Error {
    /// Build an error from anything printable.
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::new(error.to_string())
    }
}

/// The result type every xtask step returns.
pub(crate) type Result<T> = std::result::Result<T, Error>;

const USAGE: &str = "\
Ferrix build driver

USAGE:
    cargo xtask <COMMAND> [OPTIONS]

COMMANDS:
    build         Compile the loader and kernel and write a bootable image
    run           Boot the image under QEMU, attached to the terminal
    test-boot     Boot the image under QEMU and assert the kernel came up
    test-shell    Boot with a static busybox built in and require its script's output
    test-vfs      Boot with busybox in the initramfs and require stage 8's exit programs
    check         Run every quality gate (fmt, clippy, layering, audits)
    model-doc     Regenerate docs/generated/ from the SysML model
    flash         Copy the loader and kernel onto a board's boot partition
    watch-serial  Watch a real serial port for the kernel's boot report
    deploy        flash, then watch-serial: one command for a board

OPTIONS:
    --arch <x86_64|aarch64|armv7a|all>   Target architecture   [default: host]
    --release                            Build with optimisations
    --smp <N>                            Virtual CPUs          [default: 4]
    --memory <MiB>                       Guest memory          [default: 512]
    --timeout <SECONDS>                  test-boot patience    [default: 120]
    --accel <auto|tcg|whpx|kvm|hvf>      QEMU accelerator      [default: tcg]
    --gdb                                Wait for a debugger on :1234
    --fast                               check: skip the cross-target clippy passes
    --to <MOUNT>                         flash: the card's mounted boot partition
    --port <DEVICE>                      watch-serial: e.g. /dev/ttyACM0
    --init <PATH>                        test-shell, test-vfs: the busybox; {arch} is replaced
    -h, --help                           This message
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("\nxtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;

    if args.help {
        println!("{USAGE}");
        return Ok(());
    }

    let Some(command) = args.command.as_deref() else {
        println!("{USAGE}");
        return Err(Error::new("no command given"));
    };

    match command {
        "build" => {
            for arch in args.arches()? {
                let (image, _) = build_image(arch, &args)?;
                println!("built {}", image.display());
            }
            Ok(())
        }
        "run" => {
            let arch = args.single_arch()?;
            let (image, _) = build_image(arch, &args)?;
            qemu::run(arch, &image, &args)
        }
        "test-boot" => {
            for arch in args.arches()? {
                let (image, kernel) = build_image(arch, &args)?;
                qemu::test_boot(arch, &image, &kernel, &args)?;
            }
            Ok(())
        }
        "test-shell" => {
            let init = args.init.as_deref().ok_or_else(|| {
                Error::new(
                    "test-shell needs --init PATH, a static busybox for each architecture; \
                     `{arch}` in the path is replaced by the architecture's name",
                )
            })?;
            for arch in args.arches()? {
                let program = PathBuf::from(init.replace("{arch}", arch.name()));
                if !program.is_file() {
                    return Err(Error::new(format!(
                        "no program at {} for {arch}",
                        program.display()
                    )));
                }
                let loader = cargo::build_loader(arch, args.release)?;
                let kernel =
                    cargo::build_kernel_with_init(arch, args.release, &program, shell::SCRIPT)?;
                let image = fat::write_image(arch, &loader, &kernel)?;
                qemu::test_shell(arch, &image, &kernel, &args)?;
            }
            Ok(())
        }
        "test-vfs" => test_vfs(&args),
        "check" => check::run(&args),
        "model-doc" => check::model_doc(),
        "flash" => {
            let arch = args.single_arch()?;
            let (loader, kernel) = build_halves(arch, &args)?;
            flash::run(arch, &loader, &kernel, &args)
        }
        "watch-serial" => serial::watch(None, &args),
        // The whole of a board round-trip. Separate commands exist because
        // each half is useful alone — reflashing without watching, watching a
        // board someone else reset — but the common case is both, and a
        // command per step is a command per step to forget.
        "deploy" => {
            let arch = args.single_arch()?;
            let (loader, kernel) = build_halves(arch, &args)?;
            flash::run(arch, &loader, &kernel, &args)?;
            serial::watch(Some(&kernel), &args)
        }
        other => Err(Error::new(format!("unknown command `{other}`\n\n{USAGE}"))),
    }
}

/// `test-vfs`: stage 8's exit programs, on each architecture asked for.
///
/// The program goes into the initramfs, at `/bin/busybox`, rather than into
/// the kernel: loading it from a file is part of what stage 8 is for. Every
/// architecture is run even after one fails, because which of the three a
/// missing call breaks is the report.
fn test_vfs(args: &Args) -> Result<()> {
    let init = args.init.as_deref().ok_or_else(|| {
        Error::new(
            "test-vfs needs --init PATH, a static busybox for each architecture; \
             `{arch}` in the path is replaced by the architecture's name",
        )
    })?;
    let commands = vfs::encode(vfs::COMMANDS)?;
    let mut failed = Vec::new();
    for arch in args.arches()? {
        let program = PathBuf::from(init.replace("{arch}", arch.name()));
        if !program.is_file() {
            return Err(Error::new(format!(
                "no program at {} for {arch}",
                program.display()
            )));
        }
        let loader = cargo::build_loader(arch, args.release)?;
        let list = paths::build_dir(arch).join("init-commands");
        vfs::write_if_changed(&list, &commands)?;
        let kernel = cargo::build_kernel_with_commands(arch, args.release, &list)?;
        let initramfs = initramfs::build(Some(&program))?;
        let image = fat::write_image_with(arch, &loader, &kernel, &initramfs)?;
        if let Err(error) = qemu::test_vfs(arch, &image, &kernel, args) {
            eprintln!("\n  {error}");
            failed.push(arch.name());
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "stage 8's exit programs failed on {}",
            failed.join(", ")
        )))
    }
}

/// Compile both halves for `arch` and assemble the bootable image.
///
/// The kernel ELF comes back beside the image, because it is what a panic
/// report's backtrace is resolved against: the image holds the same kernel
/// with nothing to look a symbol up in.
fn build_image(arch: Arch, args: &Args) -> Result<(PathBuf, PathBuf)> {
    let (loader, kernel) = build_halves(arch, args)?;
    let image = fat::write_image(arch, &loader, &kernel)?;
    Ok((image, kernel))
}

/// Compile both halves for `arch`, without assembling an image.
///
/// A board has its own filesystem already, put there by the vendor's firmware,
/// so what it wants is the two files rather than something to write over the
/// card with.
fn build_halves(arch: Arch, args: &Args) -> Result<(PathBuf, PathBuf)> {
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel(arch, args.release)?;
    Ok((loader, kernel))
}
