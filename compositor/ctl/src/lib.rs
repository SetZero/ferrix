//! `hyprctl`: find the compositor's control socket, and use it.
//!
//! Hyprland's own `hyprctl` is a program that opens
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`, writes
//! one line, reads the answer and prints it. That is the whole of it, and
//! this is the same program: a client of the socket `compositor/hyprix`
//! serves, written here because Ferrix has no Hyprland to take one from.
//!
//! The shape of a request and of an answer is `compositor/ipc`'s, which is
//! host-tested against Hyprland's own documented forms and against real
//! `hyprctl` through a committed probe. This crate is the socket and the
//! command line.
//!
//! # Finding the instance
//!
//! `HYPRLAND_INSTANCE_SIGNATURE` names it. Without the variable, Hyprland's
//! `hyprctl` lists the instance directories and takes the newest; this takes
//! the only one, and says so when there is more than one, because guessing
//! between two running compositors is worse than asking.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// How long to wait for an answer. A compositor that is drawing a frame
/// answers a moment later; one that is wedged should not hang a script.
const PATIENCE: Duration = Duration::from_secs(5);

/// Where the instance directory is: `$XDG_RUNTIME_DIR/hypr`, or the
/// temporary directory when there is no session manager to set the variable,
/// which is what `hyprix` falls back to as well.
#[must_use]
pub fn runtime() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("hypr")
}

/// The event socket, beside the request one.
///
/// # Errors
///
/// As [`socket`].
pub fn event_socket() -> Result<PathBuf, String> {
    socket().map(|path| path.with_file_name(compositor_ipc::EVENT_SOCKET))
}

/// Read the event socket for as long as it is open, giving each line to
/// `each`.
///
/// Hyprland's own readers use `socat - .socket2.sock`; Ferrix has no socat,
/// so this is it. The socket is never written to.
///
/// # Errors
///
/// A sentence saying what the socket said. A compositor that went away closes
/// the connection, which ends the read without an error.
pub fn subscribe(socket: &std::path::Path, each: &mut dyn FnMut(&str)) -> Result<(), String> {
    use std::io::{BufRead as _, BufReader};

    let stream = UnixStream::connect(socket)
        .map_err(|error| format!("connecting to {}: {error}", socket.display()))?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.map_err(|error| format!("reading an event: {error}"))?;
        each(&line);
    }
    Ok(())
}

/// The socket to talk to, named by the environment or found by looking.
///
/// # Errors
///
/// A sentence saying what was not there, or that there was more than one
/// compositor to choose between.
pub fn socket() -> Result<PathBuf, String> {
    let directory = runtime();
    if let Some(instance) = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") {
        return Ok(directory
            .join(instance)
            .join(compositor_ipc::REQUEST_SOCKET));
    }
    let entries = std::fs::read_dir(&directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let mut instances: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect();
    instances.sort();
    match instances.as_slice() {
        [] => Err(format!(
            "no compositor is running under {}",
            directory.display()
        )),
        [only] => Ok(only.join(compositor_ipc::REQUEST_SOCKET)),
        many => Err(format!(
            "{} compositors are running; set HYPRLAND_INSTANCE_SIGNATURE",
            many.len()
        )),
    }
}

/// Send `request` and give back the answer.
///
/// One request a connection, which is what the socket serves and what
/// Hyprland's `hyprctl` does: the answer ends when the compositor closes its
/// end, so the read is until end of file and needs no length.
///
/// # Errors
///
/// A sentence saying what the socket said.
pub fn ask(socket: &std::path::Path, request: &str) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|error| format!("connecting to {}: {error}", socket.display()))?;
    stream
        .set_read_timeout(Some(PATIENCE))
        .map_err(|error| format!("a read timeout: {error}"))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("writing the request: {error}"))?;
    // Hyprland's own clients write the line and then shut the write half, so
    // a compositor reading to end of file is answered rather than waiting.
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| format!("finishing the request: {error}"))?;
    let mut answer = String::new();
    let _ = stream
        .read_to_string(&mut answer)
        .map_err(|error| format!("reading the answer: {error}"))?;
    Ok(answer)
}

/// The request line the words on a command line make.
///
/// `hyprctl -j clients` is the line `j/clients`, and
/// `hyprctl dispatch movefocus l` is `dispatch movefocus l`: the flags go
/// before a `/` and the rest is the command with its arguments, separated by
/// spaces, exactly as Hyprland's `hyprctl` builds it.
///
/// `--batch` is `batchRequest`'s line: `[[BATCH]]` and the commands with
/// their `;` between them, and with `-j` the flags in front of each command
/// rather than in front of the line.
#[must_use]
pub fn line(arguments: &[String]) -> String {
    let mut flags = String::new();
    let mut batch = false;
    let mut words: Vec<&str> = Vec::new();
    for argument in arguments {
        match argument.as_str() {
            "-j" | "--json" => flags.push('j'),
            "-r" => flags.push('r'),
            "--batch" => batch = true,
            other => words.push(other),
        }
    }
    let command = words.join(" ");
    if batch {
        return format!("[[BATCH]]{}", with_flags(&command, &flags));
    }
    if flags.is_empty() {
        command
    } else {
        format!("{flags}/{command}")
    }
}

/// Put `flags` in front of every command of a batch, which is what
/// `batchRequest` does with its `;\s*` replacement.
fn with_flags(commands: &str, flags: &str) -> String {
    if flags.is_empty() {
        return commands.to_owned();
    }
    commands
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| format!("{flags}/{part}"))
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::line;

    fn owned(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_command_line_is_the_line_hyprctl_writes() {
        assert_eq!(line(&owned(&["clients"])), "clients");
        assert_eq!(line(&owned(&["-j", "clients"])), "j/clients");
        assert_eq!(
            line(&owned(&["dispatch", "movefocus", "l"])),
            "dispatch movefocus l"
        );
        assert_eq!(
            line(&owned(&["-j", "keyword", "general:gaps_in", "10"])),
            "j/keyword general:gaps_in 10"
        );
        // A flag anywhere is a flag, as `hyprctl`'s own parsing has it.
        assert_eq!(line(&owned(&["clients", "-j"])), "j/clients");
        assert_eq!(line(&[]), "");
    }

    #[test]
    fn a_batch_is_one_line_of_commands() {
        assert_eq!(
            line(&owned(&[
                "--batch",
                "dispatch",
                "togglegroup",
                ";",
                "dispatch",
                "movefocus",
                "l"
            ])),
            "[[BATCH]]dispatch togglegroup ; dispatch movefocus l"
        );
        // With `-j` the flags go in front of each command, not the line:
        // `batchRequest` replaces every `;` with `;j/` and prefixes the
        // first.
        assert_eq!(
            line(&owned(&["--batch", "-j", "clients", ";", "workspaces"])),
            "[[BATCH]]j/clients;j/workspaces"
        );
    }
}
