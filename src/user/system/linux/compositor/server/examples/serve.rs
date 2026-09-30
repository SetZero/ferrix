//! A one-client Wayland server on a socket, for the probe to talk to.
//!
//! `probe/roundtrip.sh` starts this, runs a real libwayland client against
//! it, and records what happened on both sides. It is the smallest thing
//! that makes the conversation two-way: the canned replay before it could
//! answer a client's opening requests but not a request whose answer depends
//! on ids the client chose, which is every configure.
//!
//! It is not the compositor. It draws nothing, has no layout and no output,
//! and configures every window to one fixed size. What it *is* is the shape
//! the compositor's own loop will have: accept, read, answer, write.
//!
//! Prints one line per thing that happened, and stops when the client
//! disconnects or the deadline passes, so a test can never hang.

// A probe helper that runs on the development host: its output is the point,
// and a failure should stop it rather than be written out half-made.
#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a host-only probe helper whose output is the point"
)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use compositor_protocol::{core, xdg_shell};
use compositor_server::wire::ObjectId;
use compositor_server::{Client, Event, Globals, Role};
use compositor_socket::{Connection, Listener, RecvError};

/// The size every window is configured to. A real compositor's layout
/// decides this; here it is fixed so the test can name it.
const WIDTH: i32 = 640;
const HEIGHT: i32 = 480;

/// How long to wait for the client before giving up.
const DEADLINE: Duration = Duration::from_secs(10);

fn globals() -> Globals {
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
    globals
}

fn main() {
    let path = std::env::args().nth(1).expect("a socket path");
    let listener = Listener::bind(&path).expect("a socket to listen on");
    println!("listening {}", listener.path().display());

    let started = Instant::now();
    let stream = loop {
        if let Some(stream) = listener.accept().expect("accept") {
            break stream;
        }
        if started.elapsed() > DEADLINE {
            println!("no client");
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    println!("accepted");

    let mut connection = Connection::new(stream).expect("a connection");
    let mut client = Client::new(globals());
    // Which toplevel is on which surface, and the ones whose first commit
    // has arrived and which are now owed a configure.
    let mut windows: BTreeMap<ObjectId, ObjectId> = BTreeMap::new();
    let mut awaiting: Vec<ObjectId> = Vec::new();

    while started.elapsed() < DEADLINE {
        match connection.receive() {
            Ok(0) => {}
            Ok(_) => {}
            Err(RecvError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(RecvError::Closed) => {
                println!("client gone");
                break;
            }
            Err(error) => {
                println!("read failed {error:?}");
                break;
            }
        }

        let arrived = connection.fds();
        let consumed = client.read(connection.bytes(), &arrived);
        if consumed > 0 {
            let mut claimed = 0;
            for event in client.take_events() {
                // `wl_shm.create_pool` is the only request a client sends
                // that carries a descriptor, and it makes one pool, so the
                // pools made are the descriptors the server took.
                if matches!(event, Event::PoolCreated { .. }) {
                    claimed += 1;
                }
                report(&event, &mut windows, &mut awaiting);
            }
            connection.consume(consumed, claimed);
        }

        // Configure every window that is waiting for one. A real compositor
        // does this from its layout; here every window is the same size.
        for toplevel in std::mem::take(&mut awaiting) {
            client.configure_toplevel(
                toplevel,
                WIDTH,
                HEIGHT,
                &[
                    xdg_shell::xdg_toplevel::state::ACTIVATED,
                    xdg_shell::xdg_toplevel::state::TILED_LEFT,
                ],
            );
            println!("configured {} {WIDTH} {HEIGHT}", toplevel.0);
        }

        let outgoing = client.take_outgoing();
        if !outgoing.bytes.is_empty() {
            connection
                .send(&outgoing.bytes, &outgoing.descriptors)
                .expect("send");
        }
        if let Some(reason) = client.fatal() {
            println!("protocol error {reason:?}");
            let _ = connection.flush();
            break;
        }
    }
    println!("server done");
}

/// Print what the server was told, and remember windows owed a configure.
fn report(event: &Event, windows: &mut BTreeMap<ObjectId, ObjectId>, awaiting: &mut Vec<ObjectId>) {
    match event {
        Event::Bound {
            object,
            role,
            version,
        } => {
            println!("bound {} {role:?} {version}", object.0);
        }
        Event::ToplevelCreated { toplevel, surface } => {
            println!("toplevel {} on surface {}", toplevel.0, surface.0);
            let _ = windows.insert(*surface, *toplevel);
        }
        Event::ToplevelRenamed { toplevel } => {
            println!("renamed {}", toplevel.0);
        }
        Event::SurfaceCommitted { surface, change } => {
            println!(
                "commit {} buffer {:?} mapped {} unmapped {}",
                surface.0,
                change.buffer.map(|id| id.0),
                change.mapped,
                change.unmapped
            );
        }
        Event::PoolCreated { pool, memory } => {
            println!("pool {} size {}", pool.0, memory.size);
        }
        _ => {}
    }
    // The first commit of a window carries no buffer, and is what asks for a
    // configure: `xdg_surface`'s description has the client commit once with
    // nothing attached and wait.
    if let Event::SurfaceCommitted { surface, change } = event
        && change.buffer.is_none()
        && let Some(toplevel) = windows.get(surface)
    {
        awaiting.push(*toplevel);
    }
}
