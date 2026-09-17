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
//!   `ffmpeg` takes the first frame of a video, scales it until it covers
//!   the screen and cuts the overflow evenly.

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
pub(crate) fn file(args: &Args, size: (u32, u32)) -> Option<Vec<u8>> {
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
        .filter(|name| name.ends_with(KIND))
        .collect();
    kept.sort();
    let cut = format!(".{}x{}{KIND}", size.0, size.1);
    if kept.iter().any(|name| name.ends_with(&cut)) {
        kept.retain(|name| name.ends_with(&cut));
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
    let bytes = read_kept(&dir.join(&name));
    match &bytes {
        Some(_) => println!("  wallpaper {name}"),
        None => println!("  no wallpaper: {name} is not a picture `wallpapers` made"),
    }
    bytes
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
        match convert(source, name, size) {
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
    format!("{safe}.{width}x{height}{KIND}")
}

/// A kept picture, if it is there and is one.
fn read_kept(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path)
        .ok()
        .filter(|bytes| bytes.starts_with(MAGIC))
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
    /// for, in characters any file system takes.
    #[test]
    fn a_kept_picture_is_named_for_its_screen() {
        assert_eq!(
            kept_name("__shiroko (blue archive).jpg", (1920, 1080)),
            "__shiroko__blue_archive_.jpg.1920x1080.fxwall"
        );
    }
}
