//! `hypridle [-c <path>] [-q] [-v]`: upstream's `main.cpp`, option for
//! option.

use std::path::{Path, PathBuf};

use compositor_hypridle::client::{self, Runner};
use compositor_hypridle::config;
use compositor_hypridle::idle::Idle;
use compositor_hypridle::log::{Level, Log};

/// Upstream's version, which this follows.
const VERSION: &str = "0.1.8";

const USAGE: &str = "Usage: hypridle [options]
Options:
  -v, --verbose       Enable verbose logging
  -q, --quiet         Suppress all output except errors
  -V, --version       Show version information
  -c, --config <path> Specify a custom config file path
  -h, --help          Show this help message";

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut log = Log::default();
    let mut path: Option<String> = None;
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--verbose" | "-v" => log.verbose = true,
            "--quiet" | "-q" => log.quiet = true,
            "--version" | "-V" => {
                log.say(Level::None, &format!("hypridle v{VERSION}"));
                return 0;
            }
            "--config" | "-c" => {
                let Some(given) = words.next().filter(|given| !given.starts_with('-')) else {
                    log.say(
                        Level::None,
                        &format!("After {word} you should provide a path to a config file."),
                    );
                    return 1;
                };
                if path.is_some() {
                    log.say(Level::None, "Multiple config files are provided.");
                    return 1;
                }
                path = Some(given.clone());
            }
            "--help" | "-h" => {
                log.say(Level::None, USAGE);
                return 0;
            }
            // Upstream passes over what it does not know without a word;
            // saying it costs a line.
            other => log.say(Level::Warn, &format!("Ignoring unknown argument {other}")),
        }
    }

    let found = match &path {
        Some(given) => Some(PathBuf::from(given)).filter(|path| path.exists()),
        None => config::find(&|name| std::env::var(name).ok(), &|path: &Path| {
            path.exists()
        }),
    };
    let Some(found) = found else {
        match &path {
            Some(given) => log.say(
                Level::Crit,
                &format!("ConfigManager: Specified file not found: {given}\n"),
            ),
            None => {
                log.say(
                    Level::Crit,
                    "ConfigManager: No hypridle.conf file found in:",
                );
                log.say(
                    Level::None,
                    "    $XDG_CONFIG_HOME/hypr/, ~/.config/hypr/, [XDG_CONFIG_DIRS]/hypr/, /etc/xdg/hypr/\n",
                );
                log.say(Level::None, "Create a config or specify one manually:");
                log.say(Level::None, "    hypridle -c /path/to/conf");
            }
        }
        return 1;
    };
    log.say(
        Level::Log,
        &format!("Using config file: {}", found.display()),
    );

    let environment: Vec<(String, String)> = std::env::vars().collect();
    let loaded = config::load(&found, &environment);
    if !loaded.errors.is_empty() {
        log.say(
            Level::Err,
            &format!(
                "Config has errors:\n{}\nProceeding ignoring faulty entries",
                loaded.errors.join("\n")
            ),
        );
    }
    for rule in &loaded.rules {
        log.say(
            Level::Log,
            &format!(
                "Registered timeout rule for {}s:\n      on-timeout: {}\n      on-resume: {}\n      \
                 ignore_inhibit: {}\n      condition_cmd: {}\n      condition_retry: {}",
                rule.timeout,
                rule.on_timeout,
                rule.on_resume,
                rule.ignore_inhibit,
                rule.condition_cmd,
                rule.condition_retry
            ),
        );
    }
    for note in config::ferrix_notes(&loaded) {
        log.say(Level::Warn, &note);
    }

    let mut idle = Idle::new(loaded.general, loaded.rules);
    let mut runner = Runner { log };
    match client::run(&mut idle, &mut runner) {
        Ok(()) => 0,
        Err(error) => {
            log.say(Level::Crit, &error);
            1
        }
    }
}
