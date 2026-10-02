//! `xmoveresize WINDOW [DIRECTION]`: send the root of the X display
//! `$DISPLAY` names an EWMH `_NET_WM_MOVERESIZE` for `WINDOW`, as SDL and
//! GTK do when a window that draws its own title bar is pressed there, and
//! as Steam's login window does. `DIRECTION` is EWMH's: 0 to 7 a resize by
//! the edge or corner clockwise from the top left, 8 (the default) a move.
//!
//! It is `test-xwindow`'s, which presses a window through QEMU, has this
//! hand the press to the window manager -- yserver, which passes it on to
//! hyprix as `xdg_toplevel.move` -- and moves the pointer. No X client in
//! that volume sends the message, and the whole of one is a connection
//! setup, an `InternAtom` and a `SendEvent`, so this speaks X11 itself
//! rather than linking Xlib: a static program, like the other test clients.
//!
//! Only a local display (`:N`, the socket `/tmp/.X11-unix/XN`) without
//! authorization is reached, which is how the desktop starts yserver.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

const USAGE: &str = "usage: xmoveresize WINDOW [DIRECTION]\n\
    \n\
    Ask the window manager to move (DIRECTION 8, the default) or resize\n\
    (0 to 7, clockwise from the top left) WINDOW, a number in hex (0x...)\n\
    or decimal, by the pointer, with _NET_WM_MOVERESIZE.\n";

/// `_NET_WM_MOVERESIZE_MOVE`.
const MOVE: u32 = 8;

/// The requests' major opcodes.
const INTERN_ATOM: u8 = 16;
const SEND_EVENT: u8 = 25;
const GET_INPUT_FOCUS: u8 = 43;

/// `SubstructureNotify | SubstructureRedirect`, the mask EWMH sends a
/// window manager message to the root with.
const SUBSTRUCTURE: u32 = (1 << 19) | (1 << 20);

/// A number in hex with `0x`, or decimal.
fn number(text: &str) -> Option<u32> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// The socket a local `$DISPLAY` names: `:0` and `:0.0` are
/// `/tmp/.X11-unix/X0`.
fn socket_of(display: &str) -> Option<String> {
    let (host, rest) = display.split_once(':')?;
    if !host.is_empty() && host != "unix" {
        return None;
    }
    let number = rest.split('.').next()?;
    let _: u32 = number.parse().ok()?;
    Some(format!("/tmp/.X11-unix/X{number}"))
}

/// Four bytes at `at`, little-endian.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let four: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(four))
}

/// Two bytes at `at`, little-endian.
fn half(bytes: &[u8], at: usize) -> Option<u16> {
    let two: [u8; 2] = bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(two))
}

