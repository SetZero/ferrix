//! The pattern client's command line.
//!
//! Everything it does is in the library beside this, so the compositor's own
//! test can run a client in a thread rather than as a process.

use std::io::Write;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let pattern = match arguments.first().map(String::as_str) {
        Some("checkerboard") | None => compositor_render::Pattern::Checkerboard,
        Some("gradient") => compositor_render::Pattern::Gradient,
        Some(other) => {
            say(&format!(
                "pattern: {other} is not a pattern; try checkerboard or gradient"
            ));
            std::process::exit(2);
        }
    };
    let title = arguments
        .get(1)
        .cloned()
        .unwrap_or_else(|| format!("{pattern:?}"));

    match compositor_pattern::run(pattern, &title) {
        Ok(line) => say(&line),
        Err(error) => {
            say(&format!("pattern: failed: {error}"));
            std::process::exit(1);
        }
    }
}

fn say(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
