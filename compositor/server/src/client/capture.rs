//! Screenshots as the `ext` namespace has them: a source and a session.
//!
//! `zwlr_screencopy_v1` is one frame of one screen, asked for and answered.
//! `ext-image-copy-capture-v1` is the same job taken apart: a *source* is a
//! thing that can be captured -- a screen, or a window from either toplevel
//! list -- and a *session* copies frames out of it, one after another, for
//! as long as the client wants them. That is what a screen recorder or a
//! screen-sharing portal actually needs, and it is what a `grim` or an
//! `xdg-desktop-portal` written this year binds.
//!
//! The conversation is longer than the wlroots one and the same shape. The
//! compositor says what buffer to make -- the size, and each format it can
//! give -- and ends with `done`. The client makes one, makes a frame,
//! attaches the buffer and says `capture`; the compositor fills it in and
//! answers `ready`, and the client may make another frame on the same
//! session without asking about the size again.
//!
//! # What is not offered
//!
//! `create_pointer_cursor_session`, which captures the cursor by itself for
//! a recorder that draws its own. This compositor draws the pointer into
//! the frame, so a cursor session would have nothing of its own to hand
//! over; the request is answered with a session that is stopped at once,
//! which is the protocol's way of saying "not this one".

use compositor_protocol::capture_source::{
    ext_foreign_toplevel_image_capture_source_manager_v1,
    ext_output_image_capture_source_manager_v1,
};
use compositor_protocol::image_copy::{
    ext_image_copy_capture_frame_v1, ext_image_copy_capture_manager_v1,
    ext_image_copy_capture_session_v1,
};
use compositor_protocol::{capture_source, image_copy};
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::{Client, Event, Fatal};
use crate::role::Role;

/// What a capture session is looking at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// A screen, by its place in the outputs.
    Screen(usize),
    /// A window, by the address every other protocol calls it.
    Window(u64),
}

/// One frame being copied out of a session.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// Which session it belongs to.
    pub session: ObjectId,
    /// The buffer the client attached, once it has.
    pub buffer: Option<ObjectId>,
}

