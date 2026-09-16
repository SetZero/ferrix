//! Where things are: the workspace, the build outputs, and the firmware QEMU
//! needs.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// A target architecture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Arch {
    /// 64-bit x86.
    X86_64,
    /// 64-bit Arm.
    AArch64,
    /// 32-bit Arm: ARMv7-A with the Large Physical Address Extension.
    Armv7a,
}

impl Arch {
    /// Every architecture, in the order `--arch all` takes them.
    pub(crate) const ALL: [Arch; 3] = [Arch::X86_64, Arch::AArch64, Arch::Armv7a];

    /// Parse the `--arch` value.
    pub(crate) fn parse(name: &str) -> Result<Self> {
        match name {
            "x86_64" | "x86-64" | "amd64" => Ok(Arch::X86_64),
            "aarch64" | "arm64" => Ok(Arch::AArch64),
            "armv7a" | "armv7" | "arm32" | "armhf" => Ok(Arch::Armv7a),
            other => Err(Error::new(format!(
                "unknown architecture `{other}`; expected x86_64, aarch64, armv7a or all"
            ))),
        }
    }

    /// The architecture this build machine is, which is the default because it
    /// is the one whose QEMU can use hardware acceleration.
    pub(crate) fn host() -> Self {
        if cfg!(target_arch = "aarch64") {
            Arch::AArch64
        } else if cfg!(target_arch = "arm") {
            Arch::Armv7a
        } else {
            Arch::X86_64
        }
    }

    /// Short name, used in paths and log lines.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::AArch64 => "aarch64",
            Arch::Armv7a => "armv7a",
        }
    }

    /// The Rust target the freestanding kernel is built for.
    pub(crate) const fn kernel_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-none",
            Arch::AArch64 => "aarch64-unknown-none-softfloat",
            Arch::Armv7a => "armv7a-none-eabi",
        }
    }

    /// The Rust target the loader is built for.
    ///
    /// A UEFI target on the 64-bit pair. There is no 32-bit Arm UEFI target,
    /// and the one used instead is chosen for its position-independent `core`
    /// — `docs/arm32.md` has the experiment — with no libc and no C runtime
    /// linked.
    pub(crate) const fn loader_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-uefi",
            Arch::AArch64 => "aarch64-unknown-uefi",
            Arch::Armv7a => "armv7-unknown-linux-musleabi",
        }
    }

    /// True if the loader rustc produces is an ELF static PIE that has to be
    /// converted to PE32 before firmware can run it.
    pub(crate) const fn loader_is_elf(self) -> bool {
        matches!(self, Arch::Armv7a)
    }

    /// The file name firmware looks for on the EFI system partition when no
    /// boot entry is configured, which is how a removable disk boots.
    pub(crate) const fn removable_boot_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "BOOTX64.EFI",
            Arch::AArch64 => "BOOTAA64.EFI",
            Arch::Armv7a => "BOOTARM.EFI",
        }
    }

    /// The QEMU binary that emulates this architecture.
    pub(crate) const fn qemu_binary(self) -> &'static str {
        match self {
            Arch::X86_64 => "qemu-system-x86_64",
            Arch::AArch64 => "qemu-system-aarch64",
            Arch::Armv7a => "qemu-system-arm",
        }
    }
}

impl std::fmt::Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The workspace root, found from this crate's manifest directory.
pub(crate) fn workspace_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is xtask/, so the workspace is its parent. Resolved
    // at compile time, which means it is right even when xtask is run from
    // somewhere else in the tree.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// Where built images and logs for `arch` go.
pub(crate) fn build_dir(arch: Arch) -> PathBuf {
    workspace_root().join("build").join(arch.name())
}

/// The cargo target directory, honouring `CARGO_TARGET_DIR`.
pub(crate) fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| workspace_root().join("target"), PathBuf::from)
}

/// The firmware QEMU boots an architecture with.
#[derive(Debug)]
pub(crate) enum Firmware {
    /// UEFI in flash: EDK2's read-only code image, and a writable variable
    /// store, which the Arm builds of EDK2 insist on even when nothing is
    /// stored in it.
    Pflash {
        /// The read-only firmware code image.
        code: PathBuf,
        /// A template for the variable store, if one was found.
        vars: Option<PathBuf>,
    },
    /// A firmware image QEMU loads into RAM with `-bios`: U-Boot, on ARMv7-A,
    /// which implements enough of UEFI to run the loader and is what the
    /// boards this architecture targets ship with.
    Bios(PathBuf),
}

