//! The wallpaper's settings file, and the defaults it is laid over.
//!
//! How a wallpaper is configured on a Linux desktop is not by a compositor
//! option: Hyprland has none for it, and this machine's `hyprland.conf` says
//! so in one line -- `exec-once = booru-wallpaper daemon` -- with every
//! decision about *what* is shown and *how finely* in that daemon's own
//! commented `~/.config/booru-wallpaper/config.toml`, read over a table of
//! defaults, a warning where the file is missing and the defaults where a
//! line is wrong. This is the same file for the same job, because the job is
//! the same one.
//!
//! It belongs here rather than in `compositor/config` for a reason that is
//! not taste. `compositor/config`'s option table holds *exactly* the options
//! Hyprland 0.56 has, and a differential harness checks its defaults against
//! a real `hyprctl getoption`; an invented `wallpaper:fps` in there would
//! make that comparison a lie. And it would be the wrong place anyway: every
//! setting here is spent by `ffmpeg` on **this** machine when the image is
//! built, not by the compositor in the guest when it draws. A guest that
//! wanted to change its own frame rate would have to re-encode a video it
//! has no encoder for.
//!
//! Precedence is the usual one: a command-line flag beats the file, the file
//! beats the defaults below.

use std::path::PathBuf;

/// The settings file, under the home directory.
const KEPT: &[&str] = &[".config", "ferrix", "wallpaper.toml"];

/// The variable that names it, where it is not the usual one.
const VAR: &str = "FERRIX_WALLPAPER_CONFIG";

/// Frames a second kept of a video, when nothing says otherwise.
///
/// A wallpaper that moves is its frames, so this is a size as much as a
/// rate. It was ten while the frames were kept raw; they are AV1 now, and a
/// second of 1920x1080 at thirty costs about a tenth of a megabyte rather
/// than a quarter of a gigabyte, so the rate the video was shot at is
/// affordable and a wallpaper no longer visibly steps.
const RATE: u32 = 30;

/// Seconds of a video kept, after which the wallpaper begins again.
const SECONDS: u32 = 10;

/// How large a frame is kept, as a multiple of the screen.
///
/// `1.0` is the screen itself, which is `booru-wallpaper`'s `min_scale` and
/// for the same reason: at exactly the screen's size the client's
/// cover-scaling has nothing to do, and nothing is softened on the way. It
/// was a quarter of the screen each way when frames were raw.
const SCALE: f64 = 1.0;

/// What a frame may not be scaled past, so that a typo in the file cannot
/// ask `ffmpeg` for a wallpaper larger than any screen.
const SCALE_MOST: f64 = 4.0;

/// The commented file written where there is none, so that the settings can
/// be found by looking rather than by reading this source.
const EXAMPLE: &str = "\
# Ferrix -- the wallpaper `cargo xtask run-compositor` shows.
#
# `cargo xtask wallpapers --from <directory>` reads this when it converts,
# and a command-line flag beats anything written here. Delete a line to go
# back to its default; delete the file to go back to all of them.

# --- a wallpaper that moves ----------------------------------------------
# An mp4, webm, mkv or gif in the pictures directory is kept as AV1 video
# and played on the desktop, which on a Linux desktop is what mpvpaper is
# for. These three decide what that costs: the frames live in the initramfs,
# which is built into the kernel, so each one is a size as much as a
# setting. 1920x1080 at 30 for 10 seconds is about a megabyte.

# Frames a second. 30 is the rate most video is shot at; 10 is visibly a
# slideshow and a third of the size.
fps = 30

# Seconds of the video kept, after which the wallpaper begins again.
seconds = 10

# How large each frame is kept, as a multiple of the screen -- the same
# meaning `min_scale` has in ~/.config/booru-wallpaper/config.toml. 1.0 is
# the screen itself, which leaves the client's cover-scaling nothing to do.
# 0.5 is a quarter of the pixels and a quarter of the decoding the guest has
# to do every frame, which is the setting to reach for when the desktop is
# emulated and slow.
scale = 1.0

# An exact frame size instead, if the screen's shape is not what you want
# kept. Overrides `scale` when it is not empty.
size = \"\"

# --- which one -----------------------------------------------------------
# Which kept wallpaper run-compositor shows, by any part of its name; the
# same thing --wallpaper says. Empty means a different one each run, and
# \"none\" means a plain background.
name = \"\"
";

