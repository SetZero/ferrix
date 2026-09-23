//! `test-rustc`: stage 16's exit, `rustc hello.rs && ./hello` on Ferrix,
//! with a smoke check that the matching Cargo starts too.
//!
//! The compiler is the rust-lang.org release and not one built here: a
//! position-independent glibc program whose LLVM is a 190 MiB shared library
//! of its own, run by Debian's `ld-linux`. It links the way it does on any
//! Linux machine, through `cc` -- Debian's gcc 14 driver -- which runs
//! `collect2`, which runs the `ld.lld` rustc points it at, which runs
//! `rust-lld`. So one compile is five programs nobody here wrote, four
//! `execve`s deep, over some 350 MiB of shared libraries mapped from btrfs;
//! the program it makes is a sixth, run from the shell.
//!
//! # Where the compiler lives
//!
//! On a btrfs volume, `scripts/fetch-rustc-sysroot.sh` makes it from pinned
//! downloads. It carries no `ferrix-root` label, so the kernel mounts it at
//! `/data`, as it does any other btrfs disk; this test attaches it under
//! QEMU's `snapshot=on` so the image is never changed by a run. The volume
//! holds a Debian-shaped tree at its root and the toolchain under `rust/`.
//!
//! glibc's and gcc's own paths are absolute -- `PT_INTERP` names
//! `/lib64/ld-linux-x86-64.so.2`, the linker searches
//! `/lib/x86_64-linux-gnu`, `libc.so` names both, and gcc looks in
//! `/usr/lib/gcc` and `/usr/libexec` -- so the initramfs carries a symbolic
//! link for each into `/data` ([`LINKS`]). `/usr` itself stays the
//! initramfs's, where the ports are.
//!
//! # Why x86-64 only
//!
//! The volume holds x86-64 binaries. An AArch64 sysroot is the same script
//! with other packages, and is not needed for the exit.

use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result, cargo, fat, initramfs, native, ports, qemu, zinc};

/// The script the shell runs. Version checks first, so a failure says whether
/// the toolchain ran at all or only its link failed.
const SCRIPT: &str = r#"export PATH=/data/rust/bin:/data/usr/bin:/bin
cd /tmp
rustc -vV || exit 3
cargo -V || exit 6
echo 'fn main() { println!("rustc-gate: hello from rustc on Ferrix"); }' > hello.rs
rustc hello.rs || exit 4
./hello || exit 5
exit 16
"#;

/// What the program rustc made must print.
const HELLO: &str = "rustc-gate: hello from rustc on Ferrix";

/// What `rustc -vV` prints first, which says the compiler ran.
const VERSION: &str = "rustc 1.97.1";

/// The matching Cargo release must start on the same glibc sysroot.
const CARGO_VERSION: &str = "cargo 1.97.1";

/// The status the script exits with when every step succeeded.
const STATUS: i32 = 16;

