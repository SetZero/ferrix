//! The boots about the machine under the compositor rather than the
//! compositor: `desktop`, whose image skips the kernel's self-checks as a
//! person's desktop does, and `restart`, which kills the display driver
//! under the compositor twice.
//!
//! Both end on the first boot's tiled picture, because what each claims is
//! that the desktop is still the one it would have been.

use std::time::{Duration, Instant};

use super::boot::{
    build_desktop_image, build_image, compositor_ended, say_the_marker, settle, undithered,
};
use super::picture::{differences, expected, unexpected};
use super::{Carried, DESKTOP_DEFAULTS, EITHER, EXPECTED, FAILED, MARKER, Programs, SETTLE};
use crate::args::Args;
use crate::display::{Qmp, free_port};
use crate::paths::{self, Arch};
use crate::qemu::Watching;
use crate::{Error, Result};

/// The desktop boot's configuration: the two windows `dispatchers` tiles
/// first, and nothing to press.
const DESKTOP_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
";

/// What the kernel says when it read `ferrix.checks=skip`
/// (`src/kernel/src/checks.rs`).
const CHECKS_SKIPPED: &str =
    "checks   ferrix.checks=skip: stages 2 to 12 are brought up and not checked";

/// A line only a boot that ran stage 5's checks prints.
const STAGE5_CHECKED: &str = "  stage 5  ";

/// The boot a desktop makes: the image `run-compositor` and `flash
/// --compositor` build, which carries [`DESKTOP_DEFAULTS`] and so skips the
/// kernel's self-checks.
///
/// Every other boot here runs them, and so does every other row; this one is
/// what keeps the skipping honest in both directions. The kernel must say it
/// skipped them, end in the unchecked marker and never the success one, and
/// print no check's line; and what the checks would have come with must
/// still be there without them -- the root, `devmgr` and the card, the seat
/// -- so that the compositor comes up and tiles the two windows as the first
/// picture of `dispatchers` has them, pixel for pixel.
pub(super) fn test_desktop(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (image, kernel) = build_desktop_image(
        arch,
        programs,
        &undithered(DESKTOP_CONFIG),
        Carried::none(),
        args,
        DESKTOP_DEFAULTS,
    )?;
    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("compositor.ppm");
    let (what, path) = EXPECTED[0];
    let want = expected(path)?;
    let mut said: Vec<String> = Vec::new();
    let mut screen = None;
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        watching.stop_when_done();
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        let _ = watching.read_more(Instant::now() + SETTLE, |lines| {
            lines
                .iter()
                .any(|line| line.contains(MARKER) || line.contains(FAILED))
        })?;
        let up = watching
            .lines()
            .iter()
            .chain(watching.after())
            .any(|line| line.contains(MARKER));
        if up {
            say_the_marker(watching, arch);
            screen = Some(settle(&mut qmp, &dump, &want)?);
        }
        watching.read_what_was_said(Duration::from_secs(2))?;
        said = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    judge_desktop(arch, &said)?;
    let Some(screen) = screen else {
        return Err(Error::new(format!(
            "{arch}: the compositor never printed `{MARKER}` on a boot that skipped its checks"
        )));
    };
    let (found, count) = differences(&screen, &want);
    if count != 0 {
        return Err(unexpected(arch, what, &screen, found, count));
    }
    println!(
        "  {arch}: with the kernel's self-checks skipped, {what}, every one of {} pixels",
        screen.width * screen.height
    );
    Ok(())
}

/// How many of the compositor's clients init says it put in a scope of
/// their own: `init     app-pattern-<pid>.scope: active` (§5.6).
fn scopes(said: &[String]) -> usize {
    said.iter()
        .filter(|line| line.contains("init     app-pattern-") && line.contains(".scope: active"))
        .count()
}