/// Find the firmware for `arch`.
///
/// Distributions and the Windows build of QEMU disagree about both the names
/// and the location, so this is a search rather than a path. Two variables
/// override it: `FERRIX_UBOOT` names U-Boot for ARMv7-A, and
/// `FERRIX_OVMF_CODE` and `FERRIX_OVMF_VARS` name an EDK2 image for any
/// architecture — which on ARMv7-A is a second opinion, since EDK2's 32-bit
/// Arm build still exists where it has not yet been dropped.
pub(crate) fn find_firmware(arch: Arch) -> Result<Firmware> {
    if arch == Arch::Armv7a
        && let Some(uboot) = std::env::var_os("FERRIX_UBOOT")
    {
        return Ok(Firmware::Bios(PathBuf::from(uboot)));
    }
    if let Some(code) = std::env::var_os("FERRIX_OVMF_CODE") {
        return Ok(Firmware::Pflash {
            code: PathBuf::from(code),
            vars: std::env::var_os("FERRIX_OVMF_VARS").map(PathBuf::from),
        });
    }

    let (code_names, vars_names): (&[&str], &[&str]) = match arch {
        Arch::X86_64 => (
            &[
                "edk2-x86_64-code.fd",
                "OVMF_CODE.fd",
                "OVMF_CODE_4M.fd",
                "OVMF.fd",
                "ovmf-x86_64-code.bin",
            ],
            &["edk2-i386-vars.fd", "OVMF_VARS.fd", "OVMF_VARS_4M.fd"],
        ),
        Arch::AArch64 => (
            &[
                "edk2-aarch64-code.fd",
                "AAVMF_CODE.fd",
                "QEMU_EFI.fd",
                "QEMU_EFI-pflash.raw",
            ],
            // **Not `edk2-arm-vars.fd`.** That is the 32-bit Arm build's
            // variable store, and QEMU ships it at exactly the same 64 MiB as
            // the AArch64 firmware — so it loads, and EDK2 then finds boot
            // entries belonging to another architecture in it. The visible
            // symptom is firmware announcing "Image type X64 can't be loaded on
            // AARCH64 UEFI system" on the way past. Better to supply no
            // template at all: an unrecognised store is one EDK2 formats.
            &["AAVMF_VARS.fd", "QEMU_VARS.fd", "edk2-aarch64-vars.fd"],
        ),
        Arch::Armv7a => return find_uboot(),
    };

    let roots = firmware_search_path();
    let code = find_in(&roots, code_names).ok_or_else(|| {
        Error::new(format!(
            "could not find UEFI firmware for {arch}.\n  \
             Looked for {code_names:?} under:\n{}\n  \
             Install it (Debian/Ubuntu: `ovmf` and `qemu-efi-aarch64`) or set \
             FERRIX_OVMF_CODE.",
            listing(&roots)
        ))
    })?;

    Ok(Firmware::Pflash {
        code,
        vars: find_in(&roots, vars_names),
    })
}

/// Find U-Boot's build for QEMU's 32-bit Arm `virt` machine.
///
/// Debian and Ubuntu ship it in `u-boot-qemu`; Fedora spells the directory
/// without the hyphen. QEMU's Windows build ships none, so on Windows the same
/// directories are looked for inside WSL's default distribution, where
/// `sudo apt install u-boot-qemu` puts the same file a Linux host would use.
fn find_uboot() -> Result<Firmware> {
    const DIRECTORIES: [&str; 3] = [
        "/usr/lib/u-boot/qemu_arm",
        "/usr/share/u-boot/qemu_arm",
        "/usr/share/uboot/qemu_arm",
    ];
    let roots: Vec<PathBuf> = if cfg!(windows) {
        DIRECTORIES
            .iter()
            .filter_map(|directory| crate::wsl::path(directory))
            .collect()
    } else {
        DIRECTORIES.map(PathBuf::from).to_vec()
    };

    find_in(&roots, &["u-boot.bin"])
        .map(Firmware::Bios)
        .ok_or_else(|| {
            Error::new(format!(
                "could not find U-Boot for QEMU's 32-bit Arm `virt` machine.\n  \
                 Looked for u-boot.bin under:\n{}\n  \
                 Install it (Debian/Ubuntu, or on Windows inside WSL: `u-boot-qemu`) \
                 or set FERRIX_UBOOT.",
                listing(&roots)
            ))
        })
}

