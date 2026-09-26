//! `test-restart`: a display driver killed from a shell is started again, and
//! the machine stays up.
//!
//! `docs/DEVMGR.md` §4. The gate boots zinc as an interactive shell on a
//! machine with a virtio-gpu, finds the `gpu` driver's pid through `/proc`,
//! and types `kill -9` at it -- what a person at the console would do, and
//! what once took the whole machine down. Then it requires:
//!
//! 1. devmgr's DIED for the device, printed by the kernel;
//! 2. after it, in either order, devmgr's word that it started the driver
//!    again and the card published again under the number it had;
//! 3. the shell still answering, so the kernel is alive and the console works.
//!
//! A bare shell never went down: what did was the compositor, which ended on
//! the card's `ENODEV` and was init. `test-compositor --boot restart` is that
//! half; this one is devmgr's and the kernel's alone.
//!
//! It kills the restarted driver a second time and requires the same again,
//! so a restart is shown to be repeatable rather than a one-off.
//!
//! The driver it kills is looked up afresh each round, and must be the one
//! `gpu` process it has not killed already: a driver killed in round one may
//! still be listed in `/proc` when round two looks, beside the one that
//! replaced it, and procfs says `S` for both. The kill itself checks the pid
//! is still a `gpu` in the same line, and the gate fails naming the pid if it
//! is not, rather than kill whatever has the number by then.
//!
//! x86-64 only, for the reason `test-jobs` is: `kill` and `cat` are uutils',
//! built for x86-64 alone (`docs/UUTILS.md` D3).

use std::time::{Duration, Instant};

use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result, cargo, fat, initramfs, native, ports, qemu, uutils, zinc};

/// How long to wait for the answer to one line.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long to give the shell to print its first prompt, and the desktop's
/// driver to have published, before the first keystroke.
const SETTLE: Duration = Duration::from_secs(3);

/// What the pid search prints before each pid. Typed with a quote in the
/// middle, so the echo of the typed line never matches it.
const PID_TAG: &str = "restart-gate-pid=";

/// What the pid search prints once it has looked at every process, so the
/// gate reads the whole list and not only its first line.
const LISTED: &str = "restart-gate-listed";

/// The line that finds the driver: every process whose `comm` is `gpu`,
/// then [`LISTED`].
const FIND: &[u8] = b"for p in /proc/[0-9]*; do read n < $p/comm; \
    [ \"$n\" = gpu ] && echo restart-gate-'pid='${p#/proc/}; done; \
    echo restart-gate-'listed'\n";

/// What the kill line prints when it killed the pid it was given, which it
/// does only if that pid is still a `gpu` process.
const KILLED_TAG: &str = "restart-gate-killed=";

/// What the kill line prints when the pid was not a `gpu` process by then,
/// or the kill was refused, and nothing was killed.
const SPARED_TAG: &str = "restart-gate-spared=";

/// What the kernel prints for devmgr's DIED.
const DIED: &str = "devmgr   the driver of";

/// What the kernel prints when devmgr says it started a driver again.
const RESTARTED: &str = "was started again";

/// What the kernel prints when a card is published.
const PUBLISHED: &str = "display  card0 is";

/// Where the card's holder is carried.
const BLANK: &str = "/bin/blank";

/// A line whose answer only a live shell can compute.
const ALIVE: &[u8] = b"echo restart-gate: $((6 * 7)) alive\n";
const ALIVE_ANSWER: &str = "restart-gate: 42 alive";

