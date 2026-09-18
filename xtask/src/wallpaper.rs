//! The picture `cargo xtask run-compositor` puts behind the desktop, and
//! `cargo xtask wallpapers`, which is how pictures get to where it looks.
//!
//! A desktop somebody is looking at has a wallpaper. Ferrix has no JPEG
//! decoder and no video decoder, and carrying either to show one picture is
//! the wrong trade; what it has is `compositor/pattern --wallpaper`, a
//! background layer surface that shows raw `XRGB8888` rows behind a
//! twelve-byte header. So a picture is converted on a machine that can, once,
//! and kept.
//!
//! A video is kept the same way and played rather than shown:
//! `--video` on the same surface, which is the `mpvpaper` line of a Linux
//! desktop's `hyprland.conf` with the decoding moved to the other side of the
//! conversion. Its frames are run-length encoded against the frame before
//! them ([`encode_moving`]), because a second of 1920x1080 is half a
//! gigabyte and the initramfs is built into the kernel.
//!
//! Two halves, and the line between them is the network.
//!
//! * `run-compositor` reads **this machine's** wallpapers directory --
//!   `$FERRIX_WALLPAPERS`, or `~/.local/share/ferrix/wallpapers` -- and
//!   nothing else. It names no host and opens no connection, so it does the
//!   same thing on a train as at a desk; with nothing there the background
//!   is plain and a line says how to change that. None of it can fail a
//!   boot.
//! * `cargo xtask wallpapers --from <where>` fills that directory, and is
//!   the only thing here that may reach another machine -- the one its
//!   argument names, which nothing in this file does. `<where>` is a
//!   directory of this machine's, converted by its `ffmpeg`, or
//!   `host:directory`, converted by that host's over `ssh`: the pictures are
//!   made where the pictures are, and what crosses the wire is the rows.
//!   `ffmpeg` scales a picture until it covers the screen and cuts the
//!   overflow evenly; a video it also takes at a frame rate and a length,
//!   and what is kept is those frames.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::args::Args;
use crate::{Error, Result};

/// The variable that names the wallpapers directory, where it is not the
/// usual one.
const VAR: &str = "FERRIX_WALLPAPERS";

/// The usual one, under the home directory.
const KEPT: &[&str] = &[".local", "share", "ferrix", "wallpapers"];

/// The screen a picture is cut for when `wallpapers` is not told another.
pub(crate) const SCREEN: (u32, u32) = (1920, 1080);

/// What a picture's file begins with: `compositor_pattern::Picture::MAGIC`.
const MAGIC: &[u8; 8] = b"FXWALL1\n";

/// What a kept picture's name ends in.
const KIND: &str = ".fxwall";

/// What a moving wallpaper's file begins with:
/// `compositor_pattern::Movie::MAGIC`.
const MOVIE_MAGIC: &[u8; 8] = b"FXVID01\n";

/// What a kept video's name ends in.
const MOVIE_KIND: &str = ".fxvid";

/// What is kept as a wallpaper that moves rather than as its first frame.
/// The rest of [`KINDS`] is a still picture however many frames it has.
const MOVING: [&str; 4] = ["mp4", "webm", "mkv", "gif"];

/// How much smaller than the screen a moving wallpaper's frames are kept.
///
/// A frame of 1920x1080 is 8.3 MB, and a second of them is half a gigabyte:
/// an initramfs is built into the kernel here, so a wallpaper that size is
/// not a wallpaper, it is the image. A quarter each way is 480x270, which
/// the client scales up to cover the screen the way it scales any picture cut
/// for another screen -- behind a blurred, translucent desktop, which is what
/// a wallpaper is behind, that is what it looks like on the screen it came
/// from.
const MOVING_SMALLER: u32 = 4;

/// How many frames a second of a video are kept.
///
/// Hyprland's own answer to this is `mpvpaper`, which plays the file at
/// whatever rate it was made at because it has mpv behind it. This has the
/// frames themselves, so the rate is a size: ten is motion, and twenty-five
/// is two and a half times the initramfs.
const MOVING_RATE: u32 = 10;

/// How many seconds of a video are kept, after which it begins again.
const MOVING_SECONDS: u32 = 4;

/// What `ffmpeg` can take a frame from, among what a pictures directory
/// holds.
const KINDS: [&str; 9] = [
    "jpg", "jpeg", "png", "webp", "gif", "bmp", "mp4", "webm", "mkv",
];