/// Directories, one per line, for an error message.
fn listing(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| format!("    {}", root.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Directories that might hold UEFI firmware images, most specific first.
fn firmware_search_path() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    // Next to the QEMU binary, which is how the Windows build ships them.
    if let Some(qemu) = which("qemu-system-x86_64").or_else(|| which("qemu-system-aarch64"))
        && let Some(dir) = qemu.parent()
    {
        roots.push(dir.join("share"));
        roots.push(dir.to_path_buf());
    }

    roots.extend(
        [
            "/usr/share/qemu",
            "/usr/share/OVMF",
            "/usr/share/ovmf",
            "/usr/share/AAVMF",
            "/usr/share/qemu-efi-aarch64",
            "/usr/share/edk2/ovmf",
            "/usr/share/edk2/aarch64",
            "/usr/share/edk2-ovmf",
            "/usr/local/share/qemu",
            "/opt/homebrew/share/qemu",
            "C:/Program Files/qemu/share",
            "C:/Program Files/qemu",
        ]
        .into_iter()
        .map(PathBuf::from),
    );

    roots
}

/// The first `name` that exists under any of `roots`.
fn find_in(roots: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    for root in roots {
        for name in names {
            let candidate = root.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Look up an executable on `PATH`, the way `which` does.
pub(crate) fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let suffixes: &[&str] = if cfg!(windows) { &[".exe", ""] } else { &[""] };

    for dir in std::env::split_paths(&path) {
        for suffix in suffixes {
            let candidate = dir.join(format!("{program}{suffix}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // The Windows installer does not put QEMU on PATH.
    for dir in ["C:/Program Files/qemu", "C:/Program Files (x86)/qemu"] {
        for suffix in suffixes {
            let candidate = PathBuf::from(dir).join(format!("{program}{suffix}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_architecture_spelling() {
        assert_eq!(Arch::parse("x86_64").unwrap(), Arch::X86_64);
        assert_eq!(Arch::parse("amd64").unwrap(), Arch::X86_64);
        assert_eq!(Arch::parse("arm64").unwrap(), Arch::AArch64);
        assert_eq!(Arch::parse("aarch64").unwrap(), Arch::AArch64);
        for spelling in ["armv7a", "armv7", "arm32", "armhf"] {
            assert_eq!(Arch::parse(spelling).unwrap(), Arch::Armv7a, "{spelling}");
        }
        assert!(Arch::parse("riscv64").is_err());
        assert!(
            Arch::parse("arm").is_err(),
            "`arm` alone names too many things to guess which"
        );
    }

    #[test]
    fn targets_and_boot_names_pair_correctly() {
        assert_eq!(Arch::X86_64.kernel_target(), "x86_64-unknown-none");
        assert_eq!(Arch::X86_64.loader_target(), "x86_64-unknown-uefi");
        assert_eq!(Arch::X86_64.removable_boot_name(), "BOOTX64.EFI");
        assert_eq!(
            Arch::AArch64.kernel_target(),
            "aarch64-unknown-none-softfloat"
        );
        assert_eq!(Arch::AArch64.loader_target(), "aarch64-unknown-uefi");
        assert_eq!(Arch::AArch64.removable_boot_name(), "BOOTAA64.EFI");
        assert_eq!(Arch::Armv7a.kernel_target(), "armv7a-none-eabi");
        assert_eq!(Arch::Armv7a.loader_target(), "armv7-unknown-linux-musleabi");
        assert_eq!(Arch::Armv7a.removable_boot_name(), "BOOTARM.EFI");
        assert_eq!(Arch::Armv7a.qemu_binary(), "qemu-system-arm");
    }

    #[test]
    fn only_the_loader_without_a_uefi_target_is_converted() {
        assert!(Arch::Armv7a.loader_is_elf());
        assert!(!Arch::X86_64.loader_is_elf());
        assert!(!Arch::AArch64.loader_is_elf());
        for arch in Arch::ALL {
            assert_eq!(
                arch.loader_is_elf(),
                !arch.loader_target().ends_with("-uefi"),
                "{arch}: a UEFI target already produces a PE"
            );
        }
    }

    #[test]
    fn every_boot_name_is_a_short_name() {
        for arch in Arch::ALL {
            let name = arch.removable_boot_name();
            let (stem, extension) = name.split_once('.').unwrap();
            assert!(stem.len() <= 8 && extension.len() <= 3, "{name}");
        }
    }

    #[test]
    fn workspace_root_holds_the_workspace_manifest() {
        assert!(
            workspace_root().join("Cargo.toml").is_file(),
            "workspace root should be the directory with the virtual manifest"
        );
        assert!(workspace_root().join("kernel").is_dir());
    }
}
