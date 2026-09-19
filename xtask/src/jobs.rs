//! `test-jobs`: stage 15's exit, driven by typing at the serial console.
//!
//! "An interactive shell over the serial console that a person can use."
//! Every other gate here gives the guest a script and reads what it printed;
//! this one types, one keystroke at a time, into the same serial port a
//! person would, and judges what comes back. That is the only way to test
//! the parts of a session that exist because somebody is at the terminal:
//! the line discipline's signal characters, the foreground process group,
//! and a shell that has to hand the terminal over and take it back.
//!
//! # What is typed, and what each keystroke proves
//!
//! | typed | what it shows |
//! |---|---|
//! | `echo` | the shell reads the console at all: `sh -i`, a prompt, a line |
//! | `sleep 30 &` | a background job is started and announced |
//! | `jobs` | the job table, and that the shell kept it |
//! | `sleep 30 \| cat &`, `kill %1` | one signal reaches a whole pipeline: the job is one process group |
//! | Ctrl-Z | the line discipline raises `SIGTSTP` for the foreground group, and the shell sees the stop through `wait4`'s `WUNTRACED` |
//! | `bg`, `fg` | `SIGCONT` and `tcsetpgrp`, in both directions |
//! | Ctrl-C | `SIGINT` reaches the job and not the shell |
//! | `exit` | the session ends, and the kernel says with what |
//!
//! # Why x86-64 only
//!
//! `sleep` is uutils', and uutils is built against ferrousli for x86-64
//! alone (`docs/UUTILS.md` D3). On AArch64 and ARMv7-A the image carries
//! zinc and nothing else, so there is no program to suspend. That is the
//! same backlog row as the rest of the userland's architectures, not a
//! limit of this gate.

use std::time::{Duration, Instant};

use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result, cargo, fat, initramfs, native, ports, qemu, uutils, zinc};

/// How long to wait for the answer to one keystroke.
///
/// Generous, because the transcript is read line by line under `tcg`, where
/// a shell's fork and exec take a moment; a step that has its answer goes on
/// at once, so the cost is only paid by a step that fails.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long to leave a foreground job running before typing at it. Long
/// enough for `sleep` to have been forked, exec'd and put in the terminal's
/// foreground group, and short beside [`PATIENCE`].
const SETTLE: Duration = Duration::from_millis(1500);

/// One step of the session: what to type, and what the guest must say.
struct Step {
    /// The bytes a person would type.
    keys: &'static [u8],
    /// What must appear in a line after them, in this order.
    wants: &'static [&'static str],
    /// What this step is evidence of, for the message when it fails.
    proves: &'static str,
    /// Give the guest a moment before the next keystroke: for a job to have
    /// been forked and exec'd, or for a signal just sent to have arrived.
    settle: bool,
}

/// The session, in order. Every `wants` is looked for in the lines that
/// follow its own keystrokes, so a line the shell printed earlier cannot
/// satisfy a later step.
const SESSION: &[Step] = &[
    Step {
        keys: b"echo jobs-gate: the shell reads the console\n",
        wants: &["jobs-gate: the shell reads the console"],
        proves: "the shell is interactive and reads what is typed",
        settle: false,
    },
    Step {
        keys: b"sleep 30 &\n",
        wants: &["[1]"],
        proves: "a background job is started and announced by number",
        settle: false,
    },
    Step {
        keys: b"jobs\n",
        wants: &["running", "sleep 30"],
        proves: "the shell keeps a job table",
        settle: false,
    },
    // A signal sent now is not a death yet. The shell reports what its jobs
    // did at the prompt, so a job killed here is still running when this
    // command's prompt comes round, and is reported at the next one: the
    // `jobs` below is that next one. zsh without NOTIFY reports the same way.
    Step {
        keys: b"kill %1\n",
        wants: &[],
        proves: "`kill %1` is accepted for a job that exists",
        settle: true,
    },
    Step {
        keys: b"jobs\n",
        wants: &["terminated"],
        proves: "the job died of the signal, and the shell says so",
        settle: false,
    },
    // A pipeline: two processes, one job, one process group. `kill %1`
    // reaches both, which is what the group is for -- a shell that merely
    // forked would leave `cat` behind.
    Step {
        keys: b"sleep 30 | cat &\n",
        wants: &["[1]"],
        proves: "a pipeline is one job",
        settle: true,
    },
    Step {
        keys: b"kill %1\n",
        wants: &[],
        proves: "`kill %1` is accepted for the pipeline",
        settle: true,
    },
    Step {
        keys: b"jobs\n",
        wants: &["terminated"],
        proves: "one signal ends a whole pipeline: its processes share a group",
        settle: false,
    },
    // The foreground job, and the three keystrokes that are the whole point
    // of job control.
    Step {
        keys: b"sleep 30\n",
        wants: &[],
        proves: "a foreground job starts",
        settle: true,
    },
    Step {
        keys: b"\x1a",
        wants: &["suspended"],
        proves: "Ctrl-Z stops the foreground job and the shell says so",
        settle: false,
    },
    Step {
        keys: b"jobs\n",
        wants: &["suspended", "sleep 30"],
        proves: "a stopped job stays in the table",
        settle: false,
    },
    Step {
        keys: b"bg\n",
        wants: &["continued"],
        proves: "`bg` continues it without giving it the terminal",
        settle: false,
    },
    Step {
        keys: b"jobs\n",
        wants: &["running"],
        proves: "it is running again",
        settle: false,
    },
    Step {
        keys: b"fg\n",
        wants: &["sleep 30"],
        proves: "`fg` names the job it brings back",
        settle: true,
    },
    Step {
        keys: b"\x03",
        wants: &[],
        proves: "Ctrl-C reaches the foreground job",
        settle: false,
    },
    Step {
        keys: b"echo jobs-gate: the shell survived the interrupt\n",
        wants: &["jobs-gate: the shell survived the interrupt"],
        proves: "the interrupt ended the job and not the session",
        settle: false,
    },
    Step {
        keys: b"echo jobs-gate: piped | cat\n",
        wants: &["jobs-gate: piped"],
        proves: "a foreground pipeline runs and the shell takes the terminal back",
        settle: false,
    },
    Step {
        keys: b"exit\n",
        wants: &[crate::shell::EXITED],
        proves: "the session ends, and the kernel reports the shell's status",
        settle: false,
    },
];

