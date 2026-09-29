//! What a taskbar, a night-light and a power daemon ask about the screens.
//!
//! Three protocols, each bound by a program people run.
//! `ext-foreign-toplevel-list-v1` is the window list as the newer
//! specification has it -- the same job `zwlr_foreign_toplevel_v1` does, with
//! the acting-on-a-window half taken out, which is why it is the one a
//! taskbar written this year binds. `wlr-gamma-control-unstable-v1` is what
//! `gammastep` and `hyprsunset` use to make the screen warmer at night, and
//! the table it hands over is applied to the pixels on their way out.
//! `wlr-output-power-management-unstable-v1` is `wlopm`, which turns a screen
//! off -- the same thing the `dpms` dispatcher does, reached from a program
//! rather than from a keybind.

use compositor_protocol::foreign_list::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
use compositor_protocol::gamma_control::{zwlr_gamma_control_manager_v1, zwlr_gamma_control_v1};
use compositor_protocol::output_power::{zwlr_output_power_manager_v1, zwlr_output_power_v1};
use compositor_protocol::{foreign_list, gamma_control, output_power};
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::{Client, Event, Fatal, ForeignToplevel};
use crate::role::Role;

/// The version an `ext_foreign_toplevel_handle_v1` is made at.
///
/// A handle is the server's object, so its version is the compositor's to
/// choose rather than a request's to inherit, as with the wlroots list.
const HANDLE_VERSION: u32 = 1;

