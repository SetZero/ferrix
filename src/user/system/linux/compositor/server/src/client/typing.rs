//! Typing through an input method: `text-input-v3` and `input-method-v2`.
//!
//! An application's text field says it wants text through
//! `zwp_text_input_v3`. An input method -- a program that turns keys into
//! Japanese, or a prediction into a word -- says what it typed through
//! `zwp_input_method_v2`. The two never speak to each other. The compositor
//! stands between them: it tells the input method a field has the focus,
//! and gives the field what the method committed.
//!
//! What a method types is staged in [`Typed`] and applied at its `commit`,
//! all at once, for the same reason a `wl_surface` is double-buffered.

use compositor_protocol::input_method::{self, zwp_input_method_manager_v2, zwp_input_method_v2};
use compositor_protocol::text_input::{self, zwp_text_input_manager_v3, zwp_text_input_v3};
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::{Client, Event};
use crate::role::Role;
use crate::surface::Rect;

/// What an input method has typed, staged until it commits.
///
/// The three are applied together: a method that replaced a word should not
/// be seen half way through, which is the same reason a `wl_surface` is
/// double-buffered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Typed {
    /// `commit_string`: text to insert.
    pub commit: Option<String>,
    /// `set_preedit_string`: the text being composed, and where the cursor
    /// is inside it.
    pub preedit: Option<(String, i32, i32)>,
    /// `delete_surrounding_text`: how much to take out before and after the
    /// cursor.
    pub delete: Option<(u32, u32)>,
}

