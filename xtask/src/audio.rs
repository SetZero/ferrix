//! `cargo xtask test-audio`: a second of a counter written to
//! `/dev/snd/pcmC0D0p`, and found again, frame for frame, in the file QEMU
//! wrote of what the device played.
//!
//! `docs/AUDIO.md` L7. Every part below this has its own test -- the ALSA
//! numbers against `asound.h`, virtio-snd against QEMU's source, the stream
//! against Linux's rules, the driver against a device doing what QEMU's does
//! -- and each is a part alone. This is the whole path at once, and nothing
//! in it is simulated: `userland/compositor/tone` writes frames through the ALSA
//! ioctls, the kernel's audio core copies them into its buffer and submits
//! them, the ring-3 driver posts them to a real `virtio-sound-pci`, and
//! QEMU's `wav` backend, with its mixing engine off so it neither resamples
//! nor scales, writes what the device consumed to a file this reads.
//!
//! **The negative control.** The same boot again with tone built with
//! `negative-control`, which writes period 10 twice and skips period 11. The
//! frames still reach the file, so the control does not fail by the guest
//! falling over; the check must fail, and at exactly the first frame of
//! period 11.
//!
//! **The restart.** A third boot, with tone built with `restart`: the
//! `snd` driver killed twice under a running stream of silence, the stream
//! answering `EBADFD` each time, devmgr starting the driver again
//! (`docs/DEVMGR.md` §4), and then the same second played on the third
//! driver's card and held to the same check.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::qemu::Watching;
use crate::{Error, Result};

/// What every line tone prints begins with: the boot is watched from its
/// first, so a failure in the restart's rounds, before the stream it plays
/// is prepared, is reported in tone's own words.
const SPOKE: &str = "tone: ";
/// What it prints once the drain is over.
const DONE: &str = "tone: done";
/// What it prints when anything was refused.
const FAILED: &str = "tone: failed";

/// Frames tone writes.
const FRAMES: u32 = 48_000;
/// Frames in a period, which the negative control moves one of.
const PERIOD: u32 = 960;

/// How long to wait for the second to play and drain, in an emulated guest,
/// after two restarts of the driver for the restart boot.
const PATIENCE: Duration = Duration::from_secs(120);

/// Which `userland/compositor/tone` is built.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavour {
    Plain,
    /// `negative-control`: period 10 twice, period 11 never.
    Negative,
    /// `restart`: the driver killed twice before the second is played.
    Restart,
}

/// Build `userland/compositor/tone` for `arch`, in `flavour`.
fn build_tone(arch: Arch, flavour: Flavour) -> Result<PathBuf> {
    let target = crate::display::target(arch)
        .ok_or_else(|| Error::new(format!("{arch} has no user-space target for tone")))?;
    let feature = match flavour {
        Flavour::Plain => None,
        Flavour::Negative => Some("negative-control"),
        Flavour::Restart => Some("restart"),
    };
    let flavour = match flavour {
        Flavour::Plain => "plain",
        Flavour::Negative => "negative",
        Flavour::Restart => "restart",
    };
    let target_dir = paths::target_dir()
        .join("compositor")
        .join(format!("tone-{flavour}"));
    println!("  building userland/compositor/tone ({flavour}) for {target}");
    let program = target_dir.join(target).join("release").join("tone");
    let mut build = crate::builds::Build::cargo(
        format!("cargo build (userland/compositor/tone, {flavour}) --target {target}"),
        paths::workspace_root().join("userland/compositor"),
    )
    .args([
        "build",
        "--release",
        "-p",
        "compositor-tone",
        "--target",
        target,
    ])
    .env("CARGO_TARGET_DIR", &target_dir)
    .output(&program);
    if let Some(feature) = feature {
        build = build.args(["--features", feature]);
    }
    build.run()?;
    Ok(program)
}

/// Boot tone with a virtio-snd card whose far end writes `wav`, and give the
/// lines tone printed.
fn boot_and_play(arch: Arch, program: &Path, wav: &Path, args: &Args) -> Result<Vec<String>> {
    let loader = crate::cargo::build_loader(arch, args.release)?;
    let kernel = crate::cargo::build_kernel_with_init(arch, args.release, program, "")?;
    let natives = crate::native::build(arch, args.release)?;
    let initramfs = crate::initramfs::build(None, &natives, None, &[])?;
    let image = crate::fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
    if wav.exists() {
        std::fs::remove_file(wav)
            .map_err(|error| Error::new(format!("{}: {error}", wav.display())))?;
    }
    let mut qemu_args = args.clone();
    qemu_args.audio = Some(format!("wav:{}", wav.display()));
    let mut lines = Vec::new();
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let _ = watching.read_more(Instant::now() + PATIENCE, |seen| {
            seen.iter()
                .any(|line| line.contains(DONE) || line.contains(FAILED))
        })?;
        lines = watching.lines().to_vec();
        lines.extend_from_slice(watching.after());
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, SPOKE, hook)?;
    Ok(lines)
}

