//! Object ids and one client's map of them.

use std::collections::BTreeMap;

use crate::interface::Interface;

/// An object id as the wire carries it.
///
/// Zero is the null object, which some arguments allow. One is always
/// `wl_display`, which every connection starts with and which is never
/// created or destroyed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Debug, Hash)]
pub struct ObjectId(pub u32);

impl ObjectId {
    /// The null object: `0`.
    pub const NULL: Self = Self(0);

    /// `wl_display`, the object a connection starts with.
    pub const DISPLAY: Self = Self(1);

    /// The first id a server may name, as `wayland-server-core.h`'s
    /// `WL_SERVER_ID_START`. A client names its objects below this and the
    /// server at or above it, so neither can pick an id the other is using
    /// and no handshake is needed to agree on one.
    pub const SERVER_BASE: u32 = 0xFF00_0000;

    /// Whether this is the null object.
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }

    /// Whether the client is allowed to have made this id.
    #[must_use]
    pub const fn is_client(self) -> bool {
        self.0 != 0 && self.0 < Self::SERVER_BASE
    }

    /// Whether the server is allowed to have made this id.
    #[must_use]
    pub const fn is_server(self) -> bool {
        self.0 >= Self::SERVER_BASE
    }
}

/// What one live object is: what it speaks and at which version.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    /// The interface the object was created as.
    pub interface: &'static Interface,
    /// The version it was bound at, never above the interface's own.
    pub version: u32,
}

/// Why an object could not be made, found or dropped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjectError {
    /// The id is zero, which names no object.
    Null,
    /// A client used an id in the server's range, or the other way round.
    /// `wl_display.error` answers this with `invalid_object`.
    WrongHalf(ObjectId),
    /// The id is already a live object.
    InUse(ObjectId),
    /// No object of this id is live.
    Unknown(ObjectId),
    /// A bind asked for a version above the interface's.
    Version {
        /// What was asked for.
        asked: u32,
        /// The most the interface offers.
        offered: u32,
    },
    /// Every id in the server's range is taken, which takes an object leak
    /// of sixteen million to reach.
    Exhausted,
}

/// One client's live objects.
///
/// A `BTreeMap` rather than libwayland's two arrays: a client that makes and
/// drops objects in a loop leaves libwayland's client-side array as long as
/// the highest id it ever held, and ordered iteration is what a test needs
/// to say what a connection is holding.
#[derive(Clone, Debug, Default)]
pub struct Objects {
    live: BTreeMap<u32, Entry>,
    /// The next server id to try; ids are not reused until it wraps, so a
    /// stale reference from a client names nothing rather than something new.
    next_server: u32,
}

impl Objects {
    /// An empty map. `wl_display` is not in it: the caller adds it with the
    /// interface table it is using.
    #[must_use]
    pub fn new() -> Self {
        Self {
            live: BTreeMap::new(),
            next_server: ObjectId::SERVER_BASE,
        }
    }

    /// How many objects are live.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether no object is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// The object `id` names, if it is live.
    #[must_use]
    pub fn get(&self, id: ObjectId) -> Option<&Entry> {
        self.live.get(&id.0)
    }

    /// Whether `id` is live.
    #[must_use]
    pub fn contains(&self, id: ObjectId) -> bool {
        self.live.contains_key(&id.0)
    }

    /// Make `id` an object of `interface` at `version`, as a client's
    /// `new_id` argument asks.
    ///
    /// The id must be the client's to give and not already in use, and the
    /// version not above what the interface offers.
    pub fn insert(
        &mut self,
        id: ObjectId,
        interface: &'static Interface,
        version: u32,
    ) -> Result<(), ObjectError> {
        if id.is_null() {
            return Err(ObjectError::Null);
        }
        if !id.is_client() {
            return Err(ObjectError::WrongHalf(id));
        }
        self.put(id, interface, version)
    }

    /// Make an object of the server's own, as `wl_data_device.data_offer`
    /// and `wl_seat`'s children are: the server picks the id.
    pub fn create(
        &mut self,
        interface: &'static Interface,
        version: u32,
    ) -> Result<ObjectId, ObjectError> {
        let start = self.next_server;
        loop {
            let id = ObjectId(self.next_server);
            self.next_server = match self.next_server.checked_add(1) {
                Some(next) => next,
                None => ObjectId::SERVER_BASE,
            };
            if !self.live.contains_key(&id.0) {
                self.put(id, interface, version)?;
                return Ok(id);
            }
            if self.next_server == start {
                return Err(ObjectError::Exhausted);
            }
        }
    }

    /// Drop `id`, giving back what it was.
    pub fn remove(&mut self, id: ObjectId) -> Result<Entry, ObjectError> {
        self.live.remove(&id.0).ok_or(ObjectError::Unknown(id))
    }

    /// Every live id with what it is, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (ObjectId, &Entry)> {
        self.live.iter().map(|(id, entry)| (ObjectId(*id), entry))
    }

    fn put(
        &mut self,
        id: ObjectId,
        interface: &'static Interface,
        version: u32,
    ) -> Result<(), ObjectError> {
        if version == 0 || version > interface.version {
            return Err(ObjectError::Version {
                asked: version,
                offered: interface.version,
            });
        }
        if self.live.contains_key(&id.0) {
            return Err(ObjectError::InUse(id));
        }
        let previous = self.live.insert(id.0, Entry { interface, version });
        debug_assert!(previous.is_none(), "checked just above");
        Ok(())
    }
}
