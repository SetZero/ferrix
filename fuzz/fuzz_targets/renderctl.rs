//! Fuzz the render control protocol's wire: the messages a driver and a
//! core send each other, decoded from whatever bytes arrive.
//!
//! The core reads these in the kernel and the driver in ring 3, and neither
//! wrote what the other sent. A second driver will speak the same wire
//! (`docs/GPU.md` §3.3 and §4), so what is pinned down here is the wire
//! rather than one driver's use of it.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **A decoded message re-encodes to the bytes it came from.** Decoding
//!    is strict -- exact length, reserved words zero, every enumeration a
//!    value it has -- so anything that survives it is something this
//!    protocol could have sent, and encoding it again gives that back.
//! 2. **A validated HELLO keeps its promises**: its version is the one this
//!    crate speaks, its name is non-empty text with nothing after its zero,
//!    its features are ones this version defines and include the one a
//!    render node needs, and the object limit is inside the protocol's.
//! 3. **A work range that fits lies inside the VMO**, and one that does not
//!    is refused whatever arithmetic it was built from.

#![no_main]

use ferrix_native_abi::rights::Rights;
use ferrix_renderctl::message::{
    Hello, MAX_OBJECT_BYTES, MAX_WORK_BYTES, Message, NAME_BYTES, VERSION, Work, features,
};
use libfuzzer_sys::fuzz_target;

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 4).map_or(0, |field| {
        u32::from_le_bytes(field.try_into().expect("four bytes"))
    })
}

fuzz_target!(|bytes: &[u8]| {
    // 1.
    if let Some(message) = Message::decode(bytes) {
        let again = message.encode();
        assert_eq!(again.as_bytes(), bytes, "{message:?} did not re-encode");
        assert_eq!(message.kind(), u32_at(bytes, 0));
        assert_eq!(Message::length_of(message.kind()), Some(bytes.len()));

        // 2.
        if let Message::Hello(hello) = message {
            let rights = [Rights(Rights::WRITE.0 | Rights::TRANSFER.0)];
            if hello.validate(&rights).is_ok() {
                assert_eq!(hello.version, VERSION);
                assert!(!hello.name().is_empty());
                assert!(
                    hello
                        .name()
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                );
                // The name is the text and then zeros, with nothing after.
                let end = hello.name().len();
                assert!(hello.name[end..].iter().all(|&byte| byte == 0));
                assert_eq!(hello.features & !features::KNOWN, 0);
                assert_ne!(hello.features & features::SUBMIT, 0);
                assert!(hello.object_max > 0);
                assert!(hello.object_limit() <= MAX_OBJECT_BYTES);
                assert!(hello.object_limit() <= hello.object_max);
            }
            // A name that does not fit is never made, so one that came off
            // the wire is at most the field.
            assert!(hello.name().len() <= NAME_BYTES);
        }
    }

    // 3.
    let range = Work {
        at: u32_at(bytes, 0),
        len: u32_at(bytes, 4),
    };
    let vmo = u64::from(u32_at(bytes, 8));
    if range.fits(vmo) {
        assert!(range.len <= MAX_WORK_BYTES);
        let end = u64::from(range.at) + u64::from(range.len);
        assert!(end <= vmo);
    }

    // And a name made from the input is either refused or reads back.
    if let Ok(text) = core::str::from_utf8(bytes)
        && let Some(name) = Hello::named(text)
    {
        assert!(!text.is_empty() && text.len() <= NAME_BYTES);
        assert_eq!(&name[..text.len()], text.as_bytes());
        assert!(name[text.len()..].iter().all(|&byte| byte == 0));
    }
});
