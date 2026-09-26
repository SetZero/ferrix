//! `cargo xtask test-badapple`: Bad Apple!! on the screen and in the
//! speaker, and both found again -- the picture in a screendump, the song in
//! the file QEMU wrote of what the sound card played.
//!
//! `userland/media/badapple` runs as init with a virtio-gpu and a
//! virtio-snd whose far end is QEMU's `wav` backend. It decodes the song's
//! AAC track itself, converts it to 48 kHz and plays it; it shows the video,
//! converted by the host into `.bav`, frame by frame by the sound card's
//! clock. After [`SECONDS`] it holds the last frame it showed and says which.
//! Four checks:
//!
//! * **The picture.** The screendump is compared, pixel for pixel, with that
//!   frame decoded on the host by `bav-pack frame`: every picture pixel,
//!   sampled where the player's scaling put it, must be exactly its shade of
//!   grey.
//! * **The song.** What QEMU wrote is compared with ffmpeg's own decoding of
//!   the same track at 48 kHz, two seconds at a time: each window must
//!   correlate with the reference at some small lag. A resampler other than
//!   ffmpeg's never matches sample for sample, so the check is correlation,
//!   not equality; a wrong song, a wrong rate or noise does not correlate.
//! * **The sync.** Every ten seconds of video the player says which frame it
//!   showed and where the song was; the two must be close.
//! * **The negative control.** The same boot with the player built with
//!   `negative-control`, which shows every shade inverted: the picture check
//!   must fail on every pixel.
//!
//! The video is not in the repository: `scripts/fetch/fetch-badapple.sh`
//! downloads the original upload, whose hash is [`SOURCE_SHA256`], and this
//! converts it once, with ffmpeg and `bav-pack`, into the directory it was
//! fetched to.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::display::{DEVICE_ID, Image, Qmp, free_port, parse_ppm};
use crate::paths::{self, Arch};
use crate::qemu::Watching;
use crate::{Error, Result};

/// What the player prints once the song and the screen are open.
const READY: &str = "badapple: ready";
/// What it prints with the frame it holds at the end.
const HOLDING: &str = "badapple: holding frame ";
/// What it prints when anything was refused.
const FAILED: &str = "badapple: failed";
/// Its summary line.
const DONE: &str = "badapple: done";

/// How much of the video the gate plays.
const SECONDS: u32 = 30;
/// How much the negative control plays: enough to hold a frame.
const NEGATIVE_SECONDS: u32 = 3;

/// The original upload (niconico sm8628149, 2009), as archive.org keeps it.
const SOURCE: &str = "sm8628149.mp4";
/// Its SHA-256, which `scripts/fetch/fetch-badapple.sh` checks too.
const SOURCE_SHA256: &str = "75d2261d1f75da80a3ba899641def55230c5f8564a310df3cbbe88b8c0ea2abb";
/// Snap the near-black and near-white to black and white before packing:
/// the source's compression noise in the flat areas would otherwise be most
/// of the file.
const FILTER: &str = "lutyuv=y='if(lt(val,48),0,if(gt(val,208),255,val))'";
/// Shades a pixel may drift from the source before it is repainted.
const TOLERANCE: u8 = 1;
/// The picture, as ffmpeg decodes the source.
const WIDTH: u16 = 512;
const HEIGHT: u16 = 384;
const RATE: u16 = 30;

/// Where the files go in the initramfs.
const GUEST_VIDEO: &str = "usr/share/badapple/badapple.bav";
const GUEST_SONG: &str = "usr/share/badapple/badapple.m4a";

/// The card's rate.
const SAMPLE_RATE: usize = 48_000;
/// How far apart the frame a progress line names and the song may be.
const SKEW: u64 = 200;
/// How well each window of the song must match the reference.
const CORRELATION: f64 = 0.9;

/// Where the fetched video is: `$FERRIX_BADAPPLE`, or
/// `~/.local/share/ferrix/badapple`.
fn media_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("FERRIX_BADAPPLE") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| Error::new("neither HOME nor USERPROFILE is set"))?;
    Ok(PathBuf::from(home).join(".local/share/ferrix/badapple"))
}

