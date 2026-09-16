//! What a client's requests do, and what breaking the rules gets.
//!
//! The bytes here are built with `compositor_wire`'s writer, which
//! `compositor/wire`'s own probe has already shown to be libwayland's bytes,
//! so a test that writes a request writes the one a real client would.

use compositor_protocol::{core, xdg_shell};
use compositor_wire::{Arg, ArgType, Fd, Header, ObjectId, Reader, Writer};

use crate::{Client, Event, Fatal, Globals, Role};

/// The globals a test connection offers: enough that binding, versions and
/// mistaken interfaces can all be tried.
fn globals() -> Globals {
    let mut globals = Globals::new();
    assert_eq!(
        globals.add(&core::WL_COMPOSITOR, 6, Role::Compositor),
        Some(1)
    );
    assert_eq!(globals.add(&core::WL_SHM, 1, Role::Shm), Some(2));
    assert_eq!(globals.add(&core::WL_SEAT, 7, Role::Seat), Some(3));
    assert_eq!(
        globals.add(&xdg_shell::XDG_WM_BASE, 6, Role::XdgWmBase),
        Some(4)
    );
    globals
}

fn client() -> Client {
    Client::new(globals())
}

/// One message's bytes, as a client would send them.
fn request(sender: u32, opcode: u16, signature: &'static [ArgType], args: &[Arg<'_>]) -> Vec<u8> {
    let mut writer = Writer::new();
    writer
        .write(ObjectId(sender), opcode, signature, args)
        .expect("a message a client could send");
    writer.bytes().to_vec()
}

/// `wl_display.get_registry(id)`.
fn get_registry(id: u32) -> Vec<u8> {
    request(
        1,
        core::wl_display::request::GET_REGISTRY,
        &[ArgType::NewId],
        &[Arg::NewId(ObjectId(id))],
    )
}

/// `wl_display.sync(id)`.
fn sync(id: u32) -> Vec<u8> {
    request(
        1,
        core::wl_display::request::SYNC,
        &[ArgType::NewId],
        &[Arg::NewId(ObjectId(id))],
    )
}

/// `wl_registry.bind(name, interface, version, id)`.
fn bind(registry: u32, name: u32, interface: &str, version: u32, id: u32) -> Vec<u8> {
    request(
        registry,
        core::wl_registry::request::BIND,
        &[ArgType::Uint, ArgType::AnyNewId],
        &[
            Arg::Uint(name),
            Arg::AnyNewId {
                interface,
                version,
                id: ObjectId(id),
            },
        ],
    )
}

/// One event the server queued: who sent it, which opcode, and the arguments
/// read back with the signature that interface gives.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Sent {
    sender: ObjectId,
    opcode: u16,
    args: Vec<String>,
}

/// Everything the server has queued, decoded.
///
/// Each event is read with the signature of its sender's interface, which is
/// what a real client does, so an event written with the wrong signature
/// shows up here as a message that will not decode.
fn sent(client: &mut Client) -> Vec<Sent> {
    let outgoing = client.take_outgoing();
    let mut reader = Reader::new(&outgoing.bytes, &outgoing.descriptors);
    let mut out = Vec::new();
    while !reader.is_done() {
        let header = reader.peek().expect("a header");
        let interface = interface_of(client, header);
        let method = interface
            .event(header.opcode)
            .unwrap_or_else(|| panic!("{} has no event {}", interface.name, header.opcode));
        let (_, args) = reader
            .read(method.signature)
            .unwrap_or_else(|error| panic!("{}.{}: {error:?}", interface.name, method.name));
        out.push(Sent {
            sender: header.sender,
            opcode: header.opcode,
            args: args.iter().map(|arg| format!("{arg:?}")).collect(),
        });
    }
    out
}

/// Which interface an event's sender speaks.
///
/// Objects the server has already destroyed -- a `wl_callback` that fired,
/// anything a destructor took -- are not in the map any more, so the two the
/// server sends to unconditionally are named here.
fn interface_of(client: &Client, header: Header) -> &'static compositor_protocol::Interface {
    if let Some(entry) = client.objects().get(header.sender) {
        return entry.interface;
    }
    if header.sender == ObjectId::DISPLAY {
        return &core::WL_DISPLAY;
    }
    &core::WL_CALLBACK
}