/// The frames of a WAV file's data, as `(left, right)`, whatever its header
/// says of its length: a QEMU stopped before it closed the file leaves the
/// lengths zero.
fn frames(wav: &[u8]) -> Result<Vec<(u16, u16)>> {
    let data = wav
        .windows(4)
        .position(|window| window == b"data")
        .ok_or_else(|| Error::new("the WAV file has no data chunk".to_owned()))?;
    let samples = wav.get(data + 8..).unwrap_or(&[]);
    let (whole, _) = samples.as_chunks::<4>();
    Ok(whole
        .iter()
        .map(|&[a, b, c, d]| (u16::from_le_bytes([a, b]), u16::from_le_bytes([c, d])))
        .collect())
}

/// Where the played frames stop matching the counter, as the counter's frame
/// number, or `None` when they are frames 1 to [`FRAMES`] exactly, leading
/// and trailing silence aside.
fn first_wrong(played: &[(u16, u16)]) -> Option<u32> {
    let silent = |frame: &(u16, u16)| *frame == (0, 0);
    let start = played
        .iter()
        .position(|frame| !silent(frame))
        .unwrap_or(played.len());
    let end = played
        .iter()
        .rposition(|frame| !silent(frame))
        .map_or(start, |at| at + 1);
    let heard = played.get(start..end).unwrap_or(&[]);
    for n in 1..=FRAMES {
        let expected = (n as u16, !(n as u16));
        match heard.get((n - 1) as usize) {
            Some(frame) if *frame == expected => {}
            _ => return Some(n),
        }
    }
    (heard.len() != FRAMES as usize).then_some(FRAMES + 1)
}

/// What the file holds at the frame where it went wrong, for the message.
fn describe(played: &[(u16, u16)], at: u32) -> String {
    let start = played
        .iter()
        .position(|frame| *frame != (0, 0))
        .unwrap_or(0);
    match played.get(start + (at as usize).saturating_sub(1)) {
        Some((left, right)) => format!("left {left} right {right}"),
        None => "nothing".to_owned(),
    }
}

/// `test-audio` on each architecture asked for.
///
/// # Errors
///
/// A card or stream that refused a request, frames missing, repeated or
/// changed in the file, or a negative control that passed.
pub(crate) fn test_audio(args: &Args) -> Result<()> {
    for arch in args.arches()? {
        let wav = paths::build_dir(arch).join("audio.wav");
        let plain = build_tone(arch, Flavour::Plain)?;
        let lines = boot_and_play(arch, &plain, &wav, args)?;
        played_whole(arch, &lines, &wav)?;
        println!("  {arch}: all {FRAMES} frames written reached the device whole and in order");

        let negative = build_tone(arch, Flavour::Negative)?;
        let lines = boot_and_play(arch, &negative, &wav, args)?;
        if !lines.iter().any(|line| line.contains(DONE)) {
            return Err(Error::new(format!(
                "{arch}: the negative control never finished: it proves nothing about the check"
            )));
        }
        let bytes = std::fs::read(&wav)
            .map_err(|error| Error::new(format!("{}: {error}", wav.display())))?;
        match first_wrong(&frames(&bytes)?) {
            Some(at) if at == 11 * PERIOD + 1 => println!(
                "  {arch}: the negative control's moved period was found at frame {at}, and \
                 failed the check"
            ),
            Some(at) => {
                return Err(Error::new(format!(
                    "{arch}: the negative control failed at frame {at}, not at {}",
                    11 * PERIOD + 1
                )));
            }
            None => {
                return Err(Error::new(format!(
                    "{arch}: the negative control passed the check it is built to fail"
                )));
            }
        }

        let restart = build_tone(arch, Flavour::Restart)?;
        let lines = boot_and_play(arch, &restart, &wav, args)?;
        played_whole(arch, &lines, &wav)?;
        let restarts = lines.iter().filter(|line| line.contains(RESTARTED)).count();
        if restarts < 2 {
            return Err(Error::new(format!(
                "{arch}: devmgr said `{RESTARTED}` {restarts} times, not 2"
            )));
        }
        quarantined(arch, &lines)?;
        println!(
            "  {arch}: the sound driver was killed twice under a running stream, the stream \
             answered EBADFD each time, devmgr started the driver again, and the third one's \
             card played all {FRAMES} frames whole"
        );
    }
    Ok(())
}

/// What devmgr's word of a driver started again says, printed by the kernel.
const RESTARTED: &str = "was started again and published";

