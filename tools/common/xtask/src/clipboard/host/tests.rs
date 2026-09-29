//! The bridge's session against a guest agent played by the test, over a
//! socket pair: no QEMU and no Wayland, so what is tested is the vdagent
//! conversation and the serials, which are the bridge's own.

use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use ferrix_vdagent::chunk::Reassembler;
use ferrix_vdagent::message::{ClipboardType, Message, Selection, Shape, Types};

use super::{HostClipboard, Session};
use crate::clipboard::{HOST_CAPS, send};

/// Where the guest's text went, as `wl-copy` would have taken it.
struct Recorder(Sender<Vec<u8>>);

impl HostClipboard for Recorder {
    fn copy(&mut self, text: &[u8]) {
        let _ = self.0.send(text.to_vec());
    }
}

/// The guest's end: whole messages, as the agent reads them.
struct Guest {
    stream: UnixStream,
    pending: Vec<u8>,
    buffer: Vec<u8>,
}

impl Guest {
    fn send(&mut self, message: &Message<'_>) {
        send(&mut self.stream, Shape::QEMU_CLIPBOARD, message).unwrap();
    }

    /// The next whole message's bytes.
    fn next(&mut self) -> Vec<u8> {
        let until = Instant::now() + Duration::from_secs(5);
        let mut reassembler = Reassembler::new(&mut self.buffer);
        let mut read = [0_u8; 4096];
        loop {
            while !self.pending.is_empty() {
                let taken = reassembler.feed(&self.pending).unwrap();
                if taken == 0 {
                    break;
                }
                let _ = self.pending.drain(..taken);
                if let Some(message) = reassembler.message() {
                    return message.to_vec();
                }
            }
            assert!(Instant::now() < until, "the bridge said nothing");
            if let Ok(got) = self.stream.read(&mut read) {
                self.pending.extend_from_slice(&read[..got]);
            }
        }
    }

    /// Wait for the host's grab and return its serial.
    fn grabbed(&mut self) -> Option<u32> {
        let bytes = self.next();
        match Message::decode(&bytes, Shape::QEMU_CLIPBOARD).unwrap() {
            Message::ClipboardGrab {
                selection: Selection::Clipboard,
                serial,
                types,
            } => {
                assert!(types.holds(ClipboardType::Utf8Text));
                serial
            }
            other => panic!("expected the host's grab, got {other:?}"),
        }
    }

    /// Ask for the host's text and return it.
    fn paste(&mut self) -> Vec<u8> {
        self.send(&Message::ClipboardRequest {
            selection: Selection::Clipboard,
            kind: ClipboardType::Utf8Text,
        });
        let bytes = self.next();
        match Message::decode(&bytes, Shape::QEMU_CLIPBOARD).unwrap() {
            Message::Clipboard { data, .. } => data.to_vec(),
            other => panic!("expected the host's text, got {other:?}"),
        }
    }

    fn grab(&mut self, serial: u32) {
        self.send(&Message::ClipboardGrab {
            selection: Selection::Clipboard,
            serial: Some(serial),
            types: Types::new(&[ClipboardType::Utf8Text]).unwrap(),
        });
    }

    /// Wait for the host's request for the guest's text.
    fn requested(&mut self) {
        let bytes = self.next();
        assert!(
            matches!(
                Message::decode(&bytes, Shape::QEMU_CLIPBOARD).unwrap(),
                Message::ClipboardRequest {
                    selection: Selection::Clipboard,
                    kind: ClipboardType::Utf8Text,
                }
            ),
            "expected the host to ask for the guest's text"
        );
    }
}

#[test]
fn carries_text_both_ways_with_qemus_serials() {
    let (host_end, guest_end) = UnixStream::pair().unwrap();
    guest_end
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    host_end
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let (tell_host, host_copies) = mpsc::channel();
    let (tell_copied, copied) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let session = std::thread::spawn(move || {
        Session::new(host_end).run(&host_copies, &stopped, &mut Recorder(tell_copied))
    });
    let mut guest = Guest {
        stream: guest_end,
        pending: Vec::new(),
        buffer: vec![0; 64 * 1024],
    };

    // The host speaks first, and asks for the guest's capabilities.
    let hello = guest.next();
    assert!(matches!(
        Message::decode(&hello, Shape::QEMU_CLIPBOARD).unwrap(),
        Message::AnnounceCapabilities { request: true, .. }
    ));
    guest.send(&Message::AnnounceCapabilities {
        request: false,
        caps: HOST_CAPS,
    });

    // Host to guest: a copy on the host is a grab, carrying QEMU's count.
    tell_host.send(b"copied on the host".to_vec()).unwrap();
    assert_eq!(guest.grabbed(), Some(0));
    assert_eq!(guest.paste(), b"copied on the host");

    // Guest to host. A grab below the count lost a race and is dropped, as
    // QEMU drops it; the next one is asked for, and its text copied.
    guest.grab(0);
    guest.grab(1);
    guest.requested();
    guest.send(&Message::Clipboard {
        selection: Selection::Clipboard,
        kind: ClipboardType::Utf8Text,
        data: b"copied in the guest",
    });
    assert_eq!(
        copied.recv_timeout(Duration::from_secs(5)).unwrap(),
        b"copied in the guest"
    );

    // `wl-paste --watch` then tells of that copy, which is not grabbed back
    // into the guest; the host's next copy is, one past the guest's serial.
    tell_host.send(b"copied in the guest".to_vec()).unwrap();
    tell_host
        .send(b"copied on the host again".to_vec())
        .unwrap();
    assert_eq!(guest.grabbed(), Some(1));
    assert_eq!(guest.paste(), b"copied on the host again");

    stop.store(true, Ordering::Relaxed);
    session.join().unwrap().unwrap();
}
