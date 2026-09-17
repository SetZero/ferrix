//! `hyprctl`'s command line.

use std::io::Write as _;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.is_empty() || arguments.iter().any(|word| word == "--help") {
        say("hyprctl: usage: hyprctl [-j] <command> [arguments...]");
        std::process::exit(2);
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
