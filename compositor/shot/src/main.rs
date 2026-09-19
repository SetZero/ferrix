//! `shot [screen [image]]`: take a screenshot and say what is in it, or how
//! near it is to an expected image.

use std::io::Write as _;

fn main() {
    let arguments =
        compositor_evecho::init::unshell(std::env::args().skip(1).collect::<Vec<String>>());
    let which = arguments
        .first()
        .and_then(|word| word.parse::<usize>().ok())
        .unwrap_or(0);
    // `shot <screen> <image>`: hold the picture to an expected image on this
    // machine's own filesystem, and say how near it is rather than what it
    // is. `compositor_shot::against` says when that is the judgement wanted.
    let expected = arguments.get(1);
    match socket().and_then(|path| compositor_shot::take(&path, which)) {
        Ok(shot) => match expected {
            None => say(&shot.line()),
            Some(path) => say(&judged(&shot, path)),
        },
        Err(error) => {
            say(&format!("shot: failed: {error}"));
            std::process::exit(1);
        }
    }
}

/// How far a channel may be from the expected image's and the picture still
/// be the same one: what `compositor/render` holds a GPU's frame to.
const STEP: u8 = 3;

/// The line for a picture held to the expected image at `path`.
fn judged(shot: &compositor_shot::Shot, path: &str) -> String {
    let verdict = std::fs::read(path)
        .map_err(|error| format!("{path}: {error}"))
        .and_then(|bytes| compositor_shot::against::against(shot, &bytes, STEP));
    match verdict {
        Ok(verdict) => format!(
            "shot: {}x{} against {path}: {} channels more than {STEP} apart, the furthest {}",
            shot.width, shot.height, verdict.apart, verdict.furthest
        ),
        Err(why) => format!("shot: failed: {why}"),
    }
}

/// Where the compositor's socket is.
fn socket() -> Result<std::path::PathBuf, String> {
    let display =
        std::env::var("WAYLAND_DISPLAY").map_err(|_| "WAYLAND_DISPLAY is not set".to_owned())?;
    compositor_socket::socket_path(&display).map_err(|error| format!("the socket: {error}"))
}

/// Say a line on the standard output, flushed.
fn say(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
