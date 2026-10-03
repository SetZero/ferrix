//! Who is locked out, and the one narrow door a password goes through.
//!
//! Upstream hyprlock hands the typed password to PAM. Ferrix has no PAM:
//! it authenticates through `authd` (`docs/AUTH.md`), a service that owns
//! every credential and answers a conversation on `/run/ferrix/auth`.
//! hyprlock knows authentication only through [`Backend`], the
//! conversation `docs/AUTH.md` §4.4 settles on: [`Backend::ready`] says
//! before the lock is taken whether there is anything to unlock it with,
//! [`Backend::begin`] opens a conversation and gives the first prompt,
//! whose text `$PAMPROMPT` shows before anybody types, and
//! [`Backend::respond`] answers it and gives the next prompt or a
//! [`Verdict`]. The widgets, the field and the session never see anything
//! else.
//!
//! [`Service`] is `authd`'s client and what `/bin/hyprlock` uses. It asks
//! for the service `hyprlock` (`/lib/ferrix/auth/services/hyprlock`: the
//! caller's own account, a two-second hold on a refusal) and the account
//! this process runs as -- root on a phase-1 desktop, whose session is root
//! (decision 3). hyprlock itself never sees a hash, and adds no delay of its
//! own: `authd` holds a refusal for the policy's time, and says in
//! `retry_after_ms` how long before the next attempt is looked at.

use std::io;
use std::sync::Mutex;

use ferrix_auth_client::{Answer, Connection};
pub use ferrix_auth_proto::Secret;
use ferrix_auth_proto::{MAX_RECORD, Record, Response};

/// Upstream's text for a wrong password.
pub const REJECTED: &str = "Authentication failed";

/// What the backend asks for next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    /// Its text: `Password: `, what `$PAMPROMPT` shows.
    pub text: String,
    /// Whether the answer is a secret (`PAM_PROMPT_ECHO_OFF`), which every
    /// prompt hyprlock can answer is: it draws dots, not text.
    pub secret: bool,
}

/// How a conversation ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The secret is the user's.
    Accepted,
    /// It is not. The text is what `$PAMFAIL` and `$FAIL` show; the field
    /// takes no new attempt for `retry_after_ms`.
    Failed {
        /// What to show.
        text: String,
        /// How long before the next attempt is looked at.
        retry_after_ms: u64,
    },
    /// No verdict could be had; the text says why.
    Unavailable(String),
}

impl Verdict {
    /// What `$PAMFAIL` shows after it: upstream's `Successfully
    /// authenticated`, or the failure's own text.
    #[must_use]
    pub fn fail_text(&self) -> &str {
        match self {
            Self::Accepted => "Successfully authenticated",
            Self::Failed { text, .. } | Self::Unavailable(text) => text,
        }
    }
}

/// What an answer led to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Another prompt in the same conversation.
    Prompt(Prompt),
    /// The end of it.
    Verdict(Verdict),
}

/// The door. Its calls may block, so hyprlock makes them off the thread
/// that draws, except [`Backend::ready`] and the first
/// [`Backend::begin`], which come before there is anything to draw.
pub trait Backend: Send + Sync + std::fmt::Debug {
    /// Whether this account can be unlocked at all.
    ///
    /// # Errors
    ///
    /// Why not: no service, no credential set (`docs/AUTH.md` §5.4).
    fn ready(&self) -> Result<(), Verdict> {
        Ok(())
    }

    /// Start a conversation, and say what it asks first.
    ///
    /// # Errors
    ///
    /// The verdict, when there can be no conversation.
    fn begin(&self) -> Result<Prompt, Verdict>;

    /// Answer the last prompt.
    fn respond(&self, secret: &Secret) -> Next;
}

/// What hyprlock says when no authentication service is running.
pub const NO_SERVICE: &str = "no authentication service is running";

/// The service name hyprlock converses as.
pub const SERVICE: &str = "hyprlock";

/// How a [`Service`] reaches `authd`: [`Connection::open`], or a test's
/// own end of a socket pair.
type Opener = Box<dyn Fn() -> io::Result<Connection> + Send + Sync>;

/// `authd`'s client: one [`Connection`] a conversation, kept between
/// [`Backend::begin`] and the [`Backend::respond`]s that follow it.
pub struct Service {
    open: Opener,
    connection: Mutex<Option<Connection>>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Service")
    }
}

impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}

/// A socket's failure as a verdict: `authd` not listening is no service.
fn unavailable(error: &io::Error) -> Verdict {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
            Verdict::Unavailable(NO_SERVICE.to_owned())
        }
        _ => {
            crate::say(&format!("hyprlock: authd: {error}"));
            Verdict::Unavailable(format!("the authentication service failed: {error}"))
        }
    }
}

