//! The lock channel (`docs/AUTH.md` §3.7, P2.5): how the session's
//! compositor and `sessiond` speak of the lock, beside the device channel.
//!
//! A second socket pair, inherited as descriptor [`LOCK_FD`] and named by
//! [`LOCK_FD_VARIABLE`], so that a grant arriving on its own never lands
//! where the compositor is waiting for a device's answer. Like the device
//! channel it has no path, and the compositor marks it close-on-exec, so no
//! program the compositor starts holds it.
//!
//! One line each way:
//!
//! * the compositor says `locked <epoch>` when it takes a lock, and
//!   `unlocked <epoch>` when that lock goes, `epoch` a number that only
//!   grows;
//! * `sessiond` says `grants on` once `authd`'s seat channel is up and
//!   `grants off` when it is not, and `grant <epoch>` when the session's
//!   user showed `authd` who they are while that lock was up.

use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;

/// The variable naming the lock channel's descriptor.
pub const LOCK_FD_VARIABLE: &str = "FERRIX_LOCK_FD";

/// The descriptor `sessiond` gives the lock channel in the compositor.
pub const LOCK_FD: i32 = 4;

/// The longest line either side sends, with its newline.
pub const MAX_LINE: usize = 32;

/// What the compositor says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromCompositor {
    /// It took the lock numbered so.
    Locked(u64),
    /// That lock went.
    Unlocked(u64),
}

/// What `sessiond` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToCompositor {
    /// Whether grants can come at all: `authd`'s seat channel is up.
    Grants(bool),
    /// The lock numbered so may go.
    Grant(u64),
}

/// A decimal number, plain: digits only, no sign, no leading zero but `0`.
fn number(text: &str) -> Option<u64> {
    let plain = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    plain.then(|| text.parse().ok()).flatten()
}

impl FromCompositor {
    /// Its line, newline and all.
    #[must_use]
    pub fn line(self) -> String {
        match self {
            FromCompositor::Locked(epoch) => format!("locked {epoch}\n"),
            FromCompositor::Unlocked(epoch) => format!("unlocked {epoch}\n"),
        }
    }

    /// Read a line, without its newline; `None` for anything else.
    #[must_use]
    pub fn parse(line: &str) -> Option<FromCompositor> {
        let (word, rest) = line.split_once(' ')?;
        let epoch = number(rest)?;
        match word {
            "locked" => Some(FromCompositor::Locked(epoch)),
            "unlocked" => Some(FromCompositor::Unlocked(epoch)),
            _ => None,
        }
    }
}

impl ToCompositor {
    /// Its line, newline and all.
    #[must_use]
    pub fn line(self) -> String {
        match self {
            ToCompositor::Grants(true) => "grants on\n".to_owned(),
            ToCompositor::Grants(false) => "grants off\n".to_owned(),
            ToCompositor::Grant(epoch) => format!("grant {epoch}\n"),
        }
    }

    /// Read a line, without its newline; `None` for anything else.
    #[must_use]
    pub fn parse(line: &str) -> Option<ToCompositor> {
        match line {
            "grants on" => Some(ToCompositor::Grants(true)),
            "grants off" => Some(ToCompositor::Grants(false)),
            _ => number(line.strip_prefix("grant ")?).map(ToCompositor::Grant),
        }
    }
}

/// Take the lock channel `sessiond` left this process, if it left one:
/// close-on-exec, its variable out of the environment, as
/// [`adopt`](crate::adopt) does for the device channel.
///
/// `None` when the variable is not set: no session. `Some(None)` when it is
/// set and names no socket this process holds: still a session's
/// compositor, which must then refuse its locks rather than let a holder
/// unlock alone. `Some(Some(_))`: the channel.
///
/// Call it first thing in `main`, before any thread or child: it changes the
/// environment.
#[must_use]
pub fn adopt_lock() -> Option<Option<UnixStream>> {
    let named = std::env::var(LOCK_FD_VARIABLE).ok()?;
    Some(take_lock(&named))
}

/// The channel [`LOCK_FD_VARIABLE`] named, out of the environment.
fn take_lock(named: &str) -> Option<UnixStream> {
    #[expect(
        unsafe_code,
        reason = "AUDIT: the environment is changed before the compositor starts a thread; adopt_lock's caller promises it"
    )]
    // SAFETY: called at the start of `main`, before any other thread exists
    // to read the environment at the same time.
    unsafe {
        std::env::remove_var(LOCK_FD_VARIABLE);
    }
    let fd: i32 = named.parse().ok().filter(|fd| *fd > 2)?;
    if !crate::is_socket(fd) {
        return None;
    }
    crate::set_cloexec(fd);
    #[expect(
        unsafe_code,
        reason = "AUDIT: the descriptor sessiond gave this process, named by the variable just removed; nothing else here owns it"
    )]
    // SAFETY: `fd` is an open socket (fstat said so) and is the lock
    // channel's, which no other owner in this process holds.
    Some(unsafe { UnixStream::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_reads_back_as_itself() {
        for said in [
            FromCompositor::Locked(0),
            FromCompositor::Locked(u64::MAX),
            FromCompositor::Unlocked(7),
        ] {
            let line = said.line();
            assert!(line.len() <= MAX_LINE, "{line}");
            assert_eq!(
                FromCompositor::parse(line.trim_end_matches('\n')),
                Some(said)
            );
        }
        for said in [
            ToCompositor::Grants(true),
            ToCompositor::Grants(false),
            ToCompositor::Grant(u64::MAX),
        ] {
            let line = said.line();
            assert!(line.len() <= MAX_LINE, "{line}");
            assert_eq!(ToCompositor::parse(line.trim_end_matches('\n')), Some(said));
        }
    }

    /// One spelling a line: nothing else is read as one, so nothing the
    /// compositor sends can be taken for a grant.
    #[test]
    fn anything_else_is_nothing() {
        for line in [
            "",
            "locked",
            "locked ",
            "locked -1",
            "locked +1",
            "locked 01",
            "locked 1 ",
            "locked 18446744073709551616",
            "LOCKED 1",
            "grant 1",
            "grants on",
        ] {
            assert_eq!(FromCompositor::parse(line), None, "{line:?}");
        }
        for line in [
            "",
            "grant",
            "grant x",
            "grant 01",
            "grants",
            "grants yes",
            "locked 1",
        ] {
            assert_eq!(ToCompositor::parse(line), None, "{line:?}");
        }
    }
}
