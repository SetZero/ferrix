//! The drag the compositor is carrying, while one is going on.
//!
//! Two clients that cannot see each other, joined by the compositor: the
//! source began the drag on its own surface, and whichever client the
//! pointer is now over is told there is something on it. Every step is the
//! compositor's, because only it can see both ends.
//!
//! The pointer does not enter or leave a window in the ordinary way while a
//! drag is on -- that is the protocol's rule and a practical one, since a
//! window that got a `wl_pointer.enter` mid-drag would think the person had
//! clicked it. So the pointer's own events stop at the start of a drag and
//! start again at the end.

use compositor_wire::ObjectId;

use crate::state::Slot;

/// A drag in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Carried {
    /// Which connection started it.
    pub client: usize,
    /// Its `wl_data_source`, or `None` for a drag with nothing on it --
    /// which the protocol allows and which is an icon and no data.
    pub source: Option<ObjectId>,
    /// The `wl_surface` drawn at the pointer, if it gave one.
    pub icon: Option<ObjectId>,
    /// The types the source can give the data in.
    pub mimes: Vec<String>,
    /// What the source said it can do: `wl_data_source.set_actions`.
    pub actions: u32,
    /// Which client and surface the drag is over now.
    pub over: Option<(usize, ObjectId)>,
    /// Where in that surface the target was last told the pointer is.
    pub at: Option<(f64, f64)>,
    /// Which type the target said it would take.
    pub accepted: Option<String>,
    /// Which action the two agreed on.
    pub action: u32,
    /// Whether the button has come up: from then on the drag is waiting for
    /// the target to say it has finished.
    pub dropped: bool,
}

impl Carried {
    /// Move the drag to whatever surface the pointer is over now.
    ///
    /// `under` is the client and surface under the pointer and where the
    /// pointer is inside it, or `None` for a pointer over nothing. Gives
    /// whether anything was said.
    ///
    /// Called every pass, so a pointer that has not moved says nothing. It
    /// used to send a `motion` every pass regardless and call that a change,
    /// which made the next pass come at once: Chrome, dragging its own
    /// selected text, answered each `motion` with an `accept` and a
    /// `set_actions`, the compositor answered those, and in under a second
    /// the socket to Chrome was full and the connection dropped.
    pub fn moved(
        &mut self,
        slots: &mut [Slot],
        under: Option<(usize, ObjectId, (f64, f64))>,
        time: u32,
    ) -> bool {
        let now = under.map(|(client, surface, _)| (client, surface));
        if self.over == now {
            // The same surface: only the movement.
            let (Some((client, _)), Some((_, _, at))) = (self.over, under) else {
                return false;
            };
            if self.at == Some(at) {
                return false;
            }
            self.at = Some(at);
            if let Some(slot) = slots.get_mut(client) {
                slot.client_mut().drag_motion(time, at);
            }
            return true;
        }
        self.at = None;
        let mut left = false;
        if let Some((client, _)) = self.over.take()
            && let Some(slot) = slots.get_mut(client)
        {
            slot.client_mut().drag_leave();
            // The target is gone, so the source is owed the news that
            // nothing is taking it.
            self.accepted = None;
            left = true;
        }
        let Some((client, surface, at)) = under else {
            self.accepted(slots, None);
            return true;
        };
        // A client with no `wl_data_device` is offered nothing, and is
        // asked again every pass; that is not a change to the screen.
        let entered = slots.get_mut(client).is_some_and(|slot| {
            slot.client_mut()
                .drag_enter(surface, at, &self.mimes, self.actions)
                .is_some()
        });
        if entered {
            self.over = Some((client, surface));
            self.at = Some(at);
        }
        left || entered
    }

    /// Whether the drag still holds the pointer: from the start until the
    /// button comes up. It is drawn at the pointer, and no window is sent
    /// the pointer, only while it does.
    #[must_use]
    pub const fn holding(&self) -> bool {
        !self.dropped
    }

