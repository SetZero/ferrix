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
    let sized = |flag: &str| {
        arguments
            .iter()
            .position(|word| word == flag)
            .map(|at| arguments.get(at + 1).and_then(|word| word.parse().ok()))
    };
    // `--bar <height>`: a `zwlr_layer_surface_v1` across the top rather than
    // a window. `--menu <side>`: a window with an `xdg_popup` on it, which
    // is what every right-click menu and dropdown is.
    let shape = match (sized("--bar"), sized("--menu")) {
        (Some(Some(height)), _) => compositor_pattern::Shape::Bar(height),
        (Some(None), _) => {
            say("pattern: --bar takes a height in pixels");
            std::process::exit(2);
        }
        (_, Some(Some(side))) => compositor_pattern::Shape::Menu(side),
        (_, Some(None)) => {
            say("pattern: --menu takes a size in pixels");
            std::process::exit(2);
        }
        _ => compositor_pattern::Shape::Window,
    };

    match compositor_pattern::run_shaped(pattern, &title, shape) {
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