/// The fetched source, checked.
fn source(dir: &Path) -> Result<PathBuf> {
    let path = dir.join(SOURCE);
    let bytes = std::fs::read(&path).map_err(|error| {
        Error::new(format!(
            "{}: {error}; scripts/fetch/fetch-badapple.sh fetches it",
            path.display()
        ))
    })?;
    let sum = crate::sha256::hex(&bytes);
    if sum != SOURCE_SHA256 {
        return Err(Error::new(format!(
            "{} has SHA-256 {sum}, not {SOURCE_SHA256}; fetch it again",
            path.display()
        )));
    }
    Ok(path)
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |error| Error::new(format!("{}: {error}", path.display()))
}

/// Build `bav-pack` for the host.
fn pack_tool() -> Result<PathBuf> {
    let target_dir = paths::target_dir().join("media").join("host");
    let program = target_dir.join("release").join("bav-pack");
    crate::builds::Build::cargo(
        "cargo build (userland/media, bav-pack) for the host",
        paths::workspace_root().join("userland/media"),
    )
    .args(["build", "--release", "-p", "media-bav", "--bin", "bav-pack"])
    .env("CARGO_TARGET_DIR", &target_dir)
    .output(&program)
    .run()?;
    Ok(program)
}

/// The video as `.bav` and the song as its own MP4, made from the source the
/// first time and kept beside it.
fn prepared(dir: &Path, source: &Path, tool: &Path) -> Result<(PathBuf, PathBuf)> {
    let recipe = format!("{SOURCE_SHA256} {FILTER} {TOLERANCE} {WIDTH}x{HEIGHT}@{RATE}");
    let tag = crate::sha256::hex(recipe.as_bytes());
    let video = dir.join(format!("badapple-{}.bav", tag.get(..12).unwrap_or(&tag)));
    if !video.is_file() {
        println!("  converting {} into {}", source.display(), video.display());
        let partial = video.with_extension("bav.part");
        let decode = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(source)
            .args([
                "-an", "-vf", FILTER, "-f", "rawvideo", "-pix_fmt", "gray", "-",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| Error::new(format!("ffmpeg: {error}; install ffmpeg")))?;
        let stdout = decode
            .stdout
            .ok_or_else(|| Error::new("ffmpeg gave no output pipe"))?;
        let packed = Command::new(tool)
            .args([
                WIDTH.to_string(),
                HEIGHT.to_string(),
                RATE.to_string(),
                "1".to_owned(),
                TOLERANCE.to_string(),
            ])
            .arg(&partial)
            .stdin(stdout)
            .output()
            .map_err(io(tool))?;
        if !packed.status.success() {
            return Err(Error::new(format!(
                "bav-pack: {}",
                String::from_utf8_lossy(&packed.stderr).trim()
            )));
        }
        println!("  {}", String::from_utf8_lossy(&packed.stdout).trim());
        std::fs::rename(&partial, &video).map_err(io(&video))?;
    }
    let song = dir.join("badapple.m4a");
    if !song.is_file() {
        let partial = dir.join("badapple.part.m4a");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(source)
            .args(["-vn", "-c:a", "copy"])
            .arg(&partial)
            .status()
            .map_err(|error| Error::new(format!("ffmpeg: {error}; install ffmpeg")))?;
        if !status.success() {
            return Err(Error::new("ffmpeg could not copy out the song".to_owned()));
        }
        std::fs::rename(&partial, &song).map_err(io(&song))?;
    }
    Ok((video, song))
}

/// Build `userland/media/badapple` for `arch`, with the negative control or
/// without.
fn build_player(arch: Arch, negative: bool) -> Result<PathBuf> {
    let target = crate::display::target(arch)
        .ok_or_else(|| Error::new(format!("{arch} has no user-space target for badapple")))?;
    let flavour = if negative { "negative" } else { "plain" };
    let target_dir = paths::target_dir()
        .join("media")
        .join(format!("badapple-{flavour}"));
    println!("  building userland/media/badapple ({flavour}) for {target}");
    let program = target_dir.join(target).join("release").join("badapple");
    let mut build = crate::builds::Build::cargo(
        format!("cargo build (userland/media/badapple, {flavour}) --target {target}"),
        paths::workspace_root().join("userland/media"),
    )
    .args([
        "build",
        "--release",
        "-p",
        "media-badapple",
        "--target",
        target,
    ])
    .env("CARGO_TARGET_DIR", &target_dir)
    .output(&program);
    if negative {
        build = build.args(["--features", "negative-control"]);
    }
    build.run()?;
    Ok(program)
}

/// What one boot left behind.
struct Played {
    lines: Vec<String>,
    /// The frame the player held, and the screen then.
    held: Option<(u32, Image)>,
}

/// Boot the player with the files, a screen and a card whose far end writes
/// `wav`; once it holds a frame, dump the screen.
fn boot(
    arch: Arch,
    player: &Path,
    files: &[crate::ports::File],
    seconds: u32,
    wav: &Path,
    args: &Args,
) -> Result<Played> {
    let loader = crate::cargo::build_loader(arch, args.release)?;
    let script = format!("/{GUEST_VIDEO}\n/{GUEST_SONG}\n{seconds}\n");
    let kernel = crate::cargo::build_kernel_with_init(arch, args.release, player, &script)?;
    let natives = crate::native::build(arch, args.release)?;
    let initramfs = crate::initramfs::build(None, &natives, None, files)?;
    let image = crate::fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
    if wav.exists() {
        std::fs::remove_file(wav).map_err(io(wav))?;
    }
    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.audio = Some(format!("wav:{}", wav.display()));
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    // The initramfs holds the video, and is unpacked into memory beside
    // itself.
    if !qemu_args.memory_given {
        qemu_args.memory = qemu_args.memory.max(1024);
    }
    let dump = paths::build_dir(arch).join("badapple.ppm");
    let patience = Duration::from_secs(u64::from(seconds) + 120);
    let mut played = Played {
        lines: Vec::new(),
        held: None,
    };
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        let _ = watching.read_more(Instant::now() + patience, |seen| {
            seen.iter()
                .any(|line| line.contains(HOLDING) || line.contains(FAILED))
        })?;
        let every: Vec<String> = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        let held = every
            .iter()
            .find_map(|line| line.split(HOLDING).nth(1)?.trim().parse::<u32>().ok());
        if let Some(frame) = held {
            qmp.screendump(Some(DEVICE_ID), &dump)?;
            let bytes = std::fs::read(&dump).map_err(io(&dump))?;
            played.held = Some((frame, parse_ppm(&bytes)?));
        }
        played.lines = every;
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, READY, hook)?;
    Ok(played)
}