/// What [`test_desktop`] requires of what the guest said.
fn judge_desktop(arch: Arch, said: &[String]) -> Result<()> {
    let any = |text: &str| said.iter().any(|line| line.contains(text));
    let problem = if !any(CHECKS_SKIPPED) {
        Some(format!("the kernel never said `{CHECKS_SKIPPED}`"))
    } else if any(crate::qemu::SUCCESS_MARKER) {
        Some(format!(
            "a boot that skipped its checks printed `{}`",
            crate::qemu::SUCCESS_MARKER
        ))
    } else if !any(crate::qemu::UNCHECKED_MARKER) {
        Some(format!(
            "the kernel never printed `{}`",
            crate::qemu::UNCHECKED_MARKER
        ))
    } else if any(STAGE5_CHECKED) {
        Some("stage 5's checks ran on a boot that skipped them".to_owned())
    } else if !any("  devmgr   ") {
        Some("devmgr was never started".to_owned())
    } else if !any("init     starting /sbin/init") {
        Some("the kernel did not start /sbin/init as pid 1".to_owned())
    } else if !any("init     hyprix.service: active") {
        Some("the compositor did not run as hyprix.service under init".to_owned())
    } else if scopes(said) < 2 {
        Some(format!(
            "the compositor's two clients were not each put in a scope: {} app-pattern scopes",
            scopes(said)
        ))
    } else {
        None
    };
    match problem {
        Some(problem) => Err(Error::new(format!("{arch}: {problem}"))),
        None => {
            println!(
                "  {arch}: the kernel skipped its self-checks and said `{}`",
                crate::qemu::UNCHECKED_MARKER
            );
            Ok(())
        }
    }
}

/// Where the restart boot's script is on the guest.
const KILL_GPU_PATH: &str = "etc/killgpu";

/// The script the restart boot runs from `exec-once`, twice over: wait until
/// the compositor holds `card0`, kill the display driver, and wait for devmgr
/// to have started another. zinc's, since a gate boot carries no busybox and
/// `exec-once` runs one program with no shell.
///
/// A card is open to one program at a time, so an open that fails on a card
/// that is there says the compositor has it: the second kill waits for that,
/// or it would land while the compositor was still looking for the first
/// card and the gate would see one loss where it asked for two.
const KILL_GPU: &str = r#"gpu() {
  for p in /proc/[0-9]*; do
    read n < $p/comm 2>/dev/null
    if [[ $n == gpu ]]; then echo ${p#/proc/}; return; fi
  done
}
held() { [[ -e /dev/dri/card0 ]] && ! { : 3<> /dev/dri/card0 } 2>/dev/null }
for round in 1 2; do
  until held; do :; done
  killed=$(gpu)
  echo "killgpu: killing gpu $killed"
  kill -9 $killed
  until [[ -n $(gpu) && $(gpu) != $killed ]]; do :; done
done
"#;

