//! Two hosts and a length of wire, both under the test's control.
//!
//! Frames go from one stack's egress to the other's input with nothing in
//! between, so a test exercises the real Ethernet and IP encoding -- including
//! the address resolution a first packet needs -- rather than a shortcut. The
//! clock only moves when both hosts are idle and one has a timer armed.

use alloc::vec::Vec;

use crate::addr::{Endpoint, IpAddress, IpCidr, Ipv4, Ipv6};
use crate::iface::{Address, Interface};
use crate::route::{Origin, Route};
use crate::socket::SocketId;
use crate::stack::{Config, Millis, Stack};

/// The first host's address.
pub(crate) const ONE: Ipv4 = Ipv4::new([10, 0, 0, 1]);

/// The second host's address.
pub(crate) const TWO: Ipv4 = Ipv4::new([10, 0, 0, 2]);

/// The first host's IPv6 address.
pub(crate) const ONE_V6: Ipv6 = Ipv6::new([0xFD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

/// The second host's IPv6 address.
pub(crate) const TWO_V6: Ipv6 = Ipv6::new([0xFD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);

/// How many turns of the loop one settle takes before the test is stuck.
const BUDGET: usize = 20_000;

/// How many times one settle moves the clock.
const TICKS: usize = 200;

/// Which host a test is talking about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Host {
    /// The host at [`ONE`].
    One,
    /// The host at [`TWO`].
    Two,
}

/// Two hosts on one link.
pub(crate) struct Wire {
    /// The host at [`ONE`].
    pub(crate) one: Stack,
    /// The host at [`TWO`].
    pub(crate) two: Stack,
    /// The clock.
    pub(crate) now: Millis,
    /// The interface index the link is on, the same on both.
    link: u32,
    /// How many frames have crossed.
    pub(crate) frames: usize,
    /// The ordinals of the frames to throw away.
    pub(crate) lose: Vec<usize>,
}

impl Wire {
    /// Two hosts, each with an address and a route to the other.
    pub(crate) fn new() -> Wire {
        let mut one = host(ONE, ONE_V6, [0x52, 0x54, 0, 0, 0, 1]);
        let mut two = host(TWO, TWO_V6, [0x52, 0x54, 0, 0, 0, 2]);
        one.seed(0x1234_5678_9ABC_DEF0);
        two.seed(0x0FED_CBA9_8765_4321);
        Wire {
            one,
            two,
            now: 0,
            link: 2,
            frames: 0,
            lose: Vec::new(),
        }
    }

    /// The host named.
    pub(crate) fn host(&mut self, which: Host) -> &mut Stack {
        match which {
            Host::One => &mut self.one,
            Host::Two => &mut self.two,
        }
    }

    /// Carry frames until both hosts are idle, moving the clock when they are
    /// idle and a timer is armed.
    pub(crate) fn settle(&mut self) {
        let mut ticks = 0;
        for _ in 0..BUDGET {
            if self.carry() {
                continue;
            }
            ticks += 1;
            if ticks > TICKS || !self.tick() {
                return;
            }
        }
        panic!("the hosts never settled");
    }

    /// Carry frames without letting the clock move.
    pub(crate) fn exchange(&mut self) {
        for _ in 0..BUDGET {
            if !self.carry() {
                return;
            }
        }
        panic!("the exchange never ended");
    }

    /// Move one frame, in whichever direction has one. Answers whether it
    /// did.
    fn carry(&mut self) -> bool {
        let now = self.now;
        if let Some(outgoing) = self.one.poll_transmit(now) {
            let ordinal = self.frames;
            self.frames += 1;
            if !self.lose.contains(&ordinal) {
                self.two.receive(self.link, &outgoing.frame, now);
            }
            return true;
        }
        if let Some(outgoing) = self.two.poll_transmit(now) {
            let ordinal = self.frames;
            self.frames += 1;
            if !self.lose.contains(&ordinal) {
                self.one.receive(self.link, &outgoing.frame, now);
            }
            return true;
        }
        false
    }

    /// Move the clock to the earliest armed timer. Answers whether there was
    /// one.
    fn tick(&mut self) -> bool {
        let next = [self.one.poll_at(), self.two.poll_at()]
            .into_iter()
            .flatten()
            .min();
        let Some(at) = next else {
            return false;
        };
        self.now = at.max(self.now);
        self.one.on_timer(self.now);
        self.two.on_timer(self.now);
        true
    }

    /// Move the clock by hand.
    pub(crate) fn advance(&mut self, millis: Millis) {
        self.now += millis;
        self.one.on_timer(self.now);
        self.two.on_timer(self.now);
    }

    /// Read everything a socket has, settling between reads.
    pub(crate) fn drain(&mut self, which: Host, id: SocketId) -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunk = [0_u8; 2048];
        for _ in 0..4_000 {
            let taken = match self.host(which).recv(id, &mut chunk, false) {
                Ok(received) if received.bytes > 0 => received.bytes,
                _ => return out,
            };
            out.extend(chunk.iter().take(taken).copied());
        }
        out
    }
}

/// One host: a loopback, an Ethernet interface with an address of each family,
/// and a default route out of it.
fn host(address: Ipv4, address_v6: Ipv6, mac: [u8; 6]) -> Stack {
    let mut stack = Stack::new(Config::default());
    let index = stack.add_interface(Interface::ethernet(0, b"eth0", mac, 1500));
    stack
        .set_up(index, true)
        .expect("the interface was just added");
    stack
        .add_address(
            index,
            Address {
                cidr: IpCidr::new(IpAddress::V4(address), 24),
                peer: None,
            },
        )
        .expect("an interface takes an address");
    stack
        .add_address(
            index,
            Address {
                cidr: IpCidr::new(IpAddress::V6(address_v6), 64),
                peer: None,
            },
        )
        .expect("an interface takes an address");
    stack.routes_mut().add(Route {
        destination: IpCidr::new(IpAddress::V4(Ipv4::UNSPECIFIED), 0),
        gateway: None,
        interface: index,
        metric: 100,
        origin: Origin::Static,
    });
    stack
}

/// An endpoint from an IPv4 address and a port.
pub(crate) fn at(address: Ipv4, port: u16) -> Endpoint {
    Endpoint::new(IpAddress::V4(address), port)
}

/// An endpoint from an IPv6 address and a port.
pub(crate) fn at_v6(address: Ipv6, port: u16) -> Endpoint {
    Endpoint::new(IpAddress::V6(address), port)
}

/// One host on its own, for the loopback tests.
pub(crate) struct Alone {
    /// The host.
    pub(crate) stack: Stack,
    /// The clock.
    pub(crate) now: Millis,
}

impl Alone {
    /// A host with nothing but its loopback.
    pub(crate) fn new() -> Alone {
        let mut stack = Stack::new(Config::default());
        stack.seed(0xDEAD_BEEF_CAFE_F00D);
        Alone { stack, now: 0 }
    }

    /// Drive the stack until it is idle, moving the clock when it is.
    pub(crate) fn settle(&mut self) {
        let mut ticks = 0;
        for _ in 0..BUDGET {
            if self.stack.poll_transmit(self.now).is_some() {
                // A frame for a real interface, which this host has none of.
                continue;
            }
            ticks += 1;
            if ticks > TICKS {
                return;
            }
            let Some(at) = self.stack.poll_at() else {
                return;
            };
            self.now = at.max(self.now);
            self.stack.on_timer(self.now);
        }
        panic!("the host never settled");
    }

    /// Read everything a socket has.
    pub(crate) fn drain(&mut self, id: SocketId) -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunk = [0_u8; 2048];
        for _ in 0..4_000 {
            let taken = match self.stack.recv(id, &mut chunk, false) {
                Ok(received) if received.bytes > 0 => received.bytes,
                _ => return out,
            };
            out.extend(chunk.iter().take(taken).copied());
        }
        out
    }
}
