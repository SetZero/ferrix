//! `clip copy <text>` and `clip paste`.

use std::io::Write as _;
use std::time::Duration;

/// How long a copy waits to be asked for its data before it gives up.
const PATIENCE: Duration = Duration::from_secs(30);

fn main() {
    let arguments =
        compositor_evecho::init::unshell(std::env::args().skip(1).collect::<Vec<String>>());
    let answer = match arguments.first().map(String::as_str) {
        Some("copy") => {
            let text = arguments
                .get(1..)
                .map(|words| words.join(" "))
                .unwrap_or_default();
            socket().and_then(|path| compositor_clip::copy(&path, &text, PATIENCE))
        }
        Some("paste") => socket()
            .and_then(|path| compositor_clip::paste(&path))
            .map(|text| format!("clip: pasted {text}")),
        _ => {
            say("clip: usage: clip copy <text> | clip paste");
            std::process::exit(2);
        }
    };
    match answer {
        Ok(line) => say(&line),
        Err(error) => {
            say(&format!("clip: failed: {error}"));
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