/// The configuration the restart boot is given: the first boot's two
/// windows, so the screen after the second return can be held to the
/// picture that boot blesses, and the script.
const RESTART_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor --boot restart`.
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
exec-once = /bin/zinc /etc/killgpu
";

/// What the kernel says when devmgr has started a killed driver again and it
/// has published its device.
const PUBLISHED_AGAIN: &str = "was started again and published";

/// What the compositor says when its card goes, and when it has it back.
const CARD_WENT: &str = "the card went away";
const CARD_BACK: &str = "the card is back";

/// How long the restart boot waits for both deaths and both returns.
const RESTART_PATIENCE: Duration = Duration::from_secs(120);

/// The twenty-first boot: the display driver killed under the compositor,
/// twice (`docs/DEVMGR.md` §4).
///
/// Killing `gpu` once took the whole machine down: the card answered
/// `ENODEV`, the compositor ended on it, and it was init. Now devmgr starts
/// the driver again, the kernel publishes the card as `card0` again, and the
/// compositor waits for it rather than ending. What is required, in the
/// guest's own words: the script killed the driver twice, devmgr said it
/// started it again twice, the compositor saw its card go and had it back
/// after the second kill (see [`back_after_the_last_kill`]), and it neither
/// failed nor ended. And on the screen: after that return, every pixel of
/// the first boot's tiled picture, drawn on the third driver's card.
pub(super) fn test_driver_restart(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let carried = Carried {
        zinc: crate::zinc::build(arch)?,
        ports: vec![crate::ports::File {
            path: KILL_GPU_PATH.to_owned(),
            mode: 0o755,
            content: crate::ports::Content::Bytes(KILL_GPU.as_bytes().to_vec()),
        }],
        ..Carried::none()
    };
    if carried.zinc.is_none() {
        println!("  {arch}: zinc is not built here, so nothing can kill the driver; skipped");
        return Ok(());
    }
    let (image, kernel) = build_image(arch, programs, &undithered(RESTART_CONFIG), carried, args)?;
    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("compositor.ppm");
    let (what, path) = EXPECTED[0];
    let want = expected(path)?;
    let mut said: Vec<String> = Vec::new();
    let mut screen = None;
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        watching.stop_when_done();
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        let ended = |line: &String| {
            line.contains(FAILED)
                || compositor_ended(line)
                || line.contains(crate::qemu::PANIC_MARKER)
        };
        let _ = watching.read_more(Instant::now() + RESTART_PATIENCE, |lines| {
            lines.iter().any(ended) || back_after_the_last_kill(lines)
        })?;
        if back_after_the_last_kill(watching.after()) {
            screen = Some(settle(&mut qmp, &dump, &want)?);
        }
        // What the last return set off: devmgr's second `published`, which
        // races the compositor's saying it has the card back, and anything
        // else the guest says before it goes quiet.
        let _ = watching.read_more(Instant::now() + Duration::from_secs(3), |lines| {
            lines
                .iter()
                .filter(|line| line.contains(PUBLISHED_AGAIN))
                .count()
                >= 2
                || lines.iter().any(ended)
        })?;
        watching.read_what_was_said(Duration::from_secs(3))?;
        said = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    judge_restart(arch, &said)?;
    let Some(screen) = screen else {
        return Err(Error::new(format!(
            "{arch}: no picture was taken after the card came back"
        )));
    };
    let (_, wrong) = differences(&screen, &want);
    if wrong != 0 {
        return Err(Error::new(format!(
            "{arch}: after the card came back the screen was not {what} ({path}): \
             {wrong} pixels differ"
        )));
    }
    println!("  {arch}: and the screen is {what} again, every pixel");
    Ok(())
}

/// What the restart boot's script says as it kills the driver.
const KILLING: &str = "killgpu: killing gpu";

/// Whether the compositor had its card back after the script's second kill.
///
/// Not "went and came back twice": the second kill can land while the
/// compositor is still reopening the first restart's card. It holds the card
/// open then, which is all the script can see, but has not yet said so. That
/// reopen fails, the compositor looks again, and the card it has is the
/// third driver's, one loss and one return in its own log. What matters is
/// that it ends with the card, after both kills.
fn back_after_the_last_kill(lines: &[String]) -> bool {
    let kills: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(KILLING))
        .map(|(at, _)| at)
        .collect();
    let Some(&last) = kills.get(1) else {
        return false;
    };
    lines
        .get(last..)
        .unwrap_or_default()
        .iter()
        .any(|line| line.contains(CARD_BACK))
}

/// What [`test_driver_restart`] requires of what the guest said.
fn judge_restart(arch: Arch, said: &[String]) -> Result<()> {
    let count = |want: &str| said.iter().filter(|line| line.contains(want)).count();
    if let Some(line) = said.iter().find(|line| {
        line.contains(FAILED) || compositor_ended(line) || line.contains(crate::qemu::PANIC_MARKER)
    }) {
        return Err(Error::new(format!(
            "{arch}: the machine did not survive its display driver being killed: {}",
            line.trim()
        )));
    }
    let wanted = [
        (KILLING, 2, "the script killed the driver twice"),
        (CARD_WENT, 1, "the compositor saw its card go"),
        (PUBLISHED_AGAIN, 2, "devmgr started the driver again twice"),
    ];
    let mut missing = Vec::new();
    for (want, times, what) in wanted {
        let seen = count(want);
        if seen < times {
            missing.push(format!("{what}: `{want}` {seen} of {times} times"));
        }
    }
    if !back_after_the_last_kill(said) {
        missing.push(format!(
            "the compositor had its card back after the second kill: no `{CARD_BACK}` after it"
        ));
    }
    if !missing.is_empty() {
        return Err(Error::new(format!(
            "{arch}: the display driver was not restarted under the compositor:\n    - {}",
            missing.join("\n    - ")
        )));
    }
    println!(
        "  {arch}: the display driver was killed twice under the compositor, and each time \
         devmgr started it again and the compositor drew on its card again"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::back_after_the_last_kill;

    fn said(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    const KILL: &str = "    6.67 | killgpu: killing gpu 198";
    const WENT: &str =
        "    6.68 | hyprix: Virtual-1: the card went away; waiting for it to come back";
    const BACK: &str = "    7.24 | hyprix: Virtual-1: the card is back; drawing on it again";

    /// The race the full matrix met: the second kill lands mid-reopen, so the
    /// compositor loses and regains its card once, and that is a pass.
    #[test]
    fn one_loss_and_one_return_after_both_kills_is_back() {
        assert!(back_after_the_last_kill(&said(&[KILL, WENT, KILL, BACK])));
        assert!(back_after_the_last_kill(&said(&[
            KILL, WENT, BACK, KILL, WENT, BACK
        ])));
    }

    #[test]
    fn a_return_only_before_the_second_kill_is_not_back() {
        assert!(!back_after_the_last_kill(&said(&[
            KILL, WENT, BACK, KILL, WENT
        ])));
        assert!(!back_after_the_last_kill(&said(&[KILL, WENT, BACK])));
    }
}