impl Client {
    /// Answer a request to one of this module's objects.
    pub(super) fn screen(
        &mut self,
        sender: ObjectId,
        role: Role,
        version: u32,
        opcode: u16,
        args: &[Arg<'_>],
    ) -> bool {
        match role {
            Role::ForeignList => self.foreign_list(sender, opcode),
            Role::GammaControlManager => self.gamma_manager(version, opcode, args),
            Role::GammaControl => self.gamma_control(sender, opcode, args),
            Role::OutputPowerManager => self.power_manager(version, opcode, args),
            Role::OutputPower => self.output_power(sender, opcode, args),
            _ => return false,
        }
        true
    }

    /// Drop what one of this module's objects held.
    pub(super) fn forget_screen(&mut self, id: ObjectId, role: Role) {
        match role {
            Role::ForeignList => {
                self.lists.retain(|held| *held != id);
            }
            Role::ForeignListHandle => {
                self.list_handles.retain(|_, held| *held != id);
            }
            Role::GammaControl => {
                if let Some(which) = self.gammas.remove(&id) {
                    self.events.push(Event::Gamma {
                        output: which,
                        table: None,
                    });
                }
            }
            Role::OutputPower => {
                let _ = self.powers.remove(&id);
            }
            _ => {}
        }
    }

    /// `ext_foreign_toplevel_list_v1.stop`: the client wants no more.
    ///
    /// `finished` is sent and the list stops being one; the handles it has
    /// already made stay valid, which is what the protocol says.
    fn foreign_list(&mut self, sender: ObjectId, opcode: u16) {
        if opcode != ext_foreign_toplevel_list_v1::request::STOP {
            return;
        }
        self.lists.retain(|held| *held != sender);
        let _ = self.out.write(
            sender,
            ext_foreign_toplevel_list_v1::event::FINISHED,
            &[],
            &[],
        );
    }

    /// Whether this client is watching the window list through
    /// `ext-foreign-toplevel-list-v1`, which is the newer of the two.
    #[must_use]
    pub fn lists_toplevels(&self) -> bool {
        !self.lists.is_empty()
    }

    /// Tell every `ext_foreign_toplevel_list_v1` what the windows are.
    ///
    /// The same shape as the wlroots list's own update: a handle a window
    /// does not have yet is made and announced, one whose name changed is
    /// told, and one whose window has gone is closed. The identifier is the
    /// compositor's own handle for the window, as a string, which is what
    /// the protocol asks for -- "a stable identifier for the toplevel,
    /// unique within the session".
    pub fn list_toplevels(&mut self, windows: &[ForeignToplevel]) {
        if self.lists.is_empty() && self.list_handles.is_empty() {
            return;
        }
        let here: Vec<u64> = windows.iter().map(|what| what.window).collect();
        let gone: Vec<u64> = self
            .list_handles
            .keys()
            .copied()
            .filter(|window| !here.contains(window))
            .collect();
        for window in gone {
            let Some(id) = self.list_handles.remove(&window) else {
                continue;
            };
            let _ = self
                .out
                .write(id, ext_foreign_toplevel_handle_v1::event::CLOSED, &[], &[]);
            let _ = self.list_told.remove(&window);
        }
        for what in windows {
            let window = &what.window;
            let fresh = !self.list_handles.contains_key(window);
            if fresh && self.make_handle(*window).is_none() {
                continue;
            }
            if self.list_told.get(window) == Some(what) {
                continue;
            }
            let Some(id) = self.list_handles.get(window).copied() else {
                continue;
            };
            for (event, text) in [
                (
                    ext_foreign_toplevel_handle_v1::event::TITLE,
                    what.title.clone(),
                ),
                (
                    ext_foreign_toplevel_handle_v1::event::APP_ID,
                    what.app_id.clone(),
                ),
                (
                    ext_foreign_toplevel_handle_v1::event::IDENTIFIER,
                    format!("{window:x}"),
                ),
            ] {
                let _ = self.out.write(
                    id,
                    event,
                    &[ArgType::Str { nullable: false }],
                    &[Arg::Str(Some(&text))],
                );
            }
            let _ = self
                .out
                .write(id, ext_foreign_toplevel_handle_v1::event::DONE, &[], &[]);
            let _ = self.list_told.insert(*window, what.clone());
        }
    }

    /// Make one handle and announce it on every list.
    fn make_handle(&mut self, window: u64) -> Option<ObjectId> {
        let lists = self.lists.clone();
        let id = self
            .objects
            .create(
                &foreign_list::EXT_FOREIGN_TOPLEVEL_HANDLE_V1,
                HANDLE_VERSION,
                Role::ForeignListHandle,
            )
            .ok()?;
        for list in lists {
            let _ = self.out.write(
                list,
                ext_foreign_toplevel_list_v1::event::TOPLEVEL,
                &[ArgType::NewId],
                &[Arg::NewId(id)],
            );
        }
        let _ = self.list_handles.insert(window, id);
        Some(id)
    }

    /// `zwlr_gamma_control_manager_v1.get_gamma_control`.
    ///
    /// The size is answered at once: a client writes three tables of that
    /// many 16-bit entries into the descriptor it then hands over, and one
    /// that was not told the size cannot write anything.
    fn gamma_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwlr_gamma_control_manager_v1::request::GET_GAMMA_CONTROL {
            return;
        }
        let (Some(id), Some(output)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
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
        if !self.make(
            id,
            &gamma_control::ZWLR_GAMMA_CONTROL_V1,
            version,
            Role::GammaControl,
        ) {
            return;
        }
        let _ = self.gammas.insert(id, which);
        let _ = self.out.write(
            id,
            zwlr_gamma_control_v1::event::GAMMA_SIZE,
            &[ArgType::Uint],
            &[Arg::Uint(GAMMA_SIZE)],
        );
    }

    /// `zwlr_gamma_control_v1.set_gamma`: the three tables, on a
    /// descriptor.
    ///
    /// The descriptor is passed up rather than read here: this crate holds
    /// no descriptor and reads no memory, which is the rule the whole
    /// server is written to. The compositor reads it and applies the table
    /// to the pixels on their way out.
    fn gamma_control(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwlr_gamma_control_v1::request::SET_GAMMA {
            return;
        }
        let (Some(which), Some(fd)) = (
            self.gammas.get(&sender).copied(),
            args.first().and_then(Arg::as_fd),
        ) else {
            return;
        };
        self.events.push(Event::Gamma {
            output: which,
            table: Some(fd),
        });
    }

    /// `zwlr_output_power_manager_v1.get_output_power`.
    fn power_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwlr_output_power_manager_v1::request::GET_OUTPUT_POWER {
            return;
        }
        let (Some(id), Some(output)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
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
        if !self.make(
            id,
            &output_power::ZWLR_OUTPUT_POWER_V1,
            version,
            Role::OutputPower,
        ) {
            return;
        }
        let _ = self.powers.insert(id, which);
    }

    /// `zwlr_output_power_v1.set_mode`: turn a screen off or on.
    fn output_power(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwlr_output_power_v1::request::SET_MODE {
            return;
        }
        let (Some(which), Some(mode)) = (
            self.powers.get(&sender).copied(),
            args.first().and_then(Arg::as_uint),
        ) else {
            return;
        };
        self.events.push(Event::OutputPower {
            output: which,
            on: mode == zwlr_output_power_v1::mode::ON,
        });
    }

    /// Tell every `zwlr_output_power_v1` on a screen what it is now.
    pub fn output_powered(&mut self, output: usize, on: bool) {
        let told: Vec<ObjectId> = self
            .powers
            .iter()
            .filter(|(_, which)| **which == output)
            .map(|(id, _)| *id)
            .collect();
        let mode = if on {
            zwlr_output_power_v1::mode::ON
        } else {
            zwlr_output_power_v1::mode::OFF
        };
        for id in told {
            let _ = self.out.write(
                id,
                zwlr_output_power_v1::event::MODE,
                &[ArgType::Uint],
                &[Arg::Uint(mode)],
            );
        }
    }
}

/// How many entries each of a gamma table's three ramps has.
///
/// The client is told this and writes exactly that many 16-bit values per
/// channel. 256 is what a DRM connector's legacy ramp has and what every
/// night-light program is written against.
pub const GAMMA_SIZE: u32 = 256;
