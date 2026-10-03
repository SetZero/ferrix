//! The session's scope (`docs/INIT.md` §5.6): `session-<n>.scope` under
//! `user-<uid>.slice`, asked of the init while `login` is still root, with
//! `login` itself in it, so the shell it execs and everything that shell
//! starts are the session's.
//!
//! `n` counts from 1 at every boot, in `/run/ferrix/login/next`, a file in a
//! directory only root may enter, under `flock` so that two consoles never
//! take the same number.

use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ferrix_svc_proto::control::{Answer, Call, Framer, SOCKET};

/// Where the count is kept.
const DIRECTORY: &str = "/run/ferrix/login";

/// Put this process in a new session scope of `uid`'s: its name, or why
/// not.
pub(crate) fn join(uid: u32) -> Result<String, String> {
    let n = next().map_err(|error| format!("{DIRECTORY}: {error}"))?;
    let unit = format!("session-{n}.scope");
    let slice = format!("user-{uid}.slice");
    ask(Call::Scope {
        unit: unit.clone(),
        slice: Some(slice.clone()),
        pids: vec![std::process::id()],
    })?;
    Ok(format!("{slice}/{unit}"))
}

/// The next session number.
fn next() -> std::io::Result<u64> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(DIRECTORY)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(format!("{DIRECTORY}/next"))?;
    // SAFETY: flock on a descriptor this process holds, with constants.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut text = String::new();
    let _ = file.read_to_string(&mut text)?;
    let n = text.trim().parse::<u64>().unwrap_or(0).saturating_add(1);
    let _ = file.rewind();
    file.set_len(0)?;
    file.write_all(format!("{n}\n").as_bytes())?;
    Ok(n)
}

/// Send `call` and wait for the init's final answer.
fn ask(call: Call) -> Result<(), String> {
    let mut stream = UnixStream::connect(SOCKET).map_err(|error| format!("{SOCKET}: {error}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    stream
        .write_all(&call.encode())
        .map_err(|error| format!("{SOCKET}: {error}"))?;
    let mut framer = Framer::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| format!("{SOCKET}: {error}"))?;
        if count == 0 {
            return Err("the init closed the socket without an answer".to_owned());
        }
        framer.push(buffer.get(..count).unwrap_or_default());
        while let Ok(Some(record)) = framer.next_record() {
            match Answer::decode(&record) {
                Ok(Answer::Refused(why)) => return Err(format!("the init refused: {why}")),
                Ok(answer) if answer.is_final() => return Ok(()),
                _ => {}
            }
        }
    }
}
