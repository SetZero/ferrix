//! Where the socket file goes, and accepting connections on it.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Why a listener could not be made.
#[derive(Debug)]
pub enum ListenerError {
    /// `XDG_RUNTIME_DIR` is not set, and Wayland gives no other place to put
    /// the socket.
    NoRuntimeDir,
    /// The socket could not be bound, or its directory made.
    Io(io::Error),
}

impl core::fmt::Display for ListenerError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoRuntimeDir => {
                write!(formatter, "XDG_RUNTIME_DIR is not set")
            }
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ListenerError {}

/// Where the socket named `display` lives.
///
/// Wayland's rule, from `wl_display_connect`: a name with a `/` in it is an
/// absolute path used as it stands, and anything else is joined to
/// `XDG_RUNTIME_DIR`. A client given the same name finds the same file, which
/// is the whole of the naming protocol.
pub fn socket_path(display: &str) -> Result<PathBuf, ListenerError> {
    if display.contains('/') {
        return Ok(PathBuf::from(display));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(ListenerError::NoRuntimeDir)?;
    Ok(Path::new(&runtime).join(display))
}

/// The socket clients connect to.
#[derive(Debug)]
pub struct Listener {
    inner: UnixListener,
    path: PathBuf,
}

impl Listener {
    /// Bind the socket for `display`, removing a stale file left by a
    /// compositor that did not exit cleanly.
    ///
    /// Removing it is what every compositor does and what the lock file
    /// beside it is meant to make safe; there is no lock file here yet, so a
    /// second compositor on the same name takes the first one's clients.
    /// That is a row for when more than one can run at once.
    pub fn bind(display: &str) -> Result<Self, ListenerError> {
        let path = socket_path(display)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ListenerError::Io)?;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ListenerError::Io(error)),
        }
        let inner = UnixListener::bind(&path).map_err(ListenerError::Io)?;
        inner.set_nonblocking(true).map_err(ListenerError::Io)?;
        Ok(Self { inner, path })
    }

    /// Bind an already-open listening socket, as a compositor started by a
    /// session manager is handed one.
    #[must_use]
    pub fn from_listener(inner: UnixListener, path: PathBuf) -> Self {
        Self { inner, path }
    }

    /// Where the socket file is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The descriptor to wait on.
    #[must_use]
    pub fn as_raw_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;
        self.inner.as_raw_fd()
    }

    /// Take a waiting connection, if there is one.
    ///
    /// `Ok(None)` when nothing is waiting, which on a non-blocking socket is
    /// the usual answer.
    pub fn accept(&self) -> io::Result<Option<UnixStream>> {
        match self.inner.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(true)?;
                Ok(Some(stream))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // A socket file left behind is a client that connects to nothing.
        let _ = std::fs::remove_file(&self.path);
    }
}
