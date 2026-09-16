//! ICMP echo to the world beyond the gateway, through the host's own `ping`.
//!
//! An echo request can only leave this host as ICMP from a raw socket, which
//! needs privilege, or from an ICMP datagram socket, which Linux keeps behind
//! `net.ipv4.ping_group_range` and Windows does not have; `std` offers
//! neither. What every host does have is a `ping` its users may run. So a
//! request to an address that is not the gateway's own is carried out by one
//! run of the host's `ping`, on a thread of its own so the serving loop never
//! waits for it, and a reply built from the guest's request goes back when the
//! host was answered. Nothing is sent back when it was not, which is what a
//! lost echo looks like.
//!
//! What the guest measures is the host's round trip plus starting a process,
//! tens of milliseconds more than the network's own; and the reply's TTL is
//! this gateway's, not the far host's.

use std::net::Ipv4Addr;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// How long one host `ping` waits for its answer. Longer than any real round
/// trip and shorter than a guest's `ping -W`, whose default is ten seconds.
const WAIT: Duration = Duration::from_secs(3);

/// How many host `ping`s may run at once. A guest flooding echo requests
/// should not become a host starting processes without bound; the ones over
/// this are dropped, as a busy router drops them.
const MAX_RUNNING: usize = 16;

/// A request the host has finished with.
#[derive(Debug)]
pub(super) struct Answered {
    /// Who asked.
    pub(super) guest: Ipv4Addr,
    /// Who was asked.
    pub(super) destination: Ipv4Addr,
    /// The request's identifier.
    pub(super) identifier: u16,
    /// The request's sequence number.
    pub(super) sequence: u16,
    /// The request's body, which the reply carries back.
    pub(super) body: Vec<u8>,
}

/// The requests in flight, and where their answers arrive.
#[derive(Debug)]
pub(super) struct Forwarder {
    /// How many host `ping`s are running.
    running: Arc<AtomicUsize>,
    /// Where a thread sends a request whose destination answered.
    sender: mpsc::Sender<Answered>,
    /// Where the serving loop collects them.
    receiver: mpsc::Receiver<Answered>,
}

impl Forwarder {
    /// Nothing in flight.
    pub(super) fn new() -> Forwarder {
        let (sender, receiver) = mpsc::channel();
        Forwarder {
            running: Arc::new(AtomicUsize::new(0)),
            sender,
            receiver,
        }
    }

    /// Ask the host to ping `request.destination`, and answer through
    /// [`Forwarder::answered`] if it replies. Answers whether the request was
    /// taken, or dropped because [`MAX_RUNNING`] are already out.
    pub(super) fn forward(&self, request: Answered) -> bool {
        if self.running.fetch_add(1, Ordering::Relaxed) >= MAX_RUNNING {
            let _ = self.running.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        let running = Arc::clone(&self.running);
        let sender = self.sender.clone();
        let started = std::thread::Builder::new()
            .name("ferrix-net-ping".to_owned())
            .spawn(move || {
                if host_ping(request.destination) {
                    let _ = sender.send(request);
                }
                let _ = running.fetch_sub(1, Ordering::Relaxed);
            });
        if started.is_err() {
            let _ = self.running.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// The requests whose destinations answered since the last turn.
    pub(super) fn answered(&self) -> Vec<Answered> {
        self.receiver.try_iter().collect()
    }
}

/// One run of the host's `ping` to `destination`: whether it was answered.
///
/// Windows' `ping` exits 0 when any reply came, including a router's
/// "destination host unreachable", so there the reply has to be one carrying a
/// TTL, which only an echo reply does and which reads `TTL=` in every display
/// language. Elsewhere the exit status is the answer.
pub(super) fn host_ping(destination: Ipv4Addr) -> bool {
    let address = destination.to_string();
    let mut command = Command::new("ping");
    if cfg!(windows) {
        let wait = WAIT.as_millis().to_string();
        let _ = command.args(["-n", "1", "-w", &wait, &address]);
    } else if cfg!(target_os = "macos") {
        let wait = WAIT.as_millis().to_string();
        let _ = command.args(["-c", "1", "-W", &wait, &address]);
    } else {
        let wait = WAIT.as_secs().to_string();
        let _ = command.args(["-c", "1", "-W", &wait, &address]);
    }
    let Ok(output) = command.stdin(Stdio::null()).stderr(Stdio::null()).output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    !cfg!(windows)
        || String::from_utf8_lossy(&output.stdout)
            .to_ascii_uppercase()
            .contains("TTL=")
}
