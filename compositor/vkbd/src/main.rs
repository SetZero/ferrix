//! `vkbd <key> [<key> ...]`: type keys into whatever is focused.
//!
//! This is `wtype` with the layout handling taken out: it binds
//! `zwp_virtual_keyboard_manager_v1`, makes a keyboard and sends one press
//! and release per key named on the command line. What it is *for* is
//! proving that a client acting as a device reaches the seat -- keys sent
//! this way go through the compositor's keybinds exactly as a real
//! keyboard's do, which is the whole point of the protocol and the one
//! thing a test can check from outside.
//!
//! A key is named as a bind names one: `SUPER`, `Q`, `Return`. The keys are
//! pressed in order and released in reverse, so `vkbd SUPER Q` is the same
//! chord a person holding `SUPER` and tapping `Q` sends.

use std::time::Duration;

use compositor_protocol::core::{self, wl_display, wl_registry};
use compositor_protocol::virtual_keyboard::{
    self, zwp_virtual_keyboard_manager_v1 as manager, zwp_virtual_keyboard_v1 as keyboard,
};
use compositor_socket::{Connection, RecvError};
use compositor_wire::{Arg, ArgType, ObjectId, Reader, Writer};

/// The objects this client makes, at fixed ids.
const DISPLAY: ObjectId = ObjectId(1);
const REGISTRY: ObjectId = ObjectId(2);
const SYNC: ObjectId = ObjectId(3);
const MANAGER: ObjectId = ObjectId(4);
const SEAT: ObjectId = ObjectId(5);
const KEYBOARD: ObjectId = ObjectId(6);

/// How long to wait for the compositor to answer the roundtrip.
const PATIENCE: Duration = Duration::from_secs(30);

fn main() {
    let arguments =
        compositor_evecho::init::unshell(std::env::args().skip(1).collect::<Vec<String>>());
    if arguments.is_empty() {
        say("vkbd: usage: vkbd <key> [<key> ...]");
        std::process::exit(2);
    }
    match run(&arguments) {
        Ok(line) => say(&line),
        Err(error) => {
            say(&format!("vkbd: failed: {error}"));
            std::process::exit(1);
        }
    }
}

/// Type the keys, and say what was typed.
fn run(keys: &[String]) -> Result<String, String> {
    let codes: Vec<u16> = keys
        .iter()
        .map(|name| keycode(name).ok_or_else(|| format!("{name} is not a key")))
        .collect::<Result<_, String>>()?;
    let display =
        std::env::var("WAYLAND_DISPLAY").map_err(|_| "WAYLAND_DISPLAY is not set".to_owned())?;
    let path =
        compositor_socket::socket_path(&display).map_err(|error| format!("the socket: {error}"))?;
    let stream = std::os::unix::net::UnixStream::connect(&path)
        .map_err(|error| format!("connecting to {}: {error}", path.display()))?;
    let mut connection =
        Connection::new(stream).map_err(|error| format!("the connection: {error}"))?;

    let mut writer = Writer::new();
    write(
        &mut writer,
        DISPLAY,
        wl_display::request::GET_REGISTRY,
        &[ArgType::NewId],
        &[Arg::NewId(REGISTRY)],
    )?;
    send(&mut connection, &mut writer)?;

    // The registry's globals, until the manager and a seat are both there.
    let (mut manager_name, mut seat_name) = (None, None);
    let started = std::time::Instant::now();
    while manager_name.is_none() || seat_name.is_none() {
        if started.elapsed() > PATIENCE {
            return Err("the compositor never announced a virtual keyboard".to_owned());
        }
        read(&mut connection)?;
        let bytes = connection.bytes().to_vec();
        let descriptors = connection.fds();
        let mut reader = Reader::new(&bytes, &descriptors);
        while let Ok(header) = reader.peek() {
            if header.sender != REGISTRY || header.opcode != wl_registry::event::GLOBAL {
                if reader.skip(0).is_err() {
                    break;
                }
                continue;
            }
            let Ok((_, args)) = reader.read(&[
                ArgType::Uint,
                ArgType::Str { nullable: false },
                ArgType::Uint,
            ]) else {
                break;
            };
            let (Some(name), Some(interface)) = (
                args.first().and_then(Arg::as_uint),
                args.get(1).and_then(Arg::as_str),
            ) else {
                continue;
            };
            match interface {
                "zwp_virtual_keyboard_manager_v1" => manager_name = Some(name),
                "wl_seat" => seat_name = Some(name),
                _ => {}
            }
        }
        let taken = reader.consumed();
        connection.consume(taken, 0);
    }
    let (Some(manager_name), Some(seat_name)) = (manager_name, seat_name) else {
        return Err("no virtual keyboard manager".to_owned());
    };

    bind(&mut writer, seat_name, "wl_seat", 7, SEAT)?;
    bind(
        &mut writer,
        manager_name,
        "zwp_virtual_keyboard_manager_v1",
        1,
        MANAGER,
    )?;
    write(
        &mut writer,
        MANAGER,
        manager::request::CREATE_VIRTUAL_KEYBOARD,
        &[ArgType::Object { nullable: false }, ArgType::NewId],
        &[Arg::Object(SEAT), Arg::NewId(KEYBOARD)],
    )?;
    // Down in order, up in reverse: `vkbd SUPER Q` is the chord a person
    // holding `SUPER` and tapping `Q` sends.
    for (time, code) in codes.iter().enumerate() {
        press(&mut writer, time, *code, true)?;
    }
    for (time, code) in codes.iter().enumerate().rev() {
        press(&mut writer, codes.len() + time, *code, false)?;
    }
    // A roundtrip, so the keys are in the compositor's hands before this
    // program ends: a client that exits with bytes still in the socket is a
    // client whose keys arrive with its goodbye.
    write(
        &mut writer,
        DISPLAY,
        wl_display::request::SYNC,
        &[ArgType::NewId],
        &[Arg::NewId(SYNC)],
    )?;
    send(&mut connection, &mut writer)?;
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > PATIENCE {
            return Err("the compositor never answered the roundtrip".to_owned());
        }
        read(&mut connection)?;
        let bytes = connection.bytes().to_vec();
        let descriptors = connection.fds();
        let mut reader = Reader::new(&bytes, &descriptors);
        let mut done = false;
        while let Ok(header) = reader.peek() {
            if header.sender == SYNC {
                done = true;
            }
            if reader.skip(0).is_err() {
                break;
            }
        }
        let taken = reader.consumed();
        connection.consume(taken, 0);
        if done {
            break;
        }
    }
    Ok(format!("vkbd: typed {}", keys.join(" ")))
}

