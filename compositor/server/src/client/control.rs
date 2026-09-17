//! The clipboard as a manager sees it: `wlr-data-control` and
//! `ext-data-control`.
//!
//! `wl_data_device` gives a client the selection only while it has the
//! keyboard, which is exactly right for an application and exactly wrong for
//! a clipboard manager: `cliphist`, `clipman` and `wl-paste --watch` have no
//! window at all and must be told every time anything is copied. That is
//! what these two protocols are, and they are the same protocol twice --
//! wlroots wrote the first, the `ext` namespace standardised it, and the
//! request and event shapes are identical. So they are one module with a
//! table of the four interfaces each uses.
//!
//! # Both selections
//!
//! A device carries the clipboard and the primary selection apart, and a
//! manager reads or sets either. The compositor's own clipboard already
//! keeps the two apart; this is the same two, offered to a client that has
//! no focus and never will.

use compositor_protocol::Interface;
use compositor_protocol::ext_data_control::{
    ext_data_control_device_v1, ext_data_control_manager_v1, ext_data_control_offer_v1,
    ext_data_control_source_v1,
};
use compositor_protocol::{data_control, ext_data_control};
use compositor_wire::{Arg, ArgType, Fd, ObjectId};

use crate::client::{Client, Event};
use crate::role::Role;

/// Which of the two protocols an object belongs to.
///
/// They have the same requests at the same opcodes and the same events at
/// the same opcodes; only the interfaces a new object is made with differ,
/// which is the whole of what this carries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Flavour {
    /// `zwlr_data_control_*`, which is what `wl-clipboard` binds.
    Wlr,
    /// `ext_data_control_*`, the standardised one.
    Ext,
}

impl Flavour {
    /// The four interfaces this protocol's objects speak.
    const fn interfaces(self) -> (&'static Interface, &'static Interface, &'static Interface) {
        match self {
            Self::Wlr => (
                &data_control::ZWLR_DATA_CONTROL_DEVICE_V1,
                &data_control::ZWLR_DATA_CONTROL_SOURCE_V1,
                &data_control::ZWLR_DATA_CONTROL_OFFER_V1,
            ),
            Self::Ext => (
                &ext_data_control::EXT_DATA_CONTROL_DEVICE_V1,
                &ext_data_control::EXT_DATA_CONTROL_SOURCE_V1,
                &ext_data_control::EXT_DATA_CONTROL_OFFER_V1,
            ),
        }
    }
}

/// One `zwlr_data_control_device_v1` or `ext_data_control_device_v1`.
#[derive(Clone, Debug)]
pub struct Manager {
    /// Which protocol it belongs to.
    pub flavour: Flavour,
    /// The offer it was last given for the clipboard, if it still has one.
    pub offer: Option<ObjectId>,
    /// The same for the primary selection.
    pub primary: Option<ObjectId>,
}