impl Service {
    /// A client for the `authd` at [`ferrix_auth_proto::SOCKET`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_opener(Box::new(Connection::open))
    }

    /// A client that reaches `authd` through `open`.
    #[must_use]
    pub fn with_opener(open: Opener) -> Self {
        Self {
            open,
            connection: Mutex::new(None),
        }
    }

    /// Read records until one that ends this step: a prompt, or a verdict.
    /// An INFO or ERROR on the way is what `$PAMFAIL` shows if the step
    /// ends in a refusal ("wait 16 s" while throttled), and is said on
    /// standard error either way.
    fn step(connection: &Connection) -> io::Result<Next> {
        let mut said: Option<String> = None;
        loop {
            let mut buffer = [0_u8; MAX_RECORD + 1];
            match connection.receive(&mut buffer)? {
                Record::Prompt { visible, text } => {
                    return Ok(Next::Prompt(Prompt {
                        text: text.to_owned(),
                        secret: !visible,
                    }));
                }
                Record::Info(text) | Record::Error(text) => {
                    crate::say(&format!("hyprlock: authd: {text}"));
                    said = Some(text.to_owned());
                }
                Record::Accepted { .. } => return Ok(Next::Verdict(Verdict::Accepted)),
                Record::Failed {
                    retry_after_ms,
                    text,
                } => {
                    return Ok(Next::Verdict(Verdict::Failed {
                        text: said.unwrap_or_else(|| text.to_owned()),
                        retry_after_ms: u64::from(retry_after_ms),
                    }));
                }
                Record::Unavailable(text) => {
                    return Ok(Next::Verdict(Verdict::Unavailable(text.to_owned())));
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "authd sent a record a conversation does not have",
                    ));
                }
            }
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Connection>> {
        self.connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Backend for Service {
    fn ready(&self) -> Result<(), Verdict> {
        let connection = (self.open)().map_err(|error| unavailable(&error))?;
        match connection.request(&Record::Status { account: "" }) {
            Ok(Answer::State(state)) if state.credential => Ok(()),
            Ok(Answer::State(_)) => {
                let who = account(std::path::Path::new("/"), uid())
                    .map_or_else(|| "this account".to_owned(), |account| account.name);
                Err(Verdict::Unavailable(format!(
                    "no password is set for {who}: run `passwd` first"
                )))
            }
            Ok(Answer::Verdict(verdict)) => Err(match verdict {
                ferrix_auth_client::Verdict::Unavailable(text)
                | ferrix_auth_client::Verdict::Failed { text, .. } => Verdict::Unavailable(text),
                ferrix_auth_client::Verdict::Accepted { .. } => {
                    Verdict::Unavailable("authd answered STATUS out of turn".to_owned())
                }
            }),
            Err(error) => Err(unavailable(&error)),
        }
    }

    fn begin(&self) -> Result<Prompt, Verdict> {
        let connection = (self.open)().map_err(|error| unavailable(&error))?;
        connection
            .send(&Record::Begin {
                service: SERVICE,
                account: "",
                method: "",
            })
            .map_err(|error| unavailable(&error))?;
        match Self::step(&connection) {
            Ok(Next::Prompt(prompt)) => {
                *self.slot() = Some(connection);
                Ok(prompt)
            }
            Ok(Next::Verdict(verdict)) => Err(verdict),
            Err(error) => Err(unavailable(&error)),
        }
    }

    fn respond(&self, secret: &Secret) -> Next {
        let mut slot = self.slot();
        let Some(connection) = slot.as_ref() else {
            return Next::Verdict(Verdict::Unavailable(
                "no conversation is open with authd".to_owned(),
            ));
        };
        let mut next = connection
            .send(&Record::Respond(Response(secret.expose())))
            .and_then(|()| Self::step(connection));
        // A conversation begun when the lock was taken may have been closed
        // by authd while nobody typed: begin again, once, and answer that.
        if next.is_err() {
            *slot = None;
            drop(slot);
            if self.begin().is_ok() {
                slot = self.slot();
                if let Some(connection) = slot.as_ref() {
                    next = connection
                        .send(&Record::Respond(Response(secret.expose())))
                        .and_then(|()| Self::step(connection));
                }
            } else {
                slot = self.slot();
            }
        }
        let next = next.unwrap_or_else(|error| Next::Verdict(unavailable(&error)));
        // A verdict ends the conversation: authd closes it, and the next
        // attempt opens a new one.
        if matches!(next, Next::Verdict(_)) {
            *slot = None;
        }
        next
    }
}

/// No authentication service at all: what a test, or a build without
/// `authd`, has.
#[derive(Clone, Copy, Debug, Default)]
pub struct Missing;

impl Backend for Missing {
    fn ready(&self) -> Result<(), Verdict> {
        Err(Verdict::Unavailable(NO_SERVICE.to_owned()))
    }

    fn begin(&self) -> Result<Prompt, Verdict> {
        Err(Verdict::Unavailable(NO_SERVICE.to_owned()))
    }

    fn respond(&self, _secret: &Secret) -> Next {
        Next::Verdict(Verdict::Unavailable(NO_SERVICE.to_owned()))
    }
}

/// The user this process runs as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// The login name, which `$USER` shows.
    pub name: String,
    /// The GECOS field, which `$DESC` shows.
    pub gecos: String,
}

/// The account `uid` is, from the `/etc/passwd` under `root`: upstream's
/// `getpwuid(getuid())`, for `$USER` and `$DESC`. Nothing here reads a
/// credential.
#[must_use]
pub fn account(root: &std::path::Path, uid: u32) -> Option<Account> {
    let text = std::fs::read_to_string(root.join("etc/passwd")).ok()?;
    text.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let (Some(name), Some(id), Some(gecos)) = (fields.first(), fields.get(2), fields.get(4))
        else {
            return None;
        };
        (id.parse::<u32>().ok() == Some(uid) && !name.is_empty()).then(|| Account {
            name: (*name).to_owned(),
            gecos: (*gecos).to_owned(),
        })
    })
}

/// The real uid of this process.
#[must_use]
pub fn uid() -> u32 {
    #[expect(
        unsafe_code,
        reason = "AUDIT: getuid(2) is not in std; it takes nothing and cannot fail"
    )]
    // SAFETY: getuid has no preconditions and touches no memory of ours.
    unsafe {
        libc::getuid()
    }
}

#[cfg(test)]
mod tests;
