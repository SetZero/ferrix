//! The Linux a Windows host already has: WSL.
//!
//! Two things this tool needs are Linux and nothing else. ferrousli's tests
//! compile C programs against the library and run them, and what they build
//! is a Linux executable that only a Linux kernel can start. And ARMv7-A boots
//! through U-Boot, which QEMU's Windows build does not ship and distributions
//! package for Linux. Everything else — the kernel, the loaders, QEMU, the
//! gateway — builds and runs natively.
//!
//! So on Windows those two go through WSL's default distribution: the one
//! `wsl.exe` starts with no `-d`, which is also the one a developer set up.
//! Only that one is looked at. Opening a path into a distribution starts it,
//! and a machine with Docker Desktop or Podman has distributions of theirs
//! that nobody wants booted to look for a firmware file.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{Error, Result};

/// Where Windows keeps the WSL distributions, for the user running this.
const LXSS: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss";

/// The default distribution's name, if WSL has one.
///
/// Read from the registry with `reg`, which every Windows has, rather than
/// from `wsl.exe --list`, whose output is UTF-16 on some versions and UTF-8
/// on others and translated into the display language on all of them.
pub(crate) fn default_distribution() -> Option<String> {
    let default = reg_value(LXSS, "DefaultDistribution")?;
    reg_value(&format!(r"{LXSS}\{default}"), "DistributionName")
}

/// One string value from `reg query`, whose line for it reads
/// `    NAME    REG_SZ    VALUE` in every display language.
fn reg_value(key: &str, name: &str) -> Option<String> {
    let output = Command::new("reg")
        .args(["query", key, "/v", name])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some(name) && fields.next() == Some("REG_SZ"))
                .then(|| fields.collect::<Vec<_>>().join(" "))
        })
        .filter(|value| !value.is_empty())
}

/// A path inside the default distribution, as Windows opens it:
/// `\\wsl.localhost\<name>\<path>`. `None` without WSL.
pub(crate) fn path(linux: &str) -> Option<PathBuf> {
    let name = default_distribution()?;
    let relative = linux.trim_start_matches('/').replace('/', r"\");
    Some(PathBuf::from(format!(r"\\wsl.localhost\{name}\{relative}")))
}

/// `cargo` with `arguments`, run in the default distribution in `dir`.
///
/// The target directory is on the distribution's own filesystem, under
/// `~/.cache/ferrix/`, rather than the checkout's `target/`: a Windows build
/// already lives there, cargo's fingerprints for one host mean nothing to the
/// other, and a build over WSL's view of an NTFS drive is several times slower
/// than one on ext4. Each checkout gets its own, named after its path, so two
/// worktrees never wait on each other's lock or rebuild each other's crates.
///
/// Through a login shell, so that `~/.cargo/env` has put `cargo` on `PATH`,
/// and started with `--exec`: after a plain `--`, `wsl.exe` joins the words
/// back into one line for the user's own shell to split again, and a script
/// and its `"$@"` do not survive that.
pub(crate) fn cargo(dir: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new("wsl.exe");
    let _ = command
        .arg("--cd")
        .arg(dir)
        .args(["--exec", "bash", "-lc"])
        .arg(format!(
            "export CARGO_TARGET_DIR=\"$HOME/.cache/ferrix/target/{}\"; exec cargo \"$@\"",
            target_name(dir)
        ))
        // `$0` for the script, and then the arguments as `$@`, so that none of
        // them is ever parsed by the shell.
        .arg("cargo")
        .args(arguments);
    command
}

/// `script` run by bash in the default distribution in `dir`, with
/// `CARGO_TARGET_DIR` set as [`cargo`] sets it and `arguments` as `"$@"`.
pub(crate) fn bash(dir: &Path, script: &str, arguments: &[&str]) -> Command {
    let mut command = Command::new("wsl.exe");
    let _ = command
        .arg("--cd")
        .arg(dir)
        .args(["--exec", "bash", "-lc"])
        .arg(format!(
            "export CARGO_TARGET_DIR=\"$HOME/.cache/ferrix/target/{}\"; {script}",
            target_name(dir)
        ))
        .arg("bash")
        .args(arguments);
    command
}

/// A directory name for `dir`'s build: its path with everything but letters,
/// digits and hyphens made a hyphen, so it is one path component on Linux and
/// the same on every run.
fn target_name(dir: &Path) -> String {
    dir.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

/// Refuse early, and say what to install, if the default distribution cannot
/// build what `need` describes: no WSL, or no `cargo` or `cc` in it.
///
/// `need` is the half-sentence before "which on Windows needs WSL", because
/// three gates want this and they want it for different reasons: ferrousli
/// builds C programs a Linux kernel has to start, zinc drives a
/// pseudoterminal, and the compositor opens render nodes and Unix sockets
/// that Windows has no equivalent of. A person without a distribution should
/// be told which of the three they are being stopped by.
pub(crate) fn require_toolchain(need: &str) -> Result<()> {
    let Some(name) = default_distribution() else {
        return Err(Error::new(format!(
            "{need}, which on Windows needs WSL, and WSL has no default distribution here.\n  \
             Install one: `wsl --install -d Ubuntu`, then inside it rustup and `build-essential`."
        )));
    };
    let status = Command::new("wsl.exe")
        .args(["--exec", "bash", "-lc", "command -v cargo && command -v cc"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| Error::new(format!("could not run wsl.exe: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "WSL's default distribution, {name}, has no `cargo` or no `cc`.\n  \
             Inside it: install rustup (https://rustup.rs) and `sudo apt install build-essential`."
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::target_name;

    #[test]
    fn a_target_name_is_one_stable_path_component() {
        assert_eq!(
            target_name(Path::new(
                r"F:\Dokumente\projekte\os\.claude\worktrees\a b\ferrousli"
            )),
            "F--Dokumente-projekte-os--claude-worktrees-a-b-ferrousli"
        );
    }
}