/// Each path glibc or gcc names absolutely, and where on the volume it is.
const LINKS: &[(&str, &str)] = &[
    ("lib64", "/data/usr/lib64"),
    ("lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("usr/lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("usr/lib/gcc", "/data/usr/lib/gcc"),
    ("usr/libexec", "/data/usr/libexec"),
];

/// Normal boots also carry ports under `/usr/libexec`, so only gcc's child
/// directory can be linked there. Put the compiler and its C driver on the
/// shell's `/bin` PATH; the gate sets its own PATH instead.
const DEFAULT_LINKS: &[(&str, &str)] = &[
    ("lib64", "/data/usr/lib64"),
    ("lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("usr/lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("usr/lib/gcc", "/data/usr/lib/gcc"),
    ("usr/libexec/gcc", "/data/usr/libexec/gcc"),
    ("bin/rustc", "/data/rust/bin/rustc"),
    ("bin/cargo", "/data/rust/bin/cargo"),
    ("bin/cc", "/data/usr/bin/cc"),
    ("bin/gcc", "/data/usr/bin/gcc"),
];

/// Guest memory unless `--memory` says otherwise: the compiler's libraries
/// alone are 350 MiB, and the page cache holds them all.
const MEMORY: u32 = 4096;

/// Seconds to wait unless `--timeout` says otherwise. Generous for `tcg`,
/// where LLVM runs emulated.
const TIMEOUT: u64 = 1800;

/// Where the volume is, unless `FERRIX_RUSTC_SYSROOT` names another
/// directory: where `scripts/fetch-rustc-sysroot.sh` writes it.
fn volume() -> Result<std::path::PathBuf> {
    let directory = match std::env::var_os("FERRIX_RUSTC_SYSROOT") {
        Some(directory) => std::path::PathBuf::from(directory),
        None => {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .ok_or_else(|| Error::new("neither HOME nor USERPROFILE is set"))?;
            std::path::PathBuf::from(home).join(".local/share/ferrix/rustc")
        }
    };
    let image = directory.join("rustc.img");
    if !image.is_file() {
        return Err(Error::new(format!(
            "{} is not there: scripts/fetch-rustc-sysroot.sh makes it",
            image.display()
        )));
    }
    Ok(image)
}

/// Attach the installed toolchain to a person-facing x86-64 boot. A machine
/// without the optional download still boots, but an explicitly named
/// `FERRIX_RUSTC_SYSROOT` must be valid rather than silently ignored.
pub(crate) fn prepare_default(arch: Arch, args: &mut Args) -> Result<()> {
    if arch != Arch::X86_64 {
        return Ok(());
    }
    let image = match volume() {
        Ok(image) => image,
        Err(error) if std::env::var_os("FERRIX_RUSTC_SYSROOT").is_some() => return Err(error),
        Err(error) => {
            println!("  rustc not in this boot: {error}");
            return Ok(());
        }
    };
    println!("  rustc from {} in the default system", image.display());
    args.data_image = Some(image);
    if !args.memory_given {
        args.memory = MEMORY;
    }
    Ok(())
}

/// Links in the initramfs (and persistent root) for the attached compiler.
pub(crate) fn default_links(args: &Args) -> Vec<ports::File> {
    if args.data_image.is_some() {
        files(DEFAULT_LINKS)
    } else {
        Vec::new()
    }
}

fn files(links: &[(&str, &str)]) -> Vec<ports::File> {
    links
        .iter()
        .map(|(path, target)| ports::File {
            path: (*path).to_owned(),
            mode: 0o777,
            content: ports::Content::Link((*target).to_owned()),
        })
        .collect()
}

/// Boot a shell whose script compiles a program with rustc and runs it.
///
/// # Errors
///
/// When the volume is missing, the image cannot be built, the boot fails, or
/// any step of the script does not do what it must.
pub(crate) fn test_rustc(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "test-rustc runs on x86-64 only: the volume holds x86-64 binaries",
        ));
    }
    let mut args = args.clone();
    args.data_image = Some(volume()?);
    if !args.memory_given {
        args.memory = MEMORY;
    }
    if !args.timeout_given {
        args.timeout = TIMEOUT;
    }

    let shell =
        zinc::built(arch)?.ok_or_else(|| Error::new("zinc could not be built for x86-64"))?;
    println!("  {arch}: building an image whose shell compiles with rustc");
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &shell, SCRIPT)?;
    let natives = native::build(arch, args.release)?;
    let bytes = std::fs::read(&shell)
        .map_err(|error| Error::new(format!("reading {}: {error}", shell.display())))?;
    let links = files(LINKS);
    // zinc alone: the script is builtins, and every program it runs is on
    // the volume.
    let archive = initramfs::build(None, &natives, Some(&bytes), &links)?;
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, None)?;

    println!(
        "  {arch}: compiling hello.rs on Ferrix with {} MiB (timeout {}s)",
        args.memory, args.timeout
    );
    let lines = qemu::watch_then(arch, &image, &kernel, &args, crate::shell::EXITED, |_| {
        Ok(())
    })?;
    judge(arch, &lines)
}

