//! `waybar`: `-c config`, `-s style`, `-l level`, `-b bar`, `-v`, `-h`, as
//! upstream's `Client::main` takes them.
//!
//! Until the drawing lands -- it waits on the foundation's text and image
//! crates reaching main -- this reads and checks both files, saying what
//! waybar would say about them, and exits, saying that it draws nothing yet.

use std::io::Write;
use std::process::ExitCode;

use compositor_waybar::cli::{self, Parsed};
use compositor_waybar::config::{self, System};
use compositor_waybar::css::Stylesheet;
use compositor_waybar::diag::{Diagnostics, Level};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut err = std::io::stderr().lock();
    let options = match cli::parse(&args) {
        Ok(Parsed::Run(options)) => options,
        Ok(Parsed::Help) => {
            let _ = writeln!(std::io::stdout(), "{}", cli::HELP);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            let _ = writeln!(
                std::io::stdout(),
                "Waybar v{} (Ferrix)",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            let _ = writeln!(err, "[error] Error in command line: {message}");
            return ExitCode::from(1);
        }
    };
    let mut diag = Diagnostics::default();
    let loaded = config::load(options.config.as_deref(), &System, &mut diag);
    let style = options
        .style
        .clone()
        .or_else(|| config::find(&["style.css"], &System));
    if let Some(path) = &style {
        diag.info(format!("Using CSS file {}", path.display()));
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let _ =
                    Stylesheet::parse(&text, path, &|p| std::fs::read_to_string(p).ok(), &mut diag);
            }
            Err(_) => diag.error("Can't open style file".to_owned()),
        }
    }
    for line in diag.drain(options.level) {
        let _ = writeln!(err, "{line}");
    }
    if let Err(message) = loaded {
        let _ = writeln!(err, "[error] {message}");
        return ExitCode::from(1);
    }
    if style.is_none() {
        let _ = writeln!(err, "[error] Missing required resource files");
        return ExitCode::from(1);
    }
    if options.level <= Level::Critical {
        let _ = writeln!(
            err,
            "[critical] this waybar does not draw yet: its drawing waits on userland/compositor/text and image"
        );
    }
    ExitCode::from(1)
}