/// Where the player put the picture: `fit X Y W H` in its screen line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fit {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

fn fit(lines: &[String]) -> Option<Fit> {
    let line = lines
        .iter()
        .find(|line| line.contains("badapple: screen "))?;
    let mut words = line
        .split_whitespace()
        .skip_while(|word| *word != "fit")
        .skip(1);
    let mut number = || words.next()?.parse::<usize>().ok();
    Some(Fit {
        x: number()?,
        y: number()?,
        width: number()?,
        height: number()?,
    })
}

/// The picture pixels whose place on `screen` does not show their shade,
/// out of those checked, and the first few, described.
fn picture_mismatches(screen: &Image, fit: Fit, shades: &[u8]) -> (usize, usize, Vec<String>) {
    let (width, height) = (usize::from(WIDTH), usize::from(HEIGHT));
    let (mut wrong, mut checked, mut first) = (0, 0, Vec::new());
    for sy in 0..height {
        // The first screen row showing picture row `sy`, as the player's
        // nearest-pixel scaling places it.
        let row = (sy * fit.height).div_ceil(height);
        if row * height / fit.height != sy {
            continue;
        }
        for sx in 0..width {
            let column = (sx * fit.width).div_ceil(width);
            if column * width / fit.width != sx {
                continue;
            }
            let (x, y) = (fit.x + column, fit.y + row);
            let Some(shade) = shades.get(sy * width + sx) else {
                continue;
            };
            let grey = shade.saturating_mul(17);
            let at = (y * screen.width + x) * 3;
            let shown = screen.pixels.get(at..at + 3);
            checked += 1;
            if shown != Some(&[grey, grey, grey][..]) {
                wrong += 1;
                if first.len() < 5 {
                    first.push(format!("({sx},{sy}) shade {shade} shown as {shown:?}"));
                }
            }
        }
    }
    (wrong, checked, first)
}

/// Frame `n` of `video`, decoded on the host.
fn reference_frame(tool: &Path, video: &Path, n: u32, arch: Arch) -> Result<Vec<u8>> {
    let out = paths::build_dir(arch).join("badapple-frame.raw");
    let decoded = Command::new(tool)
        .arg("frame")
        .arg(video)
        .arg(n.to_string())
        .arg(&out)
        .output()
        .map_err(io(tool))?;
    if !decoded.status.success() {
        return Err(Error::new(format!(
            "bav-pack frame: {}",
            String::from_utf8_lossy(&decoded.stderr).trim()
        )));
    }
    std::fs::read(&out).map_err(io(&out))
}

