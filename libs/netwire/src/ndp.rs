//! Neighbor Discovery for IPv6, RFC 4861: the four messages a host sends and
//! answers, and their options.
//!
//! Neighbor Discovery is IPv6's ARP and router discovery, carried in `ICMPv6`.
//! A host solicits routers and neighbors and answers solicitations for its own
//! addresses; redirects are a router's to send. Every option is a type and a
//! length in eight-byte units, and a length of zero would make a walk loop for
//! ever, which is why RFC 4861 section 6.1 has a node discard such a packet and
//! why [`Options::new`] refuses one before anything reads the chain.

use crate::Error;
use crate::ethernet::Mac;
use crate::icmpv6::{Message, kind};
use crate::wire;

/// The option types the net core reads.
pub mod option_kind {
    /// Source link-layer address.
    pub const SOURCE_LINK_LAYER: u8 = 1;
    /// Target link-layer address.
    pub const TARGET_LINK_LAYER: u8 = 2;
    /// Prefix information.
    pub const PREFIX_INFORMATION: u8 = 3;
    /// Redirected header.
    pub const REDIRECTED_HEADER: u8 = 4;
    /// MTU.
    pub const MTU: u8 = 5;
}

/// A chain of options whose lengths have been checked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options<'a> {
    /// The option bytes, every length in them nonzero and in bounds.
    bytes: &'a [u8],
}

/// One option: its type, and the bytes after its type and length.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NdpOption<'a> {
    /// The option type; see [`option_kind`].
    pub kind: u8,
    /// Its data, after the type and length bytes.
    pub data: &'a [u8],
}

/// A prefix information option's fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PrefixInformation {
    /// The prefix length in bits.
    pub prefix_len: u8,
    /// The prefix is on-link.
    pub on_link: bool,
    /// The prefix may be used for stateless address autoconfiguration.
    pub autonomous: bool,
    /// Seconds the prefix stays valid.
    pub valid_lifetime: u32,
    /// Seconds addresses from it stay preferred.
    pub preferred_lifetime: u32,
    /// The prefix.
    pub prefix: [u8; 16],
}

/// Walks an [`Options`] chain.
#[derive(Clone, Debug)]
pub struct OptionsIter<'a> {
    /// What is left of the chain.
    rest: &'a [u8],
}

impl<'a> Options<'a> {
    /// Check that `bytes` is a chain of options, each with a nonzero length
    /// that stays inside the bytes.
    pub fn new(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let units = usize::from(wire::byte(rest, 1).ok_or(Error::Truncated)?);
            if units == 0 {
                return Err(Error::Malformed(
                    "a Neighbor Discovery option of length zero",
                ));
            }
            rest = rest.get(units * 8..).ok_or(Error::Truncated)?;
        }
        Ok(Options { bytes })
    }

    /// The option bytes, as they would be written out again.
    #[must_use]
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// The options, in order.
    #[must_use]
    pub const fn iter(&self) -> OptionsIter<'a> {
        OptionsIter { rest: self.bytes }
    }

    /// The first option of `kind`.
    #[must_use]
    pub fn find(&self, kind: u8) -> Option<NdpOption<'a>> {
        self.iter().find(|option| option.kind == kind)
    }

    /// The Ethernet address a source or target link-layer option carries.
    #[must_use]
    pub fn link_layer(&self, kind: u8) -> Option<Mac> {
        wire::array(self.find(kind)?.data, 0)
    }

    /// The MTU an MTU option carries.
    #[must_use]
    pub fn mtu(&self) -> Option<u32> {
        wire::be32(self.find(option_kind::MTU)?.data, 2)
    }

    /// The fields of the first prefix information option.
    #[must_use]
    pub fn prefix(&self) -> Option<PrefixInformation> {
        let data = self.find(option_kind::PREFIX_INFORMATION)?.data;
        let flags = wire::byte(data, 1)?;
        Some(PrefixInformation {
            prefix_len: wire::byte(data, 0)?,
            on_link: flags & 0x80 != 0,
            autonomous: flags & 0x40 != 0,
            valid_lifetime: wire::be32(data, 2)?,
            preferred_lifetime: wire::be32(data, 6)?,
            prefix: wire::array(data, 14)?,
        })
    }
}

impl<'a> Iterator for OptionsIter<'a> {
    type Item = NdpOption<'a>;

    fn next(&mut self) -> Option<NdpOption<'a>> {
        let kind = wire::byte(self.rest, 0)?;
        let len = usize::from(wire::byte(self.rest, 1)?) * 8;
        let (option, rest) = match (self.rest.get(..len), self.rest.get(len..)) {
            (Some(option), Some(rest)) if len > 0 => (option, rest),
            _ => {
                self.rest = &[];
                return None;
            }
        };
        self.rest = rest;
        Some(NdpOption {
            kind,
            data: option.get(2..).unwrap_or(&[]),
        })
    }
}

