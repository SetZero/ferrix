//! `test-uvm`: NVIDIA's UVM running its own tests on Ferrix, with no GPU
//! (`docs/NVIDIA.md` §11.5, C0a).
//!
//! The program is `uvm-selftest`, from
//! `src/user/system/linux/drivers/nvrm/uvm-kpi/`: NVIDIA's unmodified UVM
//! sources, from the tree `FERRIX_NVIDIA_SRC` names (by default
//! `~/.local/share/ferrix/nvidia/580.173.02/src`, where
//! `tools/common/fetch/fetch-nvidia.sh` unpacks it), built against
//! `uvm-kpi` and linked statically against ferrousli. It is booted as init on
//! x86-64. It loads UVM as Linux's module loader would, opens UVM's device,
//! calls `UVM_INITIALIZE`, and runs UVM's fifteen tests that need no GPU
//! through `UVM_RUN_TEST`'s ioctl path, one line each; this requires every
//! one of those lines to say PASS, in order, the summary, and exit 0.
//!
//! Then the negative control: the same program with `uvm-kpi`'s `krealloc`
//! keeping a block it is asked to shrink to nothing must fail
//! `UVM_TEST_KVMALLOC` at UVM's own check, and exit 1. A run that could
//! not fail would pass that build too.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::sem::{boot, says, status};
use crate::{Error, Result};

/// The variable that names NVIDIA's source tree.
const SOURCES_VAR: &str = "FERRIX_NVIDIA_SRC";

/// Where the tree is when the variable is not set, under the home directory.
/// `tools/common/fetch/fetch-nvidia.sh` unpacks it there.
const SOURCES_DEFAULT: &[&str] = &[".local", "share", "ferrix", "nvidia", "580.173.02", "src"];

/// The seconds a boot may take when `--timeout` does not say. Under KVM a
/// run takes about 15 s; under TCG `UVM_TEST_RB_TREE_RANDOM`'s 100,000
/// iterations alone took 90 to 137 s on 2026-10-02, past the default 120.
const TIMEOUT: u64 = 400;

/// The prefix of every line the program prints.
const PREFIX: &str = "uvm-selftest: ";

/// What every run prints first.
const MODULE_INIT: &str = "uvm-selftest: module init 0";

/// The ioctls the program issues, in order, each of which must pass.
/// `UVM_INITIALIZE` and the range groups set up the tests; the rest are
/// UVM's fifteen tests that need no GPU.
const STEPS: &[&str] = &[
    "UVM_INITIALIZE",
    "UVM_TEST_RNG_SANITY",
    "UVM_TEST_RANGE_TREE_DIRECTED",
    "UVM_TEST_LOCK_SANITY",
    "UVM_TEST_PERF_UTILS_SANITY",
    "UVM_TEST_KVMALLOC",
    "UVM_TEST_PERF_EVENTS_SANITY",
    "UVM_TEST_NV_KTHREAD_Q",
    "UVM_TEST_RB_TREE_DIRECTED",
    "UVM_TEST_CPU_CHUNK_API",
    "UVM_TEST_RANGE_ALLOCATOR_SANITY",
    "UVM_TEST_RB_TREE_RANDOM",
    "UVM_CREATE_RANGE_GROUP",
    "UVM_CREATE_RANGE_GROUP",
    "UVM_CREATE_RANGE_GROUP",
    "UVM_CREATE_RANGE_GROUP",
    "UVM_TEST_RANGE_GROUP_TREE",
    "UVM_TEST_THREAD_CONTEXT_SANITY",
    "UVM_TEST_THREAD_CONTEXT_PERF",
    "UVM_TEST_GET_CPU_CHUNK_ALLOC_SIZES",
];

/// The last line of a run that passed.
const ALL: &str = "uvm-selftest: PASS, 0 failed";

/// The test the negative control must fail, UVM's check it must fail at,
/// and its last line.
const NEGATIVE_TEST: &str = "UVM_TEST_KVMALLOC";
const NEGATIVE_CHECK: &str =
    "Test check failed, condition 'uvm_kvrealloc(new_p, 0) == ZERO_SIZE_PTR' not true";
const NEGATIVE_ALL: &str = "uvm-selftest: FAIL, 1 failed";

/// The ioctl a line reports and whether it passed, or `None` for any other
/// line.
fn verdict(line: &str) -> Option<(&str, bool)> {
    let at = line.find(PREFIX)?;
    let mut words = line.get(at + PREFIX.len()..)?.split_whitespace();
    let name = words.next()?;
    match words.next()? {
        "PASS" => Some((name, true)),
        "FAIL" => Some((name, false)),
        _ => None,
    }
}

