//! Host-side driver for building, imaging and booting Ferrix.
//!
//! ```text
//! cargo xtask build     --arch x86_64 [--release]
//! cargo xtask run       --arch x86_64 [--release] [--gdb] [--smp N] [--memory M]
//! cargo xtask test-boot --arch x86_64 [--release] [--timeout SECONDS]
//! cargo xtask check     [--fast]
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
mod paths;
mod pe;
mod qemu;

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
    check         Run every quality gate (fmt, clippy, layering, audits)

OPTIONS:
    --arch <x86_64|aarch64|armv7a|all>   Target architecture   [default: host]
    --release                            Build with optimisations
    --smp <N>                            Virtual CPUs          [default: 4]
    --memory <MiB>                       Guest memory          [default: 512]
    --timeout <SECONDS>                  test-boot patience    [default: 120]
    --gdb                                Wait for a debugger on :1234
    --fast                               check: skip the cross-target clippy passes
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
                let image = build_image(arch, &args)?;
                println!("built {}", image.display());
            }
            Ok(())
        }
        "run" => {
            let arch = args.single_arch()?;
            let image = build_image(arch, &args)?;
            qemu::run(arch, &image, &args)
        }
        "test-boot" => {
            for arch in args.arches()? {
                let image = build_image(arch, &args)?;
                qemu::test_boot(arch, &image, &args)?;
            }
            Ok(())
        }
        "check" => check::run(&args),
        other => Err(Error::new(format!("unknown command `{other}`\n\n{USAGE}"))),
    }
}

/// Compile both halves for `arch` and assemble the bootable image.
fn build_image(arch: Arch, args: &Args) -> Result<std::path::PathBuf> {
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel(arch, args.release)?;
    fat::write_image(arch, &loader, &kernel)
}
