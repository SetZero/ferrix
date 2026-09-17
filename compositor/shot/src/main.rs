//! `shot [screen]`: take a screenshot and say what is in it.

use std::io::Write as _;

fn main() {
    let arguments =
        compositor_evecho::init::unshell(std::env::args().skip(1).collect::<Vec<String>>());
    let which = arguments
        .first()
        .and_then(|word| word.parse::<usize>().ok())
        .unwrap_or(0);
    match socket().and_then(|path| compositor_shot::take(&path, which)) {
        Ok(shot) => say(&shot.line()),
        Err(error) => {
            say(&format!("shot: failed: {error}"));
            std::process::exit(1);
        }
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