/// The first `seconds` of the song as ffmpeg decodes it at 48 kHz: mono.
fn reference_song(song: &Path, seconds: u32) -> Result<Vec<f64>> {
    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(song)
        .args([
            "-t",
            &seconds.to_string(),
            "-ar",
            "48000",
            "-ac",
            "2",
            "-f",
            "s16le",
            "-",
        ])
        .output()
        .map_err(|error| Error::new(format!("ffmpeg: {error}")))?;
    if !decoded.status.success() {
        return Err(Error::new("ffmpeg could not decode the song".to_owned()));
    }
    Ok(mono(&decoded.stdout))
}

/// Interleaved stereo `S16_LE` as mono.
fn mono(bytes: &[u8]) -> Vec<f64> {
    let (frames, _) = bytes.as_chunks::<4>();
    frames
        .iter()
        .map(|&[a, b, c, d]| {
            (f64::from(i16::from_le_bytes([a, b])) + f64::from(i16::from_le_bytes([c, d]))) / 2.0
        })
        .collect()
}

/// A WAV file's samples as mono, whatever its header says of its length.
fn wav_mono(wav: &[u8]) -> Result<Vec<f64>> {
    let data = wav
        .windows(4)
        .position(|window| window == b"data")
        .ok_or_else(|| Error::new("the WAV file has no data chunk".to_owned()))?;
    Ok(mono(wav.get(data + 8..).unwrap_or(&[])))
}

/// Frames to average into one: the correlation looks at 6 kHz.
const DECIMATE: usize = 8;
/// Decimated samples in a window: two seconds.
const WINDOW: usize = 2 * SAMPLE_RATE / DECIMATE;
/// How far a window may have moved from the one before, in decimated
/// samples: 100 ms, an underrun's gap or two.
const WANDER: i64 = (SAMPLE_RATE / DECIMATE / 10) as i64;

/// `samples` from the first sound on, averaged in blocks of [`DECIMATE`].
fn prepared_signal(samples: &[f64]) -> Vec<f64> {
    let start = samples
        .iter()
        .position(|sample| sample.abs() > 64.0)
        .unwrap_or(samples.len());
    samples
        .get(start..)
        .unwrap_or(&[])
        .chunks_exact(DECIMATE)
        .map(|block| block.iter().sum::<f64>() / DECIMATE as f64)
        .collect()
}

/// For each window of `heard`, the lag into `reference` it matched best at
/// and how well: `(window start in seconds, lag in ms, correlation)`.
fn windows(heard: &[f64], reference: &[f64]) -> Vec<(f64, i64, f64)> {
    let mut found = Vec::new();
    let mut lag = 0_i64;
    let mut start = 0;
    while start + WINDOW <= heard.len() {
        let piece = heard.get(start..start + WINDOW).unwrap_or(&[]);
        let mut best: Option<(i64, f64)> = None;
        for candidate in lag - WANDER..=lag + WANDER {
            let Some(at) = (start as i64 + candidate).try_into().ok() else {
                continue;
            };
            let at: usize = at;
            let Some(other) = reference.get(at..at + WINDOW) else {
                continue;
            };
            let score = correlation(piece, other);
            if best.is_none_or(|(_, most)| score > most) {
                best = Some((candidate, score));
            }
        }
        let Some((at, score)) = best else {
            break;
        };
        lag = at;
        found.push((
            start as f64 / (SAMPLE_RATE / DECIMATE) as f64,
            lag * 1000 / (SAMPLE_RATE / DECIMATE) as i64,
            score,
        ));
        start += WINDOW;
    }
    found
}

/// Pearson's correlation of two equal-length signals; 0 for silence.
fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len()) as f64;
    if n == 0.0 {
        return 0.0;
    }
    let (mean_a, mean_b) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (x - mean_a, y - mean_b);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    // Either signal flat: nothing to correlate. Energies are sums of
    // squares, so "flat" is "no larger than nothing".
    if aa <= 0.0 || bb <= 0.0 {
        0.0
    } else {
        ab / (aa * bb).sqrt()
    }
}