/// Boot a shell beside a virtio-gpu, kill the driver twice, and require it
/// back each time.
///
/// # Errors
///
/// When the image cannot be built, when the boot fails or panics, or when the
/// driver is not found, not restarted, or the shell stops answering.
pub(crate) fn test_restart(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "test-restart runs on x86-64 only: `kill` comes from uutils, which is built \
             for x86-64 alone (docs/UUTILS.md D3)",
        ));
    }
    let shell =
        zinc::built(arch)?.ok_or_else(|| Error::new("zinc could not be built for x86-64"))?;
    println!("  {arch}: building an image whose init is an interactive shell");
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &shell, "")?;
    let natives = native::build(arch, args.release)?;
    let utilities = uutils::carried(arch)?;
    if utilities.is_empty() {
        return Err(Error::new(
            "the image carries no utilities: `cargo xtask uutils` builds the ones this \
             gate types",
        ));
    }
    let bytes = std::fs::read(&shell)
        .map_err(|error| Error::new(format!("reading {}: {error}", shell.display())))?;
    // A program holding the card when its driver dies: `compositor/blank`
    // sets a mode, shows a buffer and waits, as a compositor's card stays open.
    let blank = crate::display::build_blank(arch, false)?;
    let mut carried = ports::installed(arch)?;
    carried.push(ports::File {
        path: BLANK.trim_start_matches('/').to_owned(),
        mode: 0o755,
        content: ports::Content::Bytes(
            std::fs::read(&blank)
                .map_err(|error| Error::new(format!("reading {}: {error}", blank.display())))?,
        ),
    });
    let archive = initramfs::build_with_utilities(
        Some(&shell),
        &natives,
        Some(&bytes),
        &utilities,
        &carried,
    )?;
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, None)?;

    // A virtio-gpu on the bus, and nothing drawn to a window.
    let mut booted = args.clone();
    booted.display = true;
    println!(
        "  {arch}: killing the display driver from the shell (timeout {}s)",
        args.timeout
    );
    let mut failures: Vec<String> = Vec::new();
    let lines = qemu::watch_then(
        arch,
        &image,
        &kernel,
        &booted,
        qemu::SUCCESS_MARKER,
        |watching| {
            std::thread::sleep(SETTLE);
            if let Err(failure) = hold_the_card(watching) {
                failures.push(failure);
                return Ok(());
            }
            let mut killed = Vec::new();
            for round in 1..=2 {
                if let Err(failure) = kill_and_expect_back(watching, round, &mut killed) {
                    failures.push(failure);
                    break;
                }
            }
            Ok(())
        },
    )?;
    if let Some(panic) = lines.iter().find(|line| line.contains(qemu::PANIC_MARKER)) {
        failures.push(format!("the kernel panicked: {}", panic.trim()));
    }
    if !failures.is_empty() {
        let mut message = format!("{arch}: the display driver did not come back:\n");
        for failure in &failures {
            message.push_str("    - ");
            message.push_str(failure);
            message.push('\n');
        }
        message.push_str("  The whole transcript is above and in the serial log.");
        return Err(Error::new(message));
    }
    println!("  {arch}: a killed display driver is started again, twice, and the machine stays up");
    Ok(())
}

/// Start [`BLANK`] in the background and wait for its buffer on the screen.
fn hold_the_card(watching: &mut qemu::Watching<'_>) -> std::result::Result<(), String> {
    let before = watching.after().len();
    watching
        .type_in(format!("{BLANK} &\n").as_bytes())
        .map_err(|error| error.to_string())?;
    let shown = watching
        .read_more(Instant::now() + PATIENCE, |lines| {
            lines
                .get(before..)
                .unwrap_or_default()
                .iter()
                .any(|line| line.contains(crate::display::MARKER))
        })
        .map_err(|error| error.to_string())?;
    if shown {
        Ok(())
    } else {
        Err(format!(
            "{BLANK} never showed its buffer, so nothing held the card"
        ))
    }
}

