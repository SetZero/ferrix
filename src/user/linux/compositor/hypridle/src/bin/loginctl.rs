//! `loginctl lock-session` and `loginctl unlock-session`, for a Ferrix
//! without logind.
//!
//! systemd's `loginctl` asks logind, and logind tells the session's
//! listeners -- hypridle among them -- to lock. Ferrix has no logind, so
//! this tells the one listener there is directly, over the socket
//! `compositor_hypridle::session` describes, and says so when nothing is
//! listening rather than succeeding at nothing. Every other verb is
//! logind's and is refused by name.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use compositor_hypridle::session::{self, Request};

/// How long hypridle has to answer.
const PATIENCE: Duration = Duration::from_secs(5);

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let verb = arguments
        .iter()
        .find(|word| !word.starts_with('-'))
        .map_or("", String::as_str);
    let Some(request) = Request::from_verb(verb) else {
        say(&format!(
            "loginctl: {}: Ferrix has no logind; only lock-session, unlock-session, \
             lock-sessions and unlock-sessions are carried out, by hypridle",
            if verb.is_empty() {
                "list-sessions"
            } else {
                verb
            }
        ));
        std::process::exit(1);
    };
    let path = session::path();
    match ask(&path, request) {
        Ok(()) => {}
        Err(error) => {
            say(&format!("loginctl: {error}"));
            std::process::exit(1);
        }
    }
}

fn ask(path: &std::path::Path, request: Request) -> Result<(), String> {
    let mut stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "nothing is listening for {} at {} ({error}): is hypridle running?",
            request.line().trim(),
            path.display()
        )
    })?;
    let _ = stream.set_read_timeout(Some(PATIENCE));
    stream
        .write_all(request.line().as_bytes())
        .map_err(|error| format!("writing to {}: {error}", path.display()))?;
    let mut reply = String::new();
    let _ = BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|error| format!("reading hypridle's answer: {error}"))?;
    match reply.trim() {
        "ok" => Ok(()),
        other => Err(other
            .strip_prefix("error: ")
            .unwrap_or(if other.is_empty() {
                "hypridle did not answer"
            } else {
                other
            })
            .to_owned()),
    }
}

fn say(line: &str) {
    let mut out = std::io::stderr();
    let _ = writeln!(out, "{line}");
}
