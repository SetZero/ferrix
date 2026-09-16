//! Print, as hex, what the server answers a client's opening conversation
//! with.
//!
//! `probe/roundtrip.sh` feeds this to `probe/roundtrip.c`, which replays it to
//! a real libwayland client and prints what the client made of it. Keeping the
//! two apart means the C probe links libwayland and nothing else, and the
//! bytes it replays are the server's own rather than a copy of them that could
//! go stale.
//!
//! The conversation is the one every toolkit starts with:
//! `wl_display.get_registry` for object 2, then `wl_display.sync` for object
//! 3, which is exactly what `wl_display_roundtrip` sends after a
//! `wl_display_get_registry`.

// A probe helper that runs on the development host and whose whole job is to
// print. The compositor's own rules -- no panic, no stdout -- are about a
// program that holds every client's windows; this one holds nothing, and a
// transcript that could not be built should stop it rather than be written
// out half-made.
#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a host-only probe helper whose output is the point"
)]

use compositor_protocol::{core, xdg_shell};
use compositor_server::wire::{Arg, ArgType, ObjectId, Writer};
use compositor_server::{Client, Globals, Role};

fn main() {
    let mut globals = Globals::new();
    for (interface, version, role) in [
        (&core::WL_COMPOSITOR, 6, Role::Compositor),
        (&core::WL_SUBCOMPOSITOR, 1, Role::Subcompositor),
        (&core::WL_SHM, 1, Role::Shm),
        (&core::WL_SEAT, 7, Role::Seat),
        (&core::WL_OUTPUT, 4, Role::Output),
        (&xdg_shell::XDG_WM_BASE, 6, Role::XdgWmBase),
    ] {
        assert!(
            globals.add(interface, version, role).is_some(),
            "{} at {version}",
            interface.name
        );
    }

    let mut writer = Writer::new();
    for (opcode, id) in [
        (core::wl_display::request::GET_REGISTRY, 2u32),
        (core::wl_display::request::SYNC, 3),
    ] {
        writer
            .write(
                ObjectId::DISPLAY,
                opcode,
                &[ArgType::NewId],
                &[Arg::NewId(ObjectId(id))],
            )
            .expect("a request a client sends");
    }
    let (bytes, fds) = writer.take();

    let mut client = Client::new(globals);
    let consumed = client.read(&bytes, &fds);
    assert_eq!(consumed, bytes.len(), "the server read both requests");
    assert!(!client.is_finished(), "{:?}", client.fatal());

    let outgoing = client.take_outgoing();
    assert!(
        outgoing.descriptors.is_empty(),
        "this conversation carries no descriptors"
    );
    let mut hex = String::new();
    for byte in &outgoing.bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    println!("{hex}");
}