impl Client {
    /// Answer a request to one of this module's objects.
    pub(super) fn capture(
        &mut self,
        sender: ObjectId,
        role: Role,
        version: u32,
        opcode: u16,
        args: &[Arg<'_>],
    ) -> bool {
        match role {
            Role::OutputCaptureSourceManager => self.capture_source(version, opcode, args, true),
            Role::ToplevelCaptureSourceManager => self.capture_source(version, opcode, args, false),
            Role::CaptureManager => self.capture_manager(version, opcode, args),
            Role::CaptureSession => self.capture_session(sender, version, opcode, args),
            Role::CaptureFrame => self.capture_frame(sender, opcode, args),
            // A source and a cursor session have only `destroy` and, for
            // the cursor one, a getter this compositor stops at once.
            Role::CaptureSource | Role::CursorCaptureSession => {}
            _ => return false,
        }
        true
    }

    /// Drop what one of this module's objects held.
    pub(super) fn forget_capture(&mut self, id: ObjectId, role: Role) {
        match role {
            Role::CaptureSource => {
                let _ = self.capture_sources.remove(&id);
            }
            Role::CaptureSession => {
                let _ = self.sessions.remove(&id);
            }
            Role::CaptureFrame => {
                let _ = self.frames_taken.remove(&id);
            }
            _ => {}
        }
    }

    /// `create_source` on either source manager: a screen, or a window.
    fn capture_source(&mut self, version: u32, opcode: u16, args: &[Arg<'_>], screen: bool) {
        let wanted = if screen {
            ext_output_image_capture_source_manager_v1::request::CREATE_SOURCE
        } else {
            ext_foreign_toplevel_image_capture_source_manager_v1::request::CREATE_SOURCE
        };
        if opcode != wanted {
            return;
        }
        let (Some(id), Some(thing)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
        ) else {
            return;
        };
        // Which screen or which window: an output by the object the client
        // bound, and a window by the handle either toplevel list gave it.
        let source = if screen {
            self.output_objects.get(&thing).copied().map(Source::Screen)
        } else {
            self.list_handles
                .iter()
                .find(|(_, held)| **held == thing)
                .or_else(|| self.handles.iter().find(|(_, held)| **held == thing))
                .map(|(window, _)| Source::Window(*window))
        };
        let Some(source) = source else {
            self.fail(Fatal::WrongInterface {
                object: thing,
                wanted: if screen {
                    "wl_output"
                } else {
                    "ext_foreign_toplevel_handle_v1"
                },
            });
            return;
        };
        if self.make(
            id,
            &capture_source::EXT_IMAGE_CAPTURE_SOURCE_V1,
            version,
            Role::CaptureSource,
        ) {
            let _ = self.capture_sources.insert(id, source);
        }
    }

    /// `ext_image_copy_capture_manager_v1`: a session on a source.
    fn capture_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let cursor = match opcode {
            ext_image_copy_capture_manager_v1::request::CREATE_SESSION => false,
            ext_image_copy_capture_manager_v1::request::CREATE_POINTER_CURSOR_SESSION => true,
            _ => return,
        };
        let (Some(id), Some(source)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
        ) else {
            return;
        };
        if cursor {
            // The cursor by itself, for a recorder that draws its own.
            // This compositor draws the pointer into the frame, so there
            // is nothing separate to hand over.
            let _ = self.make(
                id,
                &image_copy::EXT_IMAGE_COPY_CAPTURE_CURSOR_SESSION_V1,
                version,
                Role::CursorCaptureSession,
            );
            return;
        }
        let Some(looking) = self.capture_sources.get(&source).copied() else {
            self.fail(Fatal::WrongInterface {
                object: source,
                wanted: "ext_image_capture_source_v1",
            });
            return;
        };
        if !self.make(
            id,
            &image_copy::EXT_IMAGE_COPY_CAPTURE_SESSION_V1,
            version,
            Role::CaptureSession,
        ) {
            return;
        }
        let _ = self.sessions.insert(id, looking);
        self.events.push(Event::CaptureSession {
            session: id,
            source: looking,
        });
    }

    /// Tell a session what buffer to make.
    ///
    /// The size, then every format the compositor can give, then `done`:
    /// the protocol says a client waits for `done` and reads everything
    /// before it, which is why nothing is sent piecemeal.
    pub fn capture_offer(&mut self, session: ObjectId, width: u32, height: u32) {
        let _ = self.out.write(
            session,
            ext_image_copy_capture_session_v1::event::BUFFER_SIZE,
            &[ArgType::Uint, ArgType::Uint],
            &[Arg::Uint(width), Arg::Uint(height)],
        );
        // `XRGB8888`, as `zwlr_screencopy_v1`'s frames are: the canvas is
        // opaque, and an alpha channel that is always 0xFF is a larger
        // buffer saying the same thing.
        let _ = self.out.write(
            session,
            ext_image_copy_capture_session_v1::event::SHM_FORMAT,
            &[ArgType::Uint],
            &[Arg::Uint(crate::Format::Xrgb8888.to_wl_shm())],
        );
        let _ = self.out.write(
            session,
            ext_image_copy_capture_session_v1::event::DONE,
            &[],
            &[],
        );
    }

    /// Tell a session it is over: its source has gone.
    pub fn capture_stopped(&mut self, session: ObjectId) {
        let _ = self.out.write(
            session,
            ext_image_copy_capture_session_v1::event::STOPPED,
            &[],
            &[],
        );
        let _ = self.sessions.remove(&session);
    }

    /// `ext_image_copy_capture_session_v1.create_frame`.
    fn capture_session(&mut self, sender: ObjectId, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != ext_image_copy_capture_session_v1::request::CREATE_FRAME {
            return;
        }
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        if !self.sessions.contains_key(&sender) {
            return;
        }
        if self.make(
            id,
            &image_copy::EXT_IMAGE_COPY_CAPTURE_FRAME_V1,
            version,
            Role::CaptureFrame,
        ) {
            let _ = self.frames_taken.insert(
                id,
                Frame {
                    session: sender,
                    buffer: None,
                },
            );
        }
    }

    /// `ext_image_copy_capture_frame_v1`: the buffer, the damage and the
    /// `capture` that asks for it to be filled in.
    fn capture_frame(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        match opcode {
            ext_image_copy_capture_frame_v1::request::ATTACH_BUFFER => {
                let Some(buffer) = args.first().and_then(Arg::as_object) else {
                    return;
                };
                if let Some(frame) = self.frames_taken.get_mut(&sender) {
                    frame.buffer = Some(buffer);
                }
            }
            ext_image_copy_capture_frame_v1::request::CAPTURE => {
                let Some(frame) = self.frames_taken.get(&sender).copied() else {
                    return;
                };
                let Some(source) = self.sessions.get(&frame.session).copied() else {
                    return;
                };
                let Some(buffer) = frame.buffer else {
                    // `capture` with no buffer is a client that lost track
                    // of its own frame, which the protocol calls an error.
                    self.fail(Fatal::BadNewId(sender));
                    return;
                };
                self.events.push(Event::CaptureAsked {
                    frame: sender,
                    source,
                    buffer,
                });
            }
            // `damage_buffer` says which part the client already has, which
            // is an optimisation: a compositor that copies all of it copies
            // the same picture.
            _ => {}
        }
    }

    /// Tell a frame it has been filled in, with when it was shown.
    pub fn capture_ready(&mut self, frame: ObjectId, (seconds, nanos): (u64, u32)) {
        let _ = self.out.write(
            frame,
            ext_image_copy_capture_frame_v1::event::PRESENTATION_TIME,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[
                Arg::Uint(u32::try_from(seconds >> 32).unwrap_or(0)),
                Arg::Uint(u32::try_from(seconds & 0xffff_ffff).unwrap_or(0)),
                Arg::Uint(nanos),
            ],
        );
        let _ = self.out.write(
            frame,
            ext_image_copy_capture_frame_v1::event::READY,
            &[],
            &[],
        );
        let _ = self.frames_taken.remove(&frame);
    }

    /// Tell a frame it could not be.
    pub fn capture_failed(&mut self, frame: ObjectId) {
        let _ = self.out.write(
            frame,
            ext_image_copy_capture_frame_v1::event::FAILED,
            &[ArgType::Uint],
            &[Arg::Uint(
                ext_image_copy_capture_frame_v1::failure_reason::UNKNOWN,
            )],
        );
        let _ = self.frames_taken.remove(&frame);
    }
}