impl Client {
    /// `zwp_text_input_manager_v3`: `get_text_input`.
    pub(super) fn text_input_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_text_input_manager_v3::request::GET_TEXT_INPUT {
            return;
        }
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        if self.make(id, &text_input::ZWP_TEXT_INPUT_V3, version, Role::TextInput) {
            self.text_inputs.push(id);
        }
    }

    /// `zwp_text_input_v3`: an application saying it wants to be typed
    /// into, and what it is being typed into.
    ///
    /// `enable` and `disable` are the two that matter to the compositor:
    /// they are what an input method is told about, as `activate` and
    /// `deactivate`. The rest -- the surrounding text, the content type,
    /// where the cursor is on the screen -- is passed on so that an input
    /// method can put its candidate window in the right place and guess the
    /// right word.
    pub(super) fn text_input(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        use zwp_text_input_v3::request;
        match opcode {
            request::ENABLE => self.events.push(Event::TextInputEnabled {
                text_input: sender,
                enabled: true,
            }),
            request::DISABLE => self.events.push(Event::TextInputEnabled {
                text_input: sender,
                enabled: false,
            }),
            request::SET_SURROUNDING_TEXT => {
                let text = args.first().and_then(Arg::as_str).unwrap_or("").to_owned();
                let cursor = args.get(1).and_then(Arg::as_int).unwrap_or(0);
                let anchor = args.get(2).and_then(Arg::as_int).unwrap_or(0);
                self.events.push(Event::TextInputSurrounded {
                    text_input: sender,
                    text,
                    cursor,
                    anchor,
                });
            }
            request::SET_CURSOR_RECTANGLE => {
                let numbers: Vec<i32> = args.iter().filter_map(Arg::as_int).collect();
                let [x, y, width, height] = numbers.as_slice() else {
                    return;
                };
                self.events.push(Event::TextInputCursorAt {
                    text_input: sender,
                    rect: Rect {
                        x: *x,
                        y: *y,
                        width: *width,
                        height: *height,
                    },
                });
            }
            request::COMMIT => self
                .events
                .push(Event::TextInputCommitted { text_input: sender }),
            request::DESTROY => {
                self.text_inputs.retain(|held| *held != sender);
                self.events.push(Event::TextInputEnabled {
                    text_input: sender,
                    enabled: false,
                });
            }
            _ => {}
        }
    }

    /// `zwp_input_method_manager_v2`: `get_input_method`.
    ///
    /// One input method a seat: a second is made and told `unavailable` at
    /// once, which is what the protocol says and what stops two programs
    /// both believing they are the keyboard.
    pub(super) fn input_method_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_input_method_manager_v2::request::GET_INPUT_METHOD {
            return;
        }
        let Some(id) = args.get(1).and_then(Arg::as_object) else {
            return;
        };
        if !self.make(
            id,
            &input_method::ZWP_INPUT_METHOD_V2,
            version,
            Role::InputMethod,
        ) {
            return;
        }
        self.input_method = Some(id);
        self.events.push(Event::InputMethodMade { method: id });
    }

    /// `zwp_input_method_v2`: what the input method has typed.
    ///
    /// `commit_string`, `set_preedit_string` and `delete_surrounding_text`
    /// are staged and applied by `commit`, which is the same double-buffered
    /// shape a `wl_surface` has and for the same reason: a method that
    /// replaced a word should not be seen half way through.
    pub(super) fn input_method(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        use zwp_input_method_v2::request;
        match opcode {
            request::COMMIT_STRING => {
                self.typed.commit = args.first().and_then(Arg::as_str).map(str::to_owned);
            }
            request::SET_PREEDIT_STRING => {
                let text = args.first().and_then(Arg::as_str).unwrap_or("").to_owned();
                let begin = args.get(1).and_then(Arg::as_int).unwrap_or(0);
                let end = args.get(2).and_then(Arg::as_int).unwrap_or(0);
                self.typed.preedit = Some((text, begin, end));
            }
            request::DELETE_SURROUNDING_TEXT => {
                let before = args.first().and_then(Arg::as_uint).unwrap_or(0);
                let after = args.get(1).and_then(Arg::as_uint).unwrap_or(0);
                self.typed.delete = Some((before, after));
            }
            request::COMMIT => {
                let typed = std::mem::take(&mut self.typed);
                self.events.push(Event::InputMethodTyped {
                    method: sender,
                    typed,
                });
            }
            request::DESTROY => {
                if self.input_method == Some(sender) {
                    self.input_method = None;
                }
                self.events.push(Event::InputMethodGone { method: sender });
            }
            _ => {}
        }
    }

    /// Tell an input method that a text field has been focused, or has gone.
    pub fn input_method_active(&mut self, method: ObjectId, active: bool) {
        let opcode = if active {
            zwp_input_method_v2::event::ACTIVATE
        } else {
            zwp_input_method_v2::event::DEACTIVATE
        };
        let _ = self.out.write(method, opcode, &[], &[]);
        let _ = self
            .out
            .write(method, zwp_input_method_v2::event::DONE, &[], &[]);
    }

    /// Tell an input method it will never be the seat's: another already is.
    pub fn input_method_unavailable(&mut self, method: ObjectId) {
        let _ = self
            .out
            .write(method, zwp_input_method_v2::event::UNAVAILABLE, &[], &[]);
    }

    /// Give a text field what an input method typed.
    ///
    /// The order is the protocol's: what is deleted, then the preedit, then
    /// the commit, then `done` -- which is what applies all three at once.
    pub fn text_input_typed(&mut self, text_input: ObjectId, typed: &Typed, serial: u32) {
        if let Some((before, after)) = typed.delete {
            let _ = self.out.write(
                text_input,
                zwp_text_input_v3::event::DELETE_SURROUNDING_TEXT,
                &[ArgType::Uint, ArgType::Uint],
                &[Arg::Uint(before), Arg::Uint(after)],
            );
        }
        if let Some((text, begin, end)) = typed.preedit.as_ref() {
            let _ = self.out.write(
                text_input,
                zwp_text_input_v3::event::PREEDIT_STRING,
                &[ArgType::Str { nullable: true }, ArgType::Int, ArgType::Int],
                &[Arg::Str(Some(text)), Arg::Int(*begin), Arg::Int(*end)],
            );
        }
        if let Some(text) = typed.commit.as_ref() {
            let _ = self.out.write(
                text_input,
                zwp_text_input_v3::event::COMMIT_STRING,
                &[ArgType::Str { nullable: true }],
                &[Arg::Str(Some(text))],
            );
        }
        let _ = self.out.write(
            text_input,
            zwp_text_input_v3::event::DONE,
            &[ArgType::Uint],
            &[Arg::Uint(serial)],
        );
    }

    /// Tell a text field that an input method is there, or is gone.
    pub fn text_input_focus(&mut self, text_input: ObjectId, surface: ObjectId, entered: bool) {
        let opcode = if entered {
            zwp_text_input_v3::event::ENTER
        } else {
            zwp_text_input_v3::event::LEAVE
        };
        let _ = self.out.write(
            text_input,
            opcode,
            &[ArgType::Object { nullable: false }],
            &[Arg::Object(surface)],
        );
    }

    /// Every `zwp_text_input_v3` this client has made.
    #[must_use]
    pub fn text_inputs(&self) -> &[ObjectId] {
        &self.text_inputs
    }

    /// The `zwp_input_method_v2` this client holds, if it is the input
    /// method.
    #[must_use]
    pub const fn input_method_object(&self) -> Option<ObjectId> {
        self.input_method
    }
}