/// The key a name asks for.
///
/// A bind names a modifier as `SUPER` or `SHIFT`, which is a *modifier* and
/// not a key; a keyboard has no such key and a person presses the left one.
/// Every other name is XKB's own, which is what `compositor/xkb` knows.
fn keycode(name: &str) -> Option<u16> {
    let modifier = match name.to_ascii_uppercase().as_str() {
        "SUPER" | "MOD4" | "WIN" | "META" => Some("Super_L"),
        "SHIFT" => Some("Shift_L"),
        "CTRL" | "CONTROL" => Some("Control_L"),
        "ALT" | "MOD1" => Some("Alt_L"),
        _ => None,
    };
    compositor_xkb::code_of(modifier.unwrap_or(name))
}

/// One half of one key.
fn press(writer: &mut Writer, time: usize, code: u16, down: bool) -> Result<(), String> {
    let state = if down {
        core::wl_keyboard::key_state::PRESSED
    } else {
        core::wl_keyboard::key_state::RELEASED
    };
    write(
        writer,
        KEYBOARD,
        keyboard::request::KEY,
        &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
        &[
            Arg::Uint(u32::try_from(time).unwrap_or(0)),
            Arg::Uint(u32::from(code)),
            Arg::Uint(state),
        ],
    )
}

/// `wl_registry.bind`.
fn bind(
    writer: &mut Writer,
    name: u32,
    interface: &str,
    version: u32,
    id: ObjectId,
) -> Result<(), String> {
    write(
        writer,
        REGISTRY,
        wl_registry::request::BIND,
        &[ArgType::Uint, ArgType::AnyNewId],
        &[
            Arg::Uint(name),
            Arg::AnyNewId {
                interface,
                version,
                id,
            },
        ],
    )
}

/// One request into the outgoing buffer.
fn write(
    writer: &mut Writer,
    sender: ObjectId,
    opcode: u16,
    signature: &'static [ArgType],
    args: &[Arg<'_>],
) -> Result<(), String> {
    writer
        .write(sender, opcode, signature, args)
        .map_err(|error| format!("writing a request: {error:?}"))
}

/// Send what is queued.
fn send(connection: &mut Connection, writer: &mut Writer) -> Result<(), String> {
    let bytes = writer.bytes().to_vec();
    writer.clear();
    connection
        .send(&bytes, &[])
        .map_err(|error| format!("sending: {error:?}"))
}

/// Wait for something to arrive.
fn read(connection: &mut Connection) -> Result<(), String> {
    match connection.receive() {
        Ok(_) | Err(RecvError::WouldBlock) => Ok(()),
        Err(RecvError::Closed) => Err("the compositor closed the connection".to_owned()),
        Err(error) => Err(format!("reading: {error:?}")),
    }
}

/// Say a line where the compositor's own log goes.
fn say(line: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// The generated tables are used through the modules above; this keeps the
/// crate's own name in view for a reader looking for it.
const _: &compositor_wire::Interface = &virtual_keyboard::ZWP_VIRTUAL_KEYBOARD_V1;