/// What the settings file and the defaults together say.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    /// Frames a second kept of a video.
    pub(crate) fps: u32,
    /// Seconds of it kept.
    pub(crate) seconds: u32,
    /// How large a frame is kept, as a multiple of the screen.
    pub(crate) scale: f64,
    /// An exact frame size, which overrides [`Self::scale`].
    pub(crate) size: Option<(u32, u32)>,
    /// Which kept wallpaper to show, by part of its name.
    pub(crate) name: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: RATE,
            seconds: SECONDS,
            scale: SCALE,
            size: None,
            name: None,
        }
    }
}

impl Settings {
    /// The settings, read from the file where there is one.
    ///
    /// Never fails: a file that cannot be read or holds a line that makes no
    /// sense is said and the defaults are used, because a wallpaper is not
    /// worth failing a boot or a conversion over.
    pub(crate) fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                println!("  {}: {error}; using the defaults", path.display());
                return Self::default();
            }
        };
        let (settings, complaints) = parse(&text);
        for complaint in &complaints {
            println!("  {}: {complaint}", path.display());
        }
        settings
    }

    /// How large a frame is kept for a screen of `screen`: the exact size
    /// where one was asked for, and otherwise the screen scaled.
    pub(crate) fn frame(&self, screen: (u32, u32)) -> (u32, u32) {
        if let Some(size) = self.size {
            return size;
        }
        let scaled = |side: u32| {
            let side = f64::from(side) * self.scale.clamp(0.0, SCALE_MOST);
            (side.round().max(1.0) as u32).next_multiple_of(2)
        };
        (scaled(screen.0), scaled(screen.1))
    }
}

/// The settings file's path.
pub(crate) fn path() -> Option<PathBuf> {
    match std::env::var_os(VAR) {
        Some(path) => Some(PathBuf::from(path)),
        None => std::env::home_dir().map(|home| KEPT.iter().fold(home, |dir, name| dir.join(name))),
    }
}

/// Write the commented file where there is none, so that somebody looking
/// for the settings finds them.
///
/// Says what it did and swallows what went wrong: a read-only home directory
/// is a reason to convert pictures without a settings file, not a reason to
/// convert none.
pub(crate) fn write_example() {
    let Some(path) = path() else { return };
    if path.exists() {
        return;
    }
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, EXAMPLE));
    match written {
        Ok(()) => println!(
            "  settings: {} written, with the defaults in it",
            path.display()
        ),
        Err(error) => println!("  settings: no {} ({error})", path.display()),
    }
}

/// The settings `text` asks for, and a complaint for every line that asked
/// for something that is not a setting.
///
/// The subset of TOML a flat table of numbers and strings needs, which is
/// what `~/.config/booru-wallpaper/config.toml` is. A key that is not known
/// is said and kept out rather than refused: a file written for a later
/// version of this should still put a wallpaper on the screen.
fn parse(text: &str) -> (Settings, Vec<String>) {
    let mut settings = Settings::default();
    let mut complaints = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = uncommented(line).trim();
        if line.is_empty() {
            continue;
        }
        let at = number.saturating_add(1);
        let Some((key, value)) = line.split_once('=') else {
            complaints.push(format!("line {at} is not `name = value`, and is ignored"));
            continue;
        };
        let (key, value) = (key.trim(), unquoted(value.trim()));
        let mut wrong = |what: &str| complaints.push(format!("line {at}: {key} {what}"));
        match key {
            "fps" => match value.parse::<u32>() {
                Ok(fps) if fps > 0 => settings.fps = fps,
                _ => wrong("is frames a second, a whole number above zero"),
            },
            "seconds" => match value.parse::<u32>() {
                Ok(seconds) if seconds > 0 => settings.seconds = seconds,
                _ => wrong("is seconds, a whole number above zero"),
            },
            "scale" => match value.parse::<f64>() {
                Ok(scale) if scale > 0.0 && scale <= SCALE_MOST => settings.scale = scale,
                _ => wrong("is a multiple of the screen, above zero and at most 4"),
            },
            "size" if value.is_empty() => settings.size = None,
            "size" => match dimensions(value) {
                Some(size) => settings.size = Some(size),
                None => wrong("is a frame size, as <width>x<height>"),
            },
            "name" if value.is_empty() => settings.name = None,
            "name" => settings.name = Some(value.to_owned()),
            other => complaints.push(format!("line {at}: no setting is called {other}")),
        }
    }
    (settings, complaints)
}

