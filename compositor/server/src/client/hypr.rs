//! Hyprland's own protocols.
//!
//! Six of them, each written for something Hyprland does that no other
//! compositor had a protocol for. They are the last of what makes a session
//! Hyprland's rather than merely a tiling one: a shortcut a program
//! registers instead of a keybind, a launcher holding the focus while it is
//! up, a program told when the screen locks, the handle that joins a
//! `wl_surface` to the window every other protocol calls by address, a
//! surface asking to be drawn see-through, and a screenshot of one *window*
//! rather than of a screen.
//!
//! # What is not here
//!
//! `hyprland-input-capture-v1`, whose whole conversation is an
//! `libei` socket the compositor hands over, and there is no `libei` on
//! Ferrix. Offering the global and then never sending the descriptor would
//! leave a client waiting for ever, which is worse than not offering it.
//! `hyprland-ctm-control-v1` is left out for a different reason, which
//! `scripts/gen-wayland-protocol.py` gives: its `blocked` event has a
//! description with no summary, which this `wayland-scanner` refuses, so its
//! table could not be checked against libwayland's.

use compositor_protocol::focus_grab::{hyprland_focus_grab_manager_v1, hyprland_focus_grab_v1};
use compositor_protocol::global_shortcuts::{
    hyprland_global_shortcut_v1, hyprland_global_shortcuts_manager_v1,
};
use compositor_protocol::hyprland_surface::{hyprland_surface_manager_v1, hyprland_surface_v1};
use compositor_protocol::lock_notify::{hyprland_lock_notification_v1, hyprland_lock_notifier_v1};
use compositor_protocol::toplevel_export::{
    hyprland_toplevel_export_frame_v1, hyprland_toplevel_export_manager_v1,
};
use compositor_protocol::toplevel_mapping::{
    hyprland_toplevel_mapping_manager_v1, hyprland_toplevel_window_mapping_handle_v1,
};
use compositor_protocol::{
    focus_grab, global_shortcuts, hyprland_surface, lock_notify, toplevel_export, toplevel_mapping,
};
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::{Client, Event, Fatal};
use crate::role::Role;

/// One shortcut a program registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// What the program calls it, which is what `dispatch global` names.
    pub id: String,
    /// The program's own id, which is the other half of the name.
    pub app_id: String,
}

/// One capture of a window being taken.
#[derive(Clone, Copy, Debug)]
pub struct Export {
    /// Which window, by the address every other protocol calls it.
    pub window: u64,
    /// Whether the buffer has been handed over: a frame may be copied into
    /// once.
    pub used: bool,
}

