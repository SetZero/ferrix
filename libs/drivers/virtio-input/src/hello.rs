//! From the device's configuration queries to HELLO.

use ferrix_inputctl::message::{
    AXES, AxisRange, Bitmaps, DeviceId, Hello, Text, VERSION, bit, bits_past,
};
use ferrix_linux_abi::input::{ABS_CNT, EV_CNT, INPUT_PROP_CNT};
use ferrix_virtio::input::{self, Answer, ConfigSelect, InputError};

/// Set bit `index` of a little-endian bitmap, if it has one.
fn set(bits: &mut [u8], index: u16) {
    if let Some(byte) = bits.get_mut(usize::from(index / 8)) {
        *byte |= 1 << (index % 8);
    }
}

/// Copy the bits of `answer` below `count` into `out`; whether any at or past
/// `count` was set.
fn copy_bits(answer: &Answer, count: u16, out: &mut [u8]) -> bool {
    for index in 0..count {
        if answer.bit(index) {
            set(out, index);
        }
    }
    bits_past(answer.as_bytes(), count)
}

/// A name or serial, or none when the device has no answer.
fn text(answer: Result<Answer, InputError>) -> Result<Text, InputError> {
    match answer {
        Ok(answer) => Ok(Text::new(answer.as_str_bytes()).unwrap_or(Text::NONE)),
        Err(InputError::Absent { .. }) => Ok(Text::NONE),
        Err(error) => Err(error),
    }
}

/// Ask the device everything HELLO holds: its name, serial and ids, its
/// property bits, which event types it sends and each type's codes, and the
/// range of every absolute axis it declares. The location is left 0 for the
/// glue.
///
/// An event type is declared when its code query has an answer, even one
/// with no bit set, which is how QEMU's keyboard declares `EV_REP`; every
/// type up to `EV_MAX` is asked, `EV_SYN` included, so HELLO holds the
/// device's own declaration and the core can name what it leaves out. A
/// device without a name, serial or ids gives none, as Linux's driver treats
/// one. A declared axis without a usable range fails: the device has broken
/// its own description.
///
/// Returns the HELLO, and whether any bit lay past its kind's `*_MAX` and
/// was left out, since no field of HELLO has room for it.
pub fn read_hello<D: ConfigSelect + ?Sized>(dev: &mut D) -> Result<(Hello, bool), InputError> {
    let name = text(input::name(dev))?;
    let serial = text(input::serial(dev))?;
    let id = match input::devids(dev) {
        Ok(ids) => DeviceId {
            bustype: ids.bustype,
            vendor: ids.vendor,
            product: ids.product,
            version: ids.version,
        },
        Err(InputError::Absent { .. }) => DeviceId::default(),
        Err(error) => return Err(error),
    };

    let mut bits = Bitmaps::EMPTY;
    let mut clipped = copy_bits(&input::prop_bits(dev)?, INPUT_PROP_CNT, &mut bits.props);
    for kind in 0..EV_CNT {
        let answer = input::ev_bits(dev, kind)?;
        if answer.is_empty() {
            continue;
        }
        set(&mut bits.types, kind);
        let count = bits.codes(kind).map(|(_, count)| count);
        if let (Some(count), Some(codes)) = (count, bits.codes_mut(kind)) {
            clipped |= copy_bits(&answer, count, codes);
        }
    }

    let mut axes = [AxisRange::default(); AXES];
    for (axis, range) in (0..ABS_CNT).zip(axes.iter_mut()) {
        if !bit(&bits.abs, axis) {
            continue;
        }
        let info = input::abs_info(dev, axis)?;
        *range = AxisRange {
            minimum: info.min,
            maximum: info.max,
            fuzz: info.fuzz,
            flat: info.flat,
            resolution: info.res,
        };
    }

    Ok((
        Hello {
            version: VERSION,
            location: 0,
            id,
            name,
            serial,
            bits,
            axes,
        },
        clipped,
    ))
}
