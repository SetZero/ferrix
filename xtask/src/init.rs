//! `/sbin/init` for the image, and `test-init` (`docs/INIT.md` §15).
//!
//! # Building it
//!
//! `init/` is a workspace of its own, built as zinc is: a static program
//! against the target's own musl, linked by rust-lld, so any host builds it.
//! [`carried`] returns what an image carries for it: `/sbin/init`,
//! `/sbin/getty`, the getty generator in `/lib/ferrix/generators`, and the
//! units of `init/units` in `/lib/ferrix/units`, with `default.target` a link
//! to `multi-user.target`.
//!
//! Only images that name `/sbin/init` carry it. With nothing named and
//! nothing built in, the kernel starts `/sbin/init` (§8.1), so an image with
//! no program of its own -- `test-boot`'s, `test-btrfs`'s -- would boot into
//! a manager that never ends, where today it ends at the marker.
//!
//! # The test
//!
//! One boot per architecture, of a kernel with no program in it,
//! `ferrix.init=/sbin/init` on the command line, zinc at `/bin/sh`, the
//! test's own units in `/etc/ferrix/units`, and a fresh blank btrfs volume at
//! `/data`. It types at the console the getty gives, as `test-jobs` does, and
//! everything it judges is either the manager's own lines or a line the
//! guest printed in answer to what was typed. A marker is built from a shell
//! variable (`echo "$m-self"`), because the console echoes the typed line:
//! the echo holds `$m-self`, and only the answer holds `stat-self`.
//!
//! **Stage one, boot and a terminal.** `multi-user.target` becomes active
//! and the getty on the console prints its banner. The shell reads its own
//! `/proc/self/stat`: it must be pid 1's child, lead its own session and
//! process group, and have the console (5:1) as its controlling terminal
//! with its own group in the foreground -- read from the kernel, not taken
//! from what was typed. `flaky.service` exits 1 each time it is started; it
//! must be restarted by `Restart=on-failure` and then reported
//! `failed (start-limit-hit)` once its budget of three is spent. Then
//! `kill -TERM 1` from the prompt: every unit stops, `multi-user.target`
//! before `getty@console.service` before `basic.target` before
//! `sysinit.target`, and the machine powers off through `reboot(2)`. `btrfs
//! check` of the volume must then find it clean.
//!
//! **Stage two, groups.** `forker.service` is a oneshot that remains after
//! its main process exits, having left a grandchild behind that writes its
//! own pid to a file and blocks for good. That pid must be in the service's
//! `cgroup.procs`. It is `BindsTo=anchor.service`, whose main process writes
//! its pid too; killing that process from the prompt makes the manager stop
//! `forker.service`, and stopping it must end the grandchild. The negative
//! control is `KillMode=process` on `forker.service`, which leaves the
//! grandchild alive and fails the last check.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::btrfs_check::Checker;
use crate::paths::{self, Arch};
use crate::ports::{Content, File};
use crate::qemu::{self, SUCCESS_MARKER, Watching};
use crate::{Error, Result, btrfs_disk, cargo, fat, initramfs, native, zinc};

/// Where the init is, and what `ferrix.init=` names.
pub(crate) const PATH: &str = "/sbin/init";

/// The shipped units, beside this module in the tree.
const UNITS: &str = "init/units";

/// How long to wait for the answer to one line typed at the prompt.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long to leave the shell before its first keystroke, and between the
/// checks that retry.
const SETTLE: Duration = Duration::from_millis(1500);

/// How many times the grandchild's end is looked for, [`SETTLE`] apart.
const GONE_TRIES: usize = 20;

/// The volume the boot writes and `btrfs check` reads, under `build/`.
const VOLUME: &str = "init-data.img";

/// What the getty prints once it holds the terminal: `Ferrix <host> on
/// /dev/console`.
const BANNER: &str = " on /dev/console";

/// What the kernel says as `reboot(2)` powers off, Linux's line.
const POWER_DOWN: &str = "reboot: Power down";

/// The console's device number as `stat`'s `tty_nr` encodes it: 5:1.
const CONSOLE_TTY_NR: u32 = 5 << 8 | 1;