    /// The button came up: tell the target to take it and the source that
    /// it happened.
    ///
    /// A drop over nothing, or over a client that said it would take no
    /// type, is a drag that came to nothing: the source is cancelled, which
    /// is what tells a file manager not to delete the original.
    ///
    /// Gives whether the drop was taken. One that was not is over now --
    /// no `finish` will come for it -- and the caller ends it.
    pub fn dropped(&mut self, slots: &mut [Slot]) -> bool {
        self.dropped = true;
        let taken = self.accepted.is_some()
            && self
                .over
                .and_then(|(client, _)| slots.get_mut(client))
                .is_some_and(|slot| slot.client_mut().drag_drop());
        if taken {
            // The drop is also the target's `leave`.
            self.over = None;
            self.at = None;
        }
        let Some(source) = self.source else {
            return taken;
        };
        let Some(slot) = slots.get_mut(self.client) else {
            return taken;
        };
        if taken {
            slot.client_mut().drag_dropped(source);
        } else {
            slot.client_mut().drag_cancelled(source);
        }
        let _ = slot.flush();
        taken
    }

    /// The drag is over: tell whoever is still holding something.
    ///
    /// Called when the source's client goes, or when the target says it has
    /// finished. A target that was still being dragged over is told to
    /// leave, so it does not sit holding an offer for a drag that ended.
    pub fn ended(&mut self, slots: &mut [Slot], finished: bool) {
        if let Some((client, _)) = self.over.take()
            && let Some(slot) = slots.get_mut(client)
        {
            if finished {
                slot.client_mut().drag_done();
            } else {
                slot.client_mut().drag_leave();
            }
        }
        let (Some(source), Some(slot)) = (self.source, slots.get_mut(self.client)) else {
            return;
        };
        if finished {
            slot.client_mut().drag_finished(source);
        } else if !self.dropped {
            slot.client_mut().drag_cancelled(source);
        }
        let _ = slot.flush();
    }

    /// The target said which type it would take: tell the source.
    pub fn accepted(&mut self, slots: &mut [Slot], mime: Option<String>) {
        self.accepted.clone_from(&mime);
        let (Some(source), Some(slot)) = (self.source, slots.get_mut(self.client)) else {
            return;
        };
        slot.client_mut().drag_target(source, mime.as_deref());
    }

    /// The target said what it will do with it: settle on an action and
    /// tell both ends.
    ///
    /// The rule is the protocol's: the action is one both sides offered,
    /// and the target's preference wins when it is among them. `ask` is
    /// last, because it means "put a menu up", and this compositor has none
    /// to put.
    pub fn actions(&mut self, slots: &mut [Slot], wanted: u32, preferred: u32) {
        use compositor_protocol::core::wl_data_device_manager::dnd_action;
        let both = self.actions & wanted;
        let action = if both & preferred != 0 {
            preferred
        } else if both & dnd_action::COPY != 0 {
            dnd_action::COPY
        } else if both & dnd_action::MOVE != 0 {
            dnd_action::MOVE
        } else if both & dnd_action::ASK != 0 {
            dnd_action::ASK
        } else {
            dnd_action::NONE
        };
        self.action = action;
        if let Some((client, _)) = self.over
            && let Some(slot) = slots.get_mut(client)
            && let Some(offer) = slot.client().drag_offer()
        {
            slot.client_mut().drag_action(offer, action, false);
        }
        if let (Some(source), Some(slot)) = (self.source, slots.get_mut(self.client)) {
            slot.client_mut().drag_action(source, action, true);
        }
    }