/// Whether the transcript is a compiler and Cargo that ran, a program rustc
/// made that printed its line, and a script that got to its end.
fn judge(arch: Arch, lines: &[String]) -> Result<()> {
    let after_boot = lines
        .iter()
        .position(|line| line.contains(qemu::SUCCESS_MARKER))
        .and_then(|at| lines.get(at..))
        .unwrap_or_default();
    let exited = after_boot
        .iter()
        .find_map(|line| line.trim().strip_prefix(crate::shell::EXITED))
        .map(str::trim);
    let ran = after_boot.iter().any(|line| line.starts_with(VERSION));
    let cargo_ran = after_boot
        .iter()
        .any(|line| line.starts_with(CARGO_VERSION));
    let said = after_boot.iter().any(|line| line.trim_end() == HELLO);
    match exited {
        Some(status) if status == STATUS.to_string() && ran && cargo_ran && said => {
            println!("  {arch}: rustc compiled hello.rs on Ferrix, Cargo started, and it ran");
            Ok(())
        }
        Some("3") => Err(Error::new(format!("{arch}: `rustc -vV` failed"))),
        Some("6") => Err(Error::new(format!("{arch}: `cargo -V` failed"))),
        Some("4") => Err(Error::new(format!(
            "{arch}: rustc ran but did not compile and link hello.rs"
        ))),
        Some("5") => Err(Error::new(format!(
            "{arch}: rustc made a program, and it did not run"
        ))),
        Some(status) => Err(Error::new(format!(
            "{arch}: the script exited with {status}; rustc version {ran}, cargo version {cargo_ran}, hello line {said}"
        ))),
        None => Err(Error::new(format!("{arch}: the shell never exited"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_links_keep_ports_libexec_and_expose_rustc_on_path() {
        let args = Args {
            data_image: Some("rustc.img".into()),
            ..Args::default()
        };
        let links = default_links(&args);
        assert!(links.iter().any(|file| file.path == "bin/rustc"));
        assert!(links.iter().any(|file| file.path == "bin/cargo"));
        assert!(links.iter().any(|file| file.path == "bin/cc"));
        assert!(links.iter().any(|file| file.path == "usr/libexec/gcc"));
        assert!(!links.iter().any(|file| file.path == "usr/libexec"));
        assert!(default_links(&Args::default()).is_empty());
    }

    #[test]
    fn default_toolchain_links_coexist_with_libexec_ports() {
        let args = Args {
            data_image: Some("rustc.img".into()),
            ..Args::default()
        };
        let mut carried = vec![ports::File {
            path: "usr/libexec/git-core/git".to_owned(),
            mode: 0o755,
            content: ports::Content::Bytes(b"git".to_vec()),
        }];
        carried.extend(default_links(&args));
        let bytes = initramfs::build(None, &[], None, &carried).unwrap();
        let archive = ferrix_cpio::Archive::new(&bytes);
        assert_eq!(
            archive
                .find("usr/libexec/git-core/git")
                .unwrap()
                .unwrap()
                .data,
            b"git"
        );
        assert_eq!(
            archive.find("usr/libexec/gcc").unwrap().unwrap().data,
            b"/data/usr/libexec/gcc"
        );
        assert_eq!(
            archive.find("bin/rustc").unwrap().unwrap().data,
            b"/data/rust/bin/rustc"
        );
        assert_eq!(
            archive.find("bin/cargo").unwrap().unwrap().data,
            b"/data/rust/bin/cargo"
        );
    }

    fn transcript(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn a_compile_that_ran_passes() {
        let lines = transcript(&[
            qemu::SUCCESS_MARKER,
            "rustc 1.97.1 (8bab26f4f 2026-07-14)",
            "cargo 1.97.1 (test build)",
            HELLO,
            "  init     the shell exited with 16",
        ]);
        assert!(judge(Arch::X86_64, &lines).is_ok());
    }

    #[test]
    fn a_hello_before_the_marker_does_not_count() {
        let lines = transcript(&[
            HELLO,
            qemu::SUCCESS_MARKER,
            "rustc 1.97.1 (8bab26f4f 2026-07-14)",
            "cargo 1.97.1 (test build)",
            "  init     the shell exited with 16",
        ]);
        assert!(judge(Arch::X86_64, &lines).is_err());
    }

    #[test]
    fn each_failing_step_is_named() {
        for (status, words) in [
            ("3", "-vV"),
            ("4", "link"),
            ("5", "did not run"),
            ("6", "cargo -V"),
        ] {
            let exit = format!("  init     the shell exited with {status}");
            let lines = transcript(&[qemu::SUCCESS_MARKER, &exit]);
            let error = judge(Arch::X86_64, &lines).unwrap_err().to_string();
            assert!(error.contains(words), "{status}: {error}");
        }
    }
}
