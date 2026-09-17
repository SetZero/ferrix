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
#[derive(Clone, Debug, PartialEq, Eq)]
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
            if let Some(slot) = slots.get_mut(client) {
                slot.client_mut().drag_motion(time, at);
            }
            return true;
        }
        if let Some((client, _)) = self.over.take()
            && let Some(slot) = slots.get_mut(client)
        {
            slot.client_mut().drag_leave();
            // The target is gone, so the source is owed the news that
            // nothing is taking it.
            self.accepted = None;
        }
        let Some((client, surface, at)) = under else {
            self.accepted(slots, None);
            return true;
        };
        let entered = slots.get_mut(client).is_some_and(|slot| {
            slot.client_mut()
                .drag_enter(surface, at, &self.mimes, self.actions)
                .is_some()
        });
        if entered {
            self.over = Some((client, surface));
        }
        true
    }

    /// The button came up: tell the target to take it and the source that
    /// it happened.
    ///
    /// A drop over nothing, or over a client that said it would take no
    /// type, is a drag that came to nothing: the source is cancelled, which
    /// is what tells a file manager not to delete the original.
    pub fn dropped(&mut self, slots: &mut [Slot]) {
        self.dropped = true;
        let taken = self.accepted.is_some()
            && self
                .over
                .and_then(|(client, _)| slots.get_mut(client))
                .is_some_and(|slot| slot.client_mut().drag_drop());
        let Some(source) = self.source else {
            return;
        };
        let Some(slot) = slots.get_mut(self.client) else {
            return;
        };
        if taken {
            slot.client_mut().drag_dropped(source);
        } else {
            slot.client_mut().drag_cancelled(source);
        }
        let _ = slot.flush();
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
