//! Structures of fixed layout, read from and written into the bytes an
//! ioctl's argument points at.
//!
//! [`crate::drm`] and [`crate::input`] define their structures with
//! `layout!`, giving each field's type and `offsetof` as the probe printed
//! it, so the tests can compare every offset with the headers and the reads
//! and writes cannot disagree with the offsets. Everything is little-endian
//! and answers `None` rather than panicking when the buffer is short.
//!
//! A structure holding a C `long`, `unsigned long` or pointer has a layout
//! per width. [`crate::sound`] defines those with `wide_layout!`, which takes
//! two offsets and two sizes, and a field kind of `ulong` or `long` for the
//! word whose size is the width's: four bytes on ARMv7-A, eight on 64-bit.

use crate::socket::Width;
use crate::wire;

/// A fixed-size field of a structure: read from and written into bytes at
/// an offset, little-endian.
pub trait Field: Sized + Copy {
    /// Its size in bytes.
    const SIZE: usize;
    /// All zeroes.
    const ZERO: Self;
    /// Read it from `bytes` at `at`.
    fn get(bytes: &[u8], at: usize) -> Option<Self>;
    /// Write it into `out` at `at`.
    fn put(self, out: &mut [u8], at: usize) -> Option<()>;
}

macro_rules! integer_field {
    ($($type:ty),*) => {$(
        impl Field for $type {
            const SIZE: usize = size_of::<$type>();
            const ZERO: Self = 0;
            fn get(bytes: &[u8], at: usize) -> Option<Self> {
                wire::array(bytes, at).map(<$type>::from_le_bytes)
            }
            fn put(self, out: &mut [u8], at: usize) -> Option<()> {
                wire::put(out, at, &self.to_le_bytes())
            }
        }
    )*};
}

integer_field!(u8, u16, u32, u64, i32, i64);

impl<T: Field, const N: usize> Field for [T; N] {
    const SIZE: usize = T::SIZE * N;
    const ZERO: Self = [T::ZERO; N];
    fn get(bytes: &[u8], at: usize) -> Option<Self> {
        let mut value = Self::ZERO;
        for (index, slot) in value.iter_mut().enumerate() {
            *slot = T::get(bytes, at.checked_add(index.checked_mul(T::SIZE)?)?)?;
        }
        Some(value)
    }
    fn put(self, out: &mut [u8], at: usize) -> Option<()> {
        for (index, item) in self.into_iter().enumerate() {
            item.put(out, at.checked_add(index.checked_mul(T::SIZE)?)?)?;
        }
        Some(())
    }
}

/// A structure with one layout at every width, whose `sizeof` is its
/// [`Field::SIZE`].
pub trait Layout: Field {
    /// Its name in the UAPI headers, without `struct`.
    const C_NAME: &'static str;
    /// Every field's name in the headers and its `offsetof`, in order.
    const FIELDS: &'static [(&'static str, usize)];
    /// Read it from the start of `bytes`.
    fn read(bytes: &[u8]) -> Option<Self>;
    /// Write it into the start of `out`.
    fn write(&self, out: &mut [u8]) -> Option<()>;
}

