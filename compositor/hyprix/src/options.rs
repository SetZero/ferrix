//! What the compositor was asked to do.

use std::path::PathBuf;

/// The compositor's arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The configuration file, or `None` for the built-in defaults.
    pub config: Option<PathBuf>,
    /// The Wayland socket's name, as `WAYLAND_DISPLAY` will carry it.
    pub display: String,
    /// Draw into memory rather than onto a screen, at this size.
    pub headless: Option<(u32, u32)>,
    /// Stop after this many frames. `None` runs until it is killed, which is
    /// what a compositor does.
    pub frames: Option<u32>,
    /// Write each frame here as a PPM, for a test to look at.
    pub dump: Option<PathBuf>,
    /// Programs to start once the socket is listening, beside the
    /// configuration's own `exec-once`.
    pub exec: Vec<String>,
    /// Give up after this many milliseconds, so a test can never hang.
    pub deadline: Option<u64>,
    /// The instance name `hyprctl` finds the control socket under, in
    /// `$XDG_RUNTIME_DIR/hypr/<instance>/`. Without one the socket is not
    /// bound at all, which is what a test that does not want it asks for.
    pub instance: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            config: None,
            display: "wayland-1".to_owned(),
            headless: None,
            frames: None,
            dump: None,
            exec: Vec::new(),
            deadline: None,
            instance: None,
        }
    }
}

impl Options {
    /// What to print when the arguments are wrong.
    pub const USAGE: &'static str = "\
usage: hyprix [options]

  --config <path>     a hyprland.conf; without one, Hyprland's defaults
  --display <name>    the Wayland socket's name (default wayland-1)
  --headless <WxH>    draw into memory at this size instead of onto a screen
  --frames <n>        stop after this many frames
  --dump <dir>        write each frame there as a PPM
  --exec <command>    start this once the socket is listening, repeatable
  --deadline <ms>     give up after this long
  --instance <name>   bind hyprctl's socket under $XDG_RUNTIME_DIR/hypr/<name>";

    /// Read the arguments.
    ///
    /// # Errors
    ///
    /// A message naming the argument that was wrong.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("{argument} needs a value"))
            };
            match argument.as_str() {
                "--config" => options.config = Some(PathBuf::from(value()?)),
                "--display" => options.display = value()?,
                "--headless" => options.headless = Some(size(&value()?)?),
                "--frames" => {
                    options.frames = Some(
                        value()?
                            .parse()
                            .map_err(|_| "--frames takes a number".to_owned())?,
                    );
                }
                "--dump" => options.dump = Some(PathBuf::from(value()?)),
                "--instance" => options.instance = Some(value()?),
                "--exec" => options.exec.push(value()?),
                "--deadline" => {
                    options.deadline = Some(
                        value()?
                            .parse()
                            .map_err(|_| "--deadline takes milliseconds".to_owned())?,
                    );
                }
                "--help" | "-h" => return Err("help".to_owned()),
                other => return Err(format!("{other} is not an option")),
            }
        }
        Ok(options)
    }
}

/// `WxH`, as `--headless 1024x768` gives it.
fn size(text: &str) -> Result<(u32, u32), String> {
    let (width, height) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{text} is not a size like 1024x768"))?;
    let read = |value: &str, what: &str| {
        value
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|number| *number > 0)
            .ok_or_else(|| format!("{what} of a size must be a number above zero"))
    };
    Ok((read(width, "the width")?, read(height, "the height")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn the_defaults_are_a_compositor_on_a_screen() {
        let options = parse(&[]).expect("no arguments is valid");
        assert_eq!(options.display, "wayland-1");
        assert_eq!(options.headless, None, "a screen by default");
        assert_eq!(options.frames, None, "until it is killed");
        assert!(options.exec.is_empty());
    }

    #[test]
    fn every_option_is_read() {
        let options = parse(&[
            "--config",
            "/etc/hyprland.conf",
            "--display",
            "wayland-9",
            "--headless",
            "1024x768",
            "--frames",
            "3",
            "--dump",
            "/tmp/frames",
            "--exec",
            "one",
            "--exec",
            "two",
            "--deadline",
            "5000",
        ])
        .expect("valid");
        assert_eq!(
            options.config.as_deref(),
            Some(std::path::Path::new("/etc/hyprland.conf"))
        );
        assert_eq!(options.display, "wayland-9");
        assert_eq!(options.headless, Some((1024, 768)));
        assert_eq!(options.frames, Some(3));
        assert_eq!(options.exec, ["one", "two"]);
        assert_eq!(options.deadline, Some(5000));
        assert_eq!(
            parse(&["--instance", "ferrix"])
                .expect("valid")
                .instance
                .as_deref(),
            Some("ferrix")
        );
    }

    #[test]
    fn a_size_must_be_two_numbers_above_zero() {
        for bad in ["1024", "1024x", "x768", "0x768", "1024x0", "-1x2", "axb"] {
            assert!(
                parse(&["--headless", bad]).is_err(),
                "{bad} was accepted as a size"
            );
        }
        assert_eq!(
            parse(&["--headless", "16X9"]).expect("valid").headless,
            Some((16, 9)),
            "a capital X is a size too"
        );
    }

    #[test]
    fn an_option_without_its_value_and_an_unknown_one_are_refused() {
        assert!(parse(&["--display"]).is_err());
        assert!(parse(&["--frames", "many"]).is_err());
        assert!(parse(&["--nonsense"]).is_err());
    }
}
