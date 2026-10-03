//! What `authd` and its clients say to each other (`docs/AUTH.md` §3.3).
//!
//! ```text
//! client -> authd
//!   BEGIN        service, account (empty: the caller's own), method hint (empty: the policy's)
//!   RESPOND      the answer to the last PROMPT, as a Secret
//!   CANCEL
//!   STATUS       account                       what authctl status and hyprlock ask
//!   RESET        account                       root: clear a throttle
//!   UNLOCK-SEAT                                root: let the seat's lock go (phase 2)
//!
//! authd -> client
//!   PROMPT       secret or visible, text      PAM_PROMPT_ECHO_OFF / _ON
//!   INFO         text                         PAM_TEXT_INFO
//!   ERROR        text                         PAM_ERROR_MSG
//!   ACCEPTED     uid, account                 the conversation is over
//!   FAILED       retry after (ms), text       the conversation is over
//!   UNAVAILABLE  text                         no verdict could be had
//!   STATE        credential set, methods, throttled for (ms)
//! ```
//!
//! # On the wire
//!
//! One record per `SOCK_SEQPACKET` packet, at most [`MAX_RECORD`] bytes, so
//! a record is never cut and never joined to the next: the socket keeps the
//! boundaries. A record is a kind byte, a version byte ([`VERSION`]), then
//! the kind's fields in order. A number is little-endian; a flag one byte,
//! 0 or 1; a string a `u16` length and its bytes. Decoding is strict: an
//! unknown kind or version, a flag of 2, a string that is too long or not
//! of its alphabet, or a byte left over are each a [`DecodeError`], so that
//! one record has exactly one spelling.
//!
//! Nothing here allocates. [`Record::decode`] borrows its strings from the
//! packet, and [`Record::encode`] writes into a buffer the caller holds, so a
//! native program with no allocator can speak it as well as `authd` can.
//!
//! # Secrets
//!
//! A response is a [`Secret`]: a fixed buffer that is zeroed when dropped,
//! is never `Clone` and never shown by `Debug`. A decoded RESPOND borrows
//! the packet's bytes; [`Secret::from_bytes`] copies them into one, and the
//! caller then zeroes the packet (`docs/AUTH.md` §3.8).

#![no_std]
#![forbid(unsafe_code)]

mod secret;
#[cfg(test)]
mod tests;

pub use secret::Secret;

/// Where `authd` listens.
pub const SOCKET: &str = "/run/ferrix/auth";

/// The protocol version every record carries.
pub const VERSION: u8 = 1;

/// The longest record, in bytes.
pub const MAX_RECORD: usize = 1024;

/// The longest secret a RESPOND carries: more than any method needs, and
/// the size of [`Secret`]'s buffer.
pub const MAX_SECRET: usize = 256;

/// The longest text a PROMPT, INFO, ERROR, FAILED or UNAVAILABLE carries.
pub const MAX_TEXT: usize = 256;

/// The longest service or method name.
pub const MAX_NAME: usize = 32;

/// The longest account name: `LOGIN_NAME_MAX` less its NUL, as shadow-utils
/// allows.
pub const MAX_ACCOUNT: usize = 32;

/// Method bits a STATE reports.
pub mod method {
    /// A password.
    pub const PASSWORD: u32 = 1 << 0;
    /// A time-based one-time code (phase 3).
    pub const TOTP: u32 = 1 << 1;
    /// A FIDO2 security key (phase 3).
    pub const FIDO2: u32 = 1 << 2;
    /// A fingerprint (later).
    pub const FINGERPRINT: u32 = 1 << 3;
    /// Every bit defined.
    pub const ALL: u32 = PASSWORD | TOTP | FIDO2 | FINGERPRINT;
}

