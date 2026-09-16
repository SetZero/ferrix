//! Fuzz the netlink walks and the builder that feeds them.
//!
//! An `AF_NETLINK` socket hands the kernel a buffer of length-prefixed
//! messages, each holding length-prefixed attributes, and every one of those
//! lengths was written by the program on the other end. This is the classic
//! place a kernel hangs on a stranger's arithmetic, so the fuzzer's bytes are
//! walked exactly as `sendmsg` walks them.
//!
//! # The properties
//!
//! Not panicking, and not hanging, are the floor. Beyond them:
//!
//! 1. **A walk ends.** Every message is at least a header long, so a buffer of
//!    `n` bytes can hold no more than `n / 16` of them and its attributes no
//!    more than `n / 4`. A walk that yields more than that is walking the same
//!    bytes twice, which is the hang this target exists to catch.
//! 2. **What a walk borrows lies inside the input**, header and payload alike.
//! 3. **An error is the end.** A walk that refused something yields nothing
//!    after it.
//! 4. **The builder and the walk agree**: a message written from a header, a
//!    body and attributes the fuzzer chose walks back to exactly that header,
//!    that body and those attributes.

#![no_main]

use ferrix_linux_abi::netlink::{NlAttr, NlMsgHdr};
use ferrix_netlink::{Address, Attr, Attributes, Messages, Value, Writer};
use libfuzzer_sys::fuzz_target;

/// Property 2: `inner` lies wholly inside `outer`.
fn inside(outer: &[u8], inner: &[u8]) -> bool {
    let outer = outer.as_ptr_range();
    let inner = inner.as_ptr_range();
    inner.start >= outer.start && inner.end <= outer.end
}

/// Walk the input as a buffer of messages, and each message's payload as
/// attributes, from every fixed-body offset a routing message uses.
fn walk(bytes: &[u8]) {
    let ceiling = bytes.len() / NlMsgHdr::SIZE + 1;
    let mut seen = 0;
    let mut walker = Messages::new(bytes);
    while let Some(message) = walker.next() {
        seen += 1;
        assert!(seen <= ceiling, "a walk over {} bytes yielded {seen} messages", bytes.len());
        let Ok(message) = message else {
            // 3. An error is the last thing a walk yields.
            assert!(walker.next().is_none(), "a walk went on after a refusal");
            return;
        };
        assert!(inside(bytes, message.payload), "a payload came from outside");
        for fixed in [0, 8, 12, 16] {
            attributes(bytes, message.attributes(fixed));
        }
    }
}

/// Walk one message's attributes, holding them to the same three properties.
fn attributes(bytes: &[u8], mut walker: Attributes<'_>) {
    let ceiling = bytes.len() / NlAttr::SIZE + 1;
    let mut seen = 0;
    while let Some(attribute) = walker.next() {
        seen += 1;
        assert!(seen <= ceiling, "an attribute walk yielded {seen} attributes");
        let Ok(attribute) = attribute else {
            assert!(walker.next().is_none(), "an attribute walk went on after a refusal");
            return;
        };
        assert!(inside(bytes, attribute.payload), "an attribute came from outside");
        // Every reader answers or refuses; none of them may panic, and none
        // may read a length the payload does not have.
        assert_eq!(attribute.as_u32().is_some(), attribute.payload.len() >= 4);
        assert_eq!(attribute.as_u8().is_some(), attribute.payload.len() == 1);
        assert!(attribute.as_name().len() <= attribute.payload.len());
        assert_eq!(
            attribute.as_address().is_some(),
            matches!(attribute.payload.len(), 4 | 16)
        );
    }
}

/// Property 4: a message the builder wrote walks back to what was built.
fn round_trip(bytes: &[u8]) {
    let mut header = [0_u8; NlMsgHdr::SIZE];
    for (slot, byte) in header.iter_mut().zip(bytes) {
        *slot = *byte;
    }
    let Some(built) = NlMsgHdr::from_bytes(&header) else {
        return;
    };
    // The body and the attribute payloads are the fuzzer's bytes, cut to
    // lengths a routing message uses.
    let body = &bytes[..bytes.len().min(16)];
    let name = &bytes[..bytes.len().min(15)];
    let attributes = [
        Attr::new(3, Value::Name(name)),
        Attr::new(4, Value::U32(built.seq)),
        Attr::new(1, Value::Bytes(body)),
        Attr::new(2, Value::Address(Address::V4([10, 0, 2, 15]))),
        Attr::new(5, Value::U8(built.flags as u8)),
    ];
    let mut buffer = vec![0_u8; 4096];
    let mut writer = Writer::new(&mut buffer);
    let Ok(written) = writer.message(built, body, &attributes) else {
        return;
    };
    assert_eq!(written, writer.len());
    let mut walked = Messages::new(&buffer[..written]);
    let message = walked
        .next()
        .expect("what was written walks back")
        .expect("and is well formed");
    assert!(walked.next().is_none(), "one message was written");
    assert_eq!(message.header.kind, built.kind);
    assert_eq!(message.header.seq, built.seq);
    assert_eq!(message.header.pid, built.pid);
    assert_eq!(message.header.flags, built.flags);
    assert_eq!(message.body(body.len()), Some(body));

    let found: Vec<_> = message
        .attributes(body.len())
        .map(|attribute| attribute.expect("what was written walks back"))
        .collect();
    assert_eq!(found.len(), attributes.len(), "every attribute came back");
    assert_eq!(found[0].as_name(), name_without_nul(name));
    assert_eq!(found[1].as_u32(), Some(built.seq));
    assert_eq!(found[2].as_bytes(), body);
    assert_eq!(found[3].as_address(), Some(Address::V4([10, 0, 2, 15])));
    assert_eq!(found[4].as_u8(), Some(built.flags as u8));
    for attribute in &found {
        assert!(inside(&buffer, attribute.payload));
    }
}

/// What `Value::Name` writes of `name`: everything before its first nul.
fn name_without_nul(name: &[u8]) -> &[u8] {
    let end = name.iter().position(|byte| *byte == 0).unwrap_or(name.len());
    &name[..end]
}

fuzz_target!(|bytes: &[u8]| {
    walk(bytes);
    round_trip(bytes);
});