/// The file `compositor/pattern --wallpaper` is given: `--wallpaper <name>`
/// if one was asked for, and otherwise one of the kept pictures, a different
/// one from run to run. One cut for a screen of `size` where there is one,
/// since the client copies that and scales any other.
///
/// `None`, having said why, when there is to be none: `--wallpaper none`, or
/// nothing kept.
pub(crate) fn file(args: &Args, size: (u32, u32)) -> Option<Chosen> {
    let asked = args.wallpaper.as_deref();
    if asked == Some("none") {
        return None;
    }
    let dir = kept_dir()?;
    let mut kept: Vec<String> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(KIND) || name.ends_with(MOVIE_KIND))
        .collect();
    kept.sort();
    let cut = [
        format!(".{}x{}{KIND}", size.0, size.1),
        format!(".{}x{}{MOVIE_KIND}", size.0, size.1),
    ];
    let for_this_screen = |name: &String| cut.iter().any(|end| name.ends_with(end));
    if kept.iter().any(for_this_screen) {
        kept.retain(for_this_screen);
    }
    let Some(name) = choose(&kept, asked) else {
        if asked.is_none() {
            println!(
                "  no wallpaper: {} holds none; `cargo xtask wallpapers --from <directory>`, or \
                 `--from <host>:<directory>`, puts some there",
                dir.display()
            );
        }
        return None;
    };
    let chosen = read_kept(&dir.join(&name));
    match &chosen {
        Some(Chosen::Still(_)) => println!("  wallpaper {name}"),
        Some(Chosen::Moving(_)) => println!("  wallpaper {name}, which moves"),
        None => println!("  no wallpaper: {name} is not a picture `wallpapers` made"),
    }
    chosen
}

/// What a run found to put behind the desktop.
#[derive(Debug)]
pub(crate) enum Chosen {
    /// A picture, which `compositor/pattern --wallpaper` shows.
    Still(Vec<u8>),
    /// A video's frames, which `compositor/pattern --video` plays, and which
    /// is what `mpvpaper` is started for on a Linux desktop.
    Moving(Vec<u8>),
}

impl Chosen {
    /// The file's bytes, whichever it is.
    pub(crate) fn bytes(self) -> Vec<u8> {
        match self {
            Self::Still(bytes) | Self::Moving(bytes) => bytes,
        }
    }
}

/// `cargo xtask wallpapers --from <where>`: convert every picture `<where>`
/// holds for a screen of `--size`, or 1920x1080, and keep them where
/// `run-compositor` looks.
///
/// # Errors
///
/// No `--from`, a source that lists no pictures, or nowhere to keep them. A
/// picture that will not convert is said and left out.
pub(crate) fn import(args: &Args) -> Result<()> {
    let source = args.from.as_deref().ok_or_else(|| {
        Error::new(
            "wallpapers needs --from <directory> or --from <host>:<directory>: where the \
             pictures are"
                .to_owned(),
        )
    })?;
    let size = args.size.unwrap_or(SCREEN);
    let dir = kept_dir().ok_or_else(|| {
        Error::new(format!(
            "no home directory to keep wallpapers under; set {VAR}"
        ))
    })?;
    let names = names(source).ok_or_else(|| {
        Error::new(format!(
            "{source} lists no pictures: it is a directory of this machine's, or \
             <host>:<directory> for a host `ssh <host>` reaches without asking anything"
        ))
    })?;
    println!(
        "  {} pictures in {source}, for a {}x{} screen, into {}",
        names.len(),
        size.0,
        size.1,
        dir.display()
    );
    let mut made = 0usize;
    for name in &names {
        let path = dir.join(kept_name(name, size));
        if read_kept(&path).is_some() {
            println!("  {name}: kept already");
            made = made.saturating_add(1);
            continue;
        }
        // A video becomes a wallpaper that moves, which is what somebody who
        // put a video in the directory asked for; everything else becomes the
        // one picture it is.
        let converted = if is_moving(name) {
            convert_moving(source, name, size, Moving::asked(args))
        } else {
            convert(source, name, size)
        };
        match converted {
            Some(bytes) => {
                keep(&path, &bytes)?;
                println!("  {name}: converted");
                made = made.saturating_add(1);
            }
            None => println!("  {name}: would not convert, and is left out"),
        }
    }
    if made == 0 {
        return Err(Error::new(format!(
            "none of them converted: is there an `ffmpeg` where {source} is?"
        )));
    }
    println!("  {made} wallpapers kept; `cargo xtask run-compositor` shows one");
    Ok(())
}