/// `n` rounded up to four.
const fn padded(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// The first screen's root window, from the server's whole setup reply (X11
/// protocol, "Connection Setup"): the vendor's length is at 24 and the
/// number of pixmap formats at 29, the vendor starts at 40, and the formats,
/// eight bytes each, come after it, then the screens, each its root first.
fn root_of(setup: &[u8]) -> Option<u32> {
    let vendor = usize::from(half(setup, 24)?);
    let formats = usize::from(*setup.get(29)?);
    word(setup, 40 + padded(vendor) + 8 * formats)
}

/// The connection: open, set up, with the root it found.
struct Display {
    stream: UnixStream,
    root: u32,
}

impl Display {
    /// Connect to `socket` and set up, little-endian, with no authorization.
    fn open(socket: &str) -> Result<Self, String> {
        let mut stream = UnixStream::connect(socket)
            .map_err(|error| format!("connecting to {socket}: {error}"))?;
        // 'l', unused, protocol 11.0, no authorization name or data.
        let hello = [b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        stream
            .write_all(&hello)
            .map_err(|error| format!("setting up: {error}"))?;
        let mut head = [0u8; 8];
        stream
            .read_exact(&mut head)
            .map_err(|error| format!("reading the setup: {error}"))?;
        let length = usize::from(half(&head, 6).unwrap_or(0)) * 4;
        let mut rest = vec![0u8; length];
        stream
            .read_exact(&mut rest)
            .map_err(|error| format!("reading the setup: {error}"))?;
        if head[0] != 1 {
            let reason = rest.get(..usize::from(head[1])).unwrap_or(&[]);
            return Err(format!(
                "the server refused the connection: {}",
                String::from_utf8_lossy(reason)
            ));
        }
        let mut setup = head.to_vec();
        setup.extend_from_slice(&rest);
        let root = root_of(&setup).ok_or("the setup reply names no screen")?;
        Ok(Self { stream, root })
    }

    fn send(&mut self, request: &[u8]) -> Result<(), String> {
        self.stream
            .write_all(request)
            .map_err(|error| format!("writing a request: {error}"))
    }

    /// The next reply, skipping events; an error is an error.
    fn reply(&mut self) -> Result<[u8; 32], String> {
        loop {
            let mut packet = [0u8; 32];
            self.stream
                .read_exact(&mut packet)
                .map_err(|error| format!("reading a reply: {error}"))?;
            match packet[0] {
                0 => {
                    return Err(format!(
                        "the server answered with error {} (request {})",
                        packet[1], packet[10]
                    ));
                }
                1 => {
                    // A reply longer than 32 bytes: the rest is not wanted.
                    let extra = word(&packet, 4).unwrap_or(0) as usize * 4;
                    let mut skipped = vec![0u8; extra];
                    self.stream
                        .read_exact(&mut skipped)
                        .map_err(|error| format!("reading a reply: {error}"))?;
                    return Ok(packet);
                }
                _ => {}
            }
        }
    }

    /// `InternAtom`.
    fn atom(&mut self, name: &str) -> Result<u32, String> {
        let bytes = name.as_bytes();
        let length = u16::try_from(2 + padded(bytes.len()) / 4).map_err(|_| "a long name")?;
        let mut request = vec![INTERN_ATOM, 0];
        request.extend_from_slice(&length.to_le_bytes());
        request.extend_from_slice(&u16::try_from(bytes.len()).unwrap_or(0).to_le_bytes());
        request.extend_from_slice(&[0, 0]);
        request.extend_from_slice(bytes);
        request.resize(usize::from(length) * 4, 0);
        self.send(&request)?;
        let reply = self.reply()?;
        word(&reply, 8).ok_or_else(|| "a short reply".to_owned())
    }

    /// A round trip, so that what was sent before is known to be done.
    fn sync(&mut self) -> Result<(), String> {
        self.send(&[GET_INPUT_FOCUS, 0, 1, 0])?;
        self.reply().map(|_| ())
    }
}

/// The `SendEvent` request: the `ClientMessage` to `root`.
fn move_resize(root: u32, window: u32, message_type: u32, direction: u32) -> Vec<u8> {
    let mut request = vec![SEND_EVENT, 0];
    request.extend_from_slice(&11u16.to_le_bytes());
    request.extend_from_slice(&root.to_le_bytes());
    request.extend_from_slice(&SUBSTRUCTURE.to_le_bytes());
    // ClientMessage, format 32, a sequence number the server sets.
    request.extend_from_slice(&[33, 32, 0, 0]);
    request.extend_from_slice(&window.to_le_bytes());
    request.extend_from_slice(&message_type.to_le_bytes());
    // x_root and y_root (the window manager has the pointer), the
    // direction, button 1, and source indication 1, an application.
    for value in [0, 0, direction, 1, 1] {
        request.extend_from_slice(&value.to_le_bytes());
    }
    request
}

fn run(args: &[String]) -> Result<(), String> {
    let (window, direction) = match args {
        [window] => (window, None),
        [window, direction] => (window, Some(direction)),
        _ => return Err(USAGE.to_owned()),
    };
    let window = number(window).ok_or(USAGE)?;
    let direction = match direction {
        Some(text) => number(text).filter(|value| *value <= 11).ok_or(USAGE)?,
        None => MOVE,
    };
    let display = std::env::var("DISPLAY").map_err(|_| "DISPLAY is not set")?;
    let socket = socket_of(&display).ok_or_else(|| format!("{display} is not a local display"))?;
    let mut x = Display::open(&socket)?;
    let message_type = x.atom("_NET_WM_MOVERESIZE")?;
    let request = move_resize(x.root, window, message_type, direction);
    x.send(&request)?;
    x.sync()?;
    let _ = writeln!(
        std::io::stdout(),
        "xmoveresize: sent _NET_WM_MOVERESIZE {direction} for 0x{window:x}"
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "xmoveresize: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_display_is_its_socket() {
        assert_eq!(socket_of(":0").as_deref(), Some("/tmp/.X11-unix/X0"));
        assert_eq!(socket_of(":12.0").as_deref(), Some("/tmp/.X11-unix/X12"));
        assert_eq!(socket_of("unix:1").as_deref(), Some("/tmp/.X11-unix/X1"));
        assert_eq!(socket_of("host:0"), None);
        assert_eq!(socket_of("0"), None);
    }

    #[test]
    fn numbers_are_hex_or_decimal() {
        assert_eq!(number("0x400001"), Some(0x40_0001));
        assert_eq!(number("8"), Some(8));
        assert_eq!(number("x"), None);
    }

    /// The root is found past the vendor and the pixmap formats.
    #[test]
    fn the_root_is_the_first_screens() {
        let mut setup = vec![0u8; 40];
        setup[0] = 1;
        setup[24] = 5; // a five-byte vendor, padded to eight
        setup[29] = 2; // two formats
        setup.extend_from_slice(b"yserv\0\0\0");
        setup.extend_from_slice(&[0u8; 16]);
        setup.extend_from_slice(&0x0000_0533u32.to_le_bytes());
        assert_eq!(root_of(&setup), Some(0x533));
    }

    /// The request is eleven words, the event its last eight.
    #[test]
    fn the_message_is_a_client_message_to_the_root() {
        let request = move_resize(0x533, 0x40_0001, 300, MOVE);
        assert_eq!(request.len(), 44);
        assert_eq!(request.get(..4), Some(&[SEND_EVENT, 0, 11, 0][..]));
        assert_eq!(word(&request, 4), Some(0x533));
        assert_eq!(request.get(12..14), Some(&[33, 32][..]));
        assert_eq!(word(&request, 16), Some(0x40_0001));
        assert_eq!(word(&request, 20), Some(300));
        assert_eq!(word(&request, 32), Some(MOVE));
        assert_eq!(word(&request, 36), Some(1));
    }
}
