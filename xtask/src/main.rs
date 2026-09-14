//! Host-side driver for building, imaging and booting Ferrix.
//!
//! ```text
//! cargo xtask build     --arch x86_64 [--release] [--init PATH/{arch}/busybox]
//! cargo xtask run       --arch x86_64 [--release] [--gdb] [--smp N] [--memory M]
//!                       [--accel auto|tcg|whpx|kvm|hvf] [--init PATH/{arch}/busybox]
//! cargo xtask test-boot --arch x86_64 [--release] [--timeout SECONDS] [--reset]
//! cargo xtask test-shell --arch all --init PATH/{arch}/busybox [--timeout SECONDS]
//! cargo xtask test-vfs  --arch all --init PATH/{arch}/busybox [--timeout SECONDS]
//! cargo xtask check     [--fast] [--ferrousli] [--miri]
//! cargo xtask busybox   [--arch x86_64]
//! cargo xtask flash     [--arch armv7a] [--to MOUNT]
//! cargo xtask watch-serial            [--port DEVICE] [--timeout SECONDS]
//! cargo xtask deploy    [--arch armv7a] [--to MOUNT] [--port DEVICE]
//! ```
//!
//! `build` compiles the loader and the kernel for their two different targets
//! and writes a bootable FAT32 image. `test-boot` boots that image under QEMU,
//! watches the serial port, and fails if the kernel does not report success —
//! which is the only test in this repository that can tell us the thing runs.
//!
//! `build` and `run` given a static busybox — `--init`, or the `FERRIX_INIT`
//! variable — build it into the kernel, which starts `sh -i` on the console,
//! and put it in the initramfs at `/bin/busybox` with every applet linked
//! beside it, so that the shell finds `ls` on its `PATH=/bin`.
//!
//! `busybox` builds busybox against ferrousli, on Linux or natively on Windows,
//! and installs it for `--init ferrousli`, which names that binary instead of a
//! path.

// AUDIT: this is a command-line build tool. Its output *is* stdout and stderr,
// and routing it through a logging facade would make `cargo xtask build` read
// like a service rather than like a build.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "AUDIT: xtask is a CLI build tool; the terminal is its interface"
)]

mod args;
mod btrfs_disk;
mod busybox;
mod cargo;
mod check;
mod fat;
mod flash;
mod initramfs;
mod native;
mod paths;
mod pe;
mod qemu;
mod serial;
mod shell;
mod symbolize;
mod test_disk;
mod vfs;
mod zinc;

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
    test-vfs      Boot with busybox in the initramfs and require stage 8's exit programs and applets
    check         Run every quality gate (fmt, clippy, layering, audits)
    model-doc     Regenerate docs/generated/ from the SysML model
    busybox       Build busybox against ferrousli (x86_64) for --init ferrousli
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
    --ferrousli                          check: also ferrousli's fmt, clippy and tests, debug and release
    --miri                               check: add CI's Miri steps (needs nightly and miri)
    --reset                              test-boot: ferrix.onexit=reset in CMDLINE.TXT, and require a reset;
                                         build, run: put that CMDLINE.TXT in the image
    --to <MOUNT>                         flash: the card's mounted boot partition
    --port <DEVICE>                      watch-serial: e.g. /dev/ttyACM0
    --init <PATH|ferrousli>              The busybox; {arch} is replaced. build, run, flash, deploy: [or FERRIX_INIT]
                                         start `sh -i`, with the applets linked in /bin.
                                         `ferrousli`: the x86_64 busybox `cargo xtask busybox` installed
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
                let program = program_for(init, arch)?;
                let loader = cargo::build_loader(arch, args.release)?;
                let kernel =
                    cargo::build_kernel_with_init(arch, args.release, &program, shell::SCRIPT)?;
                let natives = native::build(arch, args.release)?;
                let image = fat::write_image(arch, &loader, &kernel, &natives, None)?;
                qemu::test_shell(arch, &image, &kernel, &args)?;
            }
            Ok(())
        }
        "test-vfs" => test_vfs(&args),
        "check" => check::run(&args),
        "model-doc" => check::model_doc(),
        "busybox" => busybox::build(args.single_arch()?).map(|_| ()),
        "flash" => {
            let arch = args.single_arch()?;
            let (loader, kernel, initramfs) = build_board_files(arch, &args)?;
            flash::run(arch, &loader, &kernel, &initramfs, &args)
        }
        "watch-serial" => serial::watch(None, &args),
        // The whole of a board round-trip. Separate commands exist because
        // each half is useful alone — reflashing without watching, watching a
        // board someone else reset — but the common case is both, and a
        // command per step is a command per step to forget.
        "deploy" => {
            let arch = args.single_arch()?;
            let (loader, kernel, initramfs) = build_board_files(arch, &args)?;
            flash::run(arch, &loader, &kernel, &initramfs, &args)?;
            serial::watch(Some(&kernel), &args)
        }
        other => Err(Error::new(format!("unknown command `{other}`\n\n{USAGE}"))),
    }
}