#[test]
fn a_fresh_connection_has_the_display_and_nothing_else() {
    let client = client();
    assert_eq!(client.objects().len(), 1);
    let display = client.objects().get(ObjectId::DISPLAY).expect("object 1");
    assert_eq!(display.interface.name, "wl_display");
    assert_eq!(display.data, Role::Display);
    assert!(!client.is_finished());
    assert_eq!(client.fatal(), None);
}

#[test]
fn get_registry_announces_every_global_in_order() {
    let mut client = client();
    let bytes = get_registry(2);
    assert_eq!(client.read(&bytes, &[]), bytes.len());

    let events = sent(&mut client);
    assert_eq!(events.len(), 4, "one global event each");
    for (event, (name, interface, version)) in events.iter().zip([
        (1u32, "wl_compositor", 6u32),
        (2, "wl_shm", 1),
        (3, "wl_seat", 7),
        (4, "xdg_wm_base", 6),
    ]) {
        assert_eq!(event.sender, ObjectId(2), "the registry sends them");
        assert_eq!(event.opcode, core::wl_registry::event::GLOBAL);
        assert_eq!(
            event.args,
            [
                format!("Uint({name})"),
                format!("Str(Some({interface:?}))"),
                format!("Uint({version})")
            ]
        );
    }
    assert!(client.objects().contains(ObjectId(2)));
}

#[test]
fn sync_fires_its_callback_and_takes_the_id_back() {
    let mut client = client();
    let bytes = sync(2);
    assert_eq!(client.read(&bytes, &[]), bytes.len());

    let events = sent(&mut client);
    assert_eq!(events.len(), 2, "done, then delete_id");
    assert_eq!(events[0].sender, ObjectId(2));
    assert_eq!(events[0].opcode, core::wl_callback::event::DONE);
    assert_eq!(events[1].sender, ObjectId::DISPLAY);
    assert_eq!(events[1].opcode, core::wl_display::event::DELETE_ID);
    assert_eq!(events[1].args, ["Uint(2)"]);

    // The id is gone, so the client may use it again -- and does.
    assert!(!client.objects().contains(ObjectId(2)));
    assert_eq!(
        client.take_events(),
        [Event::Destroyed {
            object: ObjectId(2),
            role: Role::Callback
        }]
    );
    let again = get_registry(2);
    assert_eq!(client.read(&again, &[]), again.len());
    assert!(client.objects().contains(ObjectId(2)));
    assert!(!client.is_finished());
}

#[test]
fn a_request_to_a_callback_ends_the_connection() {
    // wl_callback has no requests at all, and the server destroys it the
    // moment it fires, so a client sending to one has lost track of an id.
    let mut client = client();
    let bytes = sync(2);
    let _ = client.read(&bytes, &[]);
    let _ = client.take_outgoing();

    let stray = request(2, 0, &[], &[]);
    let _ = client.read(&stray, &[]);
    assert_eq!(client.fatal(), Some(&Fatal::NoSuchObject(ObjectId(2))));
}

#[test]
fn binding_a_global_makes_an_object_and_tells_the_compositor() {
    let mut client = client();
    let mut bytes = get_registry(2);
    bytes.extend(bind(2, 1, "wl_compositor", 6, 3));
    bytes.extend(bind(2, 2, "wl_shm", 1, 4));
    assert_eq!(client.read(&bytes, &[]), bytes.len());
    assert!(!client.is_finished());

    let entry = client.objects().get(ObjectId(3)).expect("bound");
    assert_eq!(entry.interface.name, "wl_compositor");
    assert_eq!(entry.version, 6);
    assert_eq!(entry.data, Role::Compositor);

    assert_eq!(
        client.take_events(),
        [
            Event::Bound {
                object: ObjectId(3),
                role: Role::Compositor,
                version: 6
            },
            Event::Bound {
                object: ObjectId(4),
                role: Role::Shm,
                version: 1
            },
        ]
    );
}

#[test]
fn a_global_may_be_bound_below_the_version_it_was_advertised_at() {
    let mut client = client();
    let mut bytes = get_registry(2);
    bytes.extend(bind(2, 1, "wl_compositor", 1, 3));
    assert_eq!(client.read(&bytes, &[]), bytes.len());
    assert!(!client.is_finished());
    assert_eq!(
        client.objects().get(ObjectId(3)).map(|entry| entry.version),
        Some(1)
    );
}