/// The source's host, if it has one, and its directory.
fn parts(source: &str) -> (Option<&str>, &str) {
    match source.split_once(':') {
        // `C:\Users\...` is a directory of this machine's, not a host
        // called `C`.
        Some((host, directory)) if host.len() > 1 => (Some(host), directory),
        _ => (None, source),
    }
}

/// `ssh` to `host`, never asking a question: a run that stops to ask for a
/// password is a run that hung.
fn ssh(host: &str) -> Command {
    let mut command = Command::new("ssh");
    let _ = command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", host]);
    command
}

/// `word` as one word of a POSIX shell's command line, whatever is in it.
fn quoted(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// What a command printed, if it ran and succeeded.
fn output(mut command: Command) -> Option<Vec<u8>> {
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// The names of the pictures the source holds, sorted.
fn names(source: &str) -> Option<Vec<String>> {
    let listed: Vec<String> = match parts(source) {
        (Some(host), directory) => {
            let mut command = ssh(host);
            let _ = command.arg(format!("ls -1 -- {}", quoted(directory)));
            String::from_utf8_lossy(&output(command)?)
                .lines()
                .map(str::to_owned)
                .collect()
        }
        (None, directory) => std::fs::read_dir(directory)
            .ok()?
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect(),
    };
    let mut pictures: Vec<String> = listed.into_iter().filter(|name| is_picture(name)).collect();
    pictures.sort();
    (!pictures.is_empty()).then_some(pictures)
}

/// Whether `name` ends in one of [`KINDS`].
fn is_picture(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|kind| kind.to_str())
        .is_some_and(|kind| KINDS.iter().any(|known| kind.eq_ignore_ascii_case(known)))
}

/// The one asked for -- the first whose name holds `asked` -- or, asked for
/// nothing, one the clock picks, so that a desktop started twice is not the
/// same desktop twice.
fn choose(names: &[String], asked: Option<&str>) -> Option<String> {
    if let Some(asked) = asked {
        let found = names.iter().find(|name| name.contains(asked));
        if found.is_none() {
            println!("  no wallpaper is called anything like {asked}");
        }
        return found.cloned();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let at = usize::try_from(now.checked_rem(u128::try_from(names.len()).ok()?)?).ok()?;
    names.get(at).cloned()
}

/// The rows `ffmpeg` makes of `name`, behind the picture file's header.
fn convert(source: &str, name: &str, (width, height): (u32, u32)) -> Option<Vec<u8>> {
    // Scaled until it covers the screen and cut evenly to it, the first
    // frame and no more, as `XRGB8888` rows on the standard output.
    let filter = format!(
        "scale={width}:{height}:force_original_aspect_ratio=increase,crop={width}:{height}"
    );
    let tail = ["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "bgr0", "-"];
    let command = match parts(source) {
        (Some(host), directory) => {
            let mut command = ssh(host);
            let _ = command.arg(format!(
                "ffmpeg -v error -nostdin -i {} -vf {} {}",
                quoted(&format!("{directory}/{name}")),
                quoted(&filter),
                tail.join(" ")
            ));
            command
        }
        (None, directory) => {
            let mut command = Command::new("ffmpeg");
            let _ = command
                .args(["-v", "error", "-nostdin", "-i"])
                .arg(Path::new(directory).join(name))
                .args(["-vf", &filter])
                .args(tail);
            command
        }
    };
    let rows = output(command)?;
    let expected = usize::try_from(u64::from(width) * u64::from(height) * 4).ok()?;
    if rows.len() != expected {
        println!(
            "  {name} came back as {} bytes where a {width}x{height} picture is {expected}",
            rows.len()
        );
        return None;
    }
    let mut file = Vec::with_capacity(expected.saturating_add(16));
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&width.to_le_bytes());
    file.extend_from_slice(&height.to_le_bytes());
    file.extend_from_slice(&rows);
    Some(file)
}

/// The two colours [`fixture`]'s frames are, as a screendump's `(red, green,
/// blue)`: far apart, and neither the compositor's own background nor black,
/// so a screen showing one of them is showing a frame and not a failure.
pub(crate) const FIXTURE: [(u8, u8, u8); 2] = [(0xC0, 0x30, 0x40), (0x20, 0x80, 0xC0)];

/// How many times a second [`fixture`]'s frames change.
///
/// Twice: fast enough that a gate does not wait for it, slow enough that a
/// screendump lands on a frame rather than between two.
const FIXTURE_RATE: u32 = 2;

/// A video of two frames, each one flat colour, for `cargo xtask test-video`.
///
/// Made here rather than by `ffmpeg` so that the gate needs no decoder on the
/// machine running it -- CI has none -- and so that what reaches the screen
/// is a colour a screendump can be judged against rather than a picture.
pub(crate) fn fixture(width: u32, height: u32) -> Option<Vec<u8>> {
    let pixels = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
    // `XRGB8888`, little-endian, is blue, green, red and the unused byte.
    let frame = |(red, green, blue): (u8, u8, u8)| [blue, green, red, 0].repeat(pixels);
    let (first, second) = (frame(FIXTURE[0]), frame(FIXTURE[1]));
    encode_moving(width, height, FIXTURE_RATE, &[&first, &second])
}

/// Whether `name` is kept as a wallpaper that moves.
fn is_moving(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|kind| kind.to_str())
        .is_some_and(|kind| MOVING.iter().any(|known| kind.eq_ignore_ascii_case(known)))
}

/// The frames `ffmpeg` makes of `name`, behind the video file's header.
///
/// The same conversion as [`convert`] with two more filters -- a frame rate,
/// and a length after which the wallpaper begins again -- and the frames
/// encoded rather than laid down whole, because laid down whole they are
/// megabytes each.
fn convert_moving(source: &str, name: &str, screen: (u32, u32), how: Moving) -> Option<Vec<u8>> {
    let (width, height) = how.frame.unwrap_or((
        (screen.0 / MOVING_SMALLER).max(1),
        (screen.1 / MOVING_SMALLER).max(1),
    ));
    let rate = how.rate;
    let filter = format!(
        "fps={rate},scale={width}:{height}:force_original_aspect_ratio=increase,\
         crop={width}:{height}"
    );
    let seconds = how.seconds.to_string();
    let tail = [
        "-t",
        seconds.as_str(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "bgr0",
        "-",
    ];
    let command = match parts(source) {
        (Some(host), directory) => {
            let mut command = ssh(host);
            let _ = command.arg(format!(
                "ffmpeg -v error -nostdin -i {} -vf {} {}",
                quoted(&format!("{directory}/{name}")),
                quoted(&filter),
                tail.join(" ")
            ));
            command
        }
        (None, directory) => {
            let mut command = Command::new("ffmpeg");
            let _ = command
                .args(["-v", "error", "-nostdin", "-i"])
                .arg(Path::new(directory).join(name))
                .args(["-vf", &filter])
                .args(tail);
            command
        }
    };
    let rows = output(command)?;
    let each = usize::try_from(u64::from(width) * u64::from(height) * 4).ok()?;
    if each == 0 || rows.len() < each {
        println!(
            "  {name} came back as {} bytes, which is no frame",
            rows.len()
        );
        return None;
    }
    let frames: Vec<&[u8]> = rows.chunks_exact(each).collect();
    let file = encode_moving(width, height, rate, &frames)?;
    println!(
        "  {name}: {} frames of {width}x{height} at {rate}/s, {} KiB",
        frames.len(),
        file.len() / 1024
    );
    Some(file)
}

/// How much of a video to keep, and how finely.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Moving {
    /// Frames a second.
    rate: u32,
    /// Seconds of it, after which the wallpaper begins again.
    seconds: u32,
    /// How large a frame is kept, where that is not a fraction of the screen.
    frame: Option<(u32, u32)>,
}

impl Moving {
    /// What the command line asked for, and the defaults for what it did not.
    fn asked(args: &Args) -> Self {
        Self {
            rate: args.fps.unwrap_or(MOVING_RATE),
            seconds: args.seconds.unwrap_or(MOVING_SECONDS),
            frame: args.video_size,
        }
    }
}

/// The video file `compositor_pattern::Movie` reads: the header, and then
/// each frame's rows behind its length.
///
/// A row is written as the one above it in the same frame, or as the one the
/// frame before left in its place, or as runs of a count and a pixel -- which
/// is what makes a second of video kilobytes rather than megabytes, and it is
/// `Movie`'s doc comment that says how it is read back.
fn encode_moving(width: u32, height: u32, rate: u32, frames: &[&[u8]]) -> Option<Vec<u8>> {
    let count = u32::try_from(frames.len()).ok()?;
    let period = 1000_u32.checked_div(rate).filter(|ms| *ms > 0)?;
    let mut file = Vec::new();
    file.extend_from_slice(MOVIE_MAGIC);
    for number in [width, height, count, period] {
        file.extend_from_slice(&number.to_le_bytes());
    }
    let stride = usize::try_from(u64::from(width) * 4).ok()?;
    let rows = usize::try_from(height).ok()?;
    for (at, frame) in frames.iter().enumerate() {
        // The first frame names no frame before it: a loop that has reached
        // the end begins again at this one, and it has to stand alone to be
        // begun again from.
        let before = at.checked_sub(1).and_then(|last| frames.get(last)).copied();
        let mut payload = Vec::new();
        for y in 0..rows {
            let from = y.checked_mul(stride)?;
            let upto = from.checked_add(stride)?;
            let row = frame.get(from..upto)?;
            let last = before.and_then(|before| before.get(from..upto));
            let above = from
                .checked_sub(stride)
                .and_then(|above| frame.get(above..from));
            if last == Some(row) {
                // What the frame before left here, which costs the client
                // nothing at all to keep.
                payload.push(0x02);
            } else if above == Some(row) {
                payload.push(0x00);
            } else {
                payload.push(0x01);
                for run in runs(row) {
                    payload.extend_from_slice(&run.0.to_le_bytes());
                    payload.extend_from_slice(&run.1.to_le_bytes());
                }
            }
        }
        file.extend_from_slice(&u32::try_from(payload.len()).ok()?.to_le_bytes());
        file.extend_from_slice(&payload);
    }
    Some(file)
}

/// One row's `XRGB8888` pixels as runs of a count and a value, each run no
/// longer than a count can say.
fn runs(row: &[u8]) -> Vec<(u16, u32)> {
    let mut runs: Vec<(u16, u32)> = Vec::new();
    for pixel in row.chunks_exact(4) {
        let value = u32::from_le_bytes([
            *pixel.first().unwrap_or(&0),
            *pixel.get(1).unwrap_or(&0),
            *pixel.get(2).unwrap_or(&0),
            *pixel.get(3).unwrap_or(&0),
        ]);
        match runs.last_mut() {
            Some(last) if last.1 == value && last.0 < u16::MAX => last.0 = last.0.saturating_add(1),
            _ => runs.push((1, value)),
        }
    }
    runs
}

/// The wallpapers directory.
fn kept_dir() -> Option<PathBuf> {
    match std::env::var_os(VAR) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => std::env::home_dir().map(|home| KEPT.iter().fold(home, |dir, name| dir.join(name))),
    }
}