/// `line` up to a `#` that is not inside a string.
fn uncommented(line: &str) -> &str {
    let mut quoted = false;
    for (at, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '#' if !quoted => return line.split_at(at).0,
            _ => {}
        }
    }
    line
}

/// `value` without the quotes around it, if it has them.
fn unquoted(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(value)
}

/// `<width>x<height>`, both above zero.
fn dimensions(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once(['x', 'X'])?;
    let (width, height) = (width.trim().parse().ok()?, height.trim().parse().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing written is the defaults, which are a screen's worth of frames
    /// at the rate video is shot at.
    #[test]
    fn nothing_written_is_the_defaults() {
        let (settings, complaints) = parse("# only a comment\n\n");
        assert!(complaints.is_empty(), "{complaints:?}");
        assert_eq!(settings.fps, 30);
        assert_eq!(settings.seconds, 10);
        assert_eq!(settings.frame((1920, 1080)), (1920, 1080));
        assert!(settings.name.is_none());
    }

    /// Every setting read, with the quotes and comments a TOML file has.
    #[test]
    fn a_written_setting_is_read() {
        let (settings, complaints) = parse(
            "fps = 15\nseconds = 12  # a third of a minute\nscale = 0.5\nname = \"shiroko\"\n",
        );
        assert!(complaints.is_empty(), "{complaints:?}");
        assert_eq!(settings.fps, 15);
        assert_eq!(settings.seconds, 12);
        assert_eq!(settings.frame((1920, 1080)), (960, 540));
        assert_eq!(settings.name.as_deref(), Some("shiroko"));
    }

    /// An exact size beats the scale, and an empty one is no size at all.
    #[test]
    fn a_size_beats_the_scale() {
        let (settings, _) = parse("scale = 0.5\nsize = \"1280x720\"\n");
        assert_eq!(settings.frame((1920, 1080)), (1280, 720));
        let (settings, _) = parse("scale = 0.5\nsize = \"\"\n");
        assert_eq!(settings.frame((1920, 1080)), (960, 540));
    }

    /// A frame is an even number of pixels each way, because a 4:2:0 encoder
    /// cannot take an odd one and an odd screen is not a reason to fail.
    #[test]
    fn a_scaled_frame_is_even() {
        let (settings, _) = parse("scale = 0.33\n");
        let (width, height) = settings.frame((1919, 1081));
        assert_eq!(width % 2, 0, "{width} is odd");
        assert_eq!(height % 2, 0, "{height} is odd");
    }

    /// A line that makes no sense is said and the default is kept, and a
    /// setting nothing here has heard of is said and ignored: a file is not
    /// a reason to refuse to make a wallpaper.
    #[test]
    fn a_wrong_line_is_said_and_the_default_kept() {
        let (settings, complaints) = parse("fps = soon\nscale = 9\ntags = [\"touhou\"]\nseconds\n");
        assert_eq!(settings.fps, 30, "the default is kept");
        assert!((settings.scale - SCALE).abs() < f64::EPSILON, "and kept");
        assert_eq!(complaints.len(), 4, "{complaints:?}");
        assert!(complaints.iter().any(|said| said.contains("tags")));
    }

    /// A `#` inside a name is part of the name.
    #[test]
    fn a_hash_in_a_string_is_not_a_comment() {
        let (settings, complaints) = parse("name = \"c#\"  # the language\n");
        assert!(complaints.is_empty(), "{complaints:?}");
        assert_eq!(settings.name.as_deref(), Some("c#"));
    }

    /// The written file is one this reads back without a complaint, which is
    /// the thing most easily got wrong about an example.
    #[test]
    fn the_written_example_parses_as_the_defaults() {
        let (settings, complaints) = parse(EXAMPLE);
        assert!(complaints.is_empty(), "{complaints:?}");
        assert_eq!(settings.fps, RATE);
        assert_eq!(settings.seconds, SECONDS);
        assert_eq!(settings.frame((1920, 1080)), (1920, 1080));
        assert!(settings.size.is_none() && settings.name.is_none());
    }
}
