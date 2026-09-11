//! Where things are: the workspace, the build outputs, and the UEFI firmware
//! QEMU needs.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// A target architecture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Arch {
    /// 64-bit x86.
    X86_64,
    /// 64-bit Arm.
    AArch64,
}

impl Arch {
    /// Parse the `--arch` value.
    pub(crate) fn parse(name: &str) -> Result<Self> {
        match name {
            "x86_64" | "x86-64" | "amd64" => Ok(Arch::X86_64),
            "aarch64" | "arm64" => Ok(Arch::AArch64),
            other => Err(Error::new(format!(
                "unknown architecture `{other}`; expected x86_64, aarch64 or both"
            ))),
        }
    }

    /// The architecture this build machine is, which is the default because it
    /// is the one whose QEMU can use hardware acceleration.
    pub(crate) fn host() -> Self {
        if cfg!(target_arch = "aarch64") {
            Arch::AArch64
        } else {
            Arch::X86_64
        }
    }

    /// Short name, used in paths and log lines.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::AArch64 => "aarch64",
        }
    }

    /// The Rust target the freestanding kernel is built for.
    pub(crate) const fn kernel_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-none",
            Arch::AArch64 => "aarch64-unknown-none-softfloat",
        }
    }

    /// The Rust target the UEFI loader is built for.
    pub(crate) const fn loader_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-uefi",
            Arch::AArch64 => "aarch64-unknown-uefi",
        }
    }

    /// The file name firmware looks for on the EFI system partition when no
    /// boot entry is configured, which is how a removable disk boots.
    pub(crate) const fn removable_boot_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "BOOTX64.EFI",
            Arch::AArch64 => "BOOTAA64.EFI",
        }
    }

    /// The QEMU binary that emulates this architecture.
    pub(crate) const fn qemu_binary(self) -> &'static str {
        match self {
            Arch::X86_64 => "qemu-system-x86_64",
            Arch::AArch64 => "qemu-system-aarch64",
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

/// The UEFI firmware images QEMU needs for one architecture.
#[derive(Debug)]
pub(crate) struct Firmware {
    /// The read-only firmware code image.
    pub(crate) code: PathBuf,
    /// A writable variable store, which AArch64's build of EDK2 insists on
    /// even when nothing is stored in it.
    pub(crate) vars: Option<PathBuf>,
}

/// Find the UEFI firmware for `arch`.
///
/// Distributions and the Windows build of QEMU disagree about both the names
/// and the location, so this is a search rather than a path. `FERRIX_OVMF_CODE`
/// and `FERRIX_OVMF_VARS` override it, which is what a machine with firmware in
/// an unusual place should set.
pub(crate) fn find_firmware(arch: Arch) -> Result<Firmware> {
    if let Some(code) = std::env::var_os("FERRIX_OVMF_CODE") {
        return Ok(Firmware {
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
    };

    let roots = firmware_search_path();
    let code = find_in(&roots, code_names).ok_or_else(|| {
        Error::new(format!(
            "could not find UEFI firmware for {arch}.\n  \
             Looked for {code_names:?} under:\n{}\n  \
             Install it (Debian/Ubuntu: `ovmf` and `qemu-efi-aarch64`) or set \
             FERRIX_OVMF_CODE.",
            roots
                .iter()
                .map(|root| format!("    {}", root.display()))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    })?;

    Ok(Firmware {
        code,
        vars: find_in(&roots, vars_names),
    })
}

/// Directories that might hold firmware images, most specific first.
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
        assert!(Arch::parse("riscv64").is_err());
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