/// One record, borrowing its strings from the packet it came from.
#[derive(Debug, PartialEq, Eq)]
pub enum Record<'a> {
    /// Start a conversation.
    Begin {
        /// The service whose policy applies: `hyprlock`, `login`, `passwd`.
        service: &'a str,
        /// Who is to be authenticated; empty for the caller's own account.
        account: &'a str,
        /// A method the caller would like, `fingerprint` say; empty for the
        /// policy's.
        method: &'a str,
    },
    /// The answer to the last PROMPT.
    Respond(Response<'a>),
    /// End the conversation without a verdict.
    Cancel,
    /// Ask what an account has.
    Status {
        /// The account; empty for the caller's own.
        account: &'a str,
    },
    /// Clear an account's throttle. Only root may.
    Reset {
        /// The account.
        account: &'a str,
    },
    /// Let the seat's lock go, audited. Only root may.
    UnlockSeat,
    /// The seat's current lock, from `sessiond` on the `ferrix.auth.seat`
    /// channel (`docs/AUTH.md` §3.7): the next accepted conversation of a
    /// `Grant=seat` service for `uid` grants it. Never taken from the socket.
    Arm {
        /// The session's user.
        uid: u32,
        /// The lock's number, which the compositor gave it.
        epoch: u64,
    },
    /// That lock went without a grant: grant nothing for it.
    Disarm {
        /// The lock's number.
        epoch: u64,
    },
    /// Ask the person something.
    Prompt {
        /// Whether what they type may be shown: false for a password.
        visible: bool,
        /// What to ask: `Password: `.
        text: &'a str,
    },
    /// Something to show the person.
    Info(&'a str),
    /// A problem to show the person.
    Error(&'a str),
    /// The person is who they said.
    Accepted {
        /// Their uid.
        uid: u32,
        /// Their account.
        account: &'a str,
    },
    /// They are not, or could not show it yet.
    Failed {
        /// How long before the next attempt is looked at.
        retry_after_ms: u32,
        /// What to show.
        text: &'a str,
    },
    /// No verdict could be had: no such service, no credential, no store.
    Unavailable(&'a str),
    /// The armed lock may go: `uid` showed who they are to a `Grant=seat`
    /// service, or root asked with `authctl unlock-seat`. Sent only on the
    /// seat channel.
    Grant {
        /// The session's user, whom the lock is for.
        uid: u32,
        /// The lock's number, as it was armed.
        epoch: u64,
    },
    /// `authd` took the seat channel's client: grants can come now. The
    /// first record down a new seat channel.
    SeatReady,
    /// What an account has, the answer to STATUS.
    State {
        /// Whether any credential is set.
        credential: bool,
        /// The [`method`] bits it has.
        methods: u32,
        /// How long it is throttled for; zero when it is not.
        throttled_ms: u64,
    },
}

/// The bytes of a RESPOND, borrowed from the packet. `Debug` shows only
/// their length, so a record logged by mistake does not log a password.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Response<'a>(pub &'a [u8]);

impl Response<'_> {
    /// The bytes; copy them into a [`Secret`] and zero the packet.
    #[must_use]
    pub const fn bytes(&self) -> &[u8] {
        self.0
    }
}

impl core::fmt::Debug for Response<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Response({} bytes)", self.0.len())
    }
}

/// Why a packet is not a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Empty, or longer than [`MAX_RECORD`].
    Length,
    /// A kind this version does not have.
    Kind,
    /// A version this side does not speak.
    Version,
    /// Cut short inside a field.
    Short,
    /// A flag that is neither 0 nor 1.
    Flag,
    /// A string too long for its field, or outside its alphabet.
    Field,
    /// Bytes left after the last field.
    Trailing,
}

/// Why a record could not be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// The buffer is too small for it.
    Room,
    /// A field is too long or outside its alphabet: it would not decode.
    Field,
}

/// The kinds, as their bytes.
mod kind {
    pub(super) const BEGIN: u8 = 1;
    pub(super) const RESPOND: u8 = 2;
    pub(super) const CANCEL: u8 = 3;
    pub(super) const STATUS: u8 = 4;
    pub(super) const RESET: u8 = 5;
    pub(super) const UNLOCK_SEAT: u8 = 6;
    pub(super) const ARM: u8 = 7;
    pub(super) const DISARM: u8 = 8;
    pub(super) const PROMPT: u8 = 32;
    pub(super) const INFO: u8 = 33;
    pub(super) const ERROR: u8 = 34;
    pub(super) const ACCEPTED: u8 = 35;
    pub(super) const FAILED: u8 = 36;
    pub(super) const UNAVAILABLE: u8 = 37;
    pub(super) const STATE: u8 = 38;
    pub(super) const GRANT: u8 = 39;
    pub(super) const SEAT_READY: u8 = 40;
}

/// What a string field may hold.
#[derive(Debug, Clone, Copy)]
enum Alphabet {
    /// A service or method name: lower-case letters, digits, `.`, `_`, `-`.
    Name,
    /// An account: POSIX's portable user name characters, not starting with
    /// `-`, and empty allowed (the caller's own).
    Account,
    /// Text for a person: UTF-8 with no control characters but tab and
    /// newline, so a prompt cannot move a terminal's cursor.
    Text,
}

