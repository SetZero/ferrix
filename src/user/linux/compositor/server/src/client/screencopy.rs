//! `wlr-screencopy-unstable-v1`: one frame of one screen, asked for and
//! answered.
//!
//! What `grim` binds, and every screenshot tool written for wlroots before
//! the `ext` namespace had its own. The client names a `wl_output`, and a
//! rectangle of it if it wants less; the compositor says what buffer to
//! make; the client makes one and says `copy`; the compositor fills it and
//! says `ready`. A frame is copied into once. The `ext` protocols, which do
//! the same job frame after frame for a recorder, are in `capture.rs`.

use compositor_protocol::screencopy::{self, zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1};
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::{Client, Event, Fatal};
use crate::role::Role;
use crate::shm::Format;
use crate::surface::Rect;

/// One screenshot being taken.
#[derive(Clone, Copy, Debug)]
pub(super) struct Capture {
    /// Which screen, by its place in the outputs.
    output: usize,
    /// The part of it, or `None` for all of it.
    region: Option<Rect>,
    /// Whether the buffer has been handed over.
    used: bool,
}

impl Client {
    /// `zwlr_screencopy_manager_v1`: `capture_output` and
    /// `capture_output_region`.
    ///
    /// The frame object is the client's id, made here; which screen it names
    /// is worked out from the `wl_output` it was given, since a screenshot
    /// program binds every output and asks for the one it wants. The size
    /// and format it must make a buffer of are the compositor's to say, so
    /// this only records the request and reports it.
    ///
    /// `overlay_cursor` is read and ignored: there is no cursor drawn into
    /// the frame to leave out.
    pub(super) fn screencopy_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let region = match opcode {
            zwlr_screencopy_manager_v1::request::CAPTURE_OUTPUT => None,
            zwlr_screencopy_manager_v1::request::CAPTURE_OUTPUT_REGION => {
                let value = |at: usize| args.get(at).and_then(Arg::as_int).unwrap_or(0);
                Some(Rect {
                    x: value(3),
                    y: value(4),
                    width: value(5),
                    height: value(6),
                })
            }
            _ => return,
        };
        let (Some(frame), Some(output)) = (
            args.first().and_then(Arg::as_object),
            args.get(2).and_then(Arg::as_object),
        ) else {
            return;
        };
        let Some(which) = self.output_objects.get(&output).copied() else {
            // A `wl_output` this client never bound: the frame is made and
            // failed at once, which is what the protocol has for a capture
            // that cannot be done.
            if self.make(
                frame,
                &screencopy::ZWLR_SCREENCOPY_FRAME_V1,
                version,
                Role::ScreencopyFrame,
            ) {
                self.screencopy_failed(frame);
            }
            return;
        };
        if !self.make(
            frame,
            &screencopy::ZWLR_SCREENCOPY_FRAME_V1,
            version,
            Role::ScreencopyFrame,
        ) {
            return;
        }
        let _ = self.frames.insert(
            frame,
            Capture {
                output: which,
                region,
                used: false,
            },
        );
        self.events.push(Event::ScreencopyWanted {
            frame,
            output: which,
            region,
        });
    }

    /// `zwlr_screencopy_frame_v1`: `copy`, `copy_with_damage` and `destroy`.
    ///
    /// A frame may be copied into once. A second `copy` is
    /// `already_used`, which is a protocol error and so the end of the
    /// connection: the client has lost track of an object it owns.
    pub(super) fn screencopy_frame(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        use zwlr_screencopy_frame_v1::request;
        if opcode == request::DESTROY {
            let _ = self.frames.remove(&sender);
            return;
        }
        let with_damage = match opcode {
            request::COPY => false,
            request::COPY_WITH_DAMAGE => true,
            _ => return,
        };
        let Some(buffer) = args.first().and_then(Arg::as_object) else {
            return;
        };
        let Some(capture) = self.frames.get_mut(&sender) else {
            return;
        };
        if capture.used {
            self.fail(Fatal::Interface {
                object: sender,
                code: zwlr_screencopy_frame_v1::error::ALREADY_USED,
                text: "the frame has already been used to copy".to_owned(),
            });
            return;
        }
        capture.used = true;
        let (output, region) = (capture.output, capture.region);
        self.events.push(Event::ScreencopyInto {
            frame: sender,
            buffer,
            output,
            region,
            with_damage,
        });
    }

    /// Tell a screenshot program what buffer to make: the format, the size
    /// and the stride of the screen it asked for.
    ///
    /// `buffer_done` follows at version 3, which is what says the list of
    /// formats is complete; at 1 and 2 there is no such event and the client
    /// takes the single `buffer` as the whole answer.
    pub fn screencopy_offer(&mut self, frame: ObjectId, format: Format, size: (u32, u32)) {
        let version = self.objects.get(frame).map_or(1, |entry| entry.version);
        let (width, height) = size;
        let _ = self.out.write(
            frame,
            zwlr_screencopy_frame_v1::event::BUFFER,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[
                Arg::Uint(format.to_wl_shm()),
                Arg::Uint(width),
                Arg::Uint(height),
                Arg::Uint(width.saturating_mul(4)),
            ],
        );
        if version >= 3 {
            let _ = self.out.write(
                frame,
                zwlr_screencopy_frame_v1::event::BUFFER_DONE,
                &[],
                &[],
            );
        }
    }

    /// The screenshot is in the client's buffer: `flags`, then the damage it
    /// asked for, then `ready` at `when`.
    ///
    /// `when` is the presentation time as `clock_gettime(CLOCK_MONOTONIC)`
    /// gives it, split the way the protocol splits it: the seconds in two
    /// halves so they do not overflow a `uint` until the machine has been up
    /// for longer than it will be.
    pub fn screencopy_ready(&mut self, frame: ObjectId, when: (u64, u32), damaged: Option<Rect>) {
        let version = self.objects.get(frame).map_or(1, |entry| entry.version);
        // No flags: the frame is written top row first, so `y_invert` is not
        // set, which is what a client reads to know which way up it is.
        let _ = self.out.write(
            frame,
            zwlr_screencopy_frame_v1::event::FLAGS,
            &[ArgType::Uint],
            &[Arg::Uint(0)],
        );
        if version >= 2
            && let Some(rect) = damaged
        {
            let at = |value: i32| Arg::Uint(u32::try_from(value).unwrap_or(0));
            let _ = self.out.write(
                frame,
                zwlr_screencopy_frame_v1::event::DAMAGE,
                &[ArgType::Uint, ArgType::Uint, ArgType::Uint, ArgType::Uint],
                &[at(rect.x), at(rect.y), at(rect.width), at(rect.height)],
            );
        }
        let (seconds, nanos) = when;
        let _ = self.out.write(
            frame,
            zwlr_screencopy_frame_v1::event::READY,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[
                Arg::Uint(u32::try_from(seconds >> 32).unwrap_or(0)),
                Arg::Uint(u32::try_from(seconds & 0xFFFF_FFFF).unwrap_or(0)),
                Arg::Uint(nanos),
            ],
        );
    }

    /// The screenshot could not be taken.
    ///
    /// The frame is dead from here: the protocol says the client must
    /// destroy it and ask again, which is what `grim` does.
    pub fn screencopy_failed(&mut self, frame: ObjectId) {
        let _ = self
            .out
            .write(frame, zwlr_screencopy_frame_v1::event::FAILED, &[], &[]);
    }
}
