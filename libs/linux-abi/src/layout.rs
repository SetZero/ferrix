//! Structures of fixed layout, read from and written into the bytes an
//! ioctl's argument points at.
//!
//! [`crate::drm`] and [`crate::input`] define their structures with
//! `layout!`, giving each field's type and `offsetof` as the probe printed
//! it, so the tests can compare every offset with the headers and the reads
//! and writes cannot disagree with the offsets. Everything is little-endian
//! and answers `None` rather than panicking when the buffer is short.

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

integer_field!(u8, u16, u32, u64, i32);

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