/// What the kernel says as a dead driver's quarantined pins go back
/// (`kernel/src/object/pin.rs`), before the count of pages written late.
const RELEASED: &str = "pages a dead driver's device could still write went back";

/// Require, on an architecture whose card is behind a translating IOMMU,
/// that each death's pins went to the quarantine and back, and that QEMU
/// wrote at least one of their pages after the driver had died: the writes
/// that, before the quarantine, landed in frames the allocator had given to
/// the next driver (`docs/AUDIO.md` §3.3). ARMv7-A's card is behind none, and
/// an untranslated domain keeps a dead driver's pins for good instead.
fn quarantined(arch: Arch, lines: &[String]) -> Result<()> {
    if arch == Arch::Armv7a {
        return Ok(());
    }
    let late: Vec<usize> = lines
        .iter()
        .filter_map(|line| {
            let (_, rest) = line.split_once(RELEASED)?;
            let (_, count) = rest.split_once(", ")?;
            count.split_whitespace().next()?.parse().ok()
        })
        .collect();
    if late.len() < 2 {
        return Err(Error::new(format!(
            "{arch}: the kernel said `{RELEASED}` {} times, not once for each of the 2 deaths",
            late.len()
        )));
    }
    let written: usize = late.iter().sum();
    if written == 0 {
        return Err(Error::new(format!(
            "{arch}: no quarantined page was written after its driver died, so this boot \
             no longer shows what the quarantine is for"
        )));
    }
    println!(
        "  {arch}: each dead driver's pins were quarantined until the next driver had reset \
         the card, and the device wrote {written} of their pages after its driver had died"
    );
    Ok(())
}

/// Require that tone finished and that the file holds the counter whole.
fn played_whole(arch: Arch, lines: &[String], wav: &Path) -> Result<()> {
    if let Some(line) = lines.iter().find(|line| line.contains(FAILED)) {
        return Err(Error::new(format!("{arch}: {}", line.trim())));
    }
    if !lines.iter().any(|line| line.contains(DONE)) {
        return Err(Error::new(format!("{arch}: tone never finished its drain")));
    }
    for line in lines.iter().filter(|line| line.contains("tone: ")) {
        println!("  {arch}: {}", line.trim());
    }
    let bytes =
        std::fs::read(wav).map_err(|error| Error::new(format!("{}: {error}", wav.display())))?;
    let played = frames(&bytes)?;
    if let Some(at) = first_wrong(&played) {
        return Err(Error::new(format!(
            "{arch}: the file QEMU wrote parts from the counter at frame {at} (it holds {}), \
             {} frames in all; {}",
            describe(&played, at),
            played.len(),
            wav.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FRAMES, PERIOD, first_wrong, frames, quarantined};
    use crate::paths::Arch;

    #[test]
    fn the_quarantine_must_have_caught_a_late_write_where_a_unit_translates() {
        let line = |written: usize| {
            format!(
                "    8.53 |   iommu    20 pages a dead driver's device could still write went \
                 back once its next driver had reset it, {written} of them written after it died"
            )
        };
        assert!(quarantined(Arch::X86_64, &[line(2), line(0)]).is_ok());
        assert!(quarantined(Arch::AArch64, &[line(0), line(0)]).is_err());
        assert!(
            quarantined(Arch::X86_64, &[line(3)]).is_err(),
            "one death of two"
        );
        assert!(
            quarantined(Arch::Armv7a, &[]).is_ok(),
            "no unit, no quarantine"
        );
    }

    fn counter(moved: bool) -> Vec<(u16, u16)> {
        (1..=FRAMES)
            .map(|n| {
                let n = if moved && (11 * PERIOD + 1..=12 * PERIOD).contains(&n) {
                    n - PERIOD
                } else {
                    n
                };
                (n as u16, !(n as u16))
            })
            .collect()
    }

    #[test]
    fn the_counter_passes_with_silence_around_it_and_fails_where_it_is_wrong() {
        let mut played = vec![(0, 0); 100];
        played.extend(counter(false));
        played.extend([(0, 0); 50]);
        assert_eq!(first_wrong(&played), None);
        assert_eq!(first_wrong(&counter(true)), Some(11 * PERIOD + 1));
        let mut short = counter(false);
        let _ = short.pop();
        assert_eq!(first_wrong(&short), Some(FRAMES));
        let mut repeated = counter(false);
        repeated.insert(500, repeated[499]);
        assert_eq!(first_wrong(&repeated), Some(501));
    }

    #[test]
    fn a_wav_files_frames_are_read_from_its_data_chunk() {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.extend([0; 20]);
        wav.extend(b"data\0\0\0\0");
        wav.extend([1, 0, 0xfe, 0xff, 2, 0, 0xfd, 0xff]);
        assert_eq!(frames(&wav).unwrap(), [(1, 0xfffe), (2, 0xfffd)]);
    }
}
