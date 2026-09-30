//! `wl_output`: what each screen is, as every client reads it.
//!
//! One global to a monitor, and one description each time a client binds
//! it: where the screen is, its mode, its scale and its name, closed by
//! `done`. The compositor above says what the screens are with
//! [`Client::set_outputs`]. The arrangement a settings program asks for is
//! in `outputs.rs`, and the logical size a bar reads, `xdg-output`, is in
//! `desktop.rs`.

use compositor_protocol::core::wl_output;
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::Client;
use crate::surface::Output;

impl Client {
    /// Say what the screen is, before any client binds `wl_output`.
    pub fn set_output(&mut self, output: Output) {
        self.outputs = vec![output];
    }

    /// Say what every screen is, in the order the `wl_output` globals were
    /// added: the first global describes the first monitor and so on, which
    /// is how a client tells two screens apart.
    pub fn set_outputs(&mut self, outputs: Vec<Output>) {
        if !outputs.is_empty() {
            self.outputs = outputs;
        }
    }

    /// The monitor a bound `wl_output` object names, by its place in the
    /// list [`Client::set_outputs`] was given.
    #[must_use]
    pub fn output_of(&self, object: ObjectId) -> Option<usize> {
        self.output_objects.get(&object).copied()
    }

    /// Tell a fresh `wl_output` what the screen is.
    ///
    /// Every client reads these: a toolkit with no mode has no size to scale
    /// against, and foot reports `(null): 0x0+0x0@0Hz` for an output that
    /// sent none. The `done` at the end is what says the description is
    /// whole, and a client waits for it.
    pub(super) fn describe_output(&mut self, id: ObjectId, version: u32, which: usize) {
        let Some(mode) = self.outputs.get(which).cloned() else {
            return;
        };
        let _ = self.out.write(
            id,
            wl_output::event::GEOMETRY,
            &[
                ArgType::Int,
                ArgType::Int,
                ArgType::Int,
                ArgType::Int,
                ArgType::Int,
                ArgType::Str { nullable: false },
                ArgType::Str { nullable: false },
                ArgType::Int,
            ],
            &[
                Arg::Int(mode.x),
                Arg::Int(mode.y),
                // A size in millimetres. Nothing here has a physical screen,
                // and a zero is what every headless compositor sends: a
                // client reads it as "unknown" and uses the scale instead.
                Arg::Int(0),
                Arg::Int(0),
                Arg::Int(wl_output::subpixel::UNKNOWN.cast_signed()),
                Arg::Str(Some("Ferrix")),
                Arg::Str(Some("hyprix")),
                // How the monitor is turned, which Hyprland sends as its
                // `m_transform`: the transform that makes the buffer the
                // picture a person reads.
                Arg::Int(mode.transform),
            ],
        );
        let _ = self.out.write(
            id,
            wl_output::event::MODE,
            &[ArgType::Uint, ArgType::Int, ArgType::Int, ArgType::Int],
            &[
                Arg::Uint(wl_output::mode::CURRENT | wl_output::mode::PREFERRED),
                Arg::Int(mode.width),
                Arg::Int(mode.height),
                // Millihertz, as the protocol counts it.
                Arg::Int(mode.refresh),
            ],
        );
        if version >= 2 {
            let _ = self.out.write(
                id,
                wl_output::event::SCALE,
                &[ArgType::Int],
                &[Arg::Int(mode.scale)],
            );
        }
        if version >= 4 {
            for (opcode, text) in [
                (wl_output::event::NAME, mode.name.as_str()),
                (wl_output::event::DESCRIPTION, mode.description.as_str()),
            ] {
                let _ = self.out.write(
                    id,
                    opcode,
                    &[ArgType::Str { nullable: false }],
                    &[Arg::Str(Some(text))],
                );
            }
        }
        if version >= 2 {
            let _ = self.out.write(id, wl_output::event::DONE, &[], &[]);
        }
    }
}
