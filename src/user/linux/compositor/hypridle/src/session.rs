//! The channel `loginctl lock-session` reaches hypridle through.
//!
//! On Linux, `loginctl lock-session` asks systemd-logind over D-Bus, logind
//! emits `org.freedesktop.login1.Session.Lock`, and hypridle, which listens
//! for it, runs `lock_cmd`. Ferrix has neither logind nor D-Bus, so the
//! middle of that chain is one Unix socket, `$XDG_RUNTIME_DIR/hypridle.sock`
//! (the temporary directory when nothing set the variable, as the
//! compositor and `hyprctl` fall back to), which hypridle listens on and
//! Ferrix's own `loginctl` writes to. The request is one line naming the
//! verb, and the answer one line: `ok`, or `error: ` and why.
//!
//! What the verbs mean is logind's: `lock-session` and `lock-sessions` both
//! lock the one session there is, and a session id after the verb is
//! accepted and ignored, because there is only one.

use std::path::PathBuf;

/// The file name of the socket, in the runtime directory.
pub const SOCKET: &str = "hypridle.sock";

/// Where the socket is: `$XDG_RUNTIME_DIR/hypridle.sock`, or the temporary
/// directory's.
#[must_use]
pub fn path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(SOCKET)
}

/// What `loginctl` asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// `lock-session`, `lock-sessions`.
    Lock,
    /// `unlock-session`, `unlock-sessions`.
    Unlock,
}

impl Request {
    /// The request a `loginctl` verb is, if it is one of these.
    #[must_use]
    pub fn from_verb(verb: &str) -> Option<Self> {
        match verb {
            "lock-session" | "lock-sessions" => Some(Self::Lock),
            "unlock-session" | "unlock-sessions" => Some(Self::Unlock),
            _ => None,
        }
    }

    /// The line that carries it.
    #[must_use]
    pub fn line(self) -> &'static str {
        match self {
            Self::Lock => "lock-session\n",
            Self::Unlock => "unlock-session\n",
        }
    }

    /// The request a line carries.
    ///
    /// # Errors
    ///
    /// The sentence sent back for a line that is not one.
    pub fn parse(line: &str) -> Result<Self, String> {
        let verb = line.split_whitespace().next().unwrap_or("");
        Self::from_verb(verb).ok_or_else(|| format!("hypridle does not answer `{}`", line.trim()))
    }
}
