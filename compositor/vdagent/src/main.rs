//! `vdagent`: the guest half of the host's clipboard.
//!
//! `docs/CLIPBOARD.md` §6. On one side is the socket `user/vport` offers,
//! which carries SPICE's vdagent protocol to whatever is attached to the
//! machine's virtio-serial port -- a viewer's clipboard, or `xtask` playing
//! one. On the other is the compositor, where the selection lives. This
//! program is where the two meet and nothing else: it holds no window, draws
//! nothing, and understands the clipboard only well enough to carry text
//! between two protocols that both call it a promise.
//!
//! # Both directions are the same three steps
//!
//! A clipboard is not a buffer on either side: whoever owns it says so, and
//! answers for the data when someone asks. So each direction is *a grab
//! heard*, *a request made when something asks*, and *the answer carried*:
//!
//! * The host grabs, so this agent takes the Wayland selection. A guest
//!   program pastes, the compositor asks this agent through `send`, and the
//!   agent asks the host and writes what comes back into the descriptor the
//!   compositor gave it.
//! * A guest program takes the selection, so this agent grabs on the host.
//!   The host's viewer pastes, the host asks with `CLIPBOARD_REQUEST`, and
//!   the agent asks the compositor through a pipe and sends what comes back.
//!
//! # It exits quietly when there is no port
//!
//! `docs/CLIPBOARD.md` §7d: a boot without `--clipboard` has no
//! virtio-console device, so no `vport`, so no socket. That is a boot without
//! a clipboard and not a boot with an error, and this is started as an
//! `exec-once` beside the terminal and the wallpaper where an error would be
//! noise on every boot that did not ask for one.

mod host;
mod wayland;

use std::io::Write as _;

use ferrix_vdagent::message::{ClipboardType, Message, Selection, Types};

use host::{Host, MAX_SELECTION};
use wayland::{Event, Wayland, close, unsafe_file};

/// How long the loop sleeps when neither side had anything to say.
const IDLE: std::time::Duration = std::time::Duration::from_millis(2);

fn main() {
    let arguments =
        compositor_evecho::init::unshell(std::env::args().skip(1).collect::<Vec<String>>());
    let port = flag(&arguments, "--port")
        .unwrap_or_else(|| String::from_utf8_lossy(ferrix_vdagent::SOCKET_PATH).into_owned());
    match run(&port, flag(&arguments, "--display")) {
        Ok(reason) => say(&format!("vdagent: {reason}")),
        Err(error) => {
            say(&format!("vdagent: failed: {error}"));
            std::process::exit(1);
        }
    }
}

/// The value of `name` in `arguments`, written `--name value`.
fn flag(arguments: &[String], name: &str) -> Option<String> {
    let at = arguments.iter().position(|word| word == name)?;
    arguments.get(at + 1).cloned()
}

/// Connect to both sides and carry the clipboard between them until one of
/// them goes away.
fn run(port: &str, display: Option<String>) -> Result<String, String> {
    let path = std::path::Path::new(port);
    if !path.exists() {
        // The ordinary case for a boot that did not ask for a clipboard.
        return Ok(format!("no port at {port}; this boot has no clipboard"));
    }
    let mut host = Host::connect(path)?;
    let display = match display {
        Some(display) => display,
        None => {
            std::env::var("WAYLAND_DISPLAY").map_err(|_| "WAYLAND_DISPLAY is not set".to_owned())?
        }
    };
    let socket = compositor_socket::socket_path(&display)
        .map_err(|error| format!("the compositor's socket: {error}"))?;
    let mut wayland = Wayland::connect(&socket)?;
    carry(&mut host, &mut wayland)
}

/// What a message from the host asks for, owned, so that the borrow of the
/// assembly buffer ends before anything is done about it.
#[derive(Debug)]
enum FromHost {
    /// The host owns its clipboard and can give it as text.
    Grabbed,
    /// The host gave its clipboard up.
    Released,
    /// The host wants the guest's selection.
    Wants,
    /// The host's clipboard, as text, or nothing it could give.
    Answer(Option<Vec<u8>>),
    /// Something this agent has nothing to do about.
    Nothing,
}