/// The units shutdown must stop, in this order (§15, stage one).
const STOP_ORDER: [&str; 4] = [
    "multi-user.target: stopped",
    "getty@console.service: stopped",
    "basic.target: stopped",
    "sysinit.target: stopped",
];

/// The test's own units, in `/etc/ferrix/units`.
const TEST_UNITS: &[(&str, &str)] = &[
    (
        "flaky.service",
        "[Unit]\n\
         Description=Fails every time, for its restart budget\n\
         StartLimitBurst=3\n\
         StartLimitIntervalSec=60s\n\
         \n\
         [Service]\n\
         ExecStart=/bin/sh -c \"exit 1\"\n\
         Restart=on-failure\n\
         RestartSec=100ms\n",
    ),
    (
        "anchor.service",
        "[Unit]\n\
         Description=Blocks for good, and says its pid\n\
         \n\
         [Service]\n\
         ExecStart=:/bin/sh -c 'echo $$ > /run/anchor.pid; read x < /dev/ptmx'\n",
    ),
    (
        "forker.service",
        "[Unit]\n\
         Description=Leaves a grandchild behind, bound to anchor.service\n\
         BindsTo=anchor.service\n\
         After=anchor.service\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         ExecStart=:/bin/sh -c \"(sh -c 'echo $$ > /run/forker.pid; read x < /dev/ptmx' &)\"\n",
    ),
    (
        "getty@.service.d/test.conf",
        "[Service]\n\
         Environment=TERM=dumb \"PS1=init-test%%# \"\n",
    ),
];

/// The units the test's `multi-user.target` wants besides the getty.
const WANTED: [&str; 3] = ["flaky.service", "anchor.service", "forker.service"];

/// The three programs of `init/` for one architecture.
#[derive(Debug)]
pub(crate) struct Built {
    init: PathBuf,
    getty: PathBuf,
    generator: PathBuf,
}

/// Build `init/` for `arch`, or `None` on an architecture it is not built
/// for yet.
pub(crate) fn built(arch: Arch) -> Result<Option<Built>> {
    let Some(target) = zinc::target(arch) else {
        println!("  init is not built for {} yet", arch.name());
        return Ok(None);
    };
    println!("  building init for {target}");
    let target_dir = paths::target_dir().join("init");
    let release = target_dir.join(target).join("release");
    let built = Built {
        init: release.join("init"),
        getty: release.join("getty"),
        generator: release.join("getty-generator"),
    };
    crate::builds::Build::cargo(
        format!("cargo build (init) --target {target}"),
        paths::workspace_root().join("init"),
    )
    .args(["build", "--release", "--target", target])
    .env("CARGO_TARGET_DIR", &target_dir)
    // As for zinc: RUSTFLAGS replaces the flags every config file up the
    // tree would merge, the root's ARM linker script among them.
    .env("RUSTFLAGS", zinc::RUSTFLAGS)
    .output(&built.init)
    .output(&built.getty)
    .output(&built.generator)
    .run()?;
    Ok(Some(built))
}

/// A file's bytes, or an error naming it.
fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
}

/// What an image carries for init on `arch`: the programs and the shipped
/// units. Empty on an architecture init is not built for.
pub(crate) fn carried(arch: Arch) -> Result<Vec<File>> {
    let Some(built) = built(arch)? else {
        return Ok(Vec::new());
    };
    let program = |path: &str, from: &Path| -> Result<File> {
        Ok(File {
            path: path.to_owned(),
            mode: 0o755,
            content: Content::Bytes(read(from)?),
        })
    };
    let mut files = vec![
        program("sbin/init", &built.init)?,
        program("sbin/getty", &built.getty)?,
        program("lib/ferrix/generators/getty-generator", &built.generator)?,
    ];
    let units = paths::workspace_root().join(UNITS);
    let mut names: Vec<PathBuf> = std::fs::read_dir(&units)
        .map_err(|error| Error::new(format!("reading {}: {error}", units.display())))?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    names.sort();
    for path in names {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        files.push(File {
            path: format!("lib/ferrix/units/{name}"),
            mode: 0o644,
            content: Content::Bytes(read(&path)?),
        });
    }
    files.push(File {
        path: "lib/ferrix/units/default.target".to_owned(),
        mode: 0o777,
        content: Content::Link("multi-user.target".to_owned()),
    });
    Ok(files)
}

