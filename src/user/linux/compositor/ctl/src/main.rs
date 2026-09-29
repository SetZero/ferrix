//! `hyprctl`'s command line.

use std::io::Write as _;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.is_empty() || arguments.iter().any(|word| word == "--help") {
        say("hyprctl: usage: hyprctl [-j] <command> [arguments...] | hyprctl subscribe");
        std::process::exit(2);
    }
    // `subscribe` is not one of Hyprland's commands: its own readers are
    // `socat - .socket2.sock`, and Ferrix has no socat. Everything else is a
    // request on the other socket.
    if arguments.first().map(String::as_str) == Some("subscribe") {
        let read = compositor_ctl::event_socket()
            .and_then(|socket| compositor_ctl::subscribe(&socket, &mut |line| say(line)));
        if let Err(error) = read {
            say(&format!("hyprctl: {error}"));
            std::process::exit(1);
        }
        return;
    }
    let request = compositor_ctl::line(&arguments);
    let answer = compositor_ctl::socket().and_then(|socket| compositor_ctl::ask(&socket, &request));
    match answer {
        Ok(text) => say(text.trim_end()),
        Err(error) => {
            say(&format!("hyprctl: {error}"));
            std::process::exit(1);
        }
    }
}

fn say(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
