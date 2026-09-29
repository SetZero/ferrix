//! Walking a buffer of messages, and the attributes after each fixed header.
//!
//! Both walks are iterators of `Result`, and both stop for good at the first
//! refusal: a chain whose third message is malformed has two good messages and
//! then an error, which is what the sender should be told about rather than
//! having the rest of its buffer guessed at.

use ferrix_linux_abi::netlink::{NLA_TYPE_MASK, NlAttr, NlMsgHdr, nla_align, nlmsg_align};

use crate::{Address, Error};

/// One message: its header, and the bytes after it that the header's length
/// claims. The padding to the next message is not part of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Message<'a> {
    /// The `struct nlmsghdr` the message starts with.
    pub header: NlMsgHdr,
    /// Everything after the header, inside the length the header declared.
    pub payload: &'a [u8],
}

impl<'a> Message<'a> {
    /// The first `fixed` bytes of the payload: the `struct ifinfomsg`,
    /// `ifaddrmsg`, `rtmsg` or `ndmsg` the message's type implies.
    ///
    /// `None` when the sender declared a length too short to hold it, which is
    /// the malformed message a family answers with `EINVAL`.
    #[must_use]
    pub fn body(&self, fixed: usize) -> Option<&'a [u8]> {
        self.payload.get(..fixed)
    }

    /// The attributes after a fixed body of `fixed` bytes.
    ///
    /// Linux starts them at `NLMSG_ALIGN(sizeof(body))`, not at the body's own
    /// end, so the alignment is applied here rather than left to each caller
    /// to remember.
    #[must_use]
    pub fn attributes(&self, fixed: usize) -> Attributes<'a> {
        Attributes::new(self.payload.get(nlmsg_align(fixed)..).unwrap_or(&[]))
    }
}

/// A walk over a buffer of messages.
///
/// Each step yields one message or [`Error::Truncated`], and an error is the
/// last thing it yields. The walk always ends: a message's declared length is
/// at least a header's, so every step moves forward by at least sixteen bytes.
#[derive(Clone, Debug)]
pub struct Messages<'a> {
    /// The buffer being walked.
    bytes: &'a [u8],
    /// Where the next message starts.
    at: usize,
    /// Whether the walk has ended, by running out or by refusing something.
    done: bool,
}

impl<'a> Messages<'a> {
    /// A walk over `bytes`.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Messages<'a> {
        Messages {
            bytes,
            at: 0,
            done: false,
        }
    }

    /// Stop the walk and answer `outcome`.
    fn stop(
        &mut self,
        outcome: Option<Result<Message<'a>, Error>>,
    ) -> Option<Result<Message<'a>, Error>> {
        self.done = true;
        outcome
    }
}

impl<'a> Iterator for Messages<'a> {
    type Item = Result<Message<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let rest = self.bytes.get(self.at..).unwrap_or_default();
        if rest.is_empty() {
            return self.stop(None);
        }
        // Fewer bytes than a header is a tail nobody can read. Linux drops it
        // silently; a sender is better told, and the message it would have
        // introduced is refused either way.
        let Some(header) = NlMsgHdr::from_bytes(rest) else {
            return self.stop(Some(Err(Error::Truncated)));
        };
        let Ok(len) = usize::try_from(header.len) else {
            return self.stop(Some(Err(Error::Truncated)));
        };
        // The three lengths that end a kernel: below the header, past the end,
        // and — the one both of those cover — zero.
        if len < NlMsgHdr::SIZE || len > rest.len() {
            return self.stop(Some(Err(Error::Truncated)));
        }
        let Some(payload) = rest.get(NlMsgHdr::SIZE..len) else {
            return self.stop(Some(Err(Error::Truncated)));
        };
        // At least `NlMsgHdr::SIZE`, so the walk cannot stand still.
        self.at = self.at.saturating_add(nlmsg_align(len));
        if self.at >= self.bytes.len() {
            self.done = true;
        }
        Some(Ok(Message { header, payload }))
    }
}

