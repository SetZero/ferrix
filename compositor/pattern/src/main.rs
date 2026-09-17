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
    // `--bar <height>`: a `zwlr_layer_surface_v1` across the top rather than
    // a window, which is what a bar is.
    let shape = match arguments.iter().position(|word| word == "--bar") {
        Some(at) => {
            let Some(height) = arguments.get(at + 1).and_then(|word| word.parse().ok()) else {
                say("pattern: --bar takes a height in pixels");
                std::process::exit(2);
            };
            compositor_pattern::Shape::Bar(height)
        }
        None => compositor_pattern::Shape::Window,
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