/// The test's own units and zinc, as carried files.
fn test_files(shell: &[u8]) -> Vec<File> {
    let mut files: Vec<File> = ["bin/sh", "bin/zinc"]
        .into_iter()
        .map(|path| File {
            path: path.to_owned(),
            mode: 0o755,
            content: Content::Bytes(shell.to_vec()),
        })
        .collect();
    for (name, text) in TEST_UNITS {
        files.push(File {
            path: format!("etc/ferrix/units/{name}"),
            mode: 0o644,
            content: Content::Bytes(text.as_bytes().to_vec()),
        });
    }
    for name in WANTED {
        files.push(File {
            path: format!("etc/ferrix/units/multi-user.target.wants/{name}"),
            mode: 0o777,
            content: Content::Link(format!("/etc/ferrix/units/{name}")),
        });
    }
    files
}

/// `cargo xtask test-init`.
///
/// # Errors
///
/// When an image cannot be built, a boot fails, or anything §15's stages
/// one and two require is missing, with the serial log's path.
pub(crate) fn test_init(args: &Args) -> Result<()> {
    let checker = Checker::required()?;
    for arch in args.arches()? {
        test_arch(arch, args, &checker)?;
    }
    Ok(())
}

/// One architecture's boot.
fn test_arch(arch: Arch, args: &Args, checker: &Checker) -> Result<()> {
    let shell = zinc::built(arch)?
        .ok_or_else(|| Error::new(format!("zinc could not be built for {arch}")))?;
    let shell = read(&shell)?;
    let mut files = carried(arch)?;
    if files.is_empty() {
        return Err(Error::new(format!("init is not built for {arch}")));
    }
    files.extend(test_files(&shell));
    println!("  {arch}: building an image whose init is {PATH}");
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel(arch, args.release)?;
    let natives = native::build(arch, args.release)?;
    let archive = initramfs::build(None, &natives, None, &files)?;
    let options = format!("{}\n", qemu::init_option(PATH));
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, Some(&options))?;

    let volume = btrfs_disk::blank_copy(VOLUME)?;
    let mut with_volume = args.clone();
    with_volume.data_image = Some(volume.clone());
    with_volume.data_image_kept = true;

    println!(
        "  {arch}: typing at the console the getty gives (timeout {}s)",
        args.timeout
    );
    let mut failures: Vec<String> = Vec::new();
    let lines = qemu::watch_then(arch, &image, &kernel, &with_volume, SUCCESS_MARKER, |at| {
        session(at, &mut failures)
    })?;
    let after = after_marker(&lines);
    failures.extend(judge_boot(after).err());
    failures.extend(judge_units(after).err());
    failures.extend(judge_flaky(after).err());
    failures.extend(judge_shutdown(after).err());
    if !failures.is_empty() {
        let mut message = format!("{arch}: init did not do what §15 requires:\n");
        for failure in &failures {
            message.push_str("    - ");
            message.push_str(failure);
            message.push('\n');
        }
        message.push_str(&format!(
            "  Serial output is in {}",
            paths::build_dir(arch).join("serial.log").display()
        ));
        return Err(Error::new(message));
    }
    checker.run(&volume, arch)?;
    println!(
        "  {arch}: init booted multi-user.target, gave the console a session, spent a failing \
         service's budget, ended a service's grandchild with its cgroup, and powered off clean"
    );
    Ok(())
}

