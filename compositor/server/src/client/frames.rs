//! What a client asks about where the pointer goes, what is behind it, and
//! when its frames are shown.
//!
//! Seven small protocols with one thing in common: each is a client asking
//! the compositor for something about *presentation* rather than about a
//! window.
//!
//! * `pointer-warp-v1` -- put the pointer here, inside my own window. A
//!   game's settings panel and a drawing program both want it, and it is
//!   allowed only inside a surface the client owns and only with the serial
//!   of a real input event.
//! * `ext-background-effect-v1` -- blur what is behind me. The protocol's
//!   version of `layerrule = blur`, said by the client instead of by the
//!   person.
//! * `tearing-control-v1`, `fifo-v1` and `commit-timing-v1` -- how a client
//!   would like its frames scheduled. Each is read and recorded; acting on
//!   any of them means choosing *when* to put a frame on the screen, and
//!   this compositor draws when something changed and presents at once,
//!   which is what a software renderer with no vertical blank can do.
//! * `security-context-v1` -- a sandbox asking for a socket of its own, so
//!   that the compositor can tell a flatpak's clients from the rest.
//! * `vicinae-hotkey-v1` -- a launcher asking for a key by keysym rather
//!   than by registering a name, which is `hyprland-global-shortcuts-v1`'s
//!   job done the other way round.

use compositor_protocol::background_effect::{
    ext_background_effect_manager_v1, ext_background_effect_surface_v1,
};
use compositor_protocol::hotkey::{vicinae_hotkey_manager_v1, vicinae_hotkey_v1};
use compositor_protocol::pointer_warp::wp_pointer_warp_v1;
use compositor_protocol::security_context::{
    wp_security_context_manager_v1, wp_security_context_v1,
};
use compositor_protocol::tearing_control::wp_tearing_control_v1;
use compositor_protocol::{
    background_effect, commit_timing, fifo, hotkey, security_context, tearing_control,
};
use compositor_wire::{Arg, ArgType, Fd, ObjectId};

use crate::client::{Client, Event, Fatal};
use crate::role::Role;

/// One key a launcher asked for by keysym.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    /// The XKB keysym.
    pub keysym: u32,
    /// The modifiers held with it, as a keymap gives them.
    pub modifiers: u32,
    /// Which program asked.
    pub app_id: String,
}