/// `test-vfs`: stage 8's exit programs, then its applets, on each
/// architecture asked for.
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
    let commands = vfs::encode(&[vfs::COMMANDS, vfs::APPLETS].concat())?;
    let mut failed = Vec::new();
    for arch in args.arches()? {
        let program = program_for(init, arch)?;
        let loader = cargo::build_loader(arch, args.release)?;
        let list = paths::build_dir(arch).join("init-commands");
        vfs::write_if_changed(&list, &commands)?;
        let kernel = cargo::build_kernel_with_commands(arch, args.release, &list)?;
        let natives = native::build(arch, args.release)?;
        let initramfs = initramfs::build(Some(&program), &natives, None)?;
        let image = fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
        if let Err(error) = qemu::test_vfs(arch, &image, &kernel, args) {
            eprintln!("\n  {error}");
            failed.push(arch.name());
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "stage 8's exit programs or applets failed on {}",
            failed.join(", ")
        )))
    }
}

/// The command line an image carries: `ferrix.onexit=reset` under `--reset`, so
/// the machine resets when boot ends and `test-boot` can require that it did.
fn image_cmdline(args: &Args) -> Option<&'static str> {
    args.reset.then_some(qemu::RESET_CMDLINE)
}

/// Compile both halves for `arch` and assemble the bootable image.
///
/// The kernel ELF comes back beside the image, because it is what a panic
/// report's backtrace is resolved against: the image holds the same kernel
/// with nothing to look a symbol up in.
///
/// Given a program — `--init`, or `FERRIX_INIT` when that is absent — the
/// kernel is built with it to start `sh -i`, and the initramfs carries it at
/// `/bin/busybox` with a link beside it for every applet, so that the shell's
/// `PATH=/bin` finds `ls` where a person types it. Without one the image is
/// the one it always was. Either way the initramfs carries the tree's native
/// programs in `/sbin`, built and checked for `arch` first.
fn build_image(arch: Arch, args: &Args) -> Result<(PathBuf, PathBuf)> {
    let natives = native::build(arch, args.release)?;
    let Some(program) = optional_program(arch, args)? else {
        let (loader, kernel) = build_halves(arch, args)?;
        let image = fat::write_image(arch, &loader, &kernel, &natives, image_cmdline(args))?;
        return Ok((image, kernel));
    };
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &program, "")?;
    let shell = zinc::build(arch)?;
    let initramfs = initramfs::build(Some(&program), &natives, shell.as_deref())?;
    let image = fat::write_image_with(arch, &loader, &kernel, &initramfs, image_cmdline(args))?;
    Ok((image, kernel))
}

/// Compile what `flash` copies onto a board: the loader, the kernel and the
/// initramfs.
///
/// Given a program the kernel starts `sh -i` and the archive carries busybox,
/// exactly as [`build_image`] arranges for QEMU, so a card boots to the shell
/// an image does. Without one the files are the ones they always were. Either
/// way the archive carries the tree's native programs, as an image's does.
fn build_board_files(arch: Arch, args: &Args) -> Result<(PathBuf, PathBuf, Vec<u8>)> {
    let natives = native::build(arch, args.release)?;
    let Some(program) = optional_program(arch, args)? else {
        let (loader, kernel) = build_halves(arch, args)?;
        return Ok((loader, kernel, initramfs::build(None, &natives, None)?));
    };
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &program, "")?;
    let shell = zinc::build(arch)?;
    let initramfs = initramfs::build(Some(&program), &natives, shell.as_deref())?;
    Ok((loader, kernel, initramfs))
}

/// The program `init` names for `arch`, with `{arch}` replaced by its name,
/// refused unless it is a file.
///
/// `ferrousli` is not a path: it names the busybox `cargo xtask busybox`
/// installed, and is refused if there is none.
fn program_for(init: &str, arch: Arch) -> Result<PathBuf> {
    if init == busybox::INIT_NAME {
        return busybox::program(arch);
    }
    let program = PathBuf::from(init.replace("{arch}", arch.name()));
    if !program.is_file() {
        return Err(Error::new(format!(
            "no program at {} for {arch}",
            program.display()
        )));
    }
    Ok(program)
}

/// The program `build`, `run` and `test-boot` were given, if any: `--init`,
/// or else a non-empty `FERRIX_INIT`, the variable that has always chosen the
/// shell those commands embed.
fn optional_program(arch: Arch, args: &Args) -> Result<Option<PathBuf>> {
    let init = match (&args.init, std::env::var("FERRIX_INIT")) {
        (Some(init), _) => init.clone(),
        (None, Ok(init)) if !init.is_empty() => init,
        (None, Ok(_) | Err(std::env::VarError::NotPresent)) => return Ok(None),
        (None, Err(std::env::VarError::NotUnicode(_))) => {
            return Err(Error::new("FERRIX_INIT is not valid UTF-8"));
        }
    };
    program_for(&init, arch).map(Some)
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
