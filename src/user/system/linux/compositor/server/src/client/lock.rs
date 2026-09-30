//! `ext-session-lock-v1`: a lock screen the compositor vouches for.
//!
//! A locker such as hyprlock asks to lock the session, gives every screen a
//! lock surface, and draws its password prompt on each. Only the compositor
//! can say when the screen is covered, so `locked` goes out when every
//! screen shows a lock surface with a buffer, and until then the locker
//! must assume the screen still shows what it did.
//!
//! Unlocking is the locker's to ask for. A lock destroyed without it is a
//! protocol error, because it would leave the screen locked with nothing
//! drawing on it and no way back.

use compositor_protocol::session_lock::{
    self, ext_session_lock_manager_v1, ext_session_lock_surface_v1, ext_session_lock_v1,
};
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::{Client, Event, Fatal};
use crate::role::Role;

impl Client {
    /// `ext_session_lock_manager_v1`: `lock`.
    ///
    /// The lock object is made at once and the *compositor* decides when to
    /// send `locked`: the protocol says that event means every screen is
    /// covered by a lock surface the client has drawn, and nothing but the
    /// compositor knows when that is true. Until then the client must
    /// assume the screen still shows what it did.
    pub(super) fn lock_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != ext_session_lock_manager_v1::request::LOCK {
            return;
        }
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        if !self.make(
            id,
            &session_lock::EXT_SESSION_LOCK_V1,
            version,
            Role::SessionLock,
        ) {
            return;
        }
        self.lock = Some(id);
        self.events.push(Event::SessionLocked { lock: id });
    }

    /// `ext_session_lock_v1`: `get_lock_surface`, `unlock_and_destroy` and
    /// `destroy`.
    ///
    /// `destroy` on a lock that was never unlocked is `invalid_destroy`, and
    /// `unlock_and_destroy` on one that was never told it was locked is
    /// `invalid_unlock`. Both are protocol errors because both leave a
    /// screen nobody is drawing: the client believes it is done and the
    /// compositor believes the screen is covered.
    pub(super) fn session_lock(
        &mut self,
        sender: ObjectId,
        version: u32,
        opcode: u16,
        args: &[Arg<'_>],
    ) {
        use ext_session_lock_v1::request;
        match opcode {
            request::GET_LOCK_SURFACE => {
                let (Some(id), Some(surface), Some(output)) = (
                    args.first().and_then(Arg::as_object),
                    args.get(1).and_then(Arg::as_object),
                    args.get(2).and_then(Arg::as_object),
                ) else {
                    return;
                };
                let Some(which) = self.output_objects.get(&output).copied() else {
                    self.fail(Fatal::WrongInterface {
                        object: output,
                        wanted: "wl_output",
                    });
                    return;
                };
                if self.lock_surfaces.values().any(|held| *held == which) {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: ext_session_lock_v1::error::DUPLICATE_OUTPUT,
                        text: "that screen already has a lock surface".to_owned(),
                    });
                    return;
                }
                if !self.surfaces.contains_key(&surface) {
                    self.fail(Fatal::WrongInterface {
                        object: surface,
                        wanted: "wl_surface",
                    });
                    return;
                }
                if !self.make(
                    id,
                    &session_lock::EXT_SESSION_LOCK_SURFACE_V1,
                    version,
                    Role::SessionLockSurface,
                ) {
                    return;
                }
                let _ = self.lock_surfaces.insert(id, which);
                self.events.push(Event::SessionLockSurfaceMade {
                    lock_surface: id,
                    surface,
                    output: which,
                });
            }
            request::UNLOCK_AND_DESTROY => {
                self.lock = None;
                self.lock_surfaces.clear();
                self.events.push(Event::SessionUnlocked { asked: true });
            }
            // Destroying a lock that is still held is the error the
            // protocol names, because it would leave the screen locked with
            // nothing to draw on it and no way back.
            request::DESTROY if self.lock == Some(sender) => {
                self.fail(Fatal::Interface {
                    object: sender,
                    code: ext_session_lock_v1::error::INVALID_DESTROY,
                    text: "the lock was destroyed without being unlocked".to_owned(),
                });
            }
            _ => {}
        }
    }

    /// `ext_session_lock_surface_v1`: `ack_configure` and `destroy`.
    pub(super) fn lock_surface(&mut self, sender: ObjectId, opcode: u16, _args: &[Arg<'_>]) {
        if opcode == ext_session_lock_surface_v1::request::DESTROY {
            let _ = self.lock_surfaces.remove(&sender);
        }
    }

    /// Tell a lock surface how large the screen it covers is.
    ///
    /// The client may not commit a buffer before it has acknowledged one of
    /// these, and the buffer it commits must be exactly this size.
    pub fn configure_lock_surface(&mut self, lock_surface: ObjectId, size: (u32, u32)) {
        let serial = self.serial;
        self.serial = self.serial.wrapping_add(1);
        let (width, height) = size;
        let _ = self.out.write(
            lock_surface,
            ext_session_lock_surface_v1::event::CONFIGURE,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[Arg::Uint(serial), Arg::Uint(width), Arg::Uint(height)],
        );
    }

    /// Tell the client the screen is covered by what it drew.
    ///
    /// Sent when every screen has a lock surface with a buffer on it, which
    /// is the protocol's own condition and the compositor's to judge.
    pub fn session_is_locked(&mut self) {
        let Some(lock) = self.lock else {
            return;
        };
        let _ = self
            .out
            .write(lock, ext_session_lock_v1::event::LOCKED, &[], &[]);
    }

    /// Tell the client it will never be told the screen is covered.
    ///
    /// `finished` is what a compositor sends when it refuses the lock -- a
    /// second program asking while one is held -- and the client is then to
    /// destroy the object and stop.
    pub fn session_lock_refused(&mut self, lock: ObjectId) {
        let _ = self
            .out
            .write(lock, ext_session_lock_v1::event::FINISHED, &[], &[]);
    }

    /// Whether this client holds the lock.
    #[must_use]
    pub const fn holds_lock(&self) -> bool {
        self.lock.is_some()
    }

    /// The lock surface for `output`, if this client has made one.
    #[must_use]
    pub fn lock_surface_on(&self, output: usize) -> Option<ObjectId> {
        self.lock_surfaces
            .iter()
            .find(|(_, which)| **which == output)
            .map(|(id, _)| *id)
    }
}