#[test]
fn binding_above_the_advertised_version_ends_the_connection() {
    let mut client = client();
    let mut bytes = get_registry(2);
    // wl_shm is advertised at 1 here, though the interface itself is 2.
    bytes.extend(bind(2, 2, "wl_shm", 2, 3));
    let _ = client.read(&bytes, &[]);
    assert_eq!(
        client.fatal(),
        Some(&Fatal::BadBind {
            name: 2,
            version: 2
        })
    );
    assert!(!client.objects().contains(ObjectId(3)));
}

#[test]
fn binding_a_name_with_the_wrong_interface_ends_the_connection() {
    // Name 2 is wl_shm. A client naming it while saying wl_seat would
    // otherwise get a wl_shm that answers seat requests.
    let mut client = client();
    let mut bytes = get_registry(2);
    bytes.extend(bind(2, 2, "wl_seat", 1, 3));
    let _ = client.read(&bytes, &[]);
    assert_eq!(
        client.fatal(),
        Some(&Fatal::BadBind {
            name: 2,
            version: 1
        })
    );
}

#[test]
fn binding_a_name_that_was_never_advertised_ends_the_connection() {
    let mut client = client();
    let mut bytes = get_registry(2);
    bytes.extend(bind(2, 99, "wl_compositor", 1, 3));
    let _ = client.read(&bytes, &[]);
    assert_eq!(
        client.fatal(),
        Some(&Fatal::BadBind {
            name: 99,
            version: 1
        })
    );
    // And name 0, which no global ever has.
    let mut fresh = client::new_for(globals());
    let mut bytes = get_registry(2);
    bytes.extend(bind(2, 0, "wl_compositor", 1, 3));
    let _ = fresh.read(&bytes, &[]);
    assert!(fresh.is_finished());
}

/// A second constructor for a test that needs two connections in one body.
mod client {
    use crate::{Client, Globals};

    pub(super) fn new_for(globals: Globals) -> Client {
        Client::new(globals)
    }
}

#[test]
fn an_id_already_in_use_ends_the_connection() {
    let mut client = client();
    let mut bytes = get_registry(2);
    bytes.extend(get_registry(2));
    let _ = client.read(&bytes, &[]);
    assert_eq!(client.fatal(), Some(&Fatal::BadNewId(ObjectId(2))));
}

#[test]
fn an_id_in_the_servers_half_ends_the_connection() {
    let mut client = client();
    let bytes = get_registry(ObjectId::SERVER_BASE);
    let _ = client.read(&bytes, &[]);
    assert_eq!(
        client.fatal(),
        Some(&Fatal::BadNewId(ObjectId(ObjectId::SERVER_BASE)))
    );
}

#[test]
fn a_request_to_an_object_that_is_not_live_ends_the_connection() {
    let mut client = client();
    let bytes = request(7, core::wl_surface::request::COMMIT, &[], &[]);
    let _ = client.read(&bytes, &[]);
    assert_eq!(client.fatal(), Some(&Fatal::NoSuchObject(ObjectId(7))));
}

#[test]
fn an_opcode_the_interface_does_not_have_ends_the_connection() {
    let mut client = client();
    let bytes = request(1, 9, &[], &[]);
    let _ = client.read(&bytes, &[]);
    assert_eq!(
        client.fatal(),
        Some(&Fatal::NoSuchMethod {
            object: ObjectId::DISPLAY,
            opcode: 9
        })
    );
}

#[test]
fn a_request_newer_than_the_version_bound_ends_the_connection() {
    // wl_surface.offset is since 5. A client that bound wl_compositor at 1
    // and made a surface may not send it -- but this is checked on the object
    // the request is for, so bind wl_seat at 1 and send wl_seat.release,
    // which is since 5.
    let mut client = client();
    let mut bytes = get_registry(2);
    bytes.extend(bind(2, 3, "wl_seat", 1, 3));
    assert_eq!(client.read(&bytes, &[]), bytes.len());
    assert!(!client.is_finished());
    let _ = client.take_outgoing();

    let release = request(3, core::wl_seat::request::RELEASE, &[], &[]);
    let _ = client.read(&release, &[]);
    assert_eq!(
        client.fatal(),
        Some(&Fatal::NoSuchMethod {
            object: ObjectId(3),
            opcode: core::wl_seat::request::RELEASE
        })
    );

    // At version 5 it is allowed, and is a destructor.
    let mut ok = client::new_for(globals());
    let mut bytes = get_registry(2);
    bytes.extend(bind(2, 3, "wl_seat", 5, 3));
    let _ = ok.read(&bytes, &[]);
    let _ = ok.take_outgoing();
    let _ = ok.take_events();
    let _ = ok.read(&release, &[]);
    assert!(!ok.is_finished(), "{:?}", ok.fatal());
    assert!(!ok.objects().contains(ObjectId(3)), "release destroys it");
    assert_eq!(
        ok.take_events(),
        [Event::Destroyed {
            object: ObjectId(3),
            role: Role::Seat
        }]
    );
}