/// One round: find the driver, kill it, and require DIED, the restart, the
/// card again and a live shell, in that order. `killed` is the pids earlier
/// rounds killed, which this one adds to.
fn kill_and_expect_back(
    watching: &mut qemu::Watching<'_>,
    round: u32,
    killed: &mut Vec<u32>,
) -> std::result::Result<(), String> {
    let io = |error: Error| format!("round {round}: {error}");
    let before = watching.after().len();
    watching.type_in(FIND).map_err(io)?;
    let listed = watching
        .read_more(Instant::now() + PATIENCE, |lines| {
            listing_ended(lines.get(before..).unwrap_or_default())
        })
        .map_err(io)?;
    if !listed {
        return Err(format!(
            "round {round}: the search for the gpu driver never finished"
        ));
    }
    let found = pids_in(watching.after().get(before..).unwrap_or_default());
    let pid = the_driver(&found, killed).map_err(|why| format!("round {round}: {why}"))?;

    // Checked again in the line that kills, so what is killed is the gpu
    // driver the list named or nothing.
    let before = watching.after().len();
    watching.type_in(&kill_line(pid)).map_err(io)?;
    let _ = watching
        .read_more(Instant::now() + PATIENCE, |lines| {
            kill_answer(lines.get(before..).unwrap_or_default(), pid).is_some()
        })
        .map_err(io)?;
    match kill_answer(watching.after().get(before..).unwrap_or_default(), pid) {
        Some(true) => killed.push(pid),
        Some(false) => {
            return Err(format!(
                "round {round}: pid {pid} was no longer a gpu process when the gate came to \
                 kill it, so nothing was killed"
            ));
        }
        None => {
            return Err(format!(
                "round {round}: the line that kills pid {pid} never said whether it did"
            ));
        }
    }
    let wants = [DIED, RESTARTED, PUBLISHED];
    let back = watching
        .read_more(Instant::now() + PATIENCE, |lines| {
            let lines = lines.get(before..).unwrap_or_default();
            came_back(lines) || panicked(lines)
        })
        .map_err(io)?;
    let after = watching.after().get(before..).unwrap_or_default();
    if panicked(after) {
        return Err(format!(
            "round {round}: the kernel panicked after `kill -9 {pid}`"
        ));
    }
    if !back {
        return Err(format!(
            "round {round}: after `kill -9 {pid}` the guest never said {wants:?}, the first \
             before the other two"
        ));
    }

    let before = watching.after().len();
    watching.type_in(ALIVE).map_err(io)?;
    let alive = watching
        .read_more(Instant::now() + PATIENCE, |lines| {
            lines
                .get(before..)
                .unwrap_or_default()
                .iter()
                .any(|line| line.contains(ALIVE_ANSWER))
        })
        .map_err(io)?;
    if !alive {
        return Err(format!(
            "round {round}: the shell did not answer after the driver came back"
        ));
    }
    Ok(())
}

fn panicked(lines: &[String]) -> bool {
    lines.iter().any(|line| line.contains(qemu::PANIC_MARKER))
}

