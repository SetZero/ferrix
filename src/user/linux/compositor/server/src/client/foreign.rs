//! `wlr-foreign-toplevel-management-unstable-v1`: the window list a bar
//! reads, and acts on.
//!
//! A taskbar binds the manager and is given a handle for every window. It
//! is told each one's title, application id and state, and told again
//! whenever one changes; a click on the bar is a request on the handle to
//! activate, close, maximise or minimise the window. Waybar's taskbar and
//! every dock on a wlroots compositor speak this. `screen.rs` has the newer
//! `ext-foreign-toplevel-list-v1`, which is the same list with the acting
//! half taken out.

use compositor_protocol::foreign_toplevel::{
    self, zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::{Client, Event};
use crate::role::Role;

/// The version of `zwlr_foreign_toplevel_handle_v1` a handle is made at.
///
/// A handle is the server's object, so its version is not inherited from a
/// request the way every client-made object's is: the compositor picks it,
/// and it is the manager's, which is what wlroots does.
const FOREIGN_TOPLEVEL_VERSION: u32 = 3;

/// What a bar asked the compositor to do to somebody else's window.
///
/// `set_rectangle` is left out: it says where the window's icon is on the
/// bar, for a minimise animation to fly to, and this compositor has neither.
/// So is `set_minimized`, which Hyprland answers by moving the window to the
/// special workspace; that is `movetoworkspacesilent special:minimized` and
/// is the compositor's to decide, so it comes through as the request and the
/// compositor chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignRequest {
    /// `activate`: focus it.
    Activate,
    /// `close`: ask it to close, as `killactive` does.
    Close,
    /// `set_fullscreen` or `unset_fullscreen`.
    Fullscreen(bool),
    /// `set_maximized` or `unset_maximized`.
    Maximized(bool),
    /// `set_minimized` or `unset_minimized`.
    Minimized(bool),
}

/// What one window looks like to a bar.
///
/// The four states `zwlr_foreign_toplevel_handle_v1.state` has, and the two
/// names every taskbar draws.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignToplevel {
    /// The window, as the compositor numbered it.
    pub window: u64,
    /// Its title.
    pub title: String,
    /// Its application id.
    pub app_id: String,
    /// Whether it is the focused window.
    pub activated: bool,
    /// Whether it is fullscreen.
    pub fullscreen: bool,
    /// Whether it is maximized.
    pub maximized: bool,
    /// Whether it is minimized, which here means on a workspace nothing
    /// shows.
    pub minimized: bool,
}

impl Client {
    /// `zwlr_foreign_toplevel_manager_v1`: `stop`.
    ///
    /// `stop` is a farewell, not a destroy: the protocol has the compositor
    /// answer with `finished`, after which neither side uses the manager
    /// again. The handles it made stay valid until each is destroyed, which
    /// is why they are not taken away here.
    pub(super) fn toplevel_manager(&mut self, sender: ObjectId, opcode: u16) {
        if opcode != zwlr_foreign_toplevel_manager_v1::request::STOP {
            return;
        }
        if let Some(at) = self.managers.iter().position(|held| *held == sender) {
            let _ = self.managers.remove(at);
        }
        let _ = self.out.write(
            sender,
            zwlr_foreign_toplevel_manager_v1::event::FINISHED,
            &[],
            &[],
        );
    }

    /// `zwlr_foreign_toplevel_handle_v1`: what a bar does with a window.
    ///
    /// Every one of them is the compositor's to carry out, so each becomes
    /// an event. `set_rectangle` is accepted and dropped: it says where the
    /// window's icon is on the bar so a minimise can animate towards it, and
    /// there is no such animation here.
    pub(super) fn toplevel_handle(&mut self, sender: ObjectId, opcode: u16) {
        use zwlr_foreign_toplevel_handle_v1::request;
        if opcode == request::DESTROY {
            self.forget_handle(sender);
            return;
        }
        let what = match opcode {
            request::ACTIVATE => ForeignRequest::Activate,
            request::CLOSE => ForeignRequest::Close,
            request::SET_FULLSCREEN => ForeignRequest::Fullscreen(true),
            request::UNSET_FULLSCREEN => ForeignRequest::Fullscreen(false),
            request::SET_MAXIMIZED => ForeignRequest::Maximized(true),
            request::UNSET_MAXIMIZED => ForeignRequest::Maximized(false),
            request::SET_MINIMIZED => ForeignRequest::Minimized(true),
            request::UNSET_MINIMIZED => ForeignRequest::Minimized(false),
            _ => return,
        };
        let window = self
            .handles
            .iter()
            .find(|(_, handle)| **handle == sender)
            .map(|(window, _)| *window);
        if let Some(window) = window {
            self.events
                .push(Event::ForeignToplevelAsked { window, what });
        }
    }