#[test]
fn a_protocol_error_is_told_once_and_nothing_after_it_is_answered() {
    let mut client = client();
    let mut bytes = get_registry(2);
    // A bad bind, then a sync that would otherwise be answered.
    bytes.extend(bind(2, 99, "wl_compositor", 1, 3));
    bytes.extend(sync(4));
    let consumed = client.read(&bytes, &[]);
    assert!(consumed < bytes.len(), "the rest is not read");

    let events = sent(&mut client);
    let errors: Vec<&Sent> = events
        .iter()
        .filter(|event| {
            event.sender == ObjectId::DISPLAY && event.opcode == core::wl_display::event::ERROR
        })
        .collect();
    assert_eq!(errors.len(), 1, "one error, not one per request");
    assert_eq!(errors[0].args[0], format!("Object(ObjectId({}))", 1));
    assert_eq!(
        errors[0].args[1],
        format!("Uint({})", core::wl_display::error::INVALID_OBJECT)
    );
    assert!(
        !client.objects().contains(ObjectId(4)),
        "the sync never ran"
    );

    // And a further read does nothing at all.
    let more = sync(5);
    assert_eq!(client.read(&more, &[]), 0);
    assert!(client.take_outgoing().bytes.is_empty());
}

#[test]
fn a_message_that_has_not_all_arrived_is_left_for_the_next_read() {
    let mut client = client();
    let whole = get_registry(2);
    for cut in 0..whole.len() {
        let mut half = client::new_for(globals());
        assert_eq!(half.read(&whole[..cut], &[]), 0, "{cut} bytes");
        assert!(!half.is_finished());
    }
    // Arriving in two pieces is the same as arriving in one.
    assert_eq!(client.read(&whole[..7], &[]), 0);
    assert_eq!(client.read(&whole, &[]), whole.len());
    assert!(client.objects().contains(ObjectId(2)));
}

#[test]
fn bytes_that_are_not_a_message_end_the_connection() {
    let mut client = client();
    // wl_display.get_registry with a null new_id, which the protocol forbids.
    let mut bytes = get_registry(2);
    bytes[8..12].copy_from_slice(&0u32.to_le_bytes());
    let _ = client.read(&bytes, &[]);
    assert!(
        matches!(client.fatal(), Some(Fatal::Unreadable(_))),
        "{:?}",
        client.fatal()
    );
}

#[test]
fn every_fatal_names_an_object_a_code_and_a_sentence() {
    let cases = [
        Fatal::NoSuchObject(ObjectId(7)),
        Fatal::NoSuchMethod {
            object: ObjectId(1),
            opcode: 9,
        },
        Fatal::BadNewId(ObjectId(0)),
        Fatal::BadBind {
            name: 1,
            version: 9,
        },
        Fatal::Unreadable(compositor_wire::Error::Size(3)),
    ];
    for reason in cases {
        assert!(!reason.message().is_empty(), "{reason:?}");
        assert!(
            matches!(
                reason.code(),
                core::wl_display::error::INVALID_OBJECT | core::wl_display::error::INVALID_METHOD
            ),
            "{reason:?} has code {}",
            reason.code()
        );
        // The object named must be one wl_display.error can carry: its
        // `object_id` argument is not nullable, so an error naming zero
        // could not be encoded and the client would see a silent close.
        assert!(!reason.object().is_null(), "{reason:?}");
    }
}

