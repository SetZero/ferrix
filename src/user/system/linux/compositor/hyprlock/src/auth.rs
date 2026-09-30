//! Who is locked out, and the one narrow door a password goes through.
//!
//! Upstream hyprlock hands the typed password to PAM. Ferrix has no PAM:
//! it authenticates through `authd` (`docs/AUTH.md`, approved 2026-09-26),
//! a service that owns every credential and answers a conversation on
//! `/run/ferrix/auth`. hyprlock knows authentication only through
//! [`Backend`], the conversation `docs/AUTH.md` §4.4 settles on:
//! [`Backend::begin`] opens one and gives the first prompt, whose text
//! `$PAMPROMPT` shows before anybody types; [`Backend::respond`] answers
//! it and gives the next prompt or a [`Verdict`]. The widgets, the field
//! and the session never see anything else, so `authd`'s client
//! (phase 1's `Service` backend, once `src/lib/proto/auth-proto` lands)
//! changes nothing outside this module.
//!
//! Until then `/bin/hyprlock` has [`Missing`]: no service is running, the
//! conversation cannot begin, and hyprlock does not take the lock (§5.4:
//! a lock that nothing can open locks the person out). The gate boot's
//! `hyprlock-gate` has a test-only backend of its own.

/// Upstream's text for a wrong password.
pub const REJECTED: &str = "Authentication failed";

/// A secret as typed: its bytes are overwritten when it is dropped, so a
/// password does not outlive its check in memory this program frees.
#[derive(Default)]
pub struct Secret(Vec<u8>);

impl Secret {
    /// The bytes of `text`, taken over.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self(text.into_bytes())
    }

    /// The bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // A volatile write, so the zeroing is not optimised away as a
            // store to memory about to be freed.
            let byte: *mut u8 = byte;
            #[expect(
                unsafe_code,
                reason = "AUDIT: write_volatile is how a store that must happen is written"
            )]
            // SAFETY: `byte` points at a byte of this vector, which is live.
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Secret({} bytes)", self.0.len())
    }
}

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
        /// How long before the next attempt.
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
/// that draws.
pub trait Backend: Send + Sync + std::fmt::Debug {
    /// Start a conversation, and say what it asks first.
    ///
    /// # Errors
    ///
    /// The verdict, when there can be no conversation: no service, no
    /// credential for this account.
    fn begin(&self) -> Result<Prompt, Verdict>;

    /// Answer the last prompt.
    fn respond(&self, secret: &Secret) -> Next;
}

/// What hyprlock says when no authentication service is running.
pub const NO_SERVICE: &str = "no authentication service is running";

/// No authentication service: there is nothing to converse with.
#[derive(Clone, Copy, Debug, Default)]
pub struct Missing;

impl Backend for Missing {
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
mod tests {
    use super::{Account, Backend, Missing, NO_SERVICE, Next, Secret, Verdict, account};

    #[test]
    fn the_account_is_the_uid_s_line() {
        let dir = std::env::temp_dir().join(format!("hyprlock-auth-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("etc")).expect("a test directory");
        std::fs::write(
            dir.join("etc/passwd"),
            "root:x:0:0:root:/:/bin/sh\nferrix:x:1000:1000:Ferrix User:/home/ferrix:/bin/zsh\n",
        )
        .expect("a test file");
        let found = (account(&dir, 1000), account(&dir, 0), account(&dir, 7));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            found.0,
            Some(Account {
                name: "ferrix".to_owned(),
                gecos: "Ferrix User".to_owned(),
            })
        );
        assert_eq!(found.1.map(|account| account.name), Some("root".to_owned()));
        assert_eq!(found.2, None);
    }

    #[test]
    fn with_no_service_nothing_begins() {
        assert_eq!(
            Missing.begin(),
            Err(Verdict::Unavailable(NO_SERVICE.to_owned()))
        );
        assert_eq!(
            Missing.respond(&Secret::new("x".to_owned())),
            Next::Verdict(Verdict::Unavailable(NO_SERVICE.to_owned()))
        );
    }

    #[test]
    fn a_secret_says_only_its_length() {
        assert_eq!(
            format!("{:?}", Secret::new("hunter2".to_owned())),
            "Secret(7 bytes)"
        );
    }
}
