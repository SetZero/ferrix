//! `endian.h`: host, big-endian and little-endian integer conversions.
//!
//! The header also defines each conversion as a macro so constant expressions
//! can be folded. POSIX requires a callable function behind every such macro;
//! these definitions provide the symbols used when a program suppresses the
//! macro with parentheses, takes its address, or uses `#undef`.

macro_rules! conversions {
    ($(
        $(#[$meta:meta])*
        $name:ident($ty:ty) => $conversion:path;
    )*) => {
        $(
            $(#[$meta])*
            #[cfg_attr(not(test), unsafe(no_mangle))]
            pub extern "C" fn $name(value: $ty) -> $ty {
                $conversion(value)
            }
        )*
    };
}

conversions! {
    /// Converts a host-order 16-bit value to big-endian order.
    htobe16(u16) => u16::to_be;
    /// Converts a host-order 32-bit value to big-endian order.
    htobe32(u32) => u32::to_be;
    /// Converts a host-order 64-bit value to big-endian order.
    htobe64(u64) => u64::to_be;
    /// Converts a host-order 16-bit value to little-endian order.
    htole16(u16) => u16::to_le;
    /// Converts a host-order 32-bit value to little-endian order.
    htole32(u32) => u32::to_le;
    /// Converts a host-order 64-bit value to little-endian order.
    htole64(u64) => u64::to_le;
    /// Converts a big-endian 16-bit value to host order.
    be16toh(u16) => u16::from_be;
    /// Converts a big-endian 32-bit value to host order.
    be32toh(u32) => u32::from_be;
    /// Converts a big-endian 64-bit value to host order.
    be64toh(u64) => u64::from_be;
    /// Converts a little-endian 16-bit value to host order.
    le16toh(u16) => u16::from_le;
    /// Converts a little-endian 32-bit value to host order.
    le32toh(u32) => u32::from_le;
    /// Converts a little-endian 64-bit value to host order.
    le64toh(u64) => u64::from_le;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_width_round_trips_through_either_byte_order() {
        assert_eq!(be16toh(htobe16(0x1234)), 0x1234);
        assert_eq!(be32toh(htobe32(0x1234_5678)), 0x1234_5678);
        assert_eq!(
            be64toh(htobe64(0x0123_4567_89ab_cdef)),
            0x0123_4567_89ab_cdef
        );
        assert_eq!(le16toh(htole16(0x1234)), 0x1234);
        assert_eq!(le32toh(htole32(0x1234_5678)), 0x1234_5678);
        assert_eq!(
            le64toh(htole64(0x0123_4567_89ab_cdef)),
            0x0123_4567_89ab_cdef
        );
    }
}