impl Alphabet {
    const fn limit(self) -> usize {
        match self {
            Alphabet::Name => MAX_NAME,
            Alphabet::Account => MAX_ACCOUNT,
            Alphabet::Text => MAX_TEXT,
        }
    }

    fn allows(self, text: &str) -> bool {
        if text.len() > self.limit() {
            return false;
        }
        match self {
            Alphabet::Name => text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)),
            Alphabet::Account => {
                !text.starts_with('-')
                    && text
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            }
            Alphabet::Text => text
                .chars()
                .all(|c| !c.is_control() || c == '\n' || c == '\t'),
        }
    }
}

/// A cursor over a packet being decoded.
struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.rest.len() < n {
            return Err(DecodeError::Short);
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut out = [0_u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn flag(&mut self) -> Result<bool, DecodeError> {
        match self.array::<1>()? {
            [0] => Ok(false),
            [1] => Ok(true),
            _ => Err(DecodeError::Flag),
        }
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        self.array().map(u64::from_le_bytes)
    }

    fn bytes(&mut self, limit: usize) -> Result<&'a [u8], DecodeError> {
        let len = usize::from(u16::from_le_bytes(self.array()?));
        if len > limit {
            return Err(DecodeError::Field);
        }
        self.take(len)
    }

    fn string(&mut self, alphabet: Alphabet) -> Result<&'a str, DecodeError> {
        let bytes = self.bytes(alphabet.limit())?;
        let text = core::str::from_utf8(bytes).map_err(|_| DecodeError::Field)?;
        if alphabet.allows(text) {
            Ok(text)
        } else {
            Err(DecodeError::Field)
        }
    }
}

/// A cursor over a buffer being encoded into.
struct Writer<'b> {
    out: &'b mut [u8],
    at: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        let end = self.at.checked_add(bytes.len()).ok_or(EncodeError::Room)?;
        self.out
            .get_mut(self.at..end)
            .ok_or(EncodeError::Room)?
            .copy_from_slice(bytes);
        self.at = end;
        Ok(())
    }

    fn bytes(&mut self, bytes: &[u8], limit: usize) -> Result<(), EncodeError> {
        if bytes.len() > limit {
            return Err(EncodeError::Field);
        }
        let len = u16::try_from(bytes.len()).map_err(|_| EncodeError::Field)?;
        self.put(&len.to_le_bytes())?;
        self.put(bytes)
    }

    fn string(&mut self, text: &str, alphabet: Alphabet) -> Result<(), EncodeError> {
        if !alphabet.allows(text) {
            return Err(EncodeError::Field);
        }
        self.bytes(text.as_bytes(), alphabet.limit())
    }
}

