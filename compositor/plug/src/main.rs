//! An example plugin: a program the compositor starts, which adds a
//! dispatcher of its own.
//!
//! Hyprland's plugins are shared objects loaded into the compositor;
//! `compositor/hyprix`'s `plugins` module says why this one is a program and
//! what the protocol is. This is the smallest plugin that does something a
//! person can see:
//!
//! * it says what it is, which is what `hyprctl plugin list` prints;
//! * it registers `swapthem`, so `bind = SUPER, P, swapthem` and
//!   `hyprctl dispatch swapthem` both reach it;
//! * when the dispatcher arrives it sends `dispatch movewindow r` back,
//!   which swaps the focused window with its neighbour;
//! * it subscribes, and prints every event it is told.
//!
//! It is `cargo xtask test-compositor`'s plugin, and the picture the
//! compositor draws after the keybind is the one a `movewindow r` makes:
//! proof that a dispatcher a plugin added did what the plugin says it does.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;

/// What this plugin says it is: the four fields `PLUGIN_INIT` returns in
/// Hyprland, in the order the protocol's line carries them.
const WHAT: &str = "swap,ferrix,1.0,swaps the focused window with its neighbour";

/// The dispatcher it adds.
const DISPATCHER: &str = "swapthem";

/// What it does when that dispatcher arrives: focus the window to the left
/// and move it right, which exchanges the two of them and leaves the focus
/// on the one that moved. Two dispatchers, sent as one batch, because a
/// plugin that sent them a pass apart would draw a frame in between.
const ACTION: &str = "[[BATCH]]dispatch movefocus l ; dispatch movewindow r";

fn main() {
    match run() {
        Ok(what) => say(&format!("plug: {what}")),
        Err(why) => {
            say(&format!("plug: {why}"));
            std::process::exit(1);
        }
    }
}

/// Say a line on the standard output, flushed: a plugin runs as a child of
/// the compositor with the console for its output, and a line held in a
/// buffer is a line a test never sees.
fn say(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Connect, register, and answer whatever the compositor says until it goes.
fn run() -> Result<String, String> {
    let socket = compositor_ctl::socket()?;
    let mut stream = UnixStream::connect(&socket)
        .map_err(|error| format!("connecting to {}: {error}", socket.display()))?;
    let mut writer = stream
        .try_clone()
        .map_err(|error| format!("keeping the connection: {error}"))?;
    let tell = |writer: &mut UnixStream, line: &str| -> Result<(), String> {
        writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.flush())
            .map_err(|error| format!("writing `{}`: {error}", line.trim()))
    };
    tell(&mut stream, &format!("[[PLUGIN]]{WHAT}\n"))?;
    tell(&mut stream, &format!("handle {DISPATCHER}\n"))?;
    tell(&mut stream, "subscribe\n")?;
    say(&format!("plug: loaded, {DISPATCHER} added"));

    let mut handled = 0u32;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.map_err(|error| format!("reading: {error}"))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // The compositor writes `dispatch>><name>,<argument>` for a
        // dispatcher this plugin registered, and the event socket's own
        // lines for everything it subscribed to.
        if let Some(rest) = line.strip_prefix("dispatch>>") {
            let (name, argument) = rest.split_once(',').unwrap_or((rest, ""));
            say(&format!("plug: dispatched {name} {argument}"));
            if name.eq_ignore_ascii_case(DISPATCHER) {
                handled = handled.saturating_add(1);
                tell(&mut writer, &format!("{ACTION}\n"))?;
            }
            continue;
        }
        if line.contains(">>") {
            say(&format!("plug: event {line}"));
        }
    }
    Ok(format!("the compositor went; {handled} dispatched"))
}