/// What is typed, and what it is judged by. Every failure goes into
/// `failures`, so one run reports everything that was wrong.
fn session(at: &mut Watching<'_>, failures: &mut Vec<String>) -> Result<()> {
    // The getty's banner, and the target it is part of: the prompt follows.
    let deadline = Instant::now() + PATIENCE * 2;
    let up = at.read_more(deadline, |lines| {
        has(lines, "multi-user.target: active") && has(lines, BANNER)
    })?;
    if !up {
        failures.push("multi-user.target never became active with a getty on the console".into());
        return Ok(());
    }
    thread::sleep(SETTLE);

    // Stage one: the shell's own session, read from the kernel.
    match ask(
        at,
        "m=stat; read s < /proc/self/stat; echo \"$m-self $s\"\n",
        "stat-self ",
    )? {
        Some(line) => failures.extend(judge_stat(&line).err()),
        None => failures.push("the shell never printed its /proc/self/stat".into()),
    }

    // The getty's drop-in reached the shell: a template's drop-ins apply to
    // its instances.
    match ask(at, "m=env; echo \"$m-term $TERM\"\n", "env-term ")? {
        Some(line) if line.trim() == "env-term dumb" => {}
        Some(line) => failures.push(format!(
            "getty@.service.d/test.conf did not reach the shell: `{}`",
            line.trim()
        )),
        None => failures.push("the shell never said its TERM".into()),
    }

    // Stage two: the grandchild is in its service's cgroup, and goes with it.
    let pid = ask(
        at,
        "m=grp; read g < /run/forker.pid; echo \"$m-pid $g\"\n",
        "grp-pid ",
    )?
    .and_then(|line| line.trim().strip_prefix("grp-pid ").map(str::to_owned))
    .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
    let Some(pid) = pid else {
        failures.push("forker.service's grandchild never wrote its pid to /run/forker.pid".into());
        return power_off(at, failures);
    };
    let listed = ask(
        at,
        "while read p; do [ \"$p\" = \"$g\" ] && echo \"$m-in $p\"; done \
         < /sys/fs/cgroup/system.slice/forker.service/cgroup.procs; echo \"$m-listed\"\n",
        "grp-listed",
    )?;
    let member = format!("grp-in {pid}");
    if listed.is_none() || !has(at.after(), &member) {
        failures.push(format!(
            "the grandchild {pid} is not in forker.service's cgroup.procs"
        ));
    }
    if ask(
        at,
        "read a < /run/anchor.pid; kill $a\n",
        "forker.service: stopped",
    )?
    .is_none()
    {
        failures.push(
            "killing anchor.service's main process did not stop forker.service, which is \
             BindsTo= it"
                .into(),
        );
    } else {
        let mut gone = false;
        for _ in 0..GONE_TRIES {
            let check = "[ -d /proc/$g ] && echo \"$m-alive\" || echo \"$m-gone\"\n";
            let before = at.after().len();
            at.type_in(check.as_bytes())?;
            let deadline = Instant::now() + PATIENCE;
            let _ = at.read_more(deadline, |lines| {
                lines
                    .get(before..)
                    .unwrap_or_default()
                    .iter()
                    .any(|line| matches!(line.trim(), "grp-alive" | "grp-gone"))
            })?;
            if at
                .after()
                .iter()
                .skip(before)
                .any(|line| line.trim() == "grp-gone")
            {
                gone = true;
                break;
            }
            thread::sleep(SETTLE);
        }
        if !gone {
            failures.push(format!(
                "stopping forker.service left its grandchild {pid} alive: its cgroup was not \
                 emptied"
            ));
        }
    }
    power_off(at, failures)
}

/// Write to `/data`, so `btrfs check` reads a volume that was written, then
/// `kill -TERM 1` and wait for the power to go.
fn power_off(at: &mut Watching<'_>, failures: &mut Vec<String>) -> Result<()> {
    if ask(
        at,
        "m=data; echo \"$m kept\" > /data/init-test; echo \"$m-written\"\n",
        "data-written",
    )?
    .is_none()
    {
        failures.push("the shell could not write /data/init-test".into());
    }
    at.type_in(b"kill -TERM 1\n")?;
    let deadline = Instant::now() + PATIENCE * 4;
    let _ = at.read_more(deadline, |lines| has(lines, POWER_DOWN))?;
    Ok(())
}

/// Type `keys` and wait for a line starting with `answer` after them;
/// return that line.
fn ask(at: &mut Watching<'_>, keys: &str, answer: &str) -> Result<Option<String>> {
    let before = at.after().len();
    at.type_in(keys.as_bytes())?;
    let deadline = Instant::now() + PATIENCE;
    let found = |lines: &[String]| {
        lines
            .get(before..)
            .unwrap_or_default()
            .iter()
            .find(|line| starts(line, answer))
            .cloned()
    };
    let _ = at.read_more(deadline, |lines| found(lines).is_some())?;
    Ok(found(at.after()))
}

