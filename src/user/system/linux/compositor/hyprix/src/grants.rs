//! Whether the session's lock may go: `authd`'s grant, through `sessiond`
//! (`docs/AUTH.md` §3.7, P2.5).
//!
//! A compositor that `sessiond` started as the session's user inherits the
//! lock channel, descriptor 4 (`compositor_seat::lock`), and [`adopt`] takes
//! it at the start of `main`. With it, every lock is numbered, `sessiond` is
//! told of it, and the lock goes only on a grant for that number: the
//! session's user showed `authd` who they are while it was up. Without it --
//! a desktop that is root, or a compositor run in a test's thread -- the
//! lock's own holder decides alone, as before phase 2, since every client
//! there is root anyway.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use compositor_seat::lock::{FromCompositor, MAX_LINE, ToCompositor};

/// How long an unlock waits for its grant. `authd` answers hyprlock and
/// sends the grant in one pass of its loop, and `sessiond` relays it at
/// once; but the two travel different ways, so the unlock may come first.
/// Two seconds covers that on a loaded machine many times over; an unlock
/// whose grant has not come by then is refused, and the screen stays locked.
pub(crate) const UNLOCK_WAIT: Duration = Duration::from_secs(2);

/// The longest `misc:lock_grace` is honoured for, whatever the user's file
/// says: within it a fresh lock's holder unlocks without a grant, which is
/// hyprlock's `--grace`. The file is the user's to edit, so the compositor
/// keeps the bound.
pub(crate) const LOCK_GRACE_CAP: Duration = Duration::from_secs(10);

/// The lock channel `main` took, until the compositor's state takes it.
static ADOPTED: Mutex<Option<UnixStream>> = Mutex::new(None);

/// Whether `sessiond` named a lock channel at all: then this is a session's
/// compositor, whether or not the channel could be taken.
static NAMED: AtomicBool = AtomicBool::new(false);

/// Take the lock channel `sessiond` left this process, if it left one.
/// Call it first thing in `main`, beside `compositor_seat::adopt`.
///
/// `None`: none was named, and the holder decides alone. `Some(true)`: the
/// channel. `Some(false)`: one was named and could not be taken; the session
/// then refuses its locks, since none could ever be granted, and never lets
/// a holder unlock alone (the certification consultant's S1).
pub fn adopt() -> Option<bool> {
    let adopted = compositor_seat::lock::adopt_lock()?;
    NAMED.store(true, Ordering::SeqCst);
    let taken = adopted
        .filter(|stream| stream.set_nonblocking(true).is_ok())
        .and_then(|stream| {
            let mut slot = ADOPTED.lock().ok()?;
            *slot = Some(stream);
            Some(())
        });
    Some(taken.is_some())
}

/// The lock channel and what has been said on it.
#[derive(Debug, Default)]
pub(crate) struct Grants {
    /// The channel to `sessiond`; `None` where the holder decides alone.
    stream: Option<UnixStream>,
    /// Whether `authd` can grant now, as `sessiond` last said.
    on: bool,
    /// The next lock's number.
    next: u64,
    /// Lines to send, oldest first.
    outbox: Vec<FromCompositor>,
    /// What `sessiond` has sent that is not a whole line yet.
    heard: Vec<u8>,
    /// Whether this is a session's compositor: it was given the channel,
    /// or a test says so. It stays one when the channel closes.
    session: bool,
}

impl Grants {
    /// The channel `main` adopted, if it did.
    pub(crate) fn adopted() -> Grants {
        let stream = ADOPTED.lock().ok().and_then(|mut adopted| adopted.take());
        Grants::new(NAMED.load(Ordering::SeqCst), stream)
    }

    /// A session's grants when `named`, over `stream` if there is one; with
    /// no stream nothing is ever granted, and grants stay off.
    pub(crate) fn new(named: bool, stream: Option<UnixStream>) -> Grants {
        Grants {
            session: named || stream.is_some(),
            stream,
            next: 1,
            ..Grants::default()
        }
    }

    /// A session's grants with no channel behind them, `on` or not, for a
    /// test that reads [`Grants::outbox`] and calls [`Grants::heard`].
    #[cfg(test)]
    pub(crate) fn for_test(on: bool) -> Grants {
        Grants {
            on,
            next: 1,
            session: true,
            ..Grants::default()
        }
    }

    /// Whether locks here need a grant: a session's compositor.
    pub(crate) const fn in_session(&self) -> bool {
        self.session
    }

    /// Whether a grant can come now.
    pub(crate) const fn on(&self) -> bool {
        self.on
    }

    /// A number for a new lock, told to `sessiond`.
    pub(crate) fn locked(&mut self) -> u64 {
        let epoch = self.next;
        self.next = self.next.saturating_add(1);
        if self.in_session() {
            self.outbox.push(FromCompositor::Locked(epoch));
        }
        epoch
    }

    /// The lock numbered `epoch` went.
    pub(crate) fn unlocked(&mut self, epoch: u64) {
        if self.in_session() {
            self.outbox.push(FromCompositor::Unlocked(epoch));
        }
    }

    /// The descriptor to wait on.
    pub(crate) fn raw_fd(&self) -> Option<RawFd> {
        self.stream.as_ref().map(UnixStream::as_raw_fd)
    }

    /// Send what is waiting.
    pub(crate) fn flush(&mut self) {
        let Some(stream) = self.stream.as_mut() else {
            self.outbox.clear();
            return;
        };
        for said in self.outbox.drain(..) {
            let _ = stream.write_all(said.line().as_bytes());
        }
    }

    /// What `sessiond` has said since last asked. Grants are turned off for
    /// good when the channel closes or says something it may not.
    pub(crate) fn read(&mut self) -> Vec<ToCompositor> {
        let Some(stream) = self.stream.as_mut() else {
            return Vec::new();
        };
        let mut chunk = [0_u8; 256];
        let mut closed = false;
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(got) => self
                    .heard
                    .extend_from_slice(chunk.get(..got).unwrap_or_default()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    closed = true;
                    break;
                }
            }
        }
        let mut said = Vec::new();
        while let Some(end) = self.heard.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.heard.drain(..=end).collect();
            let text = String::from_utf8_lossy(line.get(..end).unwrap_or_default()).into_owned();
            match ToCompositor::parse(&text) {
                Some(what) => said.push(what),
                None => closed = true,
            }
        }
        if self.heard.len() >= MAX_LINE {
            closed = true;
        }
        if closed {
            // Still a session: its locks are refused from now on, never
            // let go without a grant. The descriptor goes, so the loop does
            // not wake on its end for ever.
            self.stream = None;
            self.on = false;
            self.heard.clear();
            said.push(ToCompositor::Grants(false));
        }
        for what in &said {
            self.heard(*what);
        }
        said
    }

    /// Take in one thing `sessiond` said; a grant is the caller's to apply.
    pub(crate) fn heard(&mut self, what: ToCompositor) {
        if let ToCompositor::Grants(on) = what {
            self.on = on;
        }
    }

    /// The lines waiting to go.
    #[cfg(test)]
    pub(crate) fn outbox(&self) -> &[FromCompositor] {
        &self.outbox
    }
}
