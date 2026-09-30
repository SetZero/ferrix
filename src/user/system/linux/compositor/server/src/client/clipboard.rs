//! The clipboard and the primary selection, as an application sees them.
//!
//! `wl_data_device` is the clipboard. A client that copies makes a
//! `wl_data_source`, says which types it can give, and sets it as the
//! selection; a client that may paste is told of it with a `wl_data_offer`
//! it can `receive` from. The data never passes through the compositor:
//! the pasting client hands over a pipe and the copying one writes into it.
//! `zwp_primary_selection_v1` is the same protocol again under another
//! name, for the selection a middle click pastes.
//!
//! Whose selection is the current one is the compositor's to keep. These
//! report what a client set and tell a client what it is to be offered.
//! The drag half of `wl_data_device` is in `drag.rs`, and the clipboard as
//! a clipboard manager sees it is in `control.rs`.

use compositor_protocol::core::{
    self, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
};
use compositor_protocol::primary_selection::{
    self, zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1,
    zwp_primary_selection_offer_v1, zwp_primary_selection_source_v1,
};
use compositor_wire::{Arg, ArgType, Fd, ObjectId};

use crate::client::{Client, Event, Fatal};
use crate::role::Role;

impl Client {
    /// `wl_data_device_manager`: the clipboard's objects.
    ///
    /// The objects are made and nothing is ever offered through them. A
    /// compositor without a clipboard that does not advertise the global at
    /// all is one that toolkits refuse to start on -- which is how this came
    /// to be written -- and one that advertises it and then does not answer
    /// `get_data_device` is worse, because the client only finds out at its
    /// first copy. `wl_data_device.selection` is never sent, which is exactly
    /// what a client sees when no other client has ever copied anything.
    pub(super) fn data_device_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        match opcode {
            wl_data_device_manager::request::CREATE_DATA_SOURCE => {
                if self.make(id, &core::WL_DATA_SOURCE, version, Role::DataSource) {
                    let _previous = self.sources.insert(id, Vec::new());
                }
            }
            wl_data_device_manager::request::GET_DATA_DEVICE => {
                let Some(seat) = args.get(1).and_then(Arg::as_object) else {
                    return;
                };
                if self
                    .objects
                    .get(seat)
                    .is_none_or(|entry| entry.data != Role::Seat)
                {
                    self.fail(Fatal::WrongInterface {
                        object: seat,
                        wanted: "wl_seat",
                    });
                    return;
                }
                if self.make(id, &core::WL_DATA_DEVICE, version, Role::DataDevice) {
                    self.devices.push(id);
                    self.events.push(Event::DataDeviceMade { device: id });
                }
            }
            _ => {}
        }
    }

    /// `wl_data_source`: the types a client is offering, and the object
    /// going away.
    ///
    /// A source is made before it is offered and offered before it is set as
    /// the selection, so the types are collected here and read when
    /// `set_selection` arrives.
    pub(super) fn data_source_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        // `set_actions` is the drag's, and a selection source may send it
        // too; either way it is recorded there.
        if self.drag_request(sender, Role::DataSource, opcode, args) {
            return;
        }
        if opcode != wl_data_source::request::OFFER {
            return;
        }
        let Some(mime) = args.first().and_then(Arg::as_str) else {
            return;
        };
        let offered = self.sources.entry(sender).or_default();
        if !offered.iter().any(|known| known == mime) {
            offered.push(mime.to_owned());
        }
    }

    /// `wl_data_device`: the selection, and the drag this compositor does
    /// not do.
    pub(super) fn data_device_request(&mut self, opcode: u16, args: &[Arg<'_>]) {
        if opcode == wl_data_device::request::START_DRAG {
            self.start_drag(args);
            return;
        }
        if opcode != wl_data_device::request::SET_SELECTION {
            return;
        }
        // A null source clears the selection, which is what a client sends
        // when it no longer owns what it copied.
        let source = args.first().and_then(Arg::as_object);
        let mimes = source
            .and_then(|source| self.sources.get(&source).cloned())
            .unwrap_or_default();
        self.events.push(Event::SelectionSet {
            source: source.filter(|source| !source.is_null()),
            mimes,
        });
    }

    /// `wl_data_offer`: what a client does with the selection it was told
    /// about.
    pub(super) fn data_offer_request(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        // `accept`, `set_actions` and `finish` are the drag's.
        if self.drag_request(sender, Role::DataOffer, opcode, args) {
            return;
        }
        if opcode != wl_data_offer::request::RECEIVE {
            return;
        }
        let (Some(mime), Some(fd)) = (
            args.first().and_then(Arg::as_str),
            args.get(1).and_then(Arg::as_fd),
        ) else {
            return;
        };
        self.events.push(Event::SelectionWanted {
            offer: sender,
            mime: mime.to_owned(),
            fd,
        });
    }

    /// Tell this client what the selection holds.
    ///
    /// The server makes a `wl_data_offer` of its own -- an id out of the
    /// server's half of the space, which is what that half is for -- says
    /// which types it has, and then names it as the selection. That is the
    /// order `wl_data_device`'s description gives, and a client that reads
    /// them in any other order sees an offer for a selection it has not been
    /// told about.
    ///
    /// An empty `mimes` clears the selection, which is what a client sees
    /// when whoever copied has gone.
    pub fn offer_selection(&mut self, mimes: &[String]) {
        if self.devices.is_empty() {
            return;
        }
        // The offer this client had is replaced, as the protocol says a new
        // selection replaces the last.
        let offer = if mimes.is_empty() {
            None
        } else {
            let version = self
                .devices
                .first()
                .and_then(|device| self.objects.get(*device))
                .map_or(3, |entry| entry.version);
            let Ok(offer) = self
                .objects
                .create(&core::WL_DATA_OFFER, version, Role::DataOffer)
            else {
                return;
            };
            Some(offer)
        };
        let devices = self.devices.clone();
        for device in devices {
            if let Some(offer) = offer {
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
            }
            let _ = self.out.write(
                device,
                wl_data_device::event::SELECTION,
                &[ArgType::Object { nullable: true }],
                &[Arg::Object(offer.unwrap_or(ObjectId::NULL))],
            );
        }
        self.offer = offer;
    }

    /// Whether `offer` is the offer this client was last given.
    #[must_use]
    pub fn holds_offer(&self, offer: ObjectId) -> bool {
        self.offer == Some(offer)
    }

    /// Ask this client's source for the selection's data on `fd`.
    ///
    /// The client writes what it copied and closes the descriptor; whoever
    /// pasted reads until end of file. Nothing here touches the data.
    pub fn send_selection(&mut self, source: ObjectId, mime: &str, fd: Fd) {
        let _ = self.out.write(
            source,
            wl_data_source::event::SEND,
            &[ArgType::Str { nullable: false }, ArgType::Fd],
            &[Arg::Str(Some(mime)), Arg::Fd(fd)],
        );
    }

    /// Tell this client's source that it is no longer the selection.
    pub fn cancel_selection(&mut self, source: ObjectId) {
        let _ = self
            .out
            .write(source, wl_data_source::event::CANCELLED, &[], &[]);
    }

    /// Whether this client has a `wl_data_device`, which is what a client
    /// that can paste has.
    #[must_use]
    pub fn has_data_device(&self) -> bool {
        !self.devices.is_empty()
    }

    /// `zwp_primary_selection_device_manager_v1`: `create_source` and
    /// `get_device`.
    pub(super) fn primary_manager(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        match opcode {
            zwp_primary_selection_device_manager_v1::request::CREATE_SOURCE => {
                if self.make(
                    id,
                    &primary_selection::ZWP_PRIMARY_SELECTION_SOURCE_V1,
                    version,
                    Role::PrimarySource,
                ) {
                    let _ = self.primary_sources.insert(id, Vec::new());
                }
            }
            zwp_primary_selection_device_manager_v1::request::GET_DEVICE => {
                if !self.make(
                    id,
                    &primary_selection::ZWP_PRIMARY_SELECTION_DEVICE_V1,
                    version,
                    Role::PrimaryDevice,
                ) {
                    return;
                }
                self.primary_devices.push(id);
                self.events.push(Event::PrimaryDeviceMade { device: id });
            }
            _ => {}
        }
    }

    /// `zwp_primary_selection_source_v1`: the types it is offering.
    pub(super) fn primary_source(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_primary_selection_source_v1::request::OFFER {
            return;
        }
        let Some(mime) = args.first().and_then(Arg::as_str) else {
            return;
        };
        if let Some(mimes) = self.primary_sources.get_mut(&sender) {
            mimes.push(mime.to_owned());
        }
    }

    /// `zwp_primary_selection_device_v1`: `set_selection`.
    ///
    /// The primary selection is what a middle click pastes, and it is set by
    /// *selecting* rather than by asking: no serial is checked here for the
    /// same reason the clipboard's is not.
    pub(super) fn primary_device(&mut self, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_primary_selection_device_v1::request::SET_SELECTION {
            return;
        }
        let source = args.first().and_then(Arg::as_object);
        let mimes = source
            .and_then(|source| self.primary_sources.get(&source))
            .cloned()
            .unwrap_or_default();
        self.events.push(Event::PrimarySet {
            source: source.filter(|source| !source.is_null()),
            mimes,
        });
    }

    /// `zwp_primary_selection_offer_v1`: `receive`, which is a paste.
    pub(super) fn primary_offer(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwp_primary_selection_offer_v1::request::RECEIVE {
            return;
        }
        let (Some(mime), Some(fd)) = (
            args.first().and_then(Arg::as_str),
            args.get(1).and_then(Arg::as_fd),
        ) else {
            return;
        };
        self.events.push(Event::PrimaryWanted {
            offer: sender,
            mime: mime.to_owned(),
            fd,
        });
    }

    /// Tell this client what the primary selection holds.
    ///
    /// The same shape as [`Client::offer_selection`], because the primary
    /// selection is the same protocol with a different name: an offer is
    /// made, its types are sent, and it is then named as the selection.
    pub fn offer_primary(&mut self, mimes: &[String]) {
        if self.primary_devices.is_empty() {
            return;
        }
        let offer = if mimes.is_empty() {
            None
        } else {
            let Ok(offer) = self.objects.create(
                &primary_selection::ZWP_PRIMARY_SELECTION_OFFER_V1,
                1,
                Role::PrimaryOffer,
            ) else {
                return;
            };
            Some(offer)
        };
        let devices = self.primary_devices.clone();
        for device in devices {
            if let Some(offer) = offer {
                let _ = self.out.write(
                    device,
                    zwp_primary_selection_device_v1::event::DATA_OFFER,
                    &[ArgType::NewId],
                    &[Arg::NewId(offer)],
                );
                for mime in mimes {
                    let _ = self.out.write(
                        offer,
                        zwp_primary_selection_offer_v1::event::OFFER,
                        &[ArgType::Str { nullable: false }],
                        &[Arg::Str(Some(mime))],
                    );
                }
            }
            let _ = self.out.write(
                device,
                zwp_primary_selection_device_v1::event::SELECTION,
                &[ArgType::Object { nullable: true }],
                &[Arg::Object(offer.unwrap_or(ObjectId::NULL))],
            );
        }
        self.primary_offer = offer;
    }

    /// Whether `offer` is the primary offer this client was last given.
    #[must_use]
    pub fn holds_primary_offer(&self, offer: ObjectId) -> bool {
        self.primary_offer == Some(offer)
    }

    /// Ask this client's primary source for its data on `fd`.
    pub fn send_primary(&mut self, source: ObjectId, mime: &str, fd: Fd) {
        let _ = self.out.write(
            source,
            zwp_primary_selection_source_v1::event::SEND,
            &[ArgType::Str { nullable: false }, ArgType::Fd],
            &[Arg::Str(Some(mime)), Arg::Fd(fd)],
        );
    }

    /// Tell this client's primary source that it is no longer the
    /// selection.
    pub fn cancel_primary(&mut self, source: ObjectId) {
        let _ = self.out.write(
            source,
            zwp_primary_selection_source_v1::event::CANCELLED,
            &[],
            &[],
        );
    }

    /// Whether this client can paste the primary selection.
    #[must_use]
    pub fn has_primary_device(&self) -> bool {
        !self.primary_devices.is_empty()
    }
}