/// Define a structure, its [`Layout`] and its [`Field`] impl from its fields'
/// types and offsets. A field named `r#type` is `type` in C.
macro_rules! layout {
    (
        $(#[$meta:meta])*
        $name:ident = $c_name:literal, $size:literal {
            $($(#[$field_meta:meta])* $field:ident : $type:ty = $offset:literal / $c_field:literal,)*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name {
            $($(#[$field_meta])* pub $field: $type,)*
        }

        impl $crate::layout::Layout for $name {
            const C_NAME: &'static str = $c_name;
            const FIELDS: &'static [(&'static str, usize)] = &[$(($c_field, $offset),)*];
            fn read(bytes: &[u8]) -> Option<Self> {
                if bytes.len() < $size {
                    return None;
                }
                Some(Self { $($field: <$type as $crate::layout::Field>::get(bytes, $offset)?,)* })
            }
            fn write(&self, out: &mut [u8]) -> Option<()> {
                if out.len() < $size {
                    return None;
                }
                $($crate::layout::Field::put(self.$field, out, $offset)?;)*
                Some(())
            }
        }

        impl $crate::layout::Field for $name {
            const SIZE: usize = $size;
            const ZERO: Self = Self { $($field: <$type as $crate::layout::Field>::ZERO,)* };
            fn get(bytes: &[u8], at: usize) -> Option<Self> {
                <Self as $crate::layout::Layout>::read(bytes.get(at..)?)
            }
            fn put(self, out: &mut [u8], at: usize) -> Option<()> {
                <Self as $crate::layout::Layout>::write(&self, out.get_mut(at..)?)
            }
        }
    };
}

pub(crate) use layout;

/// A field of a structure with a layout per width: anything of one layout at
/// both widths, or a structure `wide_layout!` defined.
pub trait WideField: Sized + Copy {
    /// All zeroes.
    const WIDE_ZERO: Self;
    /// Its size in bytes at `width`.
    fn wide_size(width: Width) -> usize;
    /// Whether every word in it fits `width`, so that writing it cuts
    /// nothing.
    fn fits(&self, width: Width) -> bool;
    /// Read it from `bytes` at `at`, laid out for `width`.
    fn wide_get(width: Width, bytes: &[u8], at: usize) -> Option<Self>;
    /// Write it into `out` at `at`, laid out for `width`.
    fn wide_put(self, width: Width, out: &mut [u8], at: usize) -> Option<()>;
}

impl<T: Field> WideField for T {
    const WIDE_ZERO: Self = <T as Field>::ZERO;
    fn wide_size(_: Width) -> usize {
        T::SIZE
    }
    fn fits(&self, _: Width) -> bool {
        true
    }
    fn wide_get(_: Width, bytes: &[u8], at: usize) -> Option<Self> {
        T::get(bytes, at)
    }
    fn wide_put(self, _: Width, out: &mut [u8], at: usize) -> Option<()> {
        self.put(out, at)
    }
}

/// An `unsigned long` at `at`: four bytes widened, or eight.
pub(crate) fn get_ulong(width: Width, bytes: &[u8], at: usize) -> Option<u64> {
    width.word(bytes, at)
}

/// Write an `unsigned long`; `None`, with nothing written, if it does not fit.
pub(crate) fn put_ulong(width: Width, out: &mut [u8], at: usize, value: u64) -> Option<()> {
    width.put_word(out, at, value)
}

/// Whether `value` fits an `unsigned long` at `width`.
pub(crate) fn fits_ulong(width: Width, value: u64) -> bool {
    matches!(width, Width::Bits64) || u32::try_from(value).is_ok()
}

/// A `long` at `at`: four bytes sign-extended, or eight.
pub(crate) fn get_long(width: Width, bytes: &[u8], at: usize) -> Option<i64> {
    match width {
        Width::Bits32 => i32::get(bytes, at).map(i64::from),
        Width::Bits64 => i64::get(bytes, at),
    }
}

/// Write a `long`; `None`, with nothing written, if it does not fit.
pub(crate) fn put_long(width: Width, out: &mut [u8], at: usize, value: i64) -> Option<()> {
    match width {
        Width::Bits32 => i32::try_from(value).ok()?.put(out, at),
        Width::Bits64 => value.put(out, at),
    }
}

/// Whether `value` fits a `long` at `width`.
pub(crate) fn fits_long(width: Width, value: i64) -> bool {
    matches!(width, Width::Bits64) || i32::try_from(value).is_ok()
}

/// The Rust type of a `wide_layout!` field kind.
macro_rules! wide_type {
    (ulong) => {
        u64
    };
    (long) => {
        i64
    };
    ($type:ty) => {
        $type
    };
}

/// Read a field of a `wide_layout!` kind.
macro_rules! wide_get {
    (ulong, $width:expr, $bytes:expr, $at:expr) => {
        $crate::layout::get_ulong($width, $bytes, $at)
    };
    (long, $width:expr, $bytes:expr, $at:expr) => {
        $crate::layout::get_long($width, $bytes, $at)
    };
    ($type:ty, $width:expr, $bytes:expr, $at:expr) => {
        <$type as $crate::layout::WideField>::wide_get($width, $bytes, $at)
    };
}

/// Write a field of a `wide_layout!` kind.
macro_rules! wide_put {
    (ulong, $width:expr, $out:expr, $at:expr, $value:expr) => {
        $crate::layout::put_ulong($width, $out, $at, $value)
    };
    (long, $width:expr, $out:expr, $at:expr, $value:expr) => {
        $crate::layout::put_long($width, $out, $at, $value)
    };
    ($type:ty, $width:expr, $out:expr, $at:expr, $value:expr) => {
        <$type as $crate::layout::WideField>::wide_put($value, $width, $out, $at)
    };
}

/// Whether a field of a `wide_layout!` kind fits `width`.
macro_rules! wide_fits {
    (ulong, $width:expr, $value:expr) => {
        $crate::layout::fits_ulong($width, $value)
    };
    (long, $width:expr, $value:expr) => {
        $crate::layout::fits_long($width, $value)
    };
    ($type:ty, $width:expr, $value:expr) => {
        <$type as $crate::layout::WideField>::fits(&$value, $width)
    };
}

/// Define a structure with a layout per width: each field's kind, its
/// `offsetof` at 64 bits and at 32, and its C name, and the structure's
/// `sizeof` at each. A kind is `ulong`, `long`, or a type that is a
/// [`WideField`]. Bytes between fields are left as they were by `write`, so
/// an answer is written into zeroes.
macro_rules! wide_layout {
    (
        $(#[$meta:meta])*
        $name:ident = $c_name:literal, $size64:literal / $size32:literal {
            $($(#[$field_meta:meta])* $field:ident : $kind:tt = $at64:literal / $at32:literal / $c_field:literal,)*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name {
            $($(#[$field_meta])* pub $field: $crate::layout::wide_type!($kind),)*
        }

        impl $name {
            /// Its name in the UAPI headers, without `struct`.
            pub const C_NAME: &'static str = $c_name;

            /// Its `sizeof` at `width`.
            #[must_use]
            pub const fn size(width: $crate::socket::Width) -> usize {
                match width {
                    $crate::socket::Width::Bits64 => $size64,
                    $crate::socket::Width::Bits32 => $size32,
                }
            }

            /// Every field's name in the headers and its `offsetof` at
            /// `width`, in order.
            #[must_use]
            pub const fn fields(width: $crate::socket::Width) -> &'static [(&'static str, usize)] {
                match width {
                    $crate::socket::Width::Bits64 => &[$(($c_field, $at64),)*],
                    $crate::socket::Width::Bits32 => &[$(($c_field, $at32),)*],
                }
            }

            /// Read it from the start of `bytes`, laid out for `width`.
            #[must_use]
            pub fn read(width: $crate::socket::Width, bytes: &[u8]) -> Option<Self> {
                <Self as $crate::layout::WideField>::wide_get(width, bytes, 0)
            }

            /// Write it into the start of `out`, laid out for `width`.
            /// `None`, with nothing written, if `out` is short or a word does
            /// not fit a 32-bit one.
            pub fn write(&self, width: $crate::socket::Width, out: &mut [u8]) -> Option<()> {
                <Self as $crate::layout::WideField>::wide_put(*self, width, out, 0)
            }
        }

        impl $crate::layout::WideField for $name {
            const WIDE_ZERO: Self = Self {
                $($field: <$crate::layout::wide_type!($kind) as $crate::layout::WideField>::WIDE_ZERO,)*
            };
            fn wide_size(width: $crate::socket::Width) -> usize {
                Self::size(width)
            }
            fn fits(&self, width: $crate::socket::Width) -> bool {
                true $(&& $crate::layout::wide_fits!($kind, width, self.$field))*
            }
            fn wide_get(width: $crate::socket::Width, bytes: &[u8], at: usize) -> Option<Self> {
                let bytes = bytes.get(at..)?;
                if bytes.len() < Self::size(width) {
                    return None;
                }
                Some(Self {
                    $($field: $crate::layout::wide_get!(
                        $kind,
                        width,
                        bytes,
                        match width {
                            $crate::socket::Width::Bits64 => $at64,
                            $crate::socket::Width::Bits32 => $at32,
                        }
                    )?,)*
                })
            }
            fn wide_put(self, width: $crate::socket::Width, out: &mut [u8], at: usize) -> Option<()> {
                let out = out.get_mut(at..)?;
                if out.len() < Self::size(width) || !self.fits(width) {
                    return None;
                }
                $($crate::layout::wide_put!(
                    $kind,
                    width,
                    out,
                    match width {
                        $crate::socket::Width::Bits64 => $at64,
                        $crate::socket::Width::Bits32 => $at32,
                    },
                    self.$field
                )?;)*
                Some(())
            }
        }
    };
}

pub(crate) use {wide_fits, wide_get, wide_layout, wide_put, wide_type};
