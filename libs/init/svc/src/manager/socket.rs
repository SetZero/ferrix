//! The socket kind (§6, landing L9): a listening socket init holds, whose
//! service starts on the first connection.
//!
//! A start asks the backend for the sockets ([`Action::Listen`]) and is up
//! once they exist; then the backend watches them ([`Action::Watch`]) and
//! says once when a connection waits. With `Accept=no` that is
//! [`Event::Incoming`], and the socket's service is started and handed the
//! listening sockets themselves (`LISTEN_FDS`), to accept on for as long as
//! it runs; the socket is watched again when the service is down. With
//! `Accept=yes` the backend accepts the connection itself
//! ([`Event::Accepted`]), and each connection starts an instance of the
//! service's template, `name@N.service`, with the connection as its
//! `socket` streams; the socket goes on listening at once.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{Manager, OpKind, Sub};
use crate::event::{Action, ActiveState, Errno, ListenSpec, Token, UnitId};
use crate::kind::{Config, Socket};
use crate::name::UnitType;

impl Manager {
    /// A socket unit's settings.
    fn socket(&self, unit: UnitId) -> Option<Socket> {
        match &self.slot(unit)?.loaded.as_ref().ok()?.config {
            Config::Socket(socket) => Some(socket.clone()),
            _ => None,
        }
    }

    /// The name of the service a socket activates: `Service=`, or the
    /// socket's own stem; with `Accept=yes`, the stem's template.
    fn socket_service(&self, unit: UnitId, socket: &Socket) -> Option<String> {
        let name = self.slot(unit)?.name.as_str();
        let stem = name.strip_suffix(".socket")?;
        Some(match (&socket.service, socket.accept) {
            (Some(service), false) => String::from(service.as_str()),
            (_, true) => format!("{stem}@.service"),
            (None, false) => format!("{stem}.service"),
        })
    }

    /// A socket's start: make its sockets.
    pub(super) fn socket_start(&mut self, unit: UnitId) {
        let Some(socket) = self.socket(unit) else {
            return;
        };
        self.set_state(unit, ActiveState::Activating, Sub::Start);
        self.emit(Action::Listen {
            unit,
            spec: ListenSpec {
                listen: socket.listen,
                accept: socket.accept,
                mode: socket.mode,
                backlog: socket.backlog,
            },
        });
    }

    /// A socket's stop: close its sockets. A service it started goes on;
    /// the sockets it was handed are its own now.
    pub(super) fn socket_stop(&mut self, unit: UnitId) {
        self.emit(Action::Unlisten { unit });
        self.set_state(unit, ActiveState::Inactive, Sub::Dead);
    }

    /// The backend made the sockets, or could not.
    pub(super) fn listening(&mut self, unit: UnitId, result: Result<(), Errno>) {
        if self.slot(unit).map(|slot| slot.sub) != Some(Sub::Start) {
            return;
        }
        match result {
            Ok(()) => {
                self.set_state(unit, ActiveState::Active, Sub::Listening);
                self.emit(Action::Watch { unit });
            }
            Err(error) => {
                let line = format!(
                    "{}: could not listen (errno {})",
                    self.display(unit),
                    error.0
                );
                self.log(Some(unit), line);
                self.emit(Action::Unlisten { unit });
                self.set_state(unit, ActiveState::Failed, Sub::Failed);
            }
        }
    }

    /// A connection waits on an `Accept=no` socket: start its service.
    pub(super) fn incoming(&mut self, unit: UnitId) {
        if self.slot(unit).map(|slot| slot.sub) != Some(Sub::Listening) {
            return;
        }
        let Some(socket) = self.socket(unit) else {
            return;
        };
        let Some(name) = self.socket_service(unit, &socket) else {
            return;
        };
        let Some(service) = self.ensure(&name) else {
            let line = format!("{}: {name} is not a unit name", self.display(unit));
            self.log(Some(unit), line);
            return;
        };
        let up = self.slot(service).is_some_and(|slot| {
            matches!(slot.active, ActiveState::Active | ActiveState::Activating)
        });
        self.set_state(unit, ActiveState::Active, Sub::Running);
        if up {
            return;
        }
        if let Err(why) = self.transaction(service, OpKind::Start, super::Mode::Replace) {
            // The connection is still waiting, so listening on would wake
            // at once and ask again, for ever: the socket fails instead, as
            // systemd's does when it cannot start what it triggers.
            let line = format!("{}: {why}", self.display(unit));
            self.log(Some(unit), line);
            self.socket_failed(unit);
        }
    }