#[test]
fn a_new_id_of_zero_is_refused_with_an_error_the_client_can_read() {
    // The refusal names wl_display rather than the zero the client sent,
    // because wl_display.error's object is not nullable.
    let mut client = client();
    let mut bytes = get_registry(2);
    bytes.extend(bind(2, 1, "wl_compositor", 6, 3));
    let _ = client.read(&bytes, &[]);
    let _ = client.take_outgoing();

    // wl_compositor.create_surface with a null new_id. The wire reader
    // refuses it before the server sees it, so this is Unreadable -- but the
    // error still has to reach the client.
    let mut surface = request(
        3,
        core::wl_compositor::request::CREATE_SURFACE,
        &[ArgType::NewId],
        &[Arg::NewId(ObjectId(4))],
    );
    surface[8..12].copy_from_slice(&0u32.to_le_bytes());
    let _ = client.read(&surface, &[]);
    assert!(client.is_finished());

    let events = sent(&mut client);
    assert_eq!(events.len(), 1, "one error event");
    assert_eq!(events[0].sender, ObjectId::DISPLAY);
    assert_eq!(events[0].opcode, core::wl_display::event::ERROR);
    assert_eq!(events[0].args[0], "Object(ObjectId(1))");
}

#[test]
fn a_global_cannot_be_advertised_above_its_interface() {
    let mut globals = Globals::new();
    assert_eq!(globals.add(&core::WL_SHM, 99, Role::Shm), None);
    assert_eq!(globals.add(&core::WL_SHM, 0, Role::Shm), None);
    assert!(globals.is_empty());
    assert_eq!(
        globals.add(&core::WL_SHM, core::WL_SHM.version, Role::Shm),
        Some(1)
    );
    assert_eq!(
        globals.get(1).map(|global| global.version),
        Some(core::WL_SHM.version)
    );
    assert_eq!(globals.get(0), None, "names start at 1");
    assert_eq!(globals.get(2), None);
}

#[test]
fn a_descriptor_a_request_carried_is_taken_from_the_ones_that_arrived() {
    // No role answers an `fd` request yet, but the reader must still take the
    // descriptor so the next message gets the right one. wl_shm.create_pool
    // is the shape.
    let mut client = client();
    let mut bytes = get_registry(2);
    bytes.extend(bind(2, 2, "wl_shm", 1, 3));
    bytes.extend(request(
        3,
        core::wl_shm::request::CREATE_POOL,
        &[ArgType::NewId, ArgType::Fd, ArgType::Int],
        &[Arg::NewId(ObjectId(4)), Arg::Fd(Fd(0)), Arg::Int(4096)],
    ));
    assert_eq!(client.read(&bytes, &[Fd(11)]), bytes.len());
    assert!(!client.is_finished(), "{:?}", client.fatal());
}

// ---------------------------------------------------------------------------
// Against a real client
//
// Everything above builds the bytes with `compositor_wire` and reads them back
// with `compositor_wire`, which shows the crate agrees with itself.
// `probe/roundtrip.sh` is the other half: it runs `examples/transcript.rs` to
// get the server's answer to a client's opening conversation, replays it to a
// real libwayland client through `probe/roundtrip.c`, and records what the
// client made of it. libwayland is what every real client is built on, so a
// server it cannot follow is a server no application will run on.
// ---------------------------------------------------------------------------

/// The probe's output: the transcript, and libwayland's report of it.
const ROUNDTRIP: &str = include_str!("../probe/roundtrip.txt");

/// The globals `examples/transcript.rs` offers, which are the ones libwayland
/// should have reported.
const OFFERED: [(u32, &str, u32); 6] = [
    (1, "wl_compositor", 6),
    (2, "wl_subcompositor", 1),
    (3, "wl_shm", 1),
    (4, "wl_seat", 7),
    (5, "wl_output", 4),
    (6, "xdg_wm_base", 6),
];

/// The line of the probe's output beginning with `prefix`.
fn probe_line(prefix: &str) -> &'static str {
    ROUNDTRIP
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("{prefix:?} is not in probe/roundtrip.txt"))
}

#[test]
fn a_real_libwayland_client_reads_every_global_the_server_announced() {
    let reported: Vec<&str> = ROUNDTRIP
        .lines()
        .filter(|line| line.starts_with("global "))
        .collect();
    let expected: Vec<String> = OFFERED
        .iter()
        .map(|(name, interface, version)| format!("global {name} {interface} {version}"))
        .collect();
    assert_eq!(reported, expected, "libwayland saw other globals");

    // `wl_display_roundtrip` returns the number of events it dispatched, and
    // a negative number on a protocol error; `wl_display_get_error` is the
    // errno it would report. Six globals and the callback's `done` is seven,
    // and libwayland counts the `delete_id` too.
    let result = probe_line("roundtrip ");
    let fields: Vec<&str> = result.split_whitespace().collect();
    assert_eq!(fields.get(2), Some(&"globals"));
    assert_eq!(fields.get(3).map(|count| count.parse()), Some(Ok(6usize)));
    assert_eq!(fields.get(5), Some(&"0"), "libwayland reported an error");
    let dispatched: i32 = fields
        .get(1)
        .and_then(|value| value.parse().ok())
        .expect("a count");
    assert!(dispatched > 0, "the roundtrip failed: {result}");
}