impl<'a> Record<'a> {
    /// Read one packet.
    ///
    /// # Errors
    ///
    /// [`DecodeError`], saying what was wrong.
    pub fn decode(packet: &'a [u8]) -> Result<Record<'a>, DecodeError> {
        if packet.is_empty() || packet.len() > MAX_RECORD {
            return Err(DecodeError::Length);
        }
        let mut r = Reader { rest: packet };
        let [kind, version] = r.array::<2>()?;
        if version != VERSION {
            return Err(DecodeError::Version);
        }
        let record = match kind {
            kind::BEGIN => Record::Begin {
                service: r.string(Alphabet::Name)?,
                account: r.string(Alphabet::Account)?,
                method: r.string(Alphabet::Name)?,
            },
            kind::RESPOND => Record::Respond(Response(r.bytes(MAX_SECRET)?)),
            kind::CANCEL => Record::Cancel,
            kind::STATUS => Record::Status {
                account: r.string(Alphabet::Account)?,
            },
            kind::RESET => Record::Reset {
                account: r.string(Alphabet::Account)?,
            },
            kind::UNLOCK_SEAT => Record::UnlockSeat,
            kind::ARM => Record::Arm {
                uid: r.u32()?,
                epoch: r.u64()?,
            },
            kind::DISARM => Record::Disarm { epoch: r.u64()? },
            kind::SEAT_READY => Record::SeatReady,
            kind::GRANT => Record::Grant {
                uid: r.u32()?,
                epoch: r.u64()?,
            },
            kind::PROMPT => Record::Prompt {
                visible: r.flag()?,
                text: r.string(Alphabet::Text)?,
            },
            kind::INFO => Record::Info(r.string(Alphabet::Text)?),
            kind::ERROR => Record::Error(r.string(Alphabet::Text)?),
            kind::ACCEPTED => Record::Accepted {
                uid: r.u32()?,
                account: r.string(Alphabet::Account)?,
            },
            kind::FAILED => Record::Failed {
                retry_after_ms: r.u32()?,
                text: r.string(Alphabet::Text)?,
            },
            kind::UNAVAILABLE => Record::Unavailable(r.string(Alphabet::Text)?),
            kind::STATE => {
                let credential = r.flag()?;
                let methods = r.u32()?;
                if methods & !method::ALL != 0 {
                    return Err(DecodeError::Field);
                }
                Record::State {
                    credential,
                    methods,
                    throttled_ms: r.u64()?,
                }
            }
            _ => return Err(DecodeError::Kind),
        };
        if r.rest.is_empty() {
            Ok(record)
        } else {
            Err(DecodeError::Trailing)
        }
    }

    /// Write the record into `out`, giving how many bytes it took.
    ///
    /// # Errors
    ///
    /// [`EncodeError::Room`] for a buffer too small, and
    /// [`EncodeError::Field`] for a field that would not decode.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let mut w = Writer { out, at: 0 };
        w.put(&[self.kind(), VERSION])?;
        match *self {
            Record::Begin {
                service,
                account,
                method,
            } => {
                w.string(service, Alphabet::Name)?;
                w.string(account, Alphabet::Account)?;
                w.string(method, Alphabet::Name)?;
            }
            Record::Respond(Response(secret)) => w.bytes(secret, MAX_SECRET)?,
            Record::Cancel | Record::UnlockSeat | Record::SeatReady => {}
            Record::Status { account } | Record::Reset { account } => {
                w.string(account, Alphabet::Account)?;
            }
            Record::Prompt { visible, text } => {
                w.put(&[u8::from(visible)])?;
                w.string(text, Alphabet::Text)?;
            }
            Record::Info(text) | Record::Error(text) | Record::Unavailable(text) => {
                w.string(text, Alphabet::Text)?;
            }
            Record::Accepted { uid, account } => {
                w.put(&uid.to_le_bytes())?;
                w.string(account, Alphabet::Account)?;
            }
            Record::Arm { uid, epoch } | Record::Grant { uid, epoch } => {
                w.put(&uid.to_le_bytes())?;
                w.put(&epoch.to_le_bytes())?;
            }
            Record::Disarm { epoch } => w.put(&epoch.to_le_bytes())?,
            Record::Failed {
                retry_after_ms,
                text,
            } => {
                w.put(&retry_after_ms.to_le_bytes())?;
                w.string(text, Alphabet::Text)?;
            }
            Record::State {
                credential,
                methods,
                throttled_ms,
            } => {
                if methods & !method::ALL != 0 {
                    return Err(EncodeError::Field);
                }
                w.put(&[u8::from(credential)])?;
                w.put(&methods.to_le_bytes())?;
                w.put(&throttled_ms.to_le_bytes())?;
            }
        }
        Ok(w.at)
    }

    /// Its kind byte.
    const fn kind(&self) -> u8 {
        match self {
            Record::Begin { .. } => kind::BEGIN,
            Record::Respond(_) => kind::RESPOND,
            Record::Cancel => kind::CANCEL,
            Record::Status { .. } => kind::STATUS,
            Record::Reset { .. } => kind::RESET,
            Record::UnlockSeat => kind::UNLOCK_SEAT,
            Record::Arm { .. } => kind::ARM,
            Record::Disarm { .. } => kind::DISARM,
            Record::Grant { .. } => kind::GRANT,
            Record::SeatReady => kind::SEAT_READY,
            Record::Prompt { .. } => kind::PROMPT,
            Record::Info(_) => kind::INFO,
            Record::Error(_) => kind::ERROR,
            Record::Accepted { .. } => kind::ACCEPTED,
            Record::Failed { .. } => kind::FAILED,
            Record::Unavailable(_) => kind::UNAVAILABLE,
            Record::State { .. } => kind::STATE,
        }
    }

    /// Whether it ends a conversation: a verdict, or the answer to a
    /// one-record request.
    #[must_use]
    pub const fn is_final(&self) -> bool {
        matches!(
            self,
            Record::Accepted { .. }
                | Record::Failed { .. }
                | Record::Unavailable(_)
                | Record::State { .. }
        )
    }
}
