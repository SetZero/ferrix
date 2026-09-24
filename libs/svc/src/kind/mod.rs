//! Unit kinds (§4.2): each suffix is one implementation of [`Kind`].
//!
//! A kind owns its section's keys and what they mean. The graph and the
//! operations deal in units whatever their kind, so a new kind, a `.timer`
//! say, is a new implementation of the trait and a variant of [`Config`],
//! and nothing else changes.
//!
//! | Kind | Section | Needs a file | |
//! |---|---|---|---|
//! | `.service` | `[Service]` | yes | [`Service`] |
//! | `.slice` | `[Slice]` | no | [`Slice`] |
//! | `.scope` | `[Scope]` | no | [`Scope`] |
//! | `.target` | none | yes | |
//! | `.mount` | `[Mount]` | yes | [`Mount`] |
//! | `.socket` | `[Socket]` | yes | [`Socket`], version 2 |
//! | `.builtin` | none | no | |
//!
//! Slices and scopes need no file because the manager makes them: a slice
//! for every level of a `Slice=` path, a scope when a program asks. A
//! builtin needs none because its name is the contract (§7.2): `net.builtin`
//! is active whether or not anything describes it.

mod mount;
mod service;
mod socket;

use alloc::boxed::Box;
use alloc::string::String;
use core::fmt;

pub use mount::Mount;
pub use service::{
    Input, Kill, KillMode, OomPolicy, Output, Restart, Service, ServiceType, WorkingDirectory,
};
pub use socket::{Listen, Socket};

use crate::Warnings;
use crate::ini::Section;
use crate::keys::{self, Setter};
use crate::limits::{self, Limits};
use crate::name::{UnitName, UnitType};
use crate::value::Span;

/// Why a unit's settings refuse it: they parse, but contradict each other
/// or leave out what the kind cannot do without. systemd's load state for
/// this is `bad-setting`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitError {
    /// Why, in systemd's words where it has them.
    pub message: String,
}

impl UnitError {
    pub(crate) fn new(message: &str) -> Self {
        Self {
            message: String::from(message),
        }
    }
}

impl fmt::Display for UnitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// What one kind knows.
pub trait Kind: Sync {
    /// The type it implements.
    fn unit_type(&self) -> UnitType;

    /// The section its keys are in, if it has one.
    fn section(&self) -> Option<&'static str>;

    /// Whether a unit of the kind must have a file to load.
    fn needs_file(&self) -> bool;

    /// The keys of its own section, parsed; an unknown key is a warning.
    /// `section` is empty when the files had none.
    ///
    /// # Errors
    ///
    /// When the settings refuse the unit.
    fn parse(
        &self,
        name: &UnitName,
        section: &Section,
        warnings: &mut Warnings,
    ) -> Result<Config, UnitError>;
}

/// A unit's kind-specific settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Config {
    /// A service.
    Service(Box<Service>),
    /// A slice.
    Slice(Slice),
    /// A scope.
    Scope(Scope),
    /// A target, which has no settings of its own.
    Target,
    /// A mount.
    Mount(Mount),
    /// A socket.
    Socket(Socket),
    /// A builtin, which has no settings of its own.
    Builtin,
}

/// The kind a type is implemented by.
pub fn of(unit_type: UnitType) -> &'static dyn Kind {
    match unit_type {
        UnitType::Service => &service::ServiceKind,
        UnitType::Slice => &SliceKind,
        UnitType::Scope => &ScopeKind,
        UnitType::Target => &Plain(UnitType::Target),
        UnitType::Mount => &mount::MountKind,
        UnitType::Socket => &socket::SocketKind,
        UnitType::Builtin => &Plain(UnitType::Builtin),
    }
}

/// Targets and builtins: no section, no settings.
struct Plain(UnitType);

impl Kind for Plain {
    fn unit_type(&self) -> UnitType {
        self.0
    }

    fn section(&self) -> Option<&'static str> {
        None
    }

    fn needs_file(&self) -> bool {
        self.0 == UnitType::Target
    }

    fn parse(&self, _: &UnitName, _: &Section, _: &mut Warnings) -> Result<Config, UnitError> {
        Ok(match self.0 {
            UnitType::Builtin => Config::Builtin,
            _ => Config::Target,
        })
    }
}

/// A slice's settings: the limits over everything beneath it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Slice {
    /// The resource keys.
    pub limits: Limits,
}

/// The slice kind.
struct SliceKind;

impl Kind for SliceKind {
    fn unit_type(&self) -> UnitType {
        UnitType::Slice
    }

    fn section(&self) -> Option<&'static str> {
        Some("Slice")
    }

    fn needs_file(&self) -> bool {
        false
    }

    fn parse(
        &self,
        _: &UnitName,
        section: &Section,
        warnings: &mut Warnings,
    ) -> Result<Config, UnitError> {
        let mut slice = Slice::default();
        keys::apply(section, &limits::KEYS, &mut slice.limits, warnings);
        Ok(Config::Slice(slice))
    }
}

/// A scope's settings: where it goes, what bounds it, and how it is
/// stopped, since init did not start what is in it (§5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// `Slice=`; `None` is the manager's default.
    pub slice: Option<UnitName>,
    /// The resource keys.
    pub limits: Limits,
    /// `KillMode=`, `KillSignal=` and `SendSIGHUP=`.
    pub kill: Kill,
    /// `TimeoutStopSec=`.
    pub timeout_stop: Span,
    /// `Delegate=`.
    pub delegate: bool,
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            slice: None,
            limits: Limits::default(),
            kill: Kill::default(),
            timeout_stop: service::DEFAULT_TIMEOUT,
            delegate: false,
        }
    }
}

/// The scope kind.
struct ScopeKind;

/// The `[Scope]` keys that are not resource or kill keys.
const SCOPE_KEYS: [(&str, Setter<Scope>); 3] = [
    ("Slice", |scope, a, w| {
        if let Some(slice) = service::slice(a, w) {
            scope.slice = slice;
        }
    }),
    ("TimeoutStopSec", |scope, a, w| {
        if let Some(timeout) = keys::span(a, w) {
            scope.timeout_stop = service::zero_is_infinity(timeout);
        }
    }),
    ("Delegate", |scope, a, w| {
        if let Some(delegate) = service::delegate(a, w) {
            scope.delegate = delegate;
        }
    }),
];

impl Kind for ScopeKind {
    fn unit_type(&self) -> UnitType {
        UnitType::Scope
    }

    fn section(&self) -> Option<&'static str> {
        Some("Scope")
    }

    fn needs_file(&self) -> bool {
        false
    }

    fn parse(
        &self,
        _: &UnitName,
        section: &Section,
        warnings: &mut Warnings,
    ) -> Result<Config, UnitError> {
        let mut scope = Scope::default();
        for assignment in &section.assignments {
            let key = assignment.key.as_str();
            if let Some(setter) = keys::find(&SCOPE_KEYS, key) {
                setter(&mut scope, assignment, warnings);
            } else if let Some(setter) = keys::find(&limits::KEYS, key) {
                setter(&mut scope.limits, assignment, warnings);
            } else if let Some(setter) = keys::find(&service::KILL_KEYS, key) {
                setter(&mut scope.kill, assignment, warnings);
            } else {
                keys::unknown(&section.name, assignment, warnings);
            }
        }
        Ok(Config::Scope(scope))
    }
}