impl Client {
    /// Answer a request to one of this module's objects.
    pub(super) fn frames(
        &mut self,
        sender: ObjectId,
        role: Role,
        version: u32,
        opcode: u16,
        args: &[Arg<'_>],
    ) -> bool {
        match role {
            Role::PointerWarp => self.pointer_warp(opcode, args),
            Role::BackgroundEffectManager => self.effect_manager(version, opcode, args),
            Role::BackgroundEffect => self.background_effect(sender, opcode, args),
            Role::TearingManager => {
                self.for_surface(version, opcode, args, Role::Tearing);
            }
            Role::Tearing => self.tearing(sender, opcode, args),
            Role::FifoManager => self.for_surface(version, opcode, args, Role::Fifo),
            Role::CommitTimingManager => self.for_surface(version, opcode, args, Role::CommitTimer),
            Role::SecurityContextManager => self.security_manager(version, opcode, args),
            Role::SecurityContext => self.security_context(sender, opcode, args),
            Role::HotkeyManager => self.hotkey_manager(version, opcode, args),
            // A `wp_fifo_v1`'s two requests and a `wp_commit_timer_v1`'s
            // one are recorded on the surface they were made for; a
            // `vicinae_hotkey_v1` has only `destroy`.
            Role::Fifo | Role::CommitTimer | Role::Hotkey => {}
            _ => return false,
        }
        true
    }

    /// Drop what one of this module's objects held.
    pub(super) fn forget_frames(&mut self, id: ObjectId, role: Role) {
        match role {
            Role::BackgroundEffect | Role::Tearing | Role::Fifo | Role::CommitTimer => {
                let _ = self.for_surfaces.remove(&id);
            }
            Role::Hotkey => {
                let _ = self.hotkeys.remove(&id);
            }
            Role::SecurityContext => {
                let _ = self.contexts.remove(&id);
            }
            _ => {}
        }
    }

    /// The four managers whose one request is "give me an object for this
    /// surface".
    fn for_surface(&mut self, version: u32, opcode: u16, args: &[Arg<'_>], role: Role) {
        // Each of the four has `destroy` at one opcode and the getter at
        // the other; the getter is the one with two arguments.
        let (Some(id), Some(surface)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
        ) else {
            return;
        };
        let _ = opcode;
        if !self.surfaces.contains_key(&surface) {
            self.fail(Fatal::WrongInterface {
                object: surface,
                wanted: "wl_surface",
            });
            return;
        }
        let interface = match role {
            Role::Tearing => &tearing_control::WP_TEARING_CONTROL_V1,
            Role::Fifo => &fifo::WP_FIFO_V1,
            _ => &commit_timing::WP_COMMIT_TIMER_V1,
        };
        if self.make(id, interface, version, role) {
            let _ = self.for_surfaces.insert(id, surface);
        }
    }

    /// `wp_pointer_warp_v1.warp_pointer`: put the pointer at a place inside
    /// one of this client's own surfaces.
    ///
    /// The serial is the one from a real input event; a client that sends
    /// one the compositor never gave is ignored, which is what stops any
    /// program moving the pointer whenever it likes.
    fn pointer_warp(&mut self, opcode: u16, args: &[Arg<'_>]) {
        if opcode != wp_pointer_warp_v1::request::WARP_POINTER {
            return;
        }
        let (Some(surface), Some(x), Some(y), Some(serial)) = (
            args.first().and_then(Arg::as_object),
            args.get(2).and_then(Arg::as_fixed),
            args.get(3).and_then(Arg::as_fixed),
            args.get(4).and_then(Arg::as_uint),
        ) else {
            return;
        };
        if !self.surfaces.contains_key(&surface) {
            self.fail(Fatal::WrongInterface {
                object: surface,
                wanted: "wl_surface",
            });
            return;
        }
        self.events.push(Event::PointerWarped {
            surface,
            at: (x, y),
            serial,
        });
    }

    /// `ext_background_effect_manager_v1`: the capabilities, then one
    /// object a surface.
    fn effect_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != ext_background_effect_manager_v1::request::GET_BACKGROUND_EFFECT {
            return;
        }
        let (Some(id), Some(surface)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
        ) else {
            return;
        };
        if !self.surfaces.contains_key(&surface) {
            self.fail(Fatal::WrongInterface {
                object: surface,
                wanted: "wl_surface",
            });
            return;
        }
        if self.make(
            id,
            &background_effect::EXT_BACKGROUND_EFFECT_SURFACE_V1,
            version,
            Role::BackgroundEffect,
        ) {
            let _ = self.for_surfaces.insert(id, surface);
        }
    }

    /// `ext_background_effect_surface_v1.set_blur_region`: blur what is
    /// behind this surface.
    ///
    /// A null region is the whole surface, which is what the protocol says
    /// and what a client asking for a frosted panel sends.
    fn background_effect(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != ext_background_effect_surface_v1::request::SET_BLUR_REGION {
            return;
        }
        let Some(surface) = self.for_surfaces.get(&sender).copied() else {
            return;
        };
        let region = args.first().and_then(Arg::as_object);
        let whole = region.is_none_or(|region| region.is_null());
        if let Some(state) = self.surfaces.get_mut(&surface) {
            state.pending.blur = whole
                || region.is_some_and(|region| {
                    self.regions
                        .get(&region)
                        .is_some_and(|held| !held.is_empty())
                });
        }
    }

    /// Tell a client what background effects this compositor can do.
    ///
    /// Sent at bind, as the protocol requires: a client that was told
    /// nothing must assume none.
    pub fn background_capabilities(&mut self, manager: ObjectId) {
        let _ = self.out.write(
            manager,
            ext_background_effect_manager_v1::event::CAPABILITIES,
            &[ArgType::Uint],
            &[Arg::Uint(
                ext_background_effect_manager_v1::capability::BLUR,
            )],
        );
    }

    /// `wp_tearing_control_v1.set_presentation_hint`.
    fn tearing(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != wp_tearing_control_v1::request::SET_PRESENTATION_HINT {
            return;
        }
        let (Some(surface), Some(hint)) = (
            self.for_surfaces.get(&sender).copied(),
            args.first().and_then(Arg::as_uint),
        ) else {
            return;
        };
        if let Some(state) = self.surfaces.get_mut(&surface) {
            state.pending.tearing = hint == wp_tearing_control_v1::presentation_hint::ASYNC;
        }
    }

    /// `wp_security_context_manager_v1.create_listener`.
    ///
    /// The client hands over a listening socket and a descriptor that is
    /// closed when the sandbox ends. The compositor accepts connections on
    /// that socket and knows which sandbox each came from.
    fn security_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != wp_security_context_manager_v1::request::CREATE_LISTENER {
            return;
        }
        let (Some(id), Some(listener), Some(close)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_fd),
            args.get(2).and_then(Arg::as_fd),
        ) else {
            return;
        };
        if !self.make(
            id,
            &security_context::WP_SECURITY_CONTEXT_V1,
            version,
            Role::SecurityContext,
        ) {
            return;
        }
        let _ = self.contexts.insert(id, (listener, close));
    }

