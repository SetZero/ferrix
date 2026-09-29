//! Where a packet goes next.
//!
//! One table, both families, sorted so that a lookup is a walk that stops at
//! the first match: longest prefix first, and among equal prefixes the lowest
//! metric. That is the whole of the routing policy, and it is deliberately the
//! whole: multiple tables, rules and source routing are Linux features nothing
//! Ferrix runs has asked for, and each of them is a place for a packet to go
//! somewhere nobody predicted.

use alloc::vec::Vec;

use crate::addr::{IpAddress, IpCidr};

/// Where a route came from, which `ip route` prints and `/proc/net/route`
/// reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    /// Added by the kernel when an address was configured.
    Kernel,
    /// Added by whatever configured the interface at boot.
    Boot,
    /// Added by an administrator.
    Static,
    /// Learned from a router advertisement.
    RouterAdvertisement,
    /// Learned from a DHCP lease.
    Dhcp,
}

impl Origin {
    /// The number `RTPROT_` gives this origin.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Origin::Kernel => 2,
            Origin::Boot => 3,
            Origin::Static => 4,
            Origin::RouterAdvertisement => 9,
            Origin::Dhcp => 16,
        }
    }
}

/// One route.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Route {
    /// The prefix this route covers. A prefix of length zero is the default
    /// route.
    pub destination: IpCidr,
    /// The next hop, or `None` when the prefix is on the link.
    pub gateway: Option<IpAddress>,
    /// The interface to send by.
    pub interface: u32,
    /// Lower is preferred.
    pub metric: u32,
    /// Where the route came from.
    pub origin: Origin,
}

/// Where a packet goes and what it says it came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NextHop {
    /// The interface to send by.
    pub interface: u32,
    /// The address to resolve: the gateway, or the destination itself.
    pub address: IpAddress,
    /// Whether the destination is on the link rather than behind a gateway.
    pub on_link: bool,
}

/// The routing table.
#[derive(Clone, Debug, Default)]
pub struct Routes {
    /// The routes, most specific first.
    entries: Vec<Route>,
}

impl Routes {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Routes {
        Routes {
            entries: Vec::new(),
        }
    }

    /// Every route, in the order a lookup walks them.
    #[must_use]
    pub fn entries(&self) -> &[Route] {
        &self.entries
    }

    /// Add a route, replacing one with the same prefix, interface and
    /// gateway.
    pub fn add(&mut self, route: Route) {
        let _ = self.remove(route.destination, route.interface, route.gateway);
        let at = self
            .entries
            .iter()
            .position(|existing| is_less_specific(existing, &route))
            .unwrap_or(self.entries.len());
        self.entries.insert(at, route);
    }

    /// Remove a route. Answers whether there was one.
    pub fn remove(
        &mut self,
        destination: IpCidr,
        interface: u32,
        gateway: Option<IpAddress>,
    ) -> bool {
        let before = self.entries.len();
        self.entries.retain(|existing| {
            existing.destination != destination
                || existing.interface != interface
                || existing.gateway != gateway
        });
        self.entries.len() != before
    }

    /// Remove the first route to `destination` that also matches whichever of
    /// the interface, the gateway and the metric are given, as
    /// `fib_table_delete` does: what a delete leaves out matches anything, so
    /// `ip route del default` names only the destination. Answers whether one
    /// was removed.
    pub fn remove_matching(
        &mut self,
        destination: IpCidr,
        interface: Option<u32>,
        gateway: Option<IpAddress>,
        metric: Option<u32>,
    ) -> bool {
        let found = self.entries.iter().position(|existing| {
            existing.destination == destination
                && interface.is_none_or(|wanted| existing.interface == wanted)
                && gateway.is_none_or(|wanted| existing.gateway == Some(wanted))
                && metric.is_none_or(|wanted| existing.metric == wanted)
        });
        match found {
            Some(at) => {
                let _ = self.entries.remove(at);
                true
            }
            None => false,
        }
    }

    /// Remove every route that goes by `interface`, which is what taking an
    /// interface down means.
    pub fn remove_interface(&mut self, interface: u32) {
        self.entries.retain(|route| route.interface != interface);
    }

    /// Where a packet to `destination` goes.
    #[must_use]
    pub fn lookup(&self, destination: IpAddress) -> Option<NextHop> {
        let route = self
            .entries
            .iter()
            .find(|route| route.destination.contains(destination))?;
        Some(NextHop {
            interface: route.interface,
            address: route.gateway.unwrap_or(destination),
            on_link: route.gateway.is_none(),
        })
    }

    /// The route a packet to `destination` would take, for `ip route get` and
    /// for choosing a source address.
    #[must_use]
    pub fn route_for(&self, destination: IpAddress) -> Option<&Route> {
        self.entries
            .iter()
            .find(|route| route.destination.contains(destination))
    }
}

/// Whether `existing` should come after `candidate` in the table.
fn is_less_specific(existing: &Route, candidate: &Route) -> bool {
    match existing
        .destination
        .prefix_len()
        .cmp(&candidate.destination.prefix_len())
    {
        core::cmp::Ordering::Less => true,
        core::cmp::Ordering::Greater => false,
        core::cmp::Ordering::Equal => existing.metric > candidate.metric,
    }
}
