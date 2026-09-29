//! The command line, as upstream's `clara` parser takes it.

use std::path::PathBuf;

use crate::diag::Level;

/// `waybar -h`'s text.
pub const HELP: &str = "usage:
  waybar  options

where options are:
  -?, -h, --help                         display usage information
  -v, --version                          Show version
  -c, --config <config>                  Config path
  -s, --style <style>                    Style path
  -l, --log-level <trace|debug|info|warning|error|critical|off>
                                         Log level
  -b, --bar <id>                         Bar id

Ferrix's own, for a boot's expected picture:
  --render <file.ppm>                    Draw the first bar into a picture, with no compositor
  --size <WxH>                           The output --render draws for (default 1024x768)
  --output <name>                        Its name (default Virtual-1)
  --over <RRGGBB>                        The ground --render composites the bar over (default 000000)
  --fonts-dir <dir>                      Take faces from this directory alone";

/// What to run with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// `-c`.
    pub config: Option<PathBuf>,
    /// `-s`.
    pub style: Option<PathBuf>,
    /// `-l`; `info` when not given, spdlog's default.
    pub level: Level,
    /// `-b`: the bar id sway's IPC names; parsed and not used, since the
    /// bar-to-sway link is sway's.
    pub bar: Option<String>,
    /// `--render`: draw into this picture and exit.
    pub render: Option<PathBuf>,
    /// `--size`.
    pub size: (u32, u32),
    /// `--output`.
    pub output: String,
    /// `--over`.
    pub over: String,
    /// `--fonts-dir`.
    pub fonts_dir: Option<PathBuf>,
}

/// What the command line asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Run.
    Run(Options),
    /// `-h`.
    Help,
    /// `-v`.
    Version,
}

/// Parse `args`, the program's name left out.
///
/// # Errors
///
/// An unknown option, or one missing its value, in clara's words.
pub fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut options = Options {
        config: None,
        style: None,
        level: Level::Info,
        bar: None,
        render: None,
        size: (1024, 768),
        output: "Virtual-1".to_owned(),
        over: "000000".to_owned(),
        fonts_dir: None,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| rest.next().cloned())
                .ok_or_else(|| format!("Expected argument following {name}"))
        };
        match flag {
            "-h" | "-?" | "--help" => return Ok(Parsed::Help),
            "-v" | "--version" => return Ok(Parsed::Version),
            "-c" | "--config" => options.config = Some(PathBuf::from(value(flag)?)),
            "-s" | "--style" => options.style = Some(PathBuf::from(value(flag)?)),
            "-l" | "--log-level" => options.level = Level::parse(&value(flag)?),
            "-b" | "--bar" => options.bar = Some(value(flag)?),
            "--render" => options.render = Some(PathBuf::from(value(flag)?)),
            "--size" => {
                let text = value(flag)?;
                let (w, h) = text
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .ok_or_else(|| format!("--size wants WxH, not {text}"))?;
                options.size = (w, h);
            }
            "--output" => options.output = value(flag)?,
            "--over" => options.over = value(flag)?,
            "--fonts-dir" => options.fonts_dir = Some(PathBuf::from(value(flag)?)),
            other => return Err(format!("Unrecognised token: {other}")),
        }
    }
    Ok(Parsed::Run(options))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Parsed, parse};
    use crate::diag::Level;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn upstreams_options() {
        let Ok(Parsed::Run(options)) =
            parse(&args(&["-c", "/c.jsonc", "--style=/s.css", "-l", "debug"]))
        else {
            panic!("not parsed");
        };
        assert_eq!(options.config, Some(PathBuf::from("/c.jsonc")));
        assert_eq!(options.style, Some(PathBuf::from("/s.css")));
        assert_eq!(options.level, Level::Debug);
        assert_eq!(parse(&args(&["-h"])), Ok(Parsed::Help));
        assert_eq!(
            parse(&args(&["-x"])),
            Err("Unrecognised token: -x".to_owned())
        );
        assert_eq!(
            parse(&args(&["-c"])),
            Err("Expected argument following -c".to_owned())
        );
    }
}
