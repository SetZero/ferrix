//! `--arch all`, run as one child a architecture, all at once.
//!
//! The host has 24 threads and a boot uses four, and every `--arch all`
//! loop ran the architectures one after another (`docs/TEST-TIME.md`, cut
//! 2). For the commands in [`COMMANDS`] -- `test-boot`, `test-init` and
//! `test-audio`; see there why not `test-compositor` -- this program starts itself once for
//! each architecture instead, with the same arguments and that one
//! `--arch`, and waits for all of them. Each child writes what it says to
//! `build/<arch>/xtask-<command>.log`; when the last has ended, each log is
//! printed whole, in the architectures' order, so nothing interleaves, and
//! then each architecture's verdict.
//!
//! What they share is made safe to share: the disk images a boot writes are
//! one an architecture (`crate::btrfs_disk`), the read-only ones are written
//! whole and renamed into place, and cargo takes its own lock on the target
//! directory, so the builds queue while the boots run side by side.
//!
//! A child is told it is one by [`CHILD`], and runs its architecture as the
//! command always did. `FERRIX_ARCHES_IN_TURN=1` keeps the old order, for a
//! host where running them at once is not wanted.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::{Error, Result};

/// The commands whose architectures run at once.
///
/// Not `test-compositor`: its boots judge how long a frame took under
/// emulation (5 s on x86-64), and three architectures' compositors
/// emulating side by side on a loaded host pushed x86-64's frames past that
/// (6.7 s, 2026-09-30, while AArch64 and ARMv7-A passed). A budget the
/// host's load decides is already a flake row; running the suites at once
/// would make it fire more often, which is a gate checking less. It joins
/// this list when its frames are judged in the guest's own time.
pub(crate) const COMMANDS: &[&str] = &["test-boot", "test-init", "test-audio"];

/// Set in a child's environment: it runs one architecture, and must not
/// start children of its own.
const CHILD: &str = "FERRIX_XTASK_ONE_ARCH";

/// Set by a person who wants the architectures one after another.
const IN_TURN: &str = "FERRIX_ARCHES_IN_TURN";

/// Whether `command` with `args` is run as a child an architecture.
pub(crate) fn wanted(command: &str, args: &Args) -> bool {
    COMMANDS.contains(&command)
        && std::env::var_os(CHILD).is_none()
        && std::env::var_os(IN_TURN).is_none()
        && !args.gdb
        && args.arches().is_ok_and(|arches| arches.len() > 1)
}

/// One child: its architecture, where it writes, and the process.
struct Running {
    arch: Arch,
    log: PathBuf,
    child: Child,
    began: Instant,
}

/// Run `command` once an architecture, all at once, and report each.
///
/// # Errors
///
/// When a child could not be started, or when any architecture failed:
/// every one is waited for and printed first, so one failure hides none of
/// the others.
pub(crate) fn run(command: &str, args: &Args) -> Result<()> {
    let arches = args.arches()?;
    let program = std::env::current_exe()?;
    let given: Vec<String> = std::env::args().skip(1).collect();
    println!(
        "  {command}: {} at once; each one's output follows when all have ended \
         ({IN_TURN}=1 runs them in turn)",
        arches
            .iter()
            .map(|arch| arch.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut running = Vec::new();
    for arch in arches {
        let directory = paths::build_dir(arch);
        std::fs::create_dir_all(&directory)?;
        let log = directory.join(format!("xtask-{command}.log"));
        let out = std::fs::File::create(&log)?;
        let err = out.try_clone()?;
        let child = Command::new(&program)
            .args(with_arch(&given, arch))
            .env(CHILD, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|error| Error::new(format!("starting {command} for {arch}: {error}")))?;
        running.push(Running {
            arch,
            log,
            child,
            began: Instant::now(),
        });
    }

    // Said as each ends, so a person watching knows which are still going.
    let mut ended: Vec<Option<(bool, Duration)>> = vec![None; running.len()];
    while ended.iter().any(Option::is_none) {
        for (index, one) in running.iter_mut().enumerate() {
            if ended.get(index).is_some_and(Option::is_some) {
                continue;
            }
            if let Some(status) = one.child.try_wait()? {
                let took = one.began.elapsed();
                println!(
                    "  {command}: {} {} after {:.0} s",
                    one.arch,
                    if status.success() { "passed" } else { "failed" },
                    took.as_secs_f64()
                );
                if let Some(slot) = ended.get_mut(index) {
                    *slot = Some((status.success(), took));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let mut failed = Vec::new();
    for (one, outcome) in running.iter().zip(&ended) {
        println!(
            "\n==== {command} --arch {} ({})",
            one.arch,
            one.log.display()
        );
        let said = std::fs::read_to_string(&one.log).unwrap_or_default();
        print!("{said}");
        if outcome.is_some_and(|(passed, _)| !passed) {
            let why = said
                .lines()
                .rev()
                .find(|line| line.starts_with("xtask: "))
                .unwrap_or("xtask: it said nothing about why")
                .trim_start_matches("xtask: ")
                .to_owned();
            failed.push(format!("{}: {why}", one.arch));
        }
    }
    println!();
    for (one, outcome) in running.iter().zip(&ended) {
        if let Some((passed, took)) = outcome {
            println!(
                "  {command}: {} {} in {:.0} s",
                one.arch,
                if *passed { "passed" } else { "failed" },
                took.as_secs_f64()
            );
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "{command} failed on {} of the architectures:\n  {}",
            failed.len(),
            failed.join("\n  ")
        )))
    }
}

/// `given` with its `--arch` naming `arch` alone.
fn with_arch(given: &[String], arch: Arch) -> Vec<String> {
    let mut out = Vec::with_capacity(given.len() + 2);
    let mut items = given.iter();
    let mut named = false;
    while let Some(item) = items.next() {
        if item == "--arch" {
            let _ = items.next();
            out.push(item.clone());
            out.push(arch.name().to_owned());
            named = true;
        } else {
            out.push(item.clone());
        }
    }
    if !named {
        out.push("--arch".to_owned());
        out.push(arch.name().to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The child's arguments are the parent's with one architecture named.
    #[test]
    fn a_child_is_given_its_one_architecture() {
        let given: Vec<String> = ["test-boot", "--arch", "all", "--timeout", "600"]
            .iter()
            .map(|item| (*item).to_owned())
            .collect();
        assert_eq!(
            with_arch(&given, Arch::AArch64),
            ["test-boot", "--arch", "aarch64", "--timeout", "600"]
        );
        let bare: Vec<String> = vec!["test-init".to_owned()];
        assert_eq!(
            with_arch(&bare, Arch::Armv7a),
            ["test-init", "--arch", "armv7a"]
        );
    }
}