#[test]
fn the_transcript_libwayland_accepted_is_still_the_one_the_server_writes() {
    // The probe replays a recording. If the server's answer changes and the
    // recording does not, the test above is checking a conversation that no
    // longer happens, so the recording is rebuilt here and compared.
    let recorded = probe_line("transcript ")
        .strip_prefix("transcript ")
        .expect("the prefix is there");

    let mut globals = Globals::new();
    for (interface, version, role) in [
        (&core::WL_COMPOSITOR, 6, Role::Compositor),
        (&core::WL_SUBCOMPOSITOR, 1, Role::Subcompositor),
        (&core::WL_SHM, 1, Role::Shm),
        (&core::WL_SEAT, 7, Role::Seat),
        (&core::WL_OUTPUT, 4, Role::Output),
        (&xdg_shell::XDG_WM_BASE, 6, Role::XdgWmBase),
    ] {
        assert!(globals.add(interface, version, role).is_some());
    }
    let mut client = Client::new(globals);
    let mut bytes = get_registry(2);
    bytes.extend(sync(3));
    assert_eq!(client.read(&bytes, &[]), bytes.len());
    assert!(!client.is_finished(), "{:?}", client.fatal());

    let outgoing = client.take_outgoing();
    let hex: String = outgoing
        .bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        hex, recorded,
        "the server's answer changed; rerun probe/roundtrip.sh"
    );

    // And the bytes libwayland accepted carry the globals this test expects,
    // read back through the wire rather than taken on trust: six
    // `wl_registry.global`s, the callback's `done`, and the `delete_id` that
    // takes the callback's number back.
    let decoded = decode_transcript(&outgoing.bytes);
    let mut names = Vec::new();
    for (index, (sender, opcode, args)) in decoded.iter().enumerate() {
        match index {
            0..=5 => {
                assert_eq!(*sender, ObjectId(2), "the registry sends a global");
                assert_eq!(*opcode, core::wl_registry::event::GLOBAL);
                names.push(args.clone());
            }
            6 => {
                assert_eq!(*sender, ObjectId(3), "the callback fires");
                assert_eq!(*opcode, core::wl_callback::event::DONE);
            }
            _ => {
                assert_eq!(*sender, ObjectId::DISPLAY);
                assert_eq!(*opcode, core::wl_display::event::DELETE_ID);
                assert_eq!(args, &["Uint(3)".to_owned()]);
            }
        }
    }
    assert_eq!(decoded.len(), 8, "six globals, a done and a delete_id");
    let expected: Vec<Vec<String>> = OFFERED
        .iter()
        .map(|(name, interface, version)| {
            vec![
                format!("Uint({name})"),
                format!("Str(Some({interface:?}))"),
                format!("Uint({version})"),
            ]
        })
        .collect();
    assert_eq!(names, expected);
}

/// The transcript's messages: sender, opcode and arguments.
///
/// Read with the signatures of the three events the conversation holds, which
/// is all a client would need to read it either.
fn decode_transcript(bytes: &[u8]) -> Vec<(ObjectId, u16, Vec<String>)> {
    let global = &[
        ArgType::Uint,
        ArgType::Str { nullable: false },
        ArgType::Uint,
    ][..];
    let one_uint = &[ArgType::Uint][..];
    let mut reader = Reader::new(bytes, &[]);
    let mut out = Vec::new();
    while !reader.is_done() {
        let header = reader.peek().expect("a header");
        let signature = if header.sender == ObjectId(2) {
            global
        } else {
            one_uint
        };
        let (_, args) = reader.read(signature).expect("a message");
        out.push((
            header.sender,
            header.opcode,
            args.iter().map(|arg| format!("{arg:?}")).collect(),
        ));
    }
    out
}

#[test]
fn the_probe_names_the_libwayland_it_came_from() {
    let first = ROUNDTRIP.lines().next().expect("a first line");
    assert!(
        first.starts_with("# libwayland "),
        "probe/roundtrip.txt should say which libwayland wrote it: {first}"
    );
}