impl Client {
    /// Answer a request to one of this module's objects.
    pub(super) fn control(
        &mut self,
        sender: ObjectId,
        role: Role,
        version: u32,
        opcode: u16,
        args: &[Arg<'_>],
    ) -> bool {
        match role {
            Role::DataControlManager(flavour) => {
                self.control_manager(flavour, version, opcode, args);
            }
            Role::DataControlDevice => self.control_device(sender, opcode, args),
            Role::DataControlSource => self.control_source(sender, opcode, args),
            Role::DataControlOffer => self.control_offer(sender, opcode, args),
            _ => return false,
        }
        true
    }

    /// Drop what one of this module's objects held.
    pub(super) fn forget_control(&mut self, id: ObjectId, role: Role) {
        match role {
            Role::DataControlDevice => {
                let _ = self.control_devices.remove(&id);
            }
            Role::DataControlSource => {
                let _ = self.control_sources.remove(&id);
            }
            _ => {}
        }
    }

    /// `create_data_source` and `get_data_device`.
    fn control_manager(&mut self, flavour: Flavour, version: u32, opcode: u16, args: &[Arg<'_>]) {
        let Some(id) = args.first().and_then(Arg::as_object) else {
            return;
        };
        let (device, source, _) = flavour.interfaces();
        match opcode {
            ext_data_control_manager_v1::request::CREATE_DATA_SOURCE => {
                if self.make(id, source, version, Role::DataControlSource) {
                    let _ = self.control_sources.insert(id, Vec::new());
                }
            }
            ext_data_control_manager_v1::request::GET_DATA_DEVICE
                if self.make(id, device, version, Role::DataControlDevice) =>
            {
                let _ = self.control_devices.insert(
                    id,
                    Manager {
                        flavour,
                        offer: None,
                        primary: None,
                    },
                );
                self.events.push(Event::DataControlBound { device: id });
            }
            _ => {}
        }
    }

    /// `set_selection` and `set_primary_selection`: a manager putting
    /// something on the clipboard, which is how `wl-copy` works with no
    /// window.
    fn control_device(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        let primary = match opcode {
            ext_data_control_device_v1::request::SET_SELECTION => false,
            ext_data_control_device_v1::request::SET_PRIMARY_SELECTION => true,
            _ => return,
        };
        if !self.control_devices.contains_key(&sender) {
            return;
        }
        let source = args
            .first()
            .and_then(Arg::as_object)
            .filter(|id| *id != ObjectId::NULL);
        let mimes = source
            .and_then(|id| self.control_sources.get(&id).cloned())
            .unwrap_or_default();
        self.events.push(Event::DataControlSelection {
            source,
            primary,
            mimes,
        });
    }

    /// `offer`: a type this manager's source can give.
    fn control_source(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != ext_data_control_source_v1::request::OFFER {
            return;
        }
        let Some(mime) = args.first().and_then(Arg::as_str) else {
            return;
        };
        if let Some(mimes) = self.control_sources.get_mut(&sender) {
            mimes.push(mime.to_owned());
        }
    }

    /// `receive`: a manager pasting, which is the whole point of it.
    fn control_offer(&mut self, sender: ObjectId, opcode: u16, args: &[Arg<'_>]) {
        if opcode != ext_data_control_offer_v1::request::RECEIVE {
            return;
        }
        let (Some(mime), Some(fd)) = (
            args.first().and_then(Arg::as_str),
            args.get(1).and_then(Arg::as_fd),
        ) else {
            return;
        };
        // Which selection the offer was for is what the device remembers.
        let primary =
            self.control_devices
                .values()
                .find_map(|held| match (held.offer, held.primary) {
                    (Some(offer), _) if offer == sender => Some(false),
                    (_, Some(offer)) if offer == sender => Some(true),
                    _ => None,
                });
        let Some(primary) = primary else {
            return;
        };
        self.events.push(Event::DataControlPaste {
            offer: sender,
            mime: mime.to_owned(),
            fd,
            primary,
        });
    }

    /// Tell every data-control device what one selection now holds.
    ///
    /// A manager is told whether or not it has the keyboard, which is the
    /// whole difference from `wl_data_device`: it has no window and never
    /// will.
    pub fn control_offer_selection(&mut self, primary: bool, mimes: &[String]) {
        let devices: Vec<(ObjectId, Flavour, u32)> = self
            .control_devices
            .iter()
            .map(|(id, held)| {
                let version = self.objects.get(*id).map_or(1, |entry| entry.version);
                (*id, held.flavour, version)
            })
            .collect();
        for (device, flavour, version) in devices {
            let (_, _, offer_interface) = flavour.interfaces();
            let offer = if mimes.is_empty() {
                None
            } else {
                // At the interface's own version where that is lower than
                // the device's: wlroots' offer stops at 1 and its device
                // goes to 2, and an object made above what its interface
                // offers is no object at all.
                self.objects
                    .create(
                        offer_interface,
                        version.min(offer_interface.version),
                        Role::DataControlOffer,
                    )
                    .ok()
            };
            if let Some(offer) = offer {
                let _ = self.out.write(
                    device,
                    ext_data_control_device_v1::event::DATA_OFFER,
                    &[ArgType::NewId],
                    &[Arg::NewId(offer)],
                );
                for mime in mimes {
                    let _ = self.out.write(
                        offer,
                        ext_data_control_offer_v1::event::OFFER,
                        &[ArgType::Str { nullable: false }],
                        &[Arg::Str(Some(mime))],
                    );
                }
            }
            let event = if primary {
                ext_data_control_device_v1::event::PRIMARY_SELECTION
            } else {
                ext_data_control_device_v1::event::SELECTION
            };
            let _ = self.out.write(
                device,
                event,
                &[ArgType::Object { nullable: true }],
                &[Arg::Object(offer.unwrap_or(ObjectId::NULL))],
            );
            if let Some(held) = self.control_devices.get_mut(&device) {
                if primary {
                    held.primary = offer;
                } else {
                    held.offer = offer;
                }
            }
        }
    }

    /// Ask this client's data-control source for what it copied.
    pub fn control_send(&mut self, source: ObjectId, mime: &str, fd: Fd) {
        let _ = self.out.write(
            source,
            ext_data_control_source_v1::event::SEND,
            &[ArgType::Str { nullable: false }, ArgType::Fd],
            &[Arg::Str(Some(mime)), Arg::Fd(fd)],
        );
    }

    /// Tell a data-control source that something else is the selection now.
    pub fn control_cancel(&mut self, source: ObjectId) {
        let _ = self.out.write(
            source,
            ext_data_control_source_v1::event::CANCELLED,
            &[],
            &[],
        );
    }

    /// The types a data-control source of this client offered.
    #[must_use]
    pub fn control_mimes(&self, source: ObjectId) -> Option<&[String]> {
        self.control_sources.get(&source).map(Vec::as_slice)
    }

    /// Whether `offer` is an offer one of this client's data-control
    /// devices was last given, for either selection.
    #[must_use]
    pub fn holds_control_offer(&self, offer: ObjectId) -> bool {
        self.control_devices
            .values()
            .any(|held| held.offer == Some(offer) || held.primary == Some(offer))
    }

    /// Whether this client is watching the clipboard without a window.
    #[must_use]
    pub fn watches_clipboard(&self) -> bool {
        !self.control_devices.is_empty()
    }
}