/// Why the lines of a run are not a full pass, or `None` when they are.
fn judge(lines: &[String]) -> Option<String> {
    if !lines.iter().any(|line| says(line, MODULE_INIT)) {
        return Some("UVM's module init did not return 0".to_owned());
    }
    let reported: Vec<(&str, bool)> = lines.iter().filter_map(|line| verdict(line)).collect();
    if let Some((name, _)) = reported.iter().find(|(_, passed)| !passed) {
        return Some(format!("{name} failed"));
    }
    let names: Vec<&str> = reported.iter().map(|(name, _)| *name).collect();
    if names != STEPS {
        return Some(format!(
            "the program reported {} ioctls ({}), not the {} expected in order",
            names.len(),
            names.join(", "),
            STEPS.len()
        ));
    }
    if !lines.iter().any(|line| says(line, ALL)) {
        return Some(format!("it did not print `{ALL}`"));
    }
    if status(lines) != Some(0) {
        return Some("it did not exit with 0".to_owned());
    }
    None
}

/// Whether the lines of a negative-control run failed where they must:
/// UVM's realloc check, `UVM_TEST_KVMALLOC` and only it, and exit 1.
fn negative_failed_there(lines: &[String]) -> bool {
    let failed: Vec<&str> = lines
        .iter()
        .filter_map(|line| verdict(line))
        .filter(|(_, passed)| !passed)
        .map(|(name, _)| name)
        .collect();
    failed == [NEGATIVE_TEST]
        && lines.iter().any(|line| line.contains(NEGATIVE_CHECK))
        && lines.iter().any(|line| says(line, NEGATIVE_ALL))
        && status(lines) == Some(1)
}

/// NVIDIA's source tree, refused unless UVM's source list is in it.
fn sources() -> Result<PathBuf> {
    let tree = crate::ferrousli::install_root(SOURCES_VAR, SOURCES_DEFAULT, "NVIDIA's sources")?;
    let list = tree
        .join("kernel-open")
        .join("nvidia-uvm")
        .join("nvidia-uvm-sources.Kbuild");
    if !list.is_file() {
        return Err(Error::new(format!(
            "no NVIDIA open-gpu-kernel-modules 580.173.02 tree at {} (no {}); \
             tools/common/fetch/fetch-nvidia.sh fetches it there, or set {SOURCES_VAR}",
            tree.display(),
            list.display()
        )));
    }
    Ok(tree)
}

/// Build `uvm-selftest` and its negative control with `uvm-kpi`'s Makefile
/// and return where the two programs are.
fn build(tree: &Path) -> Result<(PathBuf, PathBuf)> {
    let kpi = paths::workspace_root()
        .join("src")
        .join("user")
        .join("system")
        .join("linux")
        .join("drivers")
        .join("nvrm")
        .join("uvm-kpi");
    let out = paths::target_dir().join("uvm-kpi");
    let ferrousli = paths::target_dir().join("ferrousli");
    let jobs = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    println!(
        "  building uvm-selftest against ferrousli (UVM from {})",
        tree.display()
    );
    let ran = Command::new("make")
        .arg("-s")
        .arg("-C")
        .arg(&kpi)
        .arg(format!("-j{jobs}"))
        .arg(format!("NVSRC={}", tree.display()))
        .arg(format!("OUT={}", out.display()))
        .arg(format!("FERROUSLI_TARGET={}", ferrousli.display()))
        .arg("ferrix")
        .status()
        .map_err(|error| Error::new(format!("running make in {}: {error}", kpi.display())))?;
    if !ran.success() {
        return Err(Error::new(format!(
            "building uvm-selftest failed ({ran}); make -C {} ferrix",
            kpi.display()
        )));
    }
    Ok((
        out.join("uvm-selftest-ferrousli"),
        out.join("uvm-selftest-ferrousli-negative"),
    ))
}

/// The test, on x86-64, the one architecture `uvm-kpi` is built for.
pub(crate) fn test_uvm(args: &Args) -> Result<()> {
    let arches = args.arches()?;
    if arches != [Arch::X86_64] {
        return Err(Error::new(
            "test-uvm runs on x86_64 only: uvm-kpi is built for the x86-64 Linux ABI",
        ));
    }
    let arch = Arch::X86_64;
    let log = paths::build_dir(arch).join("serial.log");
    let tree = sources()?;
    let (program, negative) = build(&tree)?;
    let mut args = args.clone();
    if !args.timeout_given {
        args.timeout = TIMEOUT;
    }
    let args = &args;

    let lines = boot(arch, &program, args)?;
    if let Some(why) = judge(&lines) {
        return Err(Error::new(format!(
            "{arch}: UVM's self-tests did not pass: {why}.\n  Serial output is in {}",
            log.display()
        )));
    }
    println!(
        "  {arch}: UVM loaded, its device opened and initialized, and its {} GPU-free tests \
         passed through UVM_RUN_TEST, exit 0",
        STEPS
            .iter()
            .filter(|name| name.starts_with("UVM_TEST_"))
            .count()
    );

    let lines = boot(arch, &negative, args)?;
    if !negative_failed_there(&lines) {
        return Err(Error::new(format!(
            "{arch}: the negative control should fail {NEGATIVE_TEST} at `{NEGATIVE_CHECK}` \
             alone and exit 1, and did not.\n  Serial output is in {}",
            log.display()
        )));
    }
    println!(
        "  {arch}: the negative control failed {NEGATIVE_TEST} at UVM's realloc check, as it must"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