    /// A socket gives up: its sockets close, and it is failed until started
    /// again.
    fn socket_failed(&mut self, unit: UnitId) {
        self.emit(Action::Unlisten { unit });
        self.set_state(unit, ActiveState::Failed, Sub::Failed);
    }

    /// An `Accept=yes` socket took a connection: an instance of its
    /// service's template, with the connection.
    pub(super) fn accepted(&mut self, unit: UnitId, connection: Token) {
        let listening = self.slot(unit).map(|slot| slot.sub) == Some(Sub::Listening);
        let template = self
            .socket(unit)
            .filter(|_| listening)
            .and_then(|socket| self.socket_service(unit, &socket));
        let Some(template) = template else {
            self.emit(Action::Close { connection });
            return;
        };
        self.instances += 1;
        let name = template.replacen("@.", &format!("@{}.", self.instances), 1);
        self.emit(Action::Watch { unit });
        let Some(instance) = self.ensure(&name) else {
            self.emit(Action::Close { connection });
            return;
        };
        if let Some(slot) = self.slot_mut(instance) {
            slot.connection = Some(connection);
        }
        if let Err(why) = self.transaction(instance, OpKind::Start, super::Mode::Replace) {
            let line = format!("{}: {why}", self.display(unit));
            self.log(Some(unit), line);
            self.close_connection(instance);
        }
    }

    /// Close a connection a service instance was given and never took.
    fn close_connection(&mut self, instance: UnitId) {
        if let Some(connection) = self
            .slot_mut(instance)
            .and_then(|slot| slot.connection.take())
        {
            self.emit(Action::Close { connection });
        }
    }

    /// A service is down: a connection it never took is closed, and every
    /// `Accept=no` socket that started it listens again.
    pub(super) fn socket_service_down(&mut self, service: UnitId) {
        self.close_connection(service);
        let Some(name) = self
            .slot(service)
            .map(|slot| String::from(slot.name.as_str()))
        else {
            return;
        };
        // A service that will not start again -- its start limit spent --
        // would be asked again by the next connection, for ever.
        let spent = self
            .slot(service)
            .is_some_and(|slot| slot.result == Some(crate::restart::Ended::StartLimitHit));
        for socket in self.sockets_of(&name, Sub::Running) {
            if self.shutdown.is_some() {
                // Everything stops; nothing is to be started by a
                // connection now.
                continue;
            }
            if spent {
                let line = format!(
                    "{}: {name} hit its start limit; not listening any more",
                    self.display(socket)
                );
                self.log(Some(socket), line);
                self.socket_failed(socket);
                continue;
            }
            self.set_state(socket, ActiveState::Active, Sub::Listening);
            self.emit(Action::Watch { unit: socket });
        }
    }

    /// The `Accept=no` sockets in sub-state `sub` that activate `service`.
    fn sockets_of(&self, service: &str, sub: Sub) -> Vec<UnitId> {
        (0..self.units.len())
            .filter_map(|index| u32::try_from(index).ok().map(UnitId))
            .filter(|&unit| {
                self.slot(unit).is_some_and(|slot| {
                    slot.name.unit_type() == UnitType::Socket && slot.sub == sub
                })
            })
            .filter(|&unit| {
                self.socket(unit)
                    .filter(|socket| !socket.accept)
                    .and_then(|socket| self.socket_service(unit, &socket))
                    .is_some_and(|name| name == service)
            })
            .collect()
    }

    /// The sockets a service is handed when it starts: every `Accept=no`
    /// socket that activates it and is up, whichever started it.
    pub(super) fn sockets_for(&self, service: UnitId) -> Vec<UnitId> {
        let Some(name) = self
            .slot(service)
            .map(|slot| String::from(slot.name.as_str()))
        else {
            return Vec::new();
        };
        let mut sockets = self.sockets_of(&name, Sub::Running);
        sockets.extend(self.sockets_of(&name, Sub::Listening));
        sockets.sort();
        sockets
    }
}
