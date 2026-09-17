//! Drag and drop: `wl_data_device.start_drag` and everything it sets off.
//!
//! The most intricate conversation in core Wayland, and the one every file
//! manager, browser and editor uses. Three objects talk at once: the
//! *source* (the client that started the drag), the *device* of whichever
//! client the pointer is now over, and an *offer* the compositor makes to
//! that client for as long as the pointer is over it.
//!
//! The compositor is the only one that can see both ends -- the two clients
//! cannot see each other at all -- so every step is the compositor's to
//! carry: it makes the offer, tells the client under the pointer what types
//! are on it, tells the source which type that client said it would take,
//! and, when the button comes up, tells one to drop and the other that the
//! drop happened.
//!
//! # Where this is simpler than Hyprland
//!
//! The actions negotiation is carried but not judged: a source says what it
//! can do (`copy`, `move`, `ask`), a target says what it wants, and this
//! compositor tells each what the other said and picks the target's
//! preference when the two overlap. Hyprland does the same; the part it has
//! and this does not is the *cursor* changing to say which action is about
//! to happen, because that needs a cursor theme.

use compositor_protocol::core::{self, wl_data_device, wl_data_offer, wl_data_source};
use compositor_wire::{Arg, ArgType, Fixed, ObjectId};

use crate::client::{Client, Event};
use crate::role::Role;

/// A drag this client is the source of, or is being dragged over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dragging {
    /// The offer this client was given for the drag, while the pointer is
    /// over one of its surfaces.
    pub offer: Option<ObjectId>,
    /// Which surface the offer was entered on.
    pub surface: Option<ObjectId>,
}

