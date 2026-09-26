//! `sys/sysmacros.h`: a device number's major and minor halves.
//!
//! C programs use the `major`, `minor` and `makedev` macros, which glibc's
//! header expands to these three functions; a program compiled against it
//! imports them by name. The encoding is Linux's 64-bit `dev_t`: the low
//! twelve bits of the major number at bits 8 to 19 and the rest from bit 32,
//! the low eight bits of the minor number at bits 0 to 7 and the rest from
//! bit 12. That is what `glibc`'s `bits/sysmacros.h` and musl's both write.

use core::ffi::c_uint;

/// The major half of `dev`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gnu_dev_major(dev: u64) -> c_uint {
    (((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff)) as c_uint
}

/// The minor half of `dev`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gnu_dev_minor(dev: u64) -> c_uint {
    ((dev & 0xff) | ((dev >> 12) & !0xff)) as c_uint
}

/// The device number with these halves.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gnu_dev_makedev(major: c_uint, minor: c_uint) -> u64 {
    let (major, minor) = (u64::from(major), u64::from(minor));
    (minor & 0xff) | ((major & 0xfff) << 8) | ((minor & !0xff) << 12) | ((major & !0xfff) << 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_round_trip_across_both_fields() {
        for (major, minor) in [
            (0, 0),
            (8, 1),
            (0xfff, 0xff),
            (0x1234, 0x5678),
            (u32::MAX, u32::MAX),
        ] {
            let dev = gnu_dev_makedev(major, minor);
            assert_eq!((gnu_dev_major(dev), gnu_dev_minor(dev)), (major, minor));
        }
        // `/dev/sda1`, as `ls -l` shows it and as the kernel encodes it.
        assert_eq!(gnu_dev_makedev(8, 1), 0x801);
    }
}