/// One attribute: its header and the bytes its length claims, the padding to
/// the next attribute excluded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attribute<'a> {
    /// The `struct nlattr` the attribute starts with.
    pub header: NlAttr,
    /// The payload, inside the length the header declared.
    pub payload: &'a [u8],
}

impl<'a> Attribute<'a> {
    /// The attribute's number, with the nested and byte-order bits masked off.
    #[must_use]
    pub const fn kind(&self) -> u16 {
        self.header.kind & NLA_TYPE_MASK
    }

    /// The payload as it is.
    #[must_use]
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.payload
    }

    /// The payload as the `u32` `IFLA_MTU`, `RTA_OIF` and `RTA_PRIORITY`
    /// carry: four bytes, host order.
    #[must_use]
    pub fn as_u32(&self) -> Option<u32> {
        self.payload
            .first_chunk::<4>()
            .map(|bytes| u32::from_le_bytes(*bytes))
    }

    /// The payload as the single byte `IFLA_OPERSTATE` and `IFLA_CARRIER`
    /// carry.
    #[must_use]
    pub fn as_u8(&self) -> Option<u8> {
        match self.payload {
            [byte] => Some(*byte),
            _ => None,
        }
    }

    /// The payload as the name `IFLA_IFNAME` and `IFA_LABEL` carry: the bytes
    /// before the first nul, and the whole payload if there is none.
    #[must_use]
    pub fn as_name(&self) -> &'a [u8] {
        let end = self
            .payload
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.payload.len());
        self.payload.get(..end).unwrap_or_default()
    }

    /// The payload as an address of either family, by its length.
    #[must_use]
    pub fn as_address(&self) -> Option<Address> {
        Address::parse(self.payload)
    }
}

/// A walk over the attributes after a message's fixed body.
///
/// The rules are [`Messages`]'s, with `nla_len` in place of `nlmsg_len` and a
/// four-byte header: a length below it, or past the end of what is left, ends
/// the walk with [`Error::Truncated`].
#[derive(Clone, Debug)]
pub struct Attributes<'a> {
    /// The bytes being walked.
    bytes: &'a [u8],
    /// Where the next attribute starts.
    at: usize,
    /// Whether the walk has ended.
    done: bool,
}

impl<'a> Attributes<'a> {
    /// A walk over `bytes`, which start at the first attribute.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Attributes<'a> {
        Attributes {
            bytes,
            at: 0,
            done: false,
        }
    }

    /// The first attribute of `kind`, which is the one Linux's
    /// `nla_parse` keeps when a sender repeats one.
    #[must_use]
    pub fn find(&self, kind: u16) -> Option<Attribute<'a>> {
        self.clone()
            .flatten()
            .find(|attribute| attribute.kind() == kind)
    }

    /// Stop the walk and answer `outcome`.
    fn stop(
        &mut self,
        outcome: Option<Result<Attribute<'a>, Error>>,
    ) -> Option<Result<Attribute<'a>, Error>> {
        self.done = true;
        outcome
    }
}

impl<'a> Iterator for Attributes<'a> {
    type Item = Result<Attribute<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let rest = self.bytes.get(self.at..).unwrap_or_default();
        if rest.is_empty() {
            return self.stop(None);
        }
        let Some(header) = NlAttr::from_bytes(rest) else {
            return self.stop(Some(Err(Error::Truncated)));
        };
        let len = usize::from(header.len);
        if len < NlAttr::SIZE || len > rest.len() {
            return self.stop(Some(Err(Error::Truncated)));
        }
        let Some(payload) = rest.get(NlAttr::SIZE..len) else {
            return self.stop(Some(Err(Error::Truncated)));
        };
        // At least `NlAttr::SIZE`, so this walk cannot stand still either.
        self.at = self.at.saturating_add(nla_align(len));
        if self.at >= self.bytes.len() {
            self.done = true;
        }
        Some(Ok(Attribute { header, payload }))
    }
}