/// The two sides, carried between until one goes away.
fn carry(host: &mut Host, wayland: &mut Wayland) -> Result<String, String> {
    // Descriptors the compositor gave for the host's clipboard, waiting on
    // the host's answer. More than one is a second program pasting before
    // the first was answered.
    let mut waiting: Vec<i32> = Vec::new();
    let mut serial = 0_u32;
    loop {
        let mut busy = false;
        if !host.receive() {
            answer_nothing(&mut waiting);
            return Ok("the port closed; the host is gone".to_owned());
        }
        while let Some(asked) = host.next_message(classify) {
            busy = true;
            match asked {
                FromHost::Grabbed => wayland.grab()?,
                FromHost::Released => wayland.release()?,
                FromHost::Wants => {
                    let held = wayland.read_selection(MAX_SELECTION)?;
                    let (kind, data) = match held.as_deref() {
                        Some(bytes) => (ClipboardType::Utf8Text, bytes),
                        None => (ClipboardType::None, [].as_slice()),
                    };
                    host.send(&Message::Clipboard {
                        selection: Selection::Clipboard,
                        kind,
                        data,
                    })?;
                }
                FromHost::Answer(held) => match held {
                    Some(bytes) => answer(&mut waiting, &bytes),
                    None => answer_nothing(&mut waiting),
                },
                FromHost::Nothing => {}
            }
        }
        if host.is_broken() {
            answer_nothing(&mut waiting);
            return Ok("the host's framing was lost".to_owned());
        }
        for event in wayland.turn()? {
            busy = true;
            match event {
                Event::Grabbed => {
                    serial = serial.wrapping_add(1);
                    host.send(&Message::ClipboardGrab {
                        selection: Selection::Clipboard,
                        serial: Some(serial),
                        types: Types::new(&[ClipboardType::Utf8Text])
                            .map_err(|error| format!("a grab: {error}"))?,
                    })?;
                }
                Event::Released => host.send(&Message::ClipboardRelease {
                    selection: Selection::Clipboard,
                })?,
                Event::Wanted(fd) => {
                    waiting.push(fd);
                    host.send(&Message::ClipboardRequest {
                        selection: Selection::Clipboard,
                        kind: ClipboardType::Utf8Text,
                    })?;
                }
                // The source is gone, so nothing this agent was asked for
                // will ever be answered.
                Event::Cancelled => answer_nothing(&mut waiting),
            }
        }
        host.flush();
        if !busy {
            std::thread::sleep(IDLE);
        }
    }
}

/// What one message from the host means, as something owned.
fn classify(message: &Message<'_>) -> FromHost {
    match *message {
        Message::ClipboardGrab {
            selection: Selection::Clipboard,
            ref types,
            ..
        } if types.holds(ClipboardType::Utf8Text) => FromHost::Grabbed,
        Message::ClipboardRelease {
            selection: Selection::Clipboard,
        } => FromHost::Released,
        Message::ClipboardRequest {
            selection: Selection::Clipboard,
            kind: ClipboardType::Utf8Text,
        } => FromHost::Wants,
        Message::Clipboard {
            selection: Selection::Clipboard,
            kind: ClipboardType::Utf8Text,
            data,
        } if data.len() <= MAX_SELECTION => FromHost::Answer(Some(data.to_vec())),
        // A refusal, a type this agent did not ask for, or a selection larger
        // than §7a allows: whoever is waiting gets nothing rather than a
        // wrong answer.
        Message::Clipboard { .. } => FromHost::Answer(None),
        _ => FromHost::Nothing,
    }
}

/// Write `bytes` to everyone waiting, and close what they were waiting on.
///
/// Closing is what tells the reader there is no more, so it happens whether
/// the write worked or not: a descriptor left open is a paste that never
/// ends.
fn answer(waiting: &mut Vec<i32>, bytes: &[u8]) {
    for fd in waiting.drain(..) {
        let mut file = unsafe_file(fd);
        let _ = file.write_all(bytes);
        let _ = file.flush();
        drop(file);
    }
}

/// Close what everyone waiting was waiting on, which is an empty answer.
fn answer_nothing(waiting: &mut Vec<i32>) {
    for fd in waiting.drain(..) {
        close(fd);
    }
}

/// Say a line on the standard output, flushed.
fn say(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
