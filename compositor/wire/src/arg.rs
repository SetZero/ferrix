//! The argument types the wire carries, and the values of them.

use crate::Fixed;
use crate::objects::ObjectId;

/// A descriptor as this crate sees it: a number and nothing else. The server
/// owns the descriptor itself; the wire only says where in the message it
/// goes and which of the ones that arrived is next.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Fd(pub i32);

/// What an argument is, from a `<arg type="...">` in the protocol.
///
/// A reader is given these because the wire format is untyped: the bytes
/// alone do not say how far the next argument starts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ArgType {
    /// `int`: one signed word.
    Int,
    /// `uint`: one unsigned word.
    Uint,
    /// `fixed`: one word of [`Fixed`].
    Fixed,
    /// `string`. `nullable` is the protocol's `allow-null`; a null string is
    /// a length of zero, which is not an empty string.
    Str {
        /// Whether a null is allowed here.
        nullable: bool,
    },
    /// `object`: one word of object id, `0` where `nullable`.
    Object {
        /// Whether a null is allowed here.
        nullable: bool,
    },
    /// `new_id` whose interface the protocol names: one word of object id.
    NewId,
    /// `new_id` with no interface named, which is only `wl_registry.bind`:
    /// an interface name, a version and an id.
    AnyNewId,
    /// `array`: a length and that many bytes.
    Array,
    /// `fd`: nothing in the stream, one descriptor beside it.
    Fd,
}

/// The types of one message's arguments, in order: a `<request>` or
/// `<event>`'s `<arg>` list.
pub type Signature = &'static [ArgType];

/// One argument's value.
///
/// Borrowed from the buffer it was read out of, so reading a message copies
/// no string and no array.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arg<'a> {
    /// `int`.
    Int(i32),
    /// `uint`.
    Uint(u32),
    /// `fixed`.
    Fixed(Fixed),
    /// `string`, or `None` for the null one.
    Str(Option<&'a str>),
    /// `object`, [`ObjectId::NULL`] for the null one.
    Object(ObjectId),
    /// `new_id` of the interface the protocol named.
    NewId(ObjectId),
    /// `new_id` of an interface the client chose: `wl_registry.bind`.
    AnyNewId {
        /// The interface the client asked to bind.
        interface: &'a str,
        /// The version it asked for.
        version: u32,
        /// The id it gave the new object.
        id: ObjectId,
    },
    /// `array`.
    Array(&'a [u8]),
    /// `fd`.
    Fd(Fd),
}

impl Arg<'_> {
    /// The type this value is of, so a message read back can be checked
    /// against the signature it was read with.
    #[must_use]
    pub const fn kind(&self) -> ArgType {
        match self {
            Self::Int(_) => ArgType::Int,
            Self::Uint(_) => ArgType::Uint,
            Self::Fixed(_) => ArgType::Fixed,
            Self::Str(value) => ArgType::Str {
                nullable: value.is_none(),
            },
            Self::Object(id) => ArgType::Object {
                nullable: id.is_null(),
            },
            Self::NewId(_) => ArgType::NewId,
            Self::AnyNewId { .. } => ArgType::AnyNewId,
            Self::Array(_) => ArgType::Array,
            Self::Fd(_) => ArgType::Fd,
        }
    }

    /// The `uint` this is, if it is one.
    #[must_use]
    pub const fn as_uint(&self) -> Option<u32> {
        match self {
            Self::Uint(value) => Some(*value),
            _ => None,
        }
    }

    /// The `int` this is, if it is one.
    #[must_use]
    pub const fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// The `fixed` this is, if it is one.
    #[must_use]
    pub const fn as_fixed(&self) -> Option<Fixed> {
        match self {
            Self::Fixed(value) => Some(*value),
            _ => None,
        }
    }

    /// The object id this names, whether as `object` or as either `new_id`.
    #[must_use]
    pub const fn as_object(&self) -> Option<ObjectId> {
        match self {
            Self::Object(id) | Self::NewId(id) | Self::AnyNewId { id, .. } => Some(*id),
            _ => None,
        }
    }

    /// The string this is, if it is one and is not null.
    #[must_use]
    pub const fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(value) => *value,
            Self::AnyNewId { interface, .. } => Some(interface),
            _ => None,
        }
    }

    /// The descriptor this is, if it is one.
    #[must_use]
    pub const fn as_fd(&self) -> Option<Fd> {
        match self {
            Self::Fd(fd) => Some(*fd),
            _ => None,
        }
    }

    /// The bytes of the `array` this is, if it is one.
    ///
    /// An array is bytes on the wire and whatever the protocol says
    /// otherwise: `zwlr_foreign_toplevel_handle_v1.state` is a list of
    /// 32-bit values in the machine's own order, and `wl_keyboard.enter` a
    /// list of keycodes the same way.
    #[must_use]
    pub const fn as_array(&self) -> Option<&[u8]> {
        match self {
            Self::Array(bytes) => Some(bytes),
            _ => None,
        }
    }
}
