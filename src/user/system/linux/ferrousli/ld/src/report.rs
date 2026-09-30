//! What the loader says when it cannot go on.
//!
//! A loader has nowhere to report to. There is no C library beneath it to set
//! `errno` in, no program above it to return a code to, and no standard
//! error stream anyone promised to leave open. So it does what every other
//! loader does: writes a line naming the program, the object and the thing
//! that was wrong, and exits 127 — the code a shell reports for a command
//! that could not be run.
//!
//! The message matters more here than in most places. "undefined symbol" with
//! no name and no object is the least useful thing a loader can say, and it is
//! what a loader says when the error type it carries has nowhere to put them.

use core::ffi::c_char;

/// Why the loader stopped.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Error {
    /// A file could not be opened; carries the kernel's negated errno.
    CannotOpen(*const c_char, isize),
    /// A file could not be read or mapped.
    CannotMap(*const c_char, isize),
    /// A file is not an ELF object this loader can use.
    NotAnObject(*const c_char),
    /// An object is malformed in the way named.
    MalformedObject(&'static str),
    /// A name no object in the scope defines.
    UnresolvedSymbol(*const c_char),
    /// A relocation type this loader does not write; carries the type.
    UnknownRelocation(u32),
    /// More objects than [`crate::object::MAX_OBJECTS`], or more
    /// dependencies than [`crate::object::MAX_NEEDED`].
    TooManyObjects,
    /// A library named by `DT_NEEDED` that is on no search path.
    LibraryNotFound(*const c_char),
}

impl Error {
    /// The fixed part of the message.
    #[must_use]
    pub(crate) const fn text(&self) -> &'static str {
        match self {
            Error::CannotOpen(..) => "cannot open",
            Error::CannotMap(..) => "cannot map",
            Error::NotAnObject(_) => "not a shared object this loader can use",
            Error::MalformedObject(what) => what,
            Error::UnresolvedSymbol(_) => "undefined symbol",
            Error::UnknownRelocation(_) => "unhandled relocation type",
            Error::TooManyObjects => "too many shared objects",
            Error::LibraryNotFound(_) => "library not found",
        }
    }

    /// The name the message should carry, when there is one.
    #[must_use]
    pub(crate) const fn subject(&self) -> Option<*const c_char> {
        match self {
            Error::CannotOpen(name, _)
            | Error::CannotMap(name, _)
            | Error::NotAnObject(name)
            | Error::UnresolvedSymbol(name)
            | Error::LibraryNotFound(name) => Some(*name),
            Error::MalformedObject(_) | Error::UnknownRelocation(_) | Error::TooManyObjects => None,
        }
    }

    /// The number the message should carry, when there is one.
    #[must_use]
    pub(crate) const fn number(&self) -> Option<isize> {
        match self {
            Error::CannotOpen(_, errno) | Error::CannotMap(_, errno) => Some(*errno),
            Error::UnknownRelocation(kind) => Some(*kind as isize),
            Error::NotAnObject(_)
            | Error::MalformedObject(_)
            | Error::UnresolvedSymbol(_)
            | Error::TooManyObjects
            | Error::LibraryNotFound(_) => None,
        }
    }
}
