//! Owned wire arguments, for the objects a program makes itself.

use compositor_wire::{Arg, Fixed, ObjectId};

/// One argument of an [`crate::Event::Object`], owned so it outlives the
/// receive buffer, or of a [`crate::Client::request`].
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `int`.
    Int(i32),
    /// `uint`.
    Uint(u32),
    /// `fixed`, as a float.
    Fixed(f64),
    /// `string`; `None` for the null one.
    Str(Option<String>),
    /// `object`, or a `new_id` the compositor made.
    Object(ObjectId),
    /// `new_id` the program made: the id [`crate::Client::new_object`] gave.
    NewId(ObjectId),
    /// `array`.
    Array(Vec<u8>),
    /// `fd`: the descriptor, which the program now owns and must close.
    Fd(i32),
}

impl Value {
    /// The `uint`, if it is one.
    #[must_use]
    pub const fn as_uint(&self) -> Option<u32> {
        match self {
            Self::Uint(value) => Some(*value),
            _ => None,
        }
    }

    /// The `int`, if it is one.
    #[must_use]
    pub const fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// The string, if it is a non-null one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(Some(text)) => Some(text),
            _ => None,
        }
    }

    /// The object id, if it is one.
    #[must_use]
    pub const fn as_object(&self) -> Option<ObjectId> {
        match self {
            Self::Object(id) | Self::NewId(id) => Some(*id),
            _ => None,
        }
    }

    /// Owned from a borrowed argument.
    #[must_use]
    pub fn from_arg(arg: &Arg<'_>) -> Self {
        match arg {
            Arg::Int(value) => Self::Int(*value),
            Arg::Uint(value) => Self::Uint(*value),
            Arg::Fixed(value) => Self::Fixed(value.to_f64()),
            Arg::Str(text) => Self::Str(text.map(str::to_owned)),
            Arg::Object(id) | Arg::AnyNewId { id, .. } => Self::Object(*id),
            Arg::NewId(id) => Self::NewId(*id),
            Arg::Array(bytes) => Self::Array(bytes.to_vec()),
            Arg::Fd(fd) => Self::Fd(fd.0),
        }
    }

    /// Borrowed, to write.
    pub(crate) fn to_arg(&self) -> Arg<'_> {
        match self {
            Self::Int(value) => Arg::Int(*value),
            Self::Uint(value) => Arg::Uint(*value),
            Self::Fixed(value) => Arg::Fixed(Fixed::from_f64(*value)),
            Self::Str(text) => Arg::Str(text.as_deref()),
            Self::Object(id) => Arg::Object(*id),
            Self::NewId(id) => Arg::NewId(*id),
            Self::Array(bytes) => Arg::Array(bytes),
            Self::Fd(fd) => Arg::Fd(compositor_wire::Fd(*fd)),
        }
    }
}