impl Client {
    /// Answer a request to one of this module's objects.
    pub(super) fn hypr(
        &mut self,
        sender: ObjectId,
        role: Role,
        version: u32,
        opcode: u16,
        args: &[Arg<'_>],
    ) -> bool {
        match role {
            Role::GlobalShortcuts => self.shortcuts_manager(version, opcode, args),
            Role::FocusGrabManager => self.grab_manager(version, opcode, args),
            Role::FocusGrab => self.focus_grab(sender, opcode, args),
            Role::LockNotifier => self.lock_notifier(version, opcode, args),
            Role::ToplevelMapping => self.mapping_manager(version, opcode, args),
            Role::HyprlandSurfaceManager => self.surface_manager(version, opcode, args),
            Role::HyprlandSurface => self.hyprland_surface(sender, opcode, args),
            Role::ToplevelExportManager => self.export_manager(version, opcode, args),
            Role::ToplevelExportFrame => self.export_frame(sender, opcode, args),
            // A shortcut, a notification and a mapping handle have only
            // `destroy`, which the destructor flag takes.
            Role::GlobalShortcut | Role::LockNotification | Role::MappingHandle => {}
            _ => return false,
        }
        true
    }

    /// Drop what one of this module's objects held.
    pub(super) fn forget_hypr(&mut self, id: ObjectId, role: Role) {
        match role {
            Role::GlobalShortcut => {
                let _ = self.shortcuts.remove(&id);
            }
            Role::FocusGrab => {
                let _ = self.grabs.remove(&id);
            }
            Role::LockNotification => self.lock_notifications.retain(|held| *held != id),
            Role::HyprlandSurface => {
                if let Some(surface) = self.hyprland_surfaces.remove(&id)
                    && let Some(state) = self.surfaces.get_mut(&surface)
                {
                    state.pending.alpha = None;
                }
            }
            Role::ToplevelExportFrame => {
                let _ = self.exports.remove(&id);
            }
            _ => {}
        }
    }

    /// `hyprland_global_shortcuts_manager_v1.register_shortcut`.
    ///
    /// A shortcut is a *name*, not a key: the person binds a key to
    /// `dispatch global <app_id>:<id>` in their configuration, and the
    /// program hears `pressed`. That is what lets a screen recorder or a
    /// push-to-talk program have a key without reading the keyboard.
    fn shortcuts_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != hyprland_global_shortcuts_manager_v1::request::REGISTER_SHORTCUT {
            return;
        }
        let (Some(object), Some(id), Some(app_id)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_str),
            args.get(2).and_then(Arg::as_str),
        ) else {
            return;
        };
        if self
            .shortcuts
            .values()
            .any(|held| held.id == id && held.app_id == app_id)
        {
            self.fail(Fatal::BadNewId(object));
            return;
        }
        if !self.make(
            object,
            &global_shortcuts::HYPRLAND_GLOBAL_SHORTCUT_V1,
            version,
            Role::GlobalShortcut,
        ) {
            return;
        }
        let _ = self.shortcuts.insert(
            object,
            Shortcut {
                id: id.to_owned(),
                app_id: app_id.to_owned(),
            },
        );
    }

    /// Fire a shortcut by the name `dispatch global` gave.
    ///
    /// The name is `<app_id>:<id>`, as Hyprland's own dispatcher takes it.
    /// Gives whether this client had it.
    pub fn fire_shortcut(&mut self, name: &str, (seconds, nanos): (u64, u32)) -> bool {
        let found = self
            .shortcuts
            .iter()
            .find(|(_, held)| format!("{}:{}", held.app_id, held.id) == name)
            .map(|(id, _)| *id);
        let Some(id) = found else {
            return false;
        };
        // Pressed and released at once: a key bound to `global` is a moment
        // and not a hold, which is how Hyprland's own dispatcher fires one.
        for event in [
            hyprland_global_shortcut_v1::event::PRESSED,
            hyprland_global_shortcut_v1::event::RELEASED,
        ] {
            let _ = self.out.write(
                id,
                event,
                &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
                &[
                    Arg::Uint(u32::try_from(seconds >> 32).unwrap_or(0)),
                    Arg::Uint(u32::try_from(seconds & 0xffff_ffff).unwrap_or(0)),
                    Arg::Uint(nanos),
                ],
            );
        }
        true
    }

    /// Every shortcut this client has registered, by the name `dispatch
    /// global` takes.
    #[must_use]
    pub fn shortcut_names(&self) -> Vec<String> {
        self.shortcuts
            .values()
            .map(|held| format!("{}:{}", held.app_id, held.id))
            .collect()
    }

    /// `hyprland_focus_grab_manager_v1.create_grab`.
    fn grab_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != hyprland_focus_grab_manager_v1::request::CREATE_GRAB {
            return;
        }
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        if self.make(
            id,
            &focus_grab::HYPRLAND_FOCUS_GRAB_V1,
            version,
            Role::FocusGrab,
        ) {
            let _ = self.grabs.insert(id, Vec::new());
        }
    }

    /// `hyprland_focus_grab_v1`: the surfaces a launcher keeps the focus
    /// on, and the `commit` that puts the set in force.
    fn focus_grab(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        let surface = args.first().and_then(Arg::as_object);
        match (opcode, surface) {
            (hyprland_focus_grab_v1::request::ADD_SURFACE, Some(surface)) => {
                if let Some(held) = self.grabs.get_mut(&sender)
                    && !held.contains(&surface)
                {
                    held.push(surface);
                }
            }
            (hyprland_focus_grab_v1::request::REMOVE_SURFACE, Some(surface)) => {
                if let Some(held) = self.grabs.get_mut(&sender) {
                    held.retain(|kept| *kept != surface);
                }
            }
            (hyprland_focus_grab_v1::request::COMMIT, _) => {
                let surfaces = self.grabs.get(&sender).cloned().unwrap_or_default();
                self.events.push(Event::FocusGrabbed {
                    grab: sender,
                    surfaces,
                });
            }
            _ => {}
        }
    }

    /// Tell a grab that it has ended, which is what a click outside it
    /// does.
    pub fn grab_cleared(&mut self, grab: ObjectId) {
        if self.grabs.remove(&grab).is_none() {
            return;
        }
        let _ = self
            .out
            .write(grab, hyprland_focus_grab_v1::event::CLEARED, &[], &[]);
    }

    /// Every grab this client holds and what it covers.
    #[must_use]
    pub fn grabbed(&self) -> Vec<(ObjectId, Vec<ObjectId>)> {
        self.grabs
            .iter()
            .map(|(id, surfaces)| (*id, surfaces.clone()))
            .collect()
    }

    /// `hyprland_lock_notifier_v1.get_lock_notification`.
    fn lock_notifier(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != hyprland_lock_notifier_v1::request::GET_LOCK_NOTIFICATION {
            return;
        }
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        if self.make(
            id,
            &lock_notify::HYPRLAND_LOCK_NOTIFICATION_V1,
            version,
            Role::LockNotification,
        ) {
            self.lock_notifications.push(id);
        }
    }

    /// Tell every `hyprland_lock_notification_v1` whether the session is
    /// locked.
    ///
    /// A program that wants to stop what it is doing while the screen is
    /// locked -- a recorder, a notifier -- has no other way to know:
    /// `ext-session-lock-v1` is the *locker's* protocol and says nothing to
    /// anybody else.
    pub fn lock_changed(&mut self, locked: bool) {
        let event = if locked {
            hyprland_lock_notification_v1::event::LOCKED
        } else {
            hyprland_lock_notification_v1::event::UNLOCKED
        };
        let told = self.lock_notifications.clone();
        for id in told {
            let _ = self.out.write(id, event, &[], &[]);
        }
    }

    /// Whether this client is waiting to hear about the lock.
    #[must_use]
    pub fn watches_lock(&self) -> bool {
        !self.lock_notifications.is_empty()
    }

    /// `hyprland_toplevel_mapping_manager_v1`: the address every other
    /// protocol calls a window by, for a client that holds a toplevel
    /// handle.
    fn mapping_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let wlr = match opcode {
            hyprland_toplevel_mapping_manager_v1::request::GET_WINDOW_FOR_TOPLEVEL => false,
            hyprland_toplevel_mapping_manager_v1::request::GET_WINDOW_FOR_TOPLEVEL_WLR => true,
            _ => return,
        };
        let (Some(id), Some(toplevel)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
        ) else {
            return;
        };
        if !self.make(
            id,
            &toplevel_mapping::HYPRLAND_TOPLEVEL_WINDOW_MAPPING_HANDLE_V1,
            version,
            Role::MappingHandle,
        ) {
            return;
        }
        // Which window a handle names: the wlroots list keeps them by
        // window, and the newer list by the same number.
        let window = if wlr {
            self.handles
                .iter()
                .find(|(_, held)| **held == toplevel)
                .map(|(window, _)| *window)
        } else {
            self.list_handles
                .iter()
                .find(|(_, held)| **held == toplevel)
                .map(|(window, _)| *window)
        };
        match window {
            Some(window) => {
                let _ = self.out.write(
                    id,
                    hyprland_toplevel_window_mapping_handle_v1::event::WINDOW_ADDRESS,
                    &[ArgType::Uint, ArgType::Uint],
                    &[
                        Arg::Uint(u32::try_from(window >> 32).unwrap_or(0)),
                        Arg::Uint(u32::try_from(window & 0xffff_ffff).unwrap_or(0)),
                    ],
                );
            }
            None => {
                let _ = self.out.write(
                    id,
                    hyprland_toplevel_window_mapping_handle_v1::event::FAILED,
                    &[],
                    &[],
                );
            }
        }
    }

    /// `hyprland_surface_manager_v1.get_hyprland_surface`.
    fn surface_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != hyprland_surface_manager_v1::request::GET_HYPRLAND_SURFACE {
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
            &hyprland_surface::HYPRLAND_SURFACE_V1,
            version,
            Role::HyprlandSurface,
        ) {
            let _ = self.hyprland_surfaces.insert(id, surface);
        }
    }

    /// `hyprland_surface_v1.set_opacity`, which is the same field
    /// `wp_alpha_modifier_v1` sets and is reached here as a fraction.
    ///
    /// `set_visible_region` is read and dropped: it says which part of the
    /// surface is worth drawing, which is an optimisation and not a
    /// picture, and a compositor that draws all of it draws the same thing.
    fn hyprland_surface(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != hyprland_surface_v1::request::SET_OPACITY {
            return;
        }
        let (Some(surface), Some(opacity)) = (
            self.hyprland_surfaces.get(&sender).copied(),
            args.first().and_then(Arg::as_fixed),
        ) else {
            return;
        };
        if let Some(state) = self.surfaces.get_mut(&surface) {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a fraction between nothing and one is far inside f32"
            )]
            let fraction = opacity.to_f64().clamp(0.0, 1.0) as f32;
            state.pending.alpha = Some(fraction);
        }
    }

    /// `hyprland_toplevel_export_manager_v1`: a screenshot of one window.
    ///
    /// The same conversation `zwlr_screencopy_v1` has -- the compositor
    /// says what buffer to make, the client makes one and hands it over --
    /// with a window in place of a screen. It is what a recorder uses for
    /// "share one window" rather than "share the screen".
    fn export_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let (frame, window) = match opcode {
            hyprland_toplevel_export_manager_v1::request::CAPTURE_TOPLEVEL => (
                args.first().and_then(Arg::as_object),
                args.get(2).and_then(Arg::as_uint).map(u64::from),
            ),
            hyprland_toplevel_export_manager_v1::request::CAPTURE_TOPLEVEL_WITH_WLR_TOPLEVEL_HANDLE => {
                let handle = args.get(2).and_then(Arg::as_object);
                (
                    args.first().and_then(Arg::as_object),
                    handle.and_then(|handle| {
                        self.handles
                            .iter()
                            .find(|(_, held)| **held == handle)
                            .map(|(window, _)| *window)
                    }),
                )
            }
            _ => return,
        };
        let Some(frame) = frame else {
            return;
        };
        if !self.make(
            frame,
            &toplevel_export::HYPRLAND_TOPLEVEL_EXPORT_FRAME_V1,
            version,
            Role::ToplevelExportFrame,
        ) {
            return;
        }
        let Some(window) = window else {
            let _ = self.out.write(
                frame,
                hyprland_toplevel_export_frame_v1::event::FAILED,
                &[],
                &[],
            );
            return;
        };
        let _ = self.exports.insert(
            frame,
            Export {
                window,
                used: false,
            },
        );
        self.events
            .push(Event::ToplevelExportAsked { frame, window });
    }

    /// Tell an export what buffer to make: the window's size, in the one
    /// format this compositor's canvas is.
    pub fn export_buffer(&mut self, frame: ObjectId, width: u32, height: u32) {
        let stride = width.saturating_mul(4);
        let _ = self.out.write(
            frame,
            hyprland_toplevel_export_frame_v1::event::BUFFER,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[
                Arg::Uint(crate::Format::Xrgb8888.to_wl_shm()),
                Arg::Uint(width),
                Arg::Uint(height),
                Arg::Uint(stride),
            ],
        );
        let _ = self.out.write(
            frame,
            hyprland_toplevel_export_frame_v1::event::BUFFER_DONE,
            &[],
            &[],
        );
    }

    /// `hyprland_toplevel_export_frame_v1.copy`.
    fn export_frame(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != hyprland_toplevel_export_frame_v1::request::COPY {
            return;
        }
        let Some(buffer) = args.first().and_then(Arg::as_object) else {
            return;
        };
        let Some(held) = self.exports.get_mut(&sender) else {
            return;
        };
        if held.used {
            // A frame may be copied into once, as its wlroots sibling says.
            self.fail(Fatal::BadNewId(sender));
            return;
        }
        held.used = true;
        let window = held.window;
        self.events.push(Event::ToplevelExportCopy {
            frame: sender,
            window,
            buffer,
        });
    }

    /// Tell an export it has been filled in, or that it could not be.
    pub fn export_done(&mut self, frame: ObjectId, at: Option<(u64, u32)>) {
        match at {
            Some((seconds, nanos)) => {
                let _ = self.out.write(
                    frame,
                    hyprland_toplevel_export_frame_v1::event::READY,
                    &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
                    &[
                        Arg::Uint(u32::try_from(seconds >> 32).unwrap_or(0)),
                        Arg::Uint(u32::try_from(seconds & 0xffff_ffff).unwrap_or(0)),
                        Arg::Uint(nanos),
                    ],
                );
            }
            None => {
                let _ = self.out.write(
                    frame,
                    hyprland_toplevel_export_frame_v1::event::FAILED,
                    &[],
                    &[],
                );
            }
        }
        let _ = self.exports.remove(&frame);
    }
}
