//! Writing messages: a header, a fixed body and a list of attributes, into a
//! buffer the caller owns.
//!
//! A reply is built exactly once, into memory that already exists, and the
//! answer is how many bytes it took. Nothing here allocates, so the kernel can
//! encode a whole dump while it holds the lock over the net core, where an
//! allocation would be a bug.
//!
//! # Every length is written by this crate
//!
//! `nlmsg_len` and `nla_len` are computed from what was passed, never taken
//! from the caller: that is what makes [`Writer`]'s output walk back through
//! [`crate::Messages`] by construction, which is the property the fuzz target
//! asserts.

use ferrix_linux_abi::netlink::{
    NLM_F_MULTI, NLMSG_DONE, NLMSG_ERROR, NlAttr, NlMsgErr, NlMsgHdr, nla_align, nlmsg_align,
};

use crate::{Address, Error};

/// The payload shapes the routing family's attributes carry.
///
/// Five of them cover everything `ip` reads and writes, and each knows its own
/// length, so a caller names the shape rather than laying out bytes and
/// counting them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Value<'a> {
    /// The bytes as they are: a hardware address, or a structure of netlink's
    /// own such as `struct ifa_cacheinfo`.
    Bytes(&'a [u8]),
    /// A `u32` in host order, as `IFLA_MTU`, `RTA_OIF` and `RTA_PRIORITY`
    /// carry it.
    U32(u32),
    /// One byte, as `IFLA_OPERSTATE` and `IFLA_CARRIER` carry it.
    U8(u8),
    /// A name, written with the nul terminator `IFLA_IFNAME` and `IFA_LABEL`
    /// carry and cut at the first nul already in it.
    Name(&'a [u8]),
    /// An address of either family, in network order.
    Address(Address),
}

impl Value<'_> {
    /// How many bytes the payload takes, the attribute header excluded.
    #[must_use]
    pub fn len(self) -> usize {
        match self {
            Value::Bytes(bytes) => bytes.len(),
            Value::U32(_) => 4,
            Value::U8(_) => 1,
            Value::Name(name) => name_len(name).saturating_add(1),
            Value::Address(address) => address.bytes().len(),
        }
    }

    /// Whether the payload is empty, which only `Bytes` can be.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Write the payload at the start of `out`.
    fn write(self, out: &mut [u8]) -> Result<(), Error> {
        match self {
            Value::Bytes(bytes) => put(out, 0, bytes),
            Value::U32(value) => put(out, 0, &value.to_le_bytes()),
            Value::U8(value) => put(out, 0, &[value]),
            Value::Name(name) => {
                let cut = name_len(name);
                put(out, 0, name.get(..cut).unwrap_or_default())?;
                put(out, cut, &[0])
            }
            Value::Address(address) => put(out, 0, address.bytes()),
        }
    }
}

/// An attribute to write: its number and its payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attr<'a> {
    /// `nla_type`, with the `NLA_F_` bits if the caller wants them.
    pub kind: u16,
    /// What it carries.
    pub value: Value<'a>,
}

impl<'a> Attr<'a> {
    /// An attribute of `kind` carrying `value`.
    #[must_use]
    pub const fn new(kind: u16, value: Value<'a>) -> Attr<'a> {
        Attr { kind, value }
    }

    /// How many bytes it takes once written and padded.
    #[must_use]
    pub fn written_len(&self) -> usize {
        nla_align(NlAttr::SIZE.saturating_add(self.value.len()))
    }
}

/// Messages written one after another into a caller's buffer.
///
/// Each message is padded to `NLMSG_ALIGNTO` so the next one starts aligned,
/// which is what a receiver's own walk assumes.
#[derive(Debug)]
pub struct Writer<'a> {
    /// The buffer being written.
    out: &'a mut [u8],
    /// How much of it has been used.
    at: usize,
}

impl<'a> Writer<'a> {
    /// A writer over `out`.
    #[must_use]
    pub const fn new(out: &'a mut [u8]) -> Writer<'a> {
        Writer { out, at: 0 }
    }

    /// How many bytes have been written.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.at
    }