    /// Forget the handle `id`, whichever window it named.
    fn forget_handle(&mut self, id: ObjectId) {
        let window = self
            .handles
            .iter()
            .find(|(_, handle)| **handle == id)
            .map(|(window, _)| *window);
        if let Some(window) = window {
            let _ = self.handles.remove(&window);
            let _ = self.told.remove(&window);
        }
    }

    /// Whether this client is a bar: it bound the toplevel manager and has
    /// not stopped it.
    #[must_use]
    pub fn watches_toplevels(&self) -> bool {
        !self.managers.is_empty()
    }

    /// Tell this client what every window is now, making and taking away
    /// handles as the list changes.
    ///
    /// The compositor calls this with the whole list each pass rather than
    /// with what changed, because the compositor is where the windows are
    /// and this is where it is known what each client was last told. A
    /// window whose fields are what this client already has is not written
    /// to at all: a bar redrawing on every frame of an animation because the
    /// compositor said `done` is a bar that burns a core.
    pub fn show_toplevels(&mut self, windows: &[ForeignToplevel]) {
        if self.managers.is_empty() {
            return;
        }
        // Gone first, so that a bar is never told about more windows than
        // there are.
        let living: Vec<u64> = windows.iter().map(|window| window.window).collect();
        let closed: Vec<u64> = self
            .handles
            .keys()
            .copied()
            .filter(|window| !living.contains(window))
            .collect();
        for window in closed {
            if let Some(handle) = self.handles.remove(&window) {
                let _ = self.out.write(
                    handle,
                    zwlr_foreign_toplevel_handle_v1::event::CLOSED,
                    &[],
                    &[],
                );
                // The handle is the client's to destroy, and it will: until
                // then it is live and may still be sent requests.
                let _ = self.told.remove(&window);
            }
        }
        for window in windows {
            self.show_toplevel(window);
        }
    }

    /// One window, made or brought up to date.
    fn show_toplevel(&mut self, window: &ForeignToplevel) {
        let fresh = !self.handles.contains_key(&window.window);
        if fresh {
            let Ok(handle) = self.objects.create(
                &foreign_toplevel::ZWLR_FOREIGN_TOPLEVEL_HANDLE_V1,
                FOREIGN_TOPLEVEL_VERSION,
                Role::ForeignToplevel,
            ) else {
                return;
            };
            let _ = self.handles.insert(window.window, handle);
            let managers = self.managers.clone();
            for manager in managers {
                let _ = self.out.write(
                    manager,
                    zwlr_foreign_toplevel_manager_v1::event::TOPLEVEL,
                    &[ArgType::NewId],
                    &[Arg::NewId(handle)],
                );
            }
        } else if self.told.get(&window.window) == Some(window) {
            return;
        }
        let Some(handle) = self.handles.get(&window.window).copied() else {
            return;
        };
        let before = self.told.get(&window.window).cloned().unwrap_or_default();
        if fresh || before.title != window.title {
            let _ = self.out.write(
                handle,
                zwlr_foreign_toplevel_handle_v1::event::TITLE,
                &[ArgType::Str { nullable: false }],
                &[Arg::Str(Some(&window.title))],
            );
        }
        if fresh || before.app_id != window.app_id {
            let _ = self.out.write(
                handle,
                zwlr_foreign_toplevel_handle_v1::event::APP_ID,
                &[ArgType::Str { nullable: false }],
                &[Arg::Str(Some(&window.app_id))],
            );
        }
        // The states go as one array, which is what the protocol says: a
        // `state` event replaces the set rather than adding to it.
        let mut states = Vec::new();
        for (on, value) in [
            (
                window.maximized,
                zwlr_foreign_toplevel_handle_v1::state::MAXIMIZED,
            ),
            (
                window.minimized,
                zwlr_foreign_toplevel_handle_v1::state::MINIMIZED,
            ),
            (
                window.activated,
                zwlr_foreign_toplevel_handle_v1::state::ACTIVATED,
            ),
            (
                window.fullscreen,
                zwlr_foreign_toplevel_handle_v1::state::FULLSCREEN,
            ),
        ] {
            if on {
                states.extend_from_slice(&value.to_ne_bytes());
            }
        }
        let _ = self.out.write(
            handle,
            zwlr_foreign_toplevel_handle_v1::event::STATE,
            &[ArgType::Array],
            &[Arg::Array(&states)],
        );
        // Everything above is one atomic change, and `done` is what says so.
        let _ = self.out.write(
            handle,
            zwlr_foreign_toplevel_handle_v1::event::DONE,
            &[],
            &[],
        );
        let _ = self.told.insert(window.window, window.clone());
    }
}