/// The progress lines' `(frame's time, song's time)`, in ms.
fn progress(lines: &[String]) -> Vec<(u64, u64)> {
    lines
        .iter()
        .filter_map(|line| {
            let rest = line.split("badapple: frame ").nth(1)?;
            let words: Vec<&str> = rest.split_whitespace().collect();
            // `N at T ms, song at S ms`
            let t = words.get(2)?.parse().ok()?;
            let s = words.get(6)?.parse().ok()?;
            Some((t, s))
        })
        .collect()
}

fn show(arch: Arch, lines: &[String]) {
    for line in lines
        .iter()
        .filter(|line| line.contains("badapple: ") || line.contains("underrun"))
    {
        println!("  {arch}: {}", line.trim());
    }
}

/// What every boot on an architecture shares.
struct Inputs {
    files: Vec<crate::ports::File>,
    tool: PathBuf,
    video: PathBuf,
    /// The song as ffmpeg hears it, prepared as the heard song will be.
    reference: Vec<f64>,
}

/// The fetched video converted, the files the guest is given, and, when a
/// gate will listen for it, the first `seconds` of the song as ffmpeg hears
/// it.
fn inputs(seconds: Option<u32>) -> Result<Inputs> {
    let dir = media_dir()?;
    let source = source(&dir)?;
    let tool = pack_tool()?;
    let (video, song) = prepared(&dir, &source, &tool)?;
    let file = |guest: &str, host: &Path| -> Result<crate::ports::File> {
        Ok(crate::ports::File {
            path: guest.to_owned(),
            mode: 0o644,
            content: crate::ports::Content::Bytes(std::fs::read(host).map_err(io(host))?),
        })
    };
    let reference = match seconds {
        Some(seconds) => prepared_signal(&reference_song(&song, seconds)?),
        None => Vec::new(),
    };
    Ok(Inputs {
        files: vec![file(GUEST_VIDEO, &video)?, file(GUEST_SONG, &song)?],
        reference,
        tool,
        video,
    })
}

/// `run-badapple`: the whole video in a window, heard on this host's sound
/// server, with the serial port on this terminal.
///
/// # Errors
///
/// No fetched video, or no QEMU for the architecture.
pub(crate) fn run_badapple(args: &Args) -> Result<()> {
    let arch = args.single_arch()?;
    if crate::display::target(arch).is_none() {
        return Err(Error::new(format!(
            "{arch} has no virtio-gpu in QEMU's machine"
        )));
    }
    let inputs = inputs(None)?;
    let player = build_player(arch, false)?;
    let loader = crate::cargo::build_loader(arch, args.release)?;
    let script = format!("/{GUEST_VIDEO}\n/{GUEST_SONG}\n");
    let kernel = crate::cargo::build_kernel_with_init(arch, args.release, &player, &script)?;
    let natives = crate::native::build(arch, args.release)?;
    let initramfs = crate::initramfs::build(None, &natives, None, &inputs.files)?;
    let image = crate::fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
    // The host's own sound server, as `run-compositor --everything` finds
    // it, unless `--audio` names a backend.
    let audio = args.audio.clone().or_else(|| {
        crate::window::qemu_for(arch.qemu_binary(), args.gl)
            .and_then(|binary| crate::window::audio_backend(&binary))
            .map(str::to_owned)
    });
    if audio.is_none() {
        println!("  {arch}: no sound server QEMU knows; name one with --audio");
    }
    let memory = if args.memory_given {
        args.memory
    } else {
        args.memory.max(1024)
    };
    let args = Args {
        display: true,
        audio,
        memory,
        accel: args
            .accel
            .clone()
            .or_else(|| (!args.gdb).then(|| "auto".to_owned())),
        ..args.clone()
    };
    println!("  {arch}: Bad Apple!! is init; its log is this terminal");
    crate::qemu::run(arch, &image, &args)
}

/// `test-badapple` on each architecture asked for.
///
/// # Errors
///
/// No fetched video, a player that failed, a screen that did not show the
/// frame held, a song not heard, a picture out of step with the song, or a
/// negative control that passed.
pub(crate) fn test_badapple(args: &Args) -> Result<()> {
    let inputs = inputs(Some(SECONDS))?;
    for arch in args.arches()? {
        if crate::display::target(arch).is_none() {
            println!("  {arch}: no virtio-gpu in QEMU's machine; skipped");
            continue;
        }
        let wav = paths::build_dir(arch).join("badapple.wav");
        let player = build_player(arch, false)?;
        let played = boot(arch, &player, &inputs.files, SECONDS, &wav, args)?;
        show(arch, &played.lines);
        if let Some(line) = played.lines.iter().find(|line| line.contains(FAILED)) {
            return Err(Error::new(format!("{arch}: {}", line.trim())));
        }
        if !played.lines.iter().any(|line| line.contains(DONE)) {
            return Err(Error::new(format!("{arch}: the player never finished")));
        }
        check_picture(arch, &inputs, played)?;
        check_song(arch, &inputs, &wav)?;

        let negative = build_player(arch, true)?;
        let played = boot(arch, &negative, &inputs.files, NEGATIVE_SECONDS, &wav, args)?;
        check_negative(arch, &inputs, played)?;
    }
    Ok(())
}

