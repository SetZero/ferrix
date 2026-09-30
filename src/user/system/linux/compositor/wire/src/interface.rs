//! What an interface is: the tables the crate above builds from the protocol
//! XML, which are what tells a reader how to read the next message.

use crate::arg::Signature;

/// One request or event of an interface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Method {
    /// Its name in the protocol, for diagnostics and for `hyprctl`-shaped
    /// tracing.
    pub name: &'static str,
    /// The interface version it appeared in, as the protocol's `since`. A
    /// client bound at a lower version may not send it.
    pub since: u32,
    /// Whether the protocol marks it `type="destructor"`: the object is gone
    /// once it has been handled, and the server sends `wl_display.delete_id`
    /// so the client may use the number again.
    pub destructor: bool,
    /// Its arguments, in order.
    pub signature: Signature,
}

/// An interface: what an object of it can be sent and what it can send back.
///
/// Requests and events are indexed by opcode, which is their position in the
/// protocol's XML, so a table's order is the protocol's order and nothing
/// may be inserted in the middle of one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interface {
    /// The name on the wire, as `wl_registry.bind` and `wl_registry.global`
    /// carry it.
    pub name: &'static str,
    /// The highest version this implementation offers.
    pub version: u32,
    /// Requests, client to server, by opcode.
    pub requests: &'static [Method],
    /// Events, server to client, by opcode.
    pub events: &'static [Method],
}

impl Interface {
    /// The request at `opcode`, if the interface has one.
    #[must_use]
    pub fn request(&self, opcode: u16) -> Option<&'static Method> {
        self.requests.get(usize::from(opcode))
    }

    /// The event at `opcode`, if the interface has one.
    #[must_use]
    pub fn event(&self, opcode: u16) -> Option<&'static Method> {
        self.events.get(usize::from(opcode))
    }

    /// The opcode of the request named `name`, which is how the server's own
    /// code and its tests name one without writing a number down twice.
    #[must_use]
    pub fn request_opcode(&self, name: &str) -> Option<u16> {
        opcode_of(self.requests, name)
    }

    /// The opcode of the event named `name`.
    #[must_use]
    pub fn event_opcode(&self, name: &str) -> Option<u16> {
        opcode_of(self.events, name)
    }
}

fn opcode_of(methods: &'static [Method], name: &str) -> Option<u16> {
    let index = methods.iter().position(|method| method.name == name)?;
    u16::try_from(index).ok()
}