/// A Neighbor Discovery message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ndp<'a> {
    /// "Routers, advertise yourselves."
    RouterSolicitation {
        /// Its options.
        options: Options<'a>,
    },
    /// A router's prefixes and flags.
    RouterAdvertisement {
        /// Addresses come from `DHCPv6`.
        managed: bool,
        /// Other configuration comes from `DHCPv6`.
        other: bool,
        /// Its options.
        options: Options<'a>,
    },
    /// "Who has the target address?"
    NeighborSolicitation {
        /// The address being resolved.
        target: [u8; 16],
        /// Its options.
        options: Options<'a>,
    },
    /// "The target address is at this link-layer address."
    NeighborAdvertisement {
        /// The sender is a router.
        router: bool,
        /// This answers a solicitation.
        solicited: bool,
        /// Replace a cached link-layer address.
        override_cache: bool,
        /// The address being advertised.
        target: [u8; 16],
        /// Its options.
        options: Options<'a>,
    },
}

impl<'a> Ndp<'a> {
    /// Read a Neighbor Discovery message out of an `ICMPv6` one; `Ok(None)` when
    /// the message is of another type.
    pub fn parse(message: &Message<'a>) -> Result<Option<Self>, Error> {
        if !matches!(
            message.kind,
            kind::ROUTER_SOLICITATION
                | kind::ROUTER_ADVERTISEMENT
                | kind::NEIGHBOR_SOLICITATION
                | kind::NEIGHBOR_ADVERTISEMENT
        ) {
            return Ok(None);
        }
        if message.code != 0 {
            return Err(Error::Malformed(
                "a Neighbor Discovery code other than zero",
            ));
        }
        let body = message.body;
        let flags = wire::byte(body, 0).ok_or(Error::Truncated)?;
        let after = |at: usize| Options::new(body.get(at..).ok_or(Error::Truncated)?);
        let target = || wire::array(body, 4).ok_or(Error::Truncated);
        let ndp = match message.kind {
            kind::ROUTER_SOLICITATION => Ndp::RouterSolicitation { options: after(4)? },
            kind::ROUTER_ADVERTISEMENT => Ndp::RouterAdvertisement {
                managed: flags & 0x80 != 0,
                other: flags & 0x40 != 0,
                options: after(4)?,
            },
            kind::NEIGHBOR_SOLICITATION => Ndp::NeighborSolicitation {
                target: target()?,
                options: after(20)?,
            },
            _ => Ndp::NeighborAdvertisement {
                router: flags & 0x80 != 0,
                solicited: flags & 0x40 != 0,
                override_cache: flags & 0x20 != 0,
                target: target()?,
                options: after(20)?,
            },
        };
        Ok(Some(ndp))
    }

    /// The `ICMPv6` type this message is sent as.
    #[must_use]
    pub const fn kind(&self) -> u8 {
        match self {
            Ndp::RouterSolicitation { .. } => kind::ROUTER_SOLICITATION,
            Ndp::RouterAdvertisement { .. } => kind::ROUTER_ADVERTISEMENT,
            Ndp::NeighborSolicitation { .. } => kind::NEIGHBOR_SOLICITATION,
            Ndp::NeighborAdvertisement { .. } => kind::NEIGHBOR_ADVERTISEMENT,
        }
    }

    /// Write the message's `ICMPv6` body — flags, target and options — at the
    /// start of `out`, returning its length.
    pub fn emit_body(&self, out: &mut [u8]) -> Result<usize, Error> {
        let bit = |set: bool, value: u8| if set { value } else { 0 };
        let (flags, target, options) = match *self {
            Ndp::RouterSolicitation { options } => (0, None, options),
            Ndp::RouterAdvertisement {
                managed,
                other,
                options,
            } => (bit(managed, 0x80) | bit(other, 0x40), None, options),
            Ndp::NeighborSolicitation { target, options } => (0, Some(target), options),
            Ndp::NeighborAdvertisement {
                router,
                solicited,
                override_cache,
                target,
                options,
            } => (
                bit(router, 0x80) | bit(solicited, 0x40) | bit(override_cache, 0x20),
                Some(target),
                options,
            ),
        };
        let fixed = if target.is_some() { 20 } else { 4 };
        let len = fixed + options.as_bytes().len();
        let body = out.get_mut(..len).ok_or(Error::NoSpace)?;
        wire::put(body, 0, &[flags, 0, 0, 0]).ok_or(Error::NoSpace)?;
        if let Some(target) = target {
            wire::put(body, 4, &target).ok_or(Error::NoSpace)?;
        }
        wire::put(body, fixed, options.as_bytes()).ok_or(Error::NoSpace)?;
        Ok(len)
    }
}