/// The frame held is on the screen exactly, and the picture kept up with
/// the song.
fn check_picture(arch: Arch, inputs: &Inputs, played: Played) -> Result<()> {
    let fit = fit(&played.lines).ok_or_else(|| {
        Error::new(format!(
            "{arch}: the player never said where the picture went"
        ))
    })?;
    let (frame, screen) = played
        .held
        .ok_or_else(|| Error::new(format!("{arch}: the player never held a frame")))?;
    let shades = reference_frame(&inputs.tool, &inputs.video, frame, arch)?;
    let (wrong, checked, first) = picture_mismatches(&screen, fit, &shades);
    if wrong > 0 || checked == 0 {
        return Err(Error::new(format!(
            "{arch}: the screen showed frame {frame} wrong at {wrong} of {checked} pixels: {}",
            first.join("; ")
        )));
    }
    println!("  {arch}: the screen showed frame {frame} exactly, all {checked} pixels");

    let steps = progress(&played.lines);
    let worst = steps.iter().map(|(t, s)| t.abs_diff(*s)).max().unwrap_or(0);
    if steps.is_empty() || worst > SKEW {
        return Err(Error::new(format!(
            "{arch}: the picture and the song were {worst} ms apart at worst over {} \
             progress lines; {SKEW} ms is allowed",
            steps.len()
        )));
    }
    println!(
        "  {arch}: picture and song within {worst} ms of each other at {} checks",
        steps.len()
    );
    Ok(())
}

/// Every two seconds of what the card played is the song.
fn check_song(arch: Arch, inputs: &Inputs, wav: &Path) -> Result<()> {
    let bytes = std::fs::read(wav).map_err(io(wav))?;
    let heard = prepared_signal(&wav_mono(&bytes)?);
    let found = windows(&heard, &inputs.reference);
    let expected = (SECONDS as usize - 1) * SAMPLE_RATE / DECIMATE / WINDOW;
    let poor: Vec<String> = found
        .iter()
        .filter(|(_, _, score)| *score < CORRELATION)
        .map(|(at, lag, score)| format!("{at:.0} s: {score:.3} at {lag} ms"))
        .collect();
    if found.len() < expected || !poor.is_empty() {
        return Err(Error::new(format!(
            "{arch}: the song was not heard: {} of {expected} two-second windows found, \
             below {CORRELATION}: {}; {}",
            found.len(),
            poor.join(", "),
            wav.display()
        )));
    }
    let least = found.iter().map(|(_, _, score)| *score).fold(1.0, f64::min);
    let lags: Vec<i64> = found.iter().map(|(_, lag, _)| *lag).collect();
    println!(
        "  {arch}: the song was heard: {} windows, correlation at least {least:.3}, lags {lags:?} ms",
        found.len()
    );
    Ok(())
}

/// The inverted frame fails the picture check at every pixel.
fn check_negative(arch: Arch, inputs: &Inputs, played: Played) -> Result<()> {
    let fit = fit(&played.lines);
    let Some(((frame, screen), fit)) = played.held.zip(fit) else {
        return Err(Error::new(format!(
            "{arch}: the negative control never held a frame: it proves nothing about the check"
        )));
    };
    let shades = reference_frame(&inputs.tool, &inputs.video, frame, arch)?;
    let (wrong, checked, _) = picture_mismatches(&screen, fit, &shades);
    if wrong < checked || checked == 0 {
        return Err(Error::new(format!(
            "{arch}: the negative control's inverted frame {frame} passed the check at {} of \
             {checked} pixels",
            checked - wrong
        )));
    }
    println!("  {arch}: the negative control's inverted frame {frame} failed at every pixel");
    Ok(())
}

#[cfg(test)]
mod tests;
