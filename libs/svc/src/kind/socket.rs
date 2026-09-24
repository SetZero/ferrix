//! `.socket`: a listening socket init holds, whose service starts on the
//! first connection. Version 2: the keys parse now, so that a unit set
//! with sockets in it loads, and the manager refuses to start one until
//! landing L9 builds activation.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{Config, Kind, UnitError};
use crate::Warnings;
use crate::ini::{Assignment, Section};
use crate::keys::{self, Setter};
use crate::name::{UnitName, UnitType};

/// One `Listen…=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    /// `ListenStream=`: an address, a path or a port.
    Stream(String),
    /// `ListenDatagram=`.
    Datagram(String),
    /// `ListenSequentialPacket=`.
    SequentialPacket(String),
    /// `ListenFIFO=`.
    Fifo(String),
}

/// A socket's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socket {
    /// What it listens on, in order.
    pub listen: Vec<Listen>,
    /// `Accept=`: a service instance per connection.
    pub accept: bool,
    /// `Service=`: the service it activates; the same stem by default.
    pub service: Option<UnitName>,
    /// `SocketMode=`: the mode of a file-system socket.
    pub mode: u32,
    /// `Backlog=`.
    pub backlog: u32,
}

impl Default for Socket {
    fn default() -> Self {
        Self {
            listen: Vec::new(),
            accept: false,
            service: None,
            mode: 0o666,
            backlog: 4096,
        }
    }
}

/// Add a `Listen…=` to the list, or clear it for an empty value.
fn listen(socket: &mut Socket, assignment: &Assignment, make: fn(String) -> Listen) {
    if assignment.value.is_empty() {
        socket.listen.clear();
    } else {
        socket.listen.push(make(assignment.value.clone()));
    }
}

/// The `[Socket]` keys.
const SOCKET_KEYS: [(&str, Setter<Socket>); 8] = [
    ("ListenStream", |s, a, _| listen(s, a, Listen::Stream)),
    ("ListenDatagram", |s, a, _| listen(s, a, Listen::Datagram)),
    ("ListenSequentialPacket", |s, a, _| {
        listen(s, a, Listen::SequentialPacket);
    }),
    ("ListenFIFO", |s, a, _| listen(s, a, Listen::Fifo)),
    ("Accept", |s, a, w| {
        s.accept = keys::boolean(a, w).unwrap_or(s.accept);
    }),
    ("Service", |s, a, w| match UnitName::parse(&a.value) {
        Ok(name) if name.unit_type() == UnitType::Service && !name.is_template() => {
            s.service = Some(name);
        }
        Ok(_) => keys::invalid(a, w, "not a service"),
        Err(error) => keys::invalid(a, w, error),
    }),
    ("SocketMode", |s, a, w| {
        let mode = keys::parsed(a, w, |text| {
            u32::from_str_radix(text, 8)
                .ok()
                .filter(|&mode| mode <= 0o7777)
                .ok_or(format!("'{text}' is not an octal mode"))
        });
        s.mode = mode.unwrap_or(s.mode);
    }),
    ("Backlog", |s, a, w| {
        s.backlog = keys::parsed(a, w, str::parse::<u32>).unwrap_or(s.backlog);
    }),
];

/// The socket kind.
pub(super) struct SocketKind;

impl Kind for SocketKind {
    fn unit_type(&self) -> UnitType {
        UnitType::Socket
    }

    fn section(&self) -> Option<&'static str> {
        Some("Socket")
    }

    fn needs_file(&self) -> bool {
        true
    }

    fn parse(
        &self,
        _: &UnitName,
        section: &Section,
        warnings: &mut Warnings,
    ) -> Result<Config, UnitError> {
        let mut socket = Socket::default();
        keys::apply(section, &SOCKET_KEYS, &mut socket, warnings);
        if socket.listen.is_empty() {
            return Err(UnitError::new(
                "Socket unit lacks Listen*= setting. Refusing.",
            ));
        }
        Ok(Config::Socket(socket))
    }
}