    /// `wp_security_context_v1`: what the sandbox calls itself, and the
    /// `commit` that puts the listener in force.
    fn security_context(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        let text = args.first().and_then(Arg::as_str).unwrap_or_default();
        match opcode {
            wp_security_context_v1::request::SET_SANDBOX_ENGINE
            | wp_security_context_v1::request::SET_APP_ID
            | wp_security_context_v1::request::SET_INSTANCE_ID => {
                let entry = self.sandboxes.entry(sender).or_default();
                match opcode {
                    wp_security_context_v1::request::SET_SANDBOX_ENGINE => {
                        entry.0 = text.to_owned();
                    }
                    wp_security_context_v1::request::SET_APP_ID => entry.1 = text.to_owned(),
                    _ => entry.2 = text.to_owned(),
                }
            }
            wp_security_context_v1::request::COMMIT => {
                let Some((listener, close)) = self.contexts.get(&sender).copied() else {
                    return;
                };
                let named = self.sandboxes.get(&sender).cloned().unwrap_or_default();
                self.events.push(Event::SecurityContext {
                    listener,
                    close,
                    engine: named.0,
                    app_id: named.1,
                    instance: named.2,
                });
            }
            _ => {}
        }
    }

    /// `vicinae_hotkey_manager_v1.bind`: a key by keysym.
    ///
    /// Answered `bound` at once: this compositor has no policy that would
    /// refuse one, and the protocol requires one of `bound` or `denied` so
    /// the launcher knows whether to wait for the key.
    fn hotkey_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != vicinae_hotkey_manager_v1::request::BIND {
            return;
        }
        let (Some(id), Some(keysym), Some(modifiers), Some(app_id)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_uint),
            args.get(2).and_then(Arg::as_uint),
            args.get(4).and_then(Arg::as_str),
        ) else {
            return;
        };
        if !self.make(id, &hotkey::VICINAE_HOTKEY_V1, version, Role::Hotkey) {
            return;
        }
        let _ = self.hotkeys.insert(
            id,
            Hotkey {
                keysym,
                modifiers,
                app_id: app_id.to_owned(),
            },
        );
        let _ = self
            .out
            .write(id, vicinae_hotkey_v1::event::BOUND, &[], &[]);
    }

    /// Fire every hotkey this client asked for that matches a key.
    ///
    /// Gives whether any did, so the compositor knows whether to keep the
    /// key from the focused window.
    pub fn fire_hotkey(&mut self, keysym: u32, modifiers: u32, time: u32) -> bool {
        let found: Vec<ObjectId> = self
            .hotkeys
            .iter()
            .filter(|(_, held)| held.keysym == keysym && held.modifiers == modifiers)
            .map(|(id, _)| *id)
            .collect();
        if found.is_empty() {
            return false;
        }
        for id in found {
            let serial = self.next_serial();
            for event in [
                vicinae_hotkey_v1::event::PRESSED,
                vicinae_hotkey_v1::event::RELEASED,
            ] {
                let _ = self.out.write(
                    id,
                    event,
                    &[ArgType::Uint, ArgType::Uint],
                    &[Arg::Uint(serial), Arg::Uint(time)],
                );
            }
        }
        true
    }

    /// Every hotkey this client asked for.
    #[must_use]
    pub fn hotkeys(&self) -> Vec<Hotkey> {
        self.hotkeys.values().cloned().collect()
    }
}

/// The descriptors a sandbox handed over: the socket to accept on and the
/// one that is closed when it ends.
pub type Listener = (Fd, Fd);