/// Whether `line` answers with `answer`: the guest's own line starting with
/// it, or one of init's lines saying it. Init's line may follow a prompt on
/// the same line, since the console interleaves init's output with the
/// shell's.
fn starts(line: &str, answer: &str) -> bool {
    line.trim_start().starts_with(answer) || line.contains(&format!("init     {answer}"))
}

/// Whether any line holds `text`.
fn has(lines: &[String], text: &str) -> bool {
    lines.iter().any(|line| line.contains(text))
}

/// The lines from the boot marker on.
fn after_marker(lines: &[String]) -> &[String] {
    lines
        .iter()
        .position(|line| line.contains(SUCCESS_MARKER))
        .and_then(|at| lines.get(at..))
        .unwrap_or_default()
}

/// The kernel started init from the file, and init booted.
fn judge_boot(after: &[String]) -> std::result::Result<(), String> {
    let started = format!("init     starting {PATH}");
    if !after.iter().any(|line| line.trim() == started) {
        return Err(format!("the kernel never said `{started}`"));
    }
    if !has(after, "init     booting ") {
        return Err("init never said which target it was booting".into());
    }
    Ok(())
}

/// No unit file, shipped or the test's, drew a warning as it loaded.
fn judge_units(after: &[String]) -> std::result::Result<(), String> {
    let warned: Vec<&str> = after
        .iter()
        .map(|line| line.trim())
        .filter(|line| {
            [
                "/lib/ferrix/units/",
                "/etc/ferrix/units/",
                "/run/ferrix/units/",
            ]
            .iter()
            .any(|directory| line.starts_with(&format!("init     {directory}")))
        })
        .collect();
    if warned.is_empty() {
        Ok(())
    } else {
        Err(format!("unit files drew warnings: {warned:?}"))
    }
}