    /// A connection went, so every slot after it moved.
    ///
    /// Gives whether the drag survived: one whose *source* went is a drag
    /// with nothing on it, and ends.
    pub fn renumber(&mut self, places: &[Option<usize>]) -> bool {
        let Some(Some(at)) = places.get(self.client).copied() else {
            return false;
        };
        self.client = at;
        self.over = self.over.and_then(|(client, surface)| {
            places
                .get(client)
                .copied()
                .flatten()
                .map(|at| (at, surface))
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use compositor_protocol::core;
    use compositor_wire::{Arg, ArgType, ObjectId, Writer};

    use super::Carried;
    use crate::state::Slot;

    fn request(
        sender: u32,
        opcode: u16,
        signature: &'static [ArgType],
        args: &[Arg<'_>],
    ) -> Vec<u8> {
        let mut writer = Writer::new();
        writer
            .write(ObjectId(sender), opcode, signature, args)
            .expect("a message a client could send");
        writer.bytes().to_vec()
    }

    fn bind(name: u32, interface: &str, version: u32, id: u32) -> Vec<u8> {
        request(
            2,
            core::wl_registry::request::BIND,
            &[ArgType::Uint, ArgType::AnyNewId],
            &[
                Arg::Uint(name),
                Arg::AnyNewId {
                    interface,
                    version,
                    id: ObjectId(id),
                },
            ],
        )
    }

    /// A client with a surface, 6, and a `wl_data_device`, so a drag can be
    /// carried over it.
    fn target() -> Slot {
        let mut slot = Slot::for_test();
        let mut bytes = request(
            1,
            core::wl_display::request::GET_REGISTRY,
            &[ArgType::NewId],
            &[Arg::NewId(ObjectId(2))],
        );
        // The names are the order `globals` adds them in.
        bytes.extend(bind(1, "wl_compositor", 6, 3));
        bytes.extend(bind(4, "wl_seat", 7, 4));
        bytes.extend(bind(5, "wl_data_device_manager", 3, 5));
        bytes.extend(request(
            3,
            core::wl_compositor::request::CREATE_SURFACE,
            &[ArgType::NewId],
            &[Arg::NewId(ObjectId(6))],
        ));
        bytes.extend(request(
            5,
            core::wl_data_device_manager::request::GET_DATA_DEVICE,
            &[ArgType::NewId, ArgType::Object { nullable: false }],
            &[Arg::NewId(ObjectId(7)), Arg::Object(ObjectId(4))],
        ));
        let client = slot.client_mut();
        assert_eq!(client.read(&bytes, &[]), bytes.len());
        assert_eq!(client.fatal(), None);
        let _ = client.take_outgoing();
        slot
    }

    fn carried() -> Carried {
        Carried {
            client: 0,
            source: None,
            icon: None,
            mimes: vec!["text/plain".to_owned()],
            actions: core::wl_data_device_manager::dnd_action::COPY,
            over: None,
            at: None,
            accepted: None,
            action: 0,
            dropped: false,
        }
    }

    /// A drag held still over a window says nothing, however many passes go
    /// by, and moving it says so once.
    ///
    /// Each pass used to send a `motion` and call it a change, which made
    /// the next pass come at once. Chrome, dragging its own selected text,
    /// answered every `motion`, and in under a second the socket to it was
    /// full and the compositor dropped it.
    #[test]
    fn a_drag_held_still_says_nothing() {
        let mut slots = [target()];
        let mut held = carried();
        let over = |x: f64| Some((0, ObjectId(6), (x, 5.0)));

        assert!(held.moved(&mut slots, over(5.0), 1), "entering is news");
        assert!(!slots[0].client_mut().take_outgoing().bytes.is_empty());

        for pass in 2..100 {
            assert!(!held.moved(&mut slots, over(5.0), pass), "pass {pass}");
        }
        assert!(slots[0].client_mut().take_outgoing().bytes.is_empty());

        assert!(held.moved(&mut slots, over(6.0), 100), "a movement is news");
        assert!(!slots[0].client_mut().take_outgoing().bytes.is_empty());
    }

    /// A drop on a window that never said it would take a type is not
    /// taken: the caller ends the drag there, since no `finish` will come.
    #[test]
    fn a_drop_nothing_accepted_is_not_taken() {
        let mut slots = [target()];
        let mut held = carried();
        assert!(held.moved(&mut slots, Some((0, ObjectId(6), (5.0, 5.0))), 1));
        assert!(!held.dropped(&mut slots));
        assert!(held.dropped, "it is marked dropped all the same");
    }

    /// A window that made no `wl_data_device` is offered nothing, and the
    /// drag sitting over it is no change either.
    #[test]
    fn a_drag_over_a_window_that_cannot_take_it_is_no_change() {
        let mut slots = [Slot::for_test()];
        let mut held = carried();
        for pass in 0..10 {
            assert!(!held.moved(&mut slots, Some((0, ObjectId(6), (1.0, 1.0))), pass));
        }
    }
}