/// Boot an interactive shell on the console and drive a session at it.
///
/// # Errors
///
/// When the image cannot be built, when the boot fails, or when a keystroke
/// does not get the answer a session at a terminal must give.
pub(crate) fn test_jobs(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "test-jobs runs on x86-64 only: `sleep` comes from uutils, which is built \
             for x86-64 alone (docs/UUTILS.md D3), and a session with nothing to \
             suspend would prove nothing",
        ));
    }
    let shell = zinc::built(arch)?
        .ok_or_else(|| Error::new("zinc could not be built for x86-64"))?;
    println!("  {arch}: building an image whose init is an interactive shell");
    let loader = cargo::build_loader(arch, args.release)?;
    // No script: the kernel starts `sh -i`, which is the thing under test.
    let kernel = cargo::build_kernel_with_init(arch, args.release, &shell, "")?;
    let natives = native::build(arch, args.release)?;
    let utilities = uutils::carried(arch)?;
    if utilities.is_empty() {
        return Err(Error::new(
            "the image carries no utilities: `cargo xtask uutils` builds the ones this \
             gate types at",
        ));
    }
    let bytes = std::fs::read(&shell)
        .map_err(|error| Error::new(format!("reading {}: {error}", shell.display())))?;
    let archive = initramfs::build_with_utilities(
        Some(&shell),
        &natives,
        Some(&bytes),
        &utilities,
        &ports::installed(arch)?,
    )?;
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, None)?;

    println!("  {arch}: typing a session at the serial console (timeout {}s)", args.timeout);
    let mut failures: Vec<String> = Vec::new();
    let lines = qemu::watch_then(
        arch,
        &image,
        &kernel,
        args,
        qemu::SUCCESS_MARKER,
        |watching| {
            // The shell has to have started and printed a prompt before the
            // first keystroke: a byte typed at a console nobody is reading
            // sits in the kernel's ring, and the shell's first read would
            // take it as the answer to a prompt it had not printed yet.
            std::thread::sleep(SETTLE);
            for step in SESSION {
                let before = watching.after().len();
                watching.type_in(step.keys)?;
                let deadline = Instant::now() + PATIENCE;
                let wants = step.wants;
                let found = watching.read_more(deadline, |lines| {
                    seen_in_order(lines.get(before..).unwrap_or_default(), wants)
                })?;
                if !found {
                    failures.push(format!(
                        "after typing {:?}, the guest never said {:?}\n      ({})",
                        String::from_utf8_lossy(step.keys),
                        step.wants,
                        step.proves
                    ));
                }
                if step.settle {
                    std::thread::sleep(SETTLE);
                }
            }
            Ok(())
        },
    )?;

    if !failures.is_empty() {
        let mut message = format!("{arch}: the session at the console did not answer:\n");
        for failure in &failures {
            message.push_str("    - ");
            message.push_str(failure);
            message.push('\n');
        }
        message.push_str("  The whole transcript is above and in the serial log.");
        return Err(Error::new(message));
    }
    // The shell exited because `exit` was typed, and the kernel said so; a
    // shell that died of a signal instead would have printed a status too,
    // so the status itself is checked.
    let exited = lines
        .iter()
        .rev()
        .find(|line| line.contains(crate::shell::EXITED));
    match exited {
        Some(line) if line.contains(&format!("{} 0", crate::shell::EXITED)) => {}
        Some(line) => {
            return Err(Error::new(format!(
                "{arch}: the session ended, but not with `exit`: {}",
                line.trim()
            )));
        }
        None => {
            return Err(Error::new(format!(
                "{arch}: the shell never exited, so the session was not ended by what was typed"
            )));
        }
    }
    println!("  {arch}: a person can hold a session with jobs at the serial console");
    Ok(())
}

/// Whether `wants` all appear in `lines`, each in a line at or after the one
/// the want before it matched.
///
/// *At or after*, rather than after, because a shell says several of these
/// things on one line: `[1]  + suspended  sleep 30` is both the state and
/// the command, and a reading that demanded a new line for each would be
/// asking for output no shell produces.
fn seen_in_order(lines: &[String], wants: &[&str]) -> bool {
    let mut from = 0;
    for want in wants {
        let found = lines
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, line)| line.contains(want));
        match found {
            Some((at, _)) => from = at,
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::seen_in_order;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn two_wants_may_share_one_line() {
        let shown = lines(&["[1]  + suspended  sleep 30"]);
        assert!(seen_in_order(&shown, &["suspended", "sleep 30"]));
    }

    #[test]
    fn a_want_may_not_be_matched_by_an_earlier_line() {
        let shown = lines(&["sleep 30", "[1]  + suspended"]);
        assert!(!seen_in_order(&shown, &["suspended", "sleep 30"]));
    }

    #[test]
    fn nothing_wanted_is_always_seen() {
        assert!(seen_in_order(&lines(&[]), &[]));
    }
}