    /// Whether nothing has been written yet.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.at == 0
    }

    /// The bytes written so far.
    #[must_use]
    pub fn written(&self) -> &[u8] {
        self.out.get(..self.at).unwrap_or_default()
    }

    /// Write one message: `header` with its length filled in, the fixed
    /// `body`, and `attributes` each padded to `NLA_ALIGNTO`.
    ///
    /// Answers how many bytes it took, the message's own padding included, so
    /// that a caller summing the answers has the offset of the next message.
    ///
    /// The body is padded to `NLMSG_ALIGNTO` before the first attribute and
    /// the declared length counts that padding, which is where Linux's
    /// `nlmsg_end` leaves `nlmsg_len` and where a receiver's `nla_parse` looks
    /// for the attributes. Every fixed body the routing family has is a
    /// multiple of four already, so this shows only for a caller that invents
    /// one.
    ///
    /// # Errors
    ///
    /// [`Error::NoSpace`] if the rest of the buffer cannot hold it, or if a
    /// length would not fit the field that carries it. Nothing is written in
    /// that case and the writer is left where it was, so a dump that runs out
    /// of room ends with whole messages rather than half of one.
    pub fn message(
        &mut self,
        header: NlMsgHdr,
        body: &[u8],
        attributes: &[Attr<'_>],
    ) -> Result<usize, Error> {
        let mut len = NlMsgHdr::SIZE
            .checked_add(nlmsg_align(body.len()))
            .ok_or(Error::NoSpace)?;
        for attribute in attributes {
            len = len
                .checked_add(attribute.written_len())
                .ok_or(Error::NoSpace)?;
        }
        let total = nlmsg_align(len);
        let end = self.at.checked_add(total).ok_or(Error::NoSpace)?;
        let room = self.out.get_mut(self.at..end).ok_or(Error::NoSpace)?;
        // The caller's buffer may hold anything; the padding a receiver skips
        // must still be zero rather than whatever was there.
        room.fill(0);
        let header = NlMsgHdr {
            len: u32::try_from(len).map_err(|_| Error::NoSpace)?,
            ..header
        };
        put(room, 0, &header.to_bytes())?;
        put(room, NlMsgHdr::SIZE, body)?;
        let mut at = NlMsgHdr::SIZE
            .checked_add(nlmsg_align(body.len()))
            .ok_or(Error::NoSpace)?;
        for attribute in attributes {
            let payload = attribute.value.len();
            let declared = NlAttr::SIZE.checked_add(payload).ok_or(Error::NoSpace)?;
            let attribute_header = NlAttr {
                len: u16::try_from(declared).map_err(|_| Error::NoSpace)?,
                kind: attribute.kind,
            };
            put(room, at, &attribute_header.to_bytes())?;
            let payload_at = at.checked_add(NlAttr::SIZE).ok_or(Error::NoSpace)?;
            attribute
                .value
                .write(room.get_mut(payload_at..).ok_or(Error::NoSpace)?)?;
            at = at.checked_add(nla_align(declared)).ok_or(Error::NoSpace)?;
        }
        self.at = end;
        Ok(total)
    }

    /// Write the `NLMSG_ERROR` that answers `request`: `errno` as Linux
    /// carries it, negative, or zero for the acknowledgement `NLM_F_ACK` asks
    /// for.
    ///
    /// The request's header is echoed in the body, as `netlink_ack` echoes it,
    /// so a sender with several requests outstanding can tell which one this
    /// answers; its payload is not, which is what `NETLINK_CAP_ACK` asks for
    /// and what every program already handles.
    ///
    /// # Errors
    ///
    /// [`Error::NoSpace`], as [`Writer::message`].
    pub fn error(&mut self, request: NlMsgHdr, errno: i32, port: u32) -> Result<usize, Error> {
        let body = NlMsgErr {
            // Saturating, so that a caller who hands over `i32::MIN` writes a
            // strange errno rather than overflowing a negation.
            error: errno.saturating_neg(),
            msg: request,
        };
        self.message(
            NlMsgHdr {
                len: 0,
                kind: NLMSG_ERROR,
                flags: 0,
                seq: request.seq,
                pid: port,
            },
            &body.to_bytes(),
            &[],
        )
    }

    /// Write the `NLMSG_DONE` that ends a multipart dump.
    ///
    /// It carries the four bytes Linux's `netlink_dump` puts there — the
    /// dump's own return value, zero for one that finished — because a program
    /// that reads them finds a number rather than the start of the next
    /// message.
    ///
    /// # Errors
    ///
    /// [`Error::NoSpace`], as [`Writer::message`].
    pub fn done(&mut self, request: NlMsgHdr, port: u32) -> Result<usize, Error> {
        self.message(
            NlMsgHdr {
                len: 0,
                kind: NLMSG_DONE,
                flags: NLM_F_MULTI,
                seq: request.seq,
                pid: port,
            },
            &0_i32.to_le_bytes(),
            &[],
        )
    }
}

/// How many bytes of `name` are the name: everything before the first nul.
fn name_len(name: &[u8]) -> usize {
    name.iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len())
}

/// Copy `field` into `out` at `at`, or answer [`Error::NoSpace`].
fn put(out: &mut [u8], at: usize, field: &[u8]) -> Result<(), Error> {
    let end = at.checked_add(field.len()).ok_or(Error::NoSpace)?;
    out.get_mut(at..end)
        .ok_or(Error::NoSpace)?
        .copy_from_slice(field);
    Ok(())
}