/// The shell's `/proc/self/stat`, as `stat-self <the file>`: its session
/// and controlling terminal are its own.
fn judge_stat(line: &str) -> std::result::Result<(), String> {
    let text = line.trim().strip_prefix("stat-self ").unwrap_or_default();
    // The fields after `comm`, which may hold spaces, start after its `)`.
    let (Some(pid), Some(rest)) = (
        text.split_whitespace().next(),
        text.rsplit_once(')').map(|(_, rest)| rest),
    ) else {
        return Err(format!(
            "the shell's /proc/self/stat did not parse: `{text}`"
        ));
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let number = |at: usize| fields.get(at).and_then(|field| field.parse::<i64>().ok());
    let Ok(pid) = pid.parse::<i64>() else {
        return Err(format!(
            "the shell's /proc/self/stat did not parse: `{text}`"
        ));
    };
    // state ppid pgrp session tty_nr tpgid, after `comm`.
    let (ppid, pgrp, session, tty, foreground) =
        (number(1), number(2), number(3), number(4), number(5));
    let mut wrong = Vec::new();
    if pid == 1 {
        wrong.push("the shell is pid 1, so init did not start it".to_owned());
    }
    if ppid != Some(1) {
        wrong.push(format!("its parent is {ppid:?}, not init"));
    }
    if session != Some(pid) {
        wrong.push(format!("its session is {session:?}, not its own ({pid})"));
    }
    if pgrp != Some(pid) {
        wrong.push(format!("its process group is {pgrp:?}, not its own"));
    }
    if tty != Some(i64::from(CONSOLE_TTY_NR)) {
        wrong.push(format!(
            "its controlling terminal is {tty:?}, not the console ({CONSOLE_TTY_NR})"
        ));
    }
    if foreground != Some(pid) {
        wrong.push(format!(
            "the console's foreground group is {foreground:?}, not the shell's"
        ));
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(format!("the shell's /proc/self/stat: {}", wrong.join("; ")))
    }
}

/// `flaky.service` was restarted, then failed on its start limit.
fn judge_flaky(after: &[String]) -> std::result::Result<(), String> {
    if !after
        .iter()
        .any(|line| line.contains("flaky.service: ") && line.contains("; restarting at "))
    {
        return Err("flaky.service was never restarted".into());
    }
    if !has(after, "flaky.service: failed (start-limit-hit)") {
        return Err("flaky.service was not failed by its start limit".into());
    }
    Ok(())
}

/// `kill -TERM 1` stopped everything in reverse order and powered off.
fn judge_shutdown(after: &[String]) -> std::result::Result<(), String> {
    let Some(from) = after
        .iter()
        .position(|line| line.contains("init     going down: poweroff.target"))
    else {
        return Err("SIGTERM to pid 1 did not start poweroff.target".into());
    };
    let mut rest = after.iter().skip(from);
    for want in STOP_ORDER {
        if !rest.any(|line| line.contains(want)) {
            return Err(format!(
                "shutdown did not say `{want}`, or not in the order {STOP_ORDER:?}"
            ));
        }
    }
    if !rest.any(|line| line.trim() == POWER_DOWN) {
        return Err(format!("the machine never said `{POWER_DOWN}`"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn a_shell_that_leads_its_session_on_the_console_passes() {
        let line = "stat-self 57 (sh) S 1 57 57 1281 57 0 0";
        assert_eq!(judge_stat(line), Ok(()));
    }

    #[test]
    fn a_shell_without_the_console_fails_on_each_field() {
        let why = judge_stat("stat-self 57 (sh) S 1 57 57 0 -1 0").unwrap_err();
        assert!(why.contains("controlling terminal"), "{why}");
        assert!(why.contains("foreground"), "{why}");
        let why = judge_stat("stat-self 57 (sh) S 12 12 12 1281 12 0").unwrap_err();
        assert!(why.contains("parent"), "{why}");
        assert!(why.contains("session"), "{why}");
    }

    #[test]
    fn a_command_name_with_spaces_parses() {
        assert_eq!(judge_stat("stat-self 9 (a b) S 1 9 9 1281 9"), Ok(()));
    }

    #[test]
    fn shutdown_must_stop_in_reverse_order() {
        let good = lines(&[
            "  init     going down: poweroff.target",
            "  init     multi-user.target: stopped",
            "  init     getty@console.service: stopped",
            "  init     basic.target: stopped",
            "  init     sysinit.target: stopped",
            "reboot: Power down",
        ]);
        assert_eq!(judge_shutdown(&good), Ok(()));
        let swapped = lines(&[
            "  init     going down: poweroff.target",
            "  init     getty@console.service: stopped",
            "  init     multi-user.target: stopped",
            "  init     basic.target: stopped",
            "  init     sysinit.target: stopped",
            "reboot: Power down",
        ]);
        assert!(judge_shutdown(&swapped).is_err());
        let no_power = lines(&good.iter().take(5).map(String::as_str).collect::<Vec<_>>());
        assert!(judge_shutdown(&no_power).unwrap_err().contains(POWER_DOWN));
    }

    #[test]
    fn a_warning_about_a_unit_file_fails() {
        let clean = lines(&["  init     booting default.target"]);
        assert_eq!(judge_units(&clean), Ok(()));
        let warned = lines(&[
            "  init     /etc/ferrix/units/getty@.service.d/test.conf:2: Failed to resolve",
        ]);
        assert!(judge_units(&warned).unwrap_err().contains("test.conf"));
    }

    #[test]
    fn flaky_must_be_restarted_and_then_fail_on_its_limit() {
        let good = lines(&[
            "  init     flaky.service: exit-code; restarting at 3.100s",
            "  init     flaky.service: failed (start-limit-hit)",
        ]);
        assert_eq!(judge_flaky(&good), Ok(()));
        let never = lines(&["  init     flaky.service: failed (exit-code)"]);
        assert!(judge_flaky(&never).is_err());
    }

    #[test]
    fn an_answer_is_found_after_inits_prefix_but_not_in_the_echo() {
        assert!(starts(
            "  init     forker.service: stopped",
            "forker.service: stopped"
        ));
        assert!(starts(
            "ferrix# \u{1b}[J\u{1b}[8C  init     forker.service: stopped",
            "forker.service: stopped"
        ));
        assert!(starts("stat-self 5 (sh)", "stat-self "));
        assert!(!starts(
            "init-test# m=stat; echo \"$m-self $s\"",
            "stat-self "
        ));
    }
}