impl Client {
    /// `wl_data_device.start_drag`.
    ///
    /// The serial is the one from the button press that began the drag; a
    /// client that sends a serial the compositor never gave is ignored,
    /// which is what stops any program starting a drag whenever it likes.
    pub(super) fn start_drag(&mut self, args: &[Arg<'_>]) {
        let source = args.first().and_then(Arg::as_object);
        let (Some(origin), Some(serial)) = (
            args.get(1).and_then(Arg::as_object),
            args.get(3).and_then(Arg::as_uint),
        ) else {
            return;
        };
        // A drag with no source is a client asking for the *icon* only,
        // which the protocol allows and which nothing can be dropped from.
        let mimes = source
            .filter(|source| !source.is_null())
            .and_then(|source| self.sources.get(&source).cloned())
            .unwrap_or_default();
        self.events.push(Event::DragStarted {
            source: source.filter(|source| !source.is_null()),
            origin,
            icon: args
                .get(2)
                .and_then(Arg::as_object)
                .filter(|icon| !icon.is_null()),
            serial,
            mimes,
        });
    }

    /// Tell this client a drag has come over one of its surfaces.
    ///
    /// Makes the offer, names every type on it, says what the source can do
    /// and then enters. That order is the protocol's: a client reads the
    /// types off the offer inside its `enter` handler, and one told after
    /// the enter would have nothing to read.
    pub fn drag_enter(
        &mut self,
        surface: ObjectId,
        (x, y): (f64, f64),
        mimes: &[String],
        actions: u32,
    ) -> Option<ObjectId> {
        let device = self.devices.first().copied()?;
        let version = self.objects.get(device).map_or(3, |entry| entry.version);
        let offer = self
            .objects
            .create(&core::WL_DATA_OFFER, version, Role::DataOffer)
            .ok()?;
        let _ = self.out.write(
            device,
            wl_data_device::event::DATA_OFFER,
            &[ArgType::NewId],
            &[Arg::NewId(offer)],
        );
        for mime in mimes {
            let _ = self.out.write(
                offer,
                wl_data_offer::event::OFFER,
                &[ArgType::Str { nullable: false }],
                &[Arg::Str(Some(mime))],
            );
        }
        // `source_actions` is version 3 and above; a client on an older one
        // never hears it and never sends `set_actions` either.
        if version >= 3 {
            let _ = self.out.write(
                offer,
                wl_data_offer::event::SOURCE_ACTIONS,
                &[ArgType::Uint],
                &[Arg::Uint(actions)],
            );
        }
        let serial = self.next_serial();
        let _ = self.out.write(
            device,
            wl_data_device::event::ENTER,
            &[
                ArgType::Uint,
                ArgType::Object { nullable: false },
                ArgType::Fixed,
                ArgType::Fixed,
                ArgType::Object { nullable: true },
            ],
            &[
                Arg::Uint(serial),
                Arg::Object(surface),
                Arg::Fixed(Fixed::from_f64(x)),
                Arg::Fixed(Fixed::from_f64(y)),
                Arg::Object(offer),
            ],
        );
        self.dragging = Dragging {
            offer: Some(offer),
            surface: Some(surface),
        };
        Some(offer)
    }

    /// The pointer moved while the drag is over this client.
    pub fn drag_motion(&mut self, time: u32, (x, y): (f64, f64)) {
        let Some(device) = self.devices.first().copied() else {
            return;
        };
        if self.dragging.offer.is_none() {
            return;
        }
        let _ = self.out.write(
            device,
            wl_data_device::event::MOTION,
            &[ArgType::Uint, ArgType::Fixed, ArgType::Fixed],
            &[
                Arg::Uint(time),
                Arg::Fixed(Fixed::from_f64(x)),
                Arg::Fixed(Fixed::from_f64(y)),
            ],
        );
    }

    /// The drag left this client's surface.
    ///
    /// The offer goes with it: the protocol says it is destroyed by the
    /// `leave`, and a client that kept it would be holding an object the
    /// compositor has taken back.
    pub fn drag_leave(&mut self) {
        let Some(device) = self.devices.first().copied() else {
            return;
        };
        let Some(offer) = self.dragging.offer.take() else {
            return;
        };
        self.dragging.surface = None;
        let _ = self
            .out
            .write(device, wl_data_device::event::LEAVE, &[], &[]);
        self.destroy(offer, Role::DataOffer);
    }

    /// The button came up over this client: it may take what was dragged.
    pub fn drag_drop(&mut self) -> bool {
        let Some(device) = self.devices.first().copied() else {
            return false;
        };
        if self.dragging.offer.is_none() {
            return false;
        }
        let _ = self
            .out
            .write(device, wl_data_device::event::DROP, &[], &[]);
        true
    }

    /// The offer this client holds for a drag, if it holds one.
    #[must_use]
    pub const fn drag_offer(&self) -> Option<ObjectId> {
        self.dragging.offer
    }

    /// Forget the drag without telling the client, which is what a drop
    /// that has been taken leaves behind.
    pub const fn drag_done(&mut self) {
        self.dragging.offer = None;
        self.dragging.surface = None;
    }

    /// Tell this client's source which type the target said it would take,
    /// or `None` for "it will take nothing".
    pub fn drag_target(&mut self, source: ObjectId, mime: Option<&str>) {
        let _ = self.out.write(
            source,
            wl_data_source::event::TARGET,
            &[ArgType::Str { nullable: true }],
            &[Arg::Str(mime)],
        );
    }

    /// Tell a source, and a target's offer, which action the two agreed on.
    pub fn drag_action(&mut self, object: ObjectId, action: u32, source: bool) {
        let event = if source {
            wl_data_source::event::ACTION
        } else {
            wl_data_offer::event::ACTION
        };
        let _ = self
            .out
            .write(object, event, &[ArgType::Uint], &[Arg::Uint(action)]);
    }

    /// Tell a source the drop happened; the data goes on a pipe as a
    /// selection's does.
    pub fn drag_dropped(&mut self, source: ObjectId) {
        let _ = self
            .out
            .write(source, wl_data_source::event::DND_DROP_PERFORMED, &[], &[]);
    }

    /// Tell a source the target has finished with what it took, which is
    /// when a move may delete the original.
    pub fn drag_finished(&mut self, source: ObjectId) {
        let _ = self
            .out
            .write(source, wl_data_source::event::DND_FINISHED, &[], &[]);
    }

    /// Tell a source the drag came to nothing.
    pub fn drag_cancelled(&mut self, source: ObjectId) {
        let _ = self
            .out
            .write(source, wl_data_source::event::CANCELLED, &[], &[]);
    }

    /// What a source said it can do: `wl_data_source.set_actions`.
    #[must_use]
    pub fn source_actions(&self, source: ObjectId) -> u32 {
        self.source_actions.get(&source).copied().unwrap_or(0)
    }

    /// `wl_data_source.set_actions`, and `wl_data_offer`'s three requests
    /// that are the drag's rather than the selection's.
    ///
    /// Gives whether the request was one of them.
    pub(super) fn drag_request(
        &mut self,
        sender: ObjectId,
        role: Role,
        opcode: u16,
        args: &[Arg<'_>],
    ) -> bool {
        match (role, opcode) {
            (Role::DataSource, wl_data_source::request::SET_ACTIONS) => {
                let Some(actions) = args.first().and_then(Arg::as_uint) else {
                    return true;
                };
                let _ = self.source_actions.insert(sender, actions);
            }
            (Role::DataOffer, wl_data_offer::request::ACCEPT) => {
                self.events.push(Event::DragAccepted {
                    offer: sender,
                    mime: args.get(1).and_then(Arg::as_str).map(ToOwned::to_owned),
                });
            }
            (Role::DataOffer, wl_data_offer::request::SET_ACTIONS) => {
                let numbers: Vec<u32> = args.iter().filter_map(Arg::as_uint).collect();
                let [actions, preferred] = numbers.as_slice() else {
                    return true;
                };
                self.events.push(Event::DragActions {
                    offer: sender,
                    actions: *actions,
                    preferred: *preferred,
                });
            }
            (Role::DataOffer, wl_data_offer::request::FINISH) => {
                self.events.push(Event::DragFinished { offer: sender });
            }
            _ => return false,
        }
        true
    }
}
