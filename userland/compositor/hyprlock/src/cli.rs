//! The command line: `src/main.cpp`.
//!
//! The command line is upstream's (`src/main.cpp`): `-c`/`--config FILE`,
//! `-g`/`--grace SECONDS`, `--immediate-render`, `--no-fade-in`,
//! `--display NAME`, `-v`/`--verbose`, `-q`/`--quiet`, `-V`/`--version`,
//! `-h`/`--help`, and the deprecated `--immediate`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use std::sync::Arc;

use crate::app::{Options, run};
use crate::auth::Backend;
use crate::config::Config;
use crate::say;

/// Upstream's version, which this follows.
const VERSION: &str = "0.9.6";

const HELP: &str = "\
Hyprlock CLI Arguments
  -h, --help               Show this help message
  -V, --version            Print hyprlock version, then exit
  -v, --verbose            Enable verbose logging
  -q, --quiet              Disable logging
  -c, --config FILE        Specify config file to use
  -g, --grace SECONDS      Seconds before authentication is required
      --immediate-render   Draw background immediately (Don't wait for resources)
      --no-fade-in         Disable the fade-in animation
      --display NAME       Specify the Wayland display to connect to
      --immediate          [Deprecated] (Use \"--grace 0\" instead)
";

/// What the command line said.
#[derive(Debug, Default)]
struct Arguments {
    config: Option<PathBuf>,
    grace: u64,
    immediate_render: bool,
    no_fade_in: bool,
    display: Option<String>,
    quiet: bool,
    verbose: bool,
    help: bool,
    version: bool,
}

fn parse(arguments: &[String]) -> Result<Arguments, String> {
    let mut parsed = Arguments::default();
    let mut words = arguments.iter().peekable();
    while let Some(word) = words.next() {
        let (name, inline) = match word.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (word.as_str(), None),
        };
        let mut value = |what: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| words.next().cloned())
                .ok_or_else(|| format!("{what} needs a value"))
        };
        match name {
            "-h" | "--help" => parsed.help = true,
            "-V" | "--version" => parsed.version = true,
            "-v" | "--verbose" => parsed.verbose = true,
            "-q" | "--quiet" => parsed.quiet = true,
            "-c" | "--config" => parsed.config = Some(PathBuf::from(value("--config")?)),
            "-g" | "--grace" => {
                let text = value("--grace")?;
                parsed.grace = text
                    .parse()
                    .map_err(|_| format!("--grace: {text} is not a number"))?;
            }
            "--immediate-render" => parsed.immediate_render = true,
            "--no-fade-in" => parsed.no_fade_in = true,
            "--display" => parsed.display = Some(value("--display")?),
            "--immediate" => {
                // An int option upstream; a number after it is taken too.
                if words.peek().is_some_and(|next| next.parse::<i64>().is_ok()) {
                    let _ = words.next();
                }
                parsed.grace = 0;
                say(r#"[WARN] "--immediate" is deprecated. Use the "--grace" option instead."#);
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(parsed)
}

/// `Hyprutils::Path::findConfig("hyprlock")`.
fn find_config() -> Option<PathBuf> {
    let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let mut candidates = Vec::new();
    if let Some(dir) = env("XDG_CONFIG_HOME") {
        candidates.push(PathBuf::from(dir).join("hypr/hyprlock.conf"));
    }
    if let Some(home) = env("HOME") {
        candidates.push(PathBuf::from(home).join(".config/hypr/hyprlock.conf"));
    }
    if let Some(dirs) = env("XDG_CONFIG_DIRS") {
        for dir in dirs
            .to_string_lossy()
            .split(':')
            .filter(|dir| !dir.is_empty())
        {
            candidates.push(Path::new(dir).join("hypr/hyprlock.conf"));
        }
    }
    candidates.push(PathBuf::from("/etc/xdg/hypr/hyprlock.conf"));
    candidates.into_iter().find(|path| path.is_file())
}

/// Run hyprlock with its command line, checking passwords with `backend`.
#[must_use]
pub fn main(backend: Arc<dyn Backend>) -> ExitCode {
    // Not `compositor_evecho::init::unshell`: hyprlock is never init, and
    // its own `-c` is the configuration, not a shell's script.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments = match parse(&arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            say(&format!("[ERR] Invalid argument: {error}"));
            return ExitCode::FAILURE;
        }
    };
    if arguments.help {
        say(HELP);
        return ExitCode::SUCCESS;
    }
    if arguments.version || !arguments.quiet {
        say(&format!("Hyprlock version v{VERSION} (Ferrix)"));
        if arguments.version {
            return ExitCode::SUCCESS;
        }
    }
    let path = match arguments.config.clone().or_else(find_config) {
        Some(path) if path.is_file() => path,
        Some(path) => {
            say(&format!(
                "[CRIT]  Config path error: No config file at \"{}\"",
                path.display()
            ));
            return ExitCode::FAILURE;
        }
        None => {
            say(
                "[CRIT]  Config path error: Could not find config. Searched in order: XDG_CONFIG_HOME, HOME, XDG_CONFIG_DIRS, /etc/xdg",
            );
            return ExitCode::FAILURE;
        }
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            say(&format!("[CRIT] Config threw: {}: {error}", path.display()));
            return ExitCode::FAILURE;
        }
    };
    let mut config = Config::parse(&text, &path);
    if !config.diagnostics.is_empty() {
        let lines: Vec<String> = config.diagnostics.iter().map(ToString::to_string).collect();
        say(&format!(
            "[ERR] Config has errors:\n{}\nProceeding ignoring faulty entries",
            lines.join("\n")
        ));
    }
    for line in config.unsupported() {
        say(&format!("[WARN] {line}"));
    }
    if !config.auth.pam && !config.auth.fingerprint {
        say("[CRIT] At least one authentication method must be enabled!");
        return ExitCode::FAILURE;
    }
    if arguments.no_fade_in {
        config.animations.disable("fadeIn");
    }
    let options = Options {
        grace: arguments.grace,
        immediate_render: arguments.immediate_render || config.general.immediate_render,
        display: arguments.display,
        verbose: arguments.verbose,
    };
    match run(config, &options, backend) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            say(&format!("[CRIT] Hyprlock: {error}"));
            ExitCode::FAILURE
        }
    }
}