/// The number after `tag` in `line`, if the tag is there.
fn number_after(line: &str, tag: &str) -> Option<u32> {
    let (_, rest) = line.split_once(tag)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Whether the search has printed [`LISTED`]: its end, and not the echo of
/// the typed line, which has a quote in the middle of the tag.
fn listing_ended(lines: &[String]) -> bool {
    lines.iter().any(|line| line.contains(LISTED))
}

/// Every pid the search printed, in order, up to its end.
fn pids_in(lines: &[String]) -> Vec<u32> {
    lines
        .iter()
        .take_while(|line| !line.contains(LISTED))
        .filter_map(|line| number_after(line, PID_TAG))
        .collect()
}

/// The driver to kill: the one `gpu` process listed that no earlier round
/// killed. A killed driver may still be listed beside the one that replaced
/// it; anything else -- none, or two the gate has not killed -- is a
/// failure, named, rather than a guess.
fn the_driver(found: &[u32], killed: &[u32]) -> std::result::Result<u32, String> {
    let fresh: Vec<u32> = found
        .iter()
        .copied()
        .filter(|pid| !killed.contains(pid))
        .collect();
    match fresh.as_slice() {
        [pid] => Ok(*pid),
        [] if found.is_empty() => Err("no process named gpu in /proc".to_owned()),
        [] => Err(format!(
            "the only gpu processes in /proc, {found:?}, are ones the gate already killed"
        )),
        more => Err(format!(
            "more than one gpu process the gate has not killed, {more:?}: it will not guess \
             which is the driver"
        )),
    }
}

/// The line that kills `pid`, if it is still a `gpu` process when the shell
/// runs the line, and says which it did. Its tags have a quote in the middle,
/// as [`FIND`]'s has.
fn kill_line(pid: u32) -> Vec<u8> {
    format!(
        "if read n < /proc/{pid}/comm && [ \"$n\" = gpu ] && kill -9 {pid}; \
         then echo restart-gate-'killed='{pid}; else echo restart-gate-'spared='{pid}; fi\n"
    )
    .into_bytes()
}

/// What the kill line said about `pid`: `Some(true)` killed, `Some(false)`
/// spared, `None` nothing yet.
fn kill_answer(lines: &[String], pid: u32) -> Option<bool> {
    lines.iter().find_map(|line| {
        if number_after(line, KILLED_TAG) == Some(pid) {
            Some(true)
        } else if number_after(line, SPARED_TAG) == Some(pid) {
            Some(false)
        } else {
            None
        }
    })
}

/// Whether `lines` say the driver died and then came back: DIED, and after
/// it both devmgr's RESTARTED and the card published again, in either order.
/// The kernel prints the card as the driver's READY goes out and devmgr
/// sends RESTARTED once it has read PUBLISHED, so the two race.
fn came_back(lines: &[String]) -> bool {
    let Some(died) = lines.iter().position(|line| line.contains(DIED)) else {
        return false;
    };
    let after = lines.get(died..).unwrap_or_default();
    [RESTARTED, PUBLISHED]
        .iter()
        .all(|want| after.iter().any(|line| line.contains(want)))
}

#[cfg(test)]
mod tests {
    use super::{
        KILLED_TAG, LISTED, PID_TAG, SPARED_TAG, came_back, kill_answer, kill_line, listing_ended,
        pids_in, the_driver,
    };

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn the_typed_lines_are_not_answers() {
        // The console echoes what was typed; every tag is split by a quote.
        let typed = String::from_utf8_lossy(super::FIND).into_owned();
        assert!(!typed.contains(PID_TAG));
        assert!(!typed.contains(LISTED));
        assert!(!listing_ended(&lines(&[&typed])));
        let kill = String::from_utf8_lossy(&kill_line(273)).into_owned();
        assert!(!kill.contains(KILLED_TAG));
        assert!(!kill.contains(SPARED_TAG));
        assert_eq!(kill_answer(&lines(&[&kill]), 273), None);
    }

    #[test]
    fn every_printed_pid_is_found() {
        let printed = lines(&[
            "x",
            "restart-gate-pid=222",
            "restart-gate-pid=273",
            "restart-gate-listed",
            "restart-gate-pid=9",
        ]);
        assert!(listing_ended(&printed));
        assert_eq!(pids_in(&printed), [222, 273]);
    }

    #[test]
    fn a_driver_killed_already_is_not_killed_again() {
        // Round two of the run that killed 222 twice: 222 was still listed,
        // first, beside the 273 that replaced it.
        assert_eq!(the_driver(&[222, 273], &[222]), Ok(273));
        assert_eq!(the_driver(&[273], &[222]), Ok(273));
    }

    #[test]
    fn no_driver_or_two_is_a_failure_not_a_guess() {
        assert!(the_driver(&[], &[]).is_err());
        assert!(the_driver(&[222], &[222]).is_err());
        assert!(the_driver(&[222, 273], &[]).is_err());
    }

    #[test]
    fn the_kill_line_says_which_pid_it_killed_or_spared() {
        let killed = lines(&["restart-gate-killed=273"]);
        assert_eq!(kill_answer(&killed, 273), Some(true));
        assert_eq!(kill_answer(&killed, 27), None);
        assert_eq!(
            kill_answer(&lines(&["restart-gate-spared=273"]), 273),
            Some(false)
        );
    }

    #[test]
    fn the_card_and_the_restart_may_come_in_either_order() {
        let died = "  devmgr   the driver of 0x18 ended with status 137; the device is quiesced";
        let restarted = "  devmgr   the driver of 0x18 was started again and published (restart 1)";
        // The kernel's line glued onto a shell's prompt, as a console has it.
        let card = "\x1b[J  display  card0 is a scanout: no 3D";
        assert!(came_back(&lines(&[died, card, restarted])));
        assert!(came_back(&lines(&[died, restarted, card])));
    }

    #[test]
    fn a_card_from_before_the_death_does_not_count() {
        let died = "  devmgr   the driver of 0x18 ended with status 137; the device is quiesced";
        let restarted = "  devmgr   the driver of 0x18 was started again and published (restart 1)";
        let card = "  display  card0 is a scanout: no 3D";
        assert!(!came_back(&lines(&[card, died, restarted])));
    }
}