/// What `name` at `size` is kept as: the name with anything a file system
/// might mind taken out, and the size, since a picture is cut for a screen.
fn kept_name(name: &str, (width, height): (u32, u32)) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let kind = if is_moving(name) { MOVIE_KIND } else { KIND };
    format!("{safe}.{width}x{height}{kind}")
}

/// A kept wallpaper, if it is there and is one: which of the two it is comes
/// from the file itself rather than from its name, since the name is a
/// person's and the header is this program's.
fn read_kept(path: &Path) -> Option<Chosen> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(MAGIC) {
        Some(Chosen::Still(bytes))
    } else if bytes.starts_with(MOVIE_MAGIC) {
        Some(Chosen::Moving(bytes))
    } else {
        None
    }
}

/// Keep `bytes` at `path`.
fn keep(path: &Path, bytes: &[u8]) -> Result<()> {
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(path, bytes))
        .map_err(|error| Error::new(format!("keeping {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host and a directory, a directory alone, and a Windows path, which
    /// has a colon and no host.
    #[test]
    fn a_source_is_a_host_and_a_directory_or_a_directory() {
        assert_eq!(parts("box:Pictures"), (Some("box"), "Pictures"));
        assert_eq!(parts("/home/u/Pictures"), (None, "/home/u/Pictures"));
        assert_eq!(
            parts("C:\\Users\\u\\Pictures"),
            (None, "C:\\Users\\u\\Pictures")
        );
    }

    /// A name reaches the far shell as one word whatever is in it.
    #[test]
    fn a_name_is_one_word_to_the_far_shell() {
        assert_eq!(quoted("a b.jpg"), "'a b.jpg'");
        assert_eq!(quoted("it's $(rm).png"), "'it'\\''s $(rm).png'");
    }

    /// Pictures and videos are, a directory and a script are not, and the
    /// one asked for is the first that holds the word.
    #[test]
    fn a_picture_is_chosen_by_its_kind_and_its_name() {
        assert!(is_picture("shiroko.1920x1080.MP4") && is_picture("a.jpg"));
        assert!(!is_picture("Screenshots") && !is_picture("pan.lua"));
        let names = ["kivotos.mp4".to_owned(), "shiroko-beach.mp4".to_owned()];
        assert_eq!(
            choose(&names, Some("beach")).as_deref(),
            Some("shiroko-beach.mp4")
        );
        assert_eq!(choose(&names, Some("hoshino")), None);
        assert!(choose(&names, None).is_some_and(|name| names.contains(&name)));
        assert_eq!(choose(&[], None), None);
    }

    /// A kept picture is named for what it is of and the screen it was cut
    /// for, in characters any file system takes. A video is named the same
    /// way and ends differently, because it is played rather than shown.
    #[test]
    fn a_kept_picture_is_named_for_its_screen() {
        assert_eq!(
            kept_name("__shiroko (blue archive).jpg", (1920, 1080)),
            "__shiroko__blue_archive_.jpg.1920x1080.fxwall"
        );
        assert_eq!(
            kept_name("kivotos.mp4", (1920, 1080)),
            "kivotos.mp4.1920x1080.fxvid"
        );
        assert!(is_moving("a.MP4") && is_moving("b.webm") && is_moving("c.gif"));
        assert!(!is_moving("a.jpg") && !is_moving("Screenshots"));
    }

    /// The bytes the encoder writes for a two-frame video, in full.
    ///
    /// This is the format's other half: `compositor_pattern::Movie` reads
    /// these bytes and its own tests spell the same rows out. The two are in
    /// different workspaces and cannot share the code, so they share the
    /// bytes, and a change to either that the other did not make fails here.
    #[test]
    fn a_video_is_rows_of_runs_the_row_above_and_the_frame_before() {
        // Two frames, two by two, as `bgr0` rows. The first is red all over.
        // The second turns its top row green and blue and leaves the bottom
        // row alone.
        let red = [0x00, 0x00, 0xFF, 0x00];
        let green = [0x00, 0xFF, 0x00, 0x00];
        let blue = [0xFF, 0x00, 0x00, 0x00];
        let first: Vec<u8> = [red, red, red, red].concat();
        let second: Vec<u8> = [green, blue, red, red].concat();
        let file = encode_moving(2, 2, MOVING_RATE, &[&first, &second]).expect("a video");

        let mut expected = MOVIE_MAGIC.to_vec();
        // Width, height, frames, and the milliseconds one is shown.
        for number in [2_u32, 2, 2, 1000 / MOVING_RATE] {
            expected.extend_from_slice(&number.to_le_bytes());
        }
        // The first frame: a row of one run of two red, then the row above.
        let frame = [0x01, 0x02, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00];
        expected.extend_from_slice(&u32::try_from(frame.len()).expect("a length").to_le_bytes());
        expected.extend_from_slice(&frame);
        // The second: two runs of one, then what the frame before left.
        let frame = [
            0x01, 0x01, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x01, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x02,
        ];
        expected.extend_from_slice(&u32::try_from(frame.len()).expect("a length").to_le_bytes());
        expected.extend_from_slice(&frame);

        assert_eq!(file, expected);
    }

    /// A rate the command line gave is the rate the file says, so a video
    /// kept at five frames a second shows each for two hundred milliseconds.
    #[test]
    fn the_rate_asked_for_is_the_period_written() {
        let row: Vec<u8> = [0x00, 0x00, 0x00, 0x00].repeat(4);
        let period = |rate: u32| -> u32 {
            let file = encode_moving(2, 2, rate, &[&row]).expect("a video");
            let at = MOVIE_MAGIC.len() + 12;
            u32::from_le_bytes(file[at..at + 4].try_into().expect("four bytes"))
        };
        assert_eq!(period(10), 100);
        assert_eq!(period(5), 200);
        assert_eq!(period(25), 40);
    }

    /// A run stops at what a count can say, so a row wider than 65535 of one
    /// colour is runs rather than one that wrapped to nothing.
    #[test]
    fn a_run_is_no_longer_than_its_count() {
        let row: Vec<u8> = std::iter::repeat_n([0x11, 0x22, 0x33, 0x00], 70_000)
            .flatten()
            .collect();
        let made = runs(&row);
        assert_eq!(made.len(), 2);
        assert_eq!(made.first().map(|run| run.0), Some(u16::MAX));
        assert_eq!(
            made.iter().map(|run| u32::from(run.0)).sum::<u32>(),
            70_000,
            "every pixel is in a run"
        );
    }
}
