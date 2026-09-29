//! virtio-net's device protocol: its configuration space, its feature bits,
//! and the header that precedes every frame on a receive or a transmit queue.
//!
//! Virtio 1.2 §5.1 defines the network device, and each piece of it is here as
//! data rather than as a driver: [`Config`] reads `struct virtio_net_config`
//! field by field, the `FEATURE_*` constants and [`DRIVER_FEATURES`] say which
//! bits this project's driver takes, [`Header`] is `struct virtio_net_hdr_v1`,
//! and [`plan`], [`publish`], [`parse_receipt`] and [`parse_sent`] turn a frame
//! into a descriptor chain and a completion back into a length. What drives a
//! device — the order of the status protocol, two queues rather than one, which
//! buffer a completion belongs to — is `ferrix-virtio-net`'s, which runs in a
//! user process over kernel handles and so cannot live in the kernel's crates.
//!
//! # The header is twelve bytes here, and the reason is not `MRG_RXBUF`
//!
//! The header's length is the classic virtio-net bug, so the rule is written
//! out rather than assumed. Virtio 1.2 §5.1.6 defines one header — `struct
//! virtio_net_hdr_v1`, twelve bytes, `num_buffers` included — and §5.1.6.1
//! says the *legacy* interface is what leaves `num_buffers` out: there the
//! header is ten bytes unless `VIRTIO_NET_F_MRG_RXBUF` was negotiated. So the
//! condition is not "merge buffers" alone:
//!
//! ```text
//! twelve bytes   if VIRTIO_F_VERSION_1 or VIRTIO_NET_F_MRG_RXBUF
//! ten bytes      otherwise, and only on the legacy interface
//! ```
//!
//! That is what Linux computes (`vi->hdr_len` in `drivers/net/virtio_net.c`)
//! and what QEMU computes (`virtio_net_set_mrg_rx_bufs`, whose `guest_hdr_len`
//! is `sizeof(struct virtio_net_hdr_mrg_rxbuf)` whenever `version_1` is set,
//! whatever `mergeable_rx_bufs` says). Reading the rule as "twelve only with
//! `MRG_RXBUF`" shifts every frame by two bytes against a modern device: the
//! Ethernet header starts two bytes early, every field after it is wrong, and
//! nothing says so until a checksum fails somewhere else entirely.
//!
//! [`REQUIRED_FEATURES`] contains [`FEATURE_VERSION_1`], so for this driver
//! the header is always twelve bytes — but [`header_len`] implements the whole
//! rule anyway, because a constant twelve would be a coincidence rather than a
//! decision, and the next reader could not tell which.
//!
//! # What this driver negotiates, and why so little
//!
//! [`DRIVER_FEATURES`] takes [`FEATURE_MAC`], [`FEATURE_STATUS`],
//! [`FEATURE_MTU`] and [`FEATURE_SPEED_DUPLEX`] — four fields of the
//! configuration block and nothing else — plus the transport's
//! [`FEATURE_VERSION_1`] and [`FEATURE_ACCESS_PLATFORM`]. Every one of them
//! only *tells* the driver something. Everything that would oblige the driver
//! to do work is declined:
//!
//! * [`FEATURE_CSUM`] and [`FEATURE_GUEST_CSUM`] hand partial checksums across
//!   in either direction. Taking them means computing or completing a checksum
//!   from `csum_start` and `csum_offset` on every frame, and a driver that
//!   accepts them and ignores them puts unchecksummed frames on the wire. The
//!   stack above already sums every header it emits.
//! * [`FEATURE_MRG_RXBUF`] lets the device spread one frame across several
//!   receive buffers, which makes `num_buffers` mean something and turns every
//!   receive into a gather the driver must reassemble. Declining it makes a
//!   frame exactly one buffer, and [`parse_receipt`] then refuses a
//!   `num_buffers` above one rather than silently handing back a fragment.
//!   The cost is a receive buffer per frame large enough for the whole MTU,
//!   which at [`DEFAULT_MTU`] is under two kibibytes.
//! * [`FEATURE_CTRL_VQ`] adds a third queue for commands — promiscuous mode,
//!   the MAC filter, multiqueue. This driver sends none of them, so the queue
//!   would exist to be empty, and [`FEATURE_MQ`], [`FEATURE_CTRL_RX`] and
//!   [`FEATURE_CTRL_VLAN`] all need it.
//! * The segmentation and offload bits — [`FEATURE_GUEST_TSO4`] through
//!   [`FEATURE_HOST_UFO`], [`FEATURE_HOST_USO`] — mean over-sized frames in
//!   one direction or the other, which the MTU-sized buffers below could not
//!   hold. [`FEATURE_HASH_REPORT`] and [`FEATURE_RSS`] are declined for a
//!   sharper reason still: `HASH_REPORT` lengthens the header to twenty bytes
//!   (`struct virtio_net_hdr_v1_hash`), and a driver that accepted it while
//!   assuming twelve would misread every frame.
//!
//! What is left is a device that puts whole Ethernet frames in, takes whole
//! Ethernet frames out, and says what its address, its link and its MTU are.
//!
//! # Trust
//!
//! Configuration bytes, headers and completions are the device's word. A field
//! whose feature was negotiated but which lies past the end of the
//! configuration block is an error, not a zero; a completion claiming more
//! bytes than the chain could hold, or fewer than the header needs, is an
//! error; and a received header that claims an offload nobody negotiated is an
//! error, because acting on it would mean handing the stack above a frame with
//! a checksum still to be finished.

use core::fmt;

use crate::pci::{FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1};
use crate::{Buffer, DeviceConfig, PAGE_SIZE, QueueError, QueueMemory, SplitQueue};

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Feature bits, virtio 1.2 §5.1.3, checked against Linux's
// `include/uapi/linux/virtio_net.h`.
// ---------------------------------------------------------------------------

/// `VIRTIO_NET_F_CSUM`: the device takes frames with a partial checksum.
pub const FEATURE_CSUM: u64 = 1 << 0;
/// `VIRTIO_NET_F_GUEST_CSUM`: the driver takes frames with a partial checksum.
pub const FEATURE_GUEST_CSUM: u64 = 1 << 1;
/// `VIRTIO_NET_F_CTRL_GUEST_OFFLOADS`: offloads can be switched at run time
/// through the control queue.
pub const FEATURE_CTRL_GUEST_OFFLOADS: u64 = 1 << 2;
/// `VIRTIO_NET_F_MTU`: [`Config::mtu`] is the device's advised MTU.
pub const FEATURE_MTU: u64 = 1 << 3;
/// `VIRTIO_NET_F_MAC`: [`Config::mac`] is the address the device was given.
pub const FEATURE_MAC: u64 = 1 << 5;
/// `VIRTIO_NET_F_GSO`, legacy: the device takes any GSO type. Never accepted.
pub const FEATURE_GSO: u64 = 1 << 6;
/// `VIRTIO_NET_F_GUEST_TSO4`: the driver takes `TCPv4` segments to reassemble.
pub const FEATURE_GUEST_TSO4: u64 = 1 << 7;
/// `VIRTIO_NET_F_GUEST_TSO6`: the same for `TCPv6`.
pub const FEATURE_GUEST_TSO6: u64 = 1 << 8;
/// `VIRTIO_NET_F_GUEST_ECN`: the driver takes segments with ECN set.
pub const FEATURE_GUEST_ECN: u64 = 1 << 9;
/// `VIRTIO_NET_F_GUEST_UFO`: the driver takes UDP fragments to reassemble.
pub const FEATURE_GUEST_UFO: u64 = 1 << 10;
/// `VIRTIO_NET_F_HOST_TSO4`: the device segments `TCPv4` for the driver.
pub const FEATURE_HOST_TSO4: u64 = 1 << 11;
/// `VIRTIO_NET_F_HOST_TSO6`: the same for `TCPv6`.
pub const FEATURE_HOST_TSO6: u64 = 1 << 12;
/// `VIRTIO_NET_F_HOST_ECN`: the device segments frames with ECN set.
pub const FEATURE_HOST_ECN: u64 = 1 << 13;
/// `VIRTIO_NET_F_HOST_UFO`: the device fragments UDP for the driver.
pub const FEATURE_HOST_UFO: u64 = 1 << 14;
/// `VIRTIO_NET_F_MRG_RXBUF`: the device may spread one frame across several
/// receive buffers, and [`Header::num_buffers`] says how many.
pub const FEATURE_MRG_RXBUF: u64 = 1 << 15;
/// `VIRTIO_NET_F_STATUS`: [`Config::status`] is valid, so the link state can
/// be read.
pub const FEATURE_STATUS: u64 = 1 << 16;
/// `VIRTIO_NET_F_CTRL_VQ`: the device has a control queue.
pub const FEATURE_CTRL_VQ: u64 = 1 << 17;
/// `VIRTIO_NET_F_CTRL_RX`: the control queue takes receive-mode commands.
pub const FEATURE_CTRL_RX: u64 = 1 << 18;
/// `VIRTIO_NET_F_CTRL_VLAN`: the control queue takes VLAN filter commands.
pub const FEATURE_CTRL_VLAN: u64 = 1 << 19;
/// `VIRTIO_NET_F_GUEST_ANNOUNCE`: the driver is asked to announce itself after
/// a migration.
pub const FEATURE_GUEST_ANNOUNCE: u64 = 1 << 21;
/// `VIRTIO_NET_F_MQ`: the device has [`Config::max_virtqueue_pairs`] queue
/// pairs.
pub const FEATURE_MQ: u64 = 1 << 22;
/// `VIRTIO_NET_F_CTRL_MAC_ADDR`: the address can be set through the control
/// queue.
pub const FEATURE_CTRL_MAC_ADDR: u64 = 1 << 23;
/// `VIRTIO_NET_F_HOST_USO`: the device does UDP segmentation for the driver.
pub const FEATURE_HOST_USO: u64 = 1 << 56;
/// `VIRTIO_NET_F_HASH_REPORT`: the device reports a flow hash, in a header
/// twenty bytes long rather than [`HEADER_LEN`].
pub const FEATURE_HASH_REPORT: u64 = 1 << 57;
/// `VIRTIO_NET_F_GUEST_HDRLEN`: the driver states an exact `hdr_len`.
pub const FEATURE_GUEST_HDRLEN: u64 = 1 << 59;
/// `VIRTIO_NET_F_RSS`: the device steers receives by hash.
pub const FEATURE_RSS: u64 = 1 << 60;
/// `VIRTIO_NET_F_RSC_EXT`: extended receive-coalescing information.
pub const FEATURE_RSC_EXT: u64 = 1 << 61;
/// `VIRTIO_NET_F_STANDBY`: the device stands by for another with the same
/// address.
pub const FEATURE_STANDBY: u64 = 1 << 62;
/// `VIRTIO_NET_F_SPEED_DUPLEX`: [`Config::speed`] and [`Config::duplex`] are
/// valid.
pub const FEATURE_SPEED_DUPLEX: u64 = 1 << 63;

/// The features the driver accepts when the device offers them.
///
/// Every bit here reports something and obliges the driver to nothing; the
/// module documentation argues each declined bit. From the transport,
/// [`FEATURE_VERSION_1`] — required — and [`FEATURE_ACCESS_PLATFORM`], without
/// which a device behind an IOMMU refuses `FEATURES_OK` (virtio 1.2 §6.1).
pub const DRIVER_FEATURES: u64 = FEATURE_MAC
    | FEATURE_STATUS
    | FEATURE_MTU
    | FEATURE_SPEED_DUPLEX
    | FEATURE_VERSION_1
    | FEATURE_ACCESS_PLATFORM;

/// The features without which the driver gives up.
///
/// [`FEATURE_VERSION_1`], because a device without it speaks the legacy
/// interface, whose configuration layout, endianness and header length are not
/// these; and [`FEATURE_MAC`], because an address has to come from somewhere.
/// Linux invents a random one, which this driver cannot do — it has no source
/// of randomness and no business choosing an address the host does not know
/// about — so a device that will not say what its address is gets refused
/// rather than driven under a made-up name.
pub const REQUIRED_FEATURES: u64 = FEATURE_VERSION_1 | FEATURE_MAC;

// ---------------------------------------------------------------------------
// Configuration space, virtio 1.2 §5.1.4. The offsets are the field order of
// `struct virtio_net_config`, which is packed, so each is the sum of the
// widths before it.
// ---------------------------------------------------------------------------

/// Offset of `mac`, six bytes in wire order ([`FEATURE_MAC`]).
pub const CONFIG_MAC: u32 = 0;
/// Offset of `status`, after the six bytes of `mac` ([`FEATURE_STATUS`]).
pub const CONFIG_STATUS: u32 = 6;
/// Offset of `max_virtqueue_pairs`, after `status` ([`FEATURE_MQ`]).
pub const CONFIG_MAX_VIRTQUEUE_PAIRS: u32 = 8;
/// Offset of `mtu`, after `max_virtqueue_pairs` ([`FEATURE_MTU`]).
pub const CONFIG_MTU: u32 = 10;
/// Offset of `speed`, after `mtu` ([`FEATURE_SPEED_DUPLEX`]).
pub const CONFIG_SPEED: u32 = 12;
/// Offset of `duplex`, after the four bytes of `speed`
/// ([`FEATURE_SPEED_DUPLEX`]).
pub const CONFIG_DUPLEX: u32 = 16;
/// Offset of `rss_max_key_size` ([`FEATURE_RSS`]).
pub const CONFIG_RSS_MAX_KEY_SIZE: u32 = 17;
/// Offset of `rss_max_indirection_table_length` ([`FEATURE_RSS`]).
pub const CONFIG_RSS_MAX_INDIRECTION: u32 = 18;
/// Offset of `supported_hash_types` ([`FEATURE_RSS`]).
pub const CONFIG_SUPPORTED_HASH_TYPES: u32 = 20;
/// Bytes of `struct virtio_net_config` as virtio 1.2 defines it.
pub const CONFIG_LEN: u32 = 24;

/// Bytes of a MAC address.
pub const MAC_LEN: usize = 6;

/// `VIRTIO_NET_S_LINK_UP`, in [`Config::status`].
pub const STATUS_LINK_UP: u16 = 1;
/// `VIRTIO_NET_S_ANNOUNCE`: the device asks the driver to announce itself.
pub const STATUS_ANNOUNCE: u16 = 2;

/// `VIRTIO_NET_DUPLEX_HALF`.
pub const DUPLEX_HALF: u8 = 0x00;
/// `VIRTIO_NET_DUPLEX_FULL`.
pub const DUPLEX_FULL: u8 = 0x01;
/// Any other `duplex` means the device does not know; virtio 1.2 §5.1.4 names
/// no constant, and Linux writes `0xff`.
pub const DUPLEX_UNKNOWN: u8 = 0xFF;

/// A `speed` the device does not know, in megabits per second. Virtio 1.2
/// §5.1.4 makes every value from 0 to `INT_MAX` legal and every other value
/// unknown, which this one is.
pub const SPEED_UNKNOWN: u32 = 0xFFFF_FFFF;

/// `struct virtio_net_config`, each field present only if its feature was
/// negotiated.
///
/// Every field is optional: unlike virtio-blk's `capacity`, no field of a
/// network device's configuration is unconditional.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// The device's MAC address, in wire order ([`FEATURE_MAC`]).
    pub mac: Option<[u8; MAC_LEN]>,
    /// Link and announce bits ([`FEATURE_STATUS`]).
    pub status: Option<u16>,
    /// Receive and transmit queue pairs ([`FEATURE_MQ`]).
    pub max_virtqueue_pairs: Option<u16>,
    /// The advised MTU, in bytes of payload ([`FEATURE_MTU`]).
    pub mtu: Option<u16>,
    /// Link speed in megabits per second ([`FEATURE_SPEED_DUPLEX`]).
    pub speed: Option<u32>,
    /// [`DUPLEX_HALF`], [`DUPLEX_FULL`] or unknown ([`FEATURE_SPEED_DUPLEX`]).
    pub duplex: Option<u8>,
}

/// Read a field guarded by `feature` and ending at `end`, if it was
/// negotiated.
fn guarded<S: DeviceConfig + ?Sized, T>(
    source: &S,
    features: u64,
    feature: u64,
    end: u32,
    read: impl FnOnce(&S) -> T,
) -> Result<Option<T>, NetError> {
    if features & feature == 0 {
        return Ok(None);
    }
    if source.config_len() < end {
        return Err(NetError::ConfigTruncated { feature });
    }
    Ok(Some(read(source)))
}

impl Config {
    /// Read the configuration a device with `features` negotiated presents.
    ///
    /// Reading the same block twice may give two answers — a link goes down
    /// while it is being read — so a caller over a live transport compares
    /// `config_generation` before and after, and reads again if it moved.
    ///
    /// # Errors
    ///
    /// [`NetError::ConfigTruncated`] if the block ends before a field whose
    /// feature was negotiated.
    pub fn read<S: DeviceConfig + ?Sized>(source: &S, features: u64) -> Result<Self, NetError> {
        Ok(Config {
            mac: guarded(source, features, FEATURE_MAC, CONFIG_STATUS, |s| {
                let mut mac = [0_u8; MAC_LEN];
                for (index, byte) in mac.iter_mut().enumerate() {
                    // `MAC_LEN` is six, so the offset cannot overflow.
                    *byte = s.config_read8(CONFIG_MAC + index as u32);
                }
                mac
            })?,
            status: guarded(
                source,
                features,
                FEATURE_STATUS,
                CONFIG_MAX_VIRTQUEUE_PAIRS,
                |s| s.config_read16(CONFIG_STATUS),
            )?,
            max_virtqueue_pairs: guarded(source, features, FEATURE_MQ, CONFIG_MTU, |s| {
                s.config_read16(CONFIG_MAX_VIRTQUEUE_PAIRS)
            })?,
            mtu: guarded(source, features, FEATURE_MTU, CONFIG_SPEED, |s| {
                s.config_read16(CONFIG_MTU)
            })?,
            speed: guarded(source, features, FEATURE_SPEED_DUPLEX, CONFIG_DUPLEX, |s| {
                s.config_read32(CONFIG_SPEED)
            })?,
            duplex: guarded(
                source,
                features,
                FEATURE_SPEED_DUPLEX,
                CONFIG_RSS_MAX_KEY_SIZE,
                |s| s.config_read8(CONFIG_DUPLEX),
            )?,
        })
    }

    /// Whether the device says the link is up.
    ///
    /// A device without [`FEATURE_STATUS`] has no status field, and virtio 1.2
    /// §5.1.4.1 has the driver assume the link is up in that case.
    #[must_use]
    pub const fn link_up(&self) -> bool {
        match self.status {
            None => true,
            Some(status) => status & STATUS_LINK_UP != 0,
        }
    }

    /// Write the fields this configuration has into `out`, as a device
    /// presents them. Bytes past the end of `out` are dropped; fields the
    /// configuration lacks are left as they were.
    ///
    /// For the device side: the kernel serving a virtqueue, and test devices.
    pub fn encode(&self, out: &mut [u8]) {
        if let Some(mac) = self.mac {
            put(out, CONFIG_MAC, &mac);
        }
        if let Some(status) = self.status {
            put(out, CONFIG_STATUS, &status.to_le_bytes());
        }
        if let Some(pairs) = self.max_virtqueue_pairs {
            put(out, CONFIG_MAX_VIRTQUEUE_PAIRS, &pairs.to_le_bytes());
        }
        if let Some(mtu) = self.mtu {
            put(out, CONFIG_MTU, &mtu.to_le_bytes());
        }
        if let Some(speed) = self.speed {
            put(out, CONFIG_SPEED, &speed.to_le_bytes());
        }
        if let Some(duplex) = self.duplex {
            put(out, CONFIG_DUPLEX, &[duplex]);
        }
    }
}

/// Copy `bytes` into `out` at `at`, dropping whatever does not fit.
fn put(out: &mut [u8], at: u32, bytes: &[u8]) {
    let Ok(at) = usize::try_from(at) else {
        return;
    };
    for (index, value) in bytes.iter().enumerate() {
        if let Some(slot) = at.checked_add(index).and_then(|i| out.get_mut(i)) {
            *slot = *value;
        }
    }
}

// ---------------------------------------------------------------------------
// The header, virtio 1.2 §5.1.6.
// ---------------------------------------------------------------------------

/// Bytes of `struct virtio_net_hdr_v1`: the modern header, `num_buffers`
/// included.
pub const HEADER_LEN: u32 = 12;

/// Bytes of the legacy `struct virtio_net_hdr`, which has no `num_buffers`.
pub const HEADER_LEN_LEGACY: u32 = 10;

/// `VIRTIO_NET_HDR_F_NEEDS_CSUM`: the checksum at `csum_start`/`csum_offset`
/// has still to be computed.
pub const HDR_F_NEEDS_CSUM: u8 = 1;
/// `VIRTIO_NET_HDR_F_DATA_VALID`: the device has checked the checksum.
pub const HDR_F_DATA_VALID: u8 = 2;
/// `VIRTIO_NET_HDR_F_RSC_INFO`: the `csum_` fields hold coalescing counts.
pub const HDR_F_RSC_INFO: u8 = 4;

/// `VIRTIO_NET_HDR_GSO_NONE`: an ordinary frame.
pub const HDR_GSO_NONE: u8 = 0;
/// `VIRTIO_NET_HDR_GSO_TCPV4`.
pub const HDR_GSO_TCPV4: u8 = 1;
/// `VIRTIO_NET_HDR_GSO_UDP`.
pub const HDR_GSO_UDP: u8 = 3;
/// `VIRTIO_NET_HDR_GSO_TCPV6`.
pub const HDR_GSO_TCPV6: u8 = 4;
/// `VIRTIO_NET_HDR_GSO_UDP_L4`.
pub const HDR_GSO_UDP_L4: u8 = 5;
/// `VIRTIO_NET_HDR_GSO_ECN`, or'd into the type rather than naming one.
pub const HDR_GSO_ECN: u8 = 0x80;

/// How long the header is with `features` negotiated.
///
/// The rule the module documentation argues, in one place: `num_buffers` is
/// part of the header whenever the modern interface is in use, and only the
/// legacy interface without [`FEATURE_MRG_RXBUF`] leaves it out.
#[must_use]
pub const fn header_len(features: u64) -> u32 {
    if features & (FEATURE_VERSION_1 | FEATURE_MRG_RXBUF) == 0 {
        HEADER_LEN_LEGACY
    } else {
        HEADER_LEN
    }
}

/// `struct virtio_net_hdr_v1`: what precedes every frame in both directions.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Header {
    /// `HDR_F_*` bits.
    pub flags: u8,
    /// `HDR_GSO_*`: the segmentation type, [`HDR_GSO_NONE`] for a whole frame.
    pub gso_type: u8,
    /// Bytes of protocol header the device may copy to each segment.
    pub hdr_len: u16,
    /// Bytes of payload per segment.
    pub gso_size: u16,
    /// Where an unfinished checksum starts.
    pub csum_start: u16,
    /// Where it is written, counted from `csum_start`.
    pub csum_offset: u16,
    /// Receive buffers the frame spans; meaningful only with
    /// [`FEATURE_MRG_RXBUF`], and part of the bytes only when [`header_len`]
    /// is [`HEADER_LEN`].
    pub num_buffers: u16,
}

/// The byte at `at`, or zero past the end.
fn byte(bytes: &[u8], at: usize) -> u8 {
    bytes.get(at).copied().unwrap_or(0)
}

/// The little-endian `u16` at `at`, zero-filled past the end.
fn word(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([byte(bytes, at), byte(bytes, at.saturating_add(1))])
}

impl Header {
    /// The header for a whole frame with nothing offloaded: every field zero
    /// but `num_buffers`, which a driver sending a frame sets to one because
    /// the frame is one buffer.
    ///
    /// The device ignores `num_buffers` on the transmit queue (virtio 1.2
    /// §5.1.6.2), so this is a statement of fact rather than a request.
    #[must_use]
    pub const fn plain() -> Self {
        Header {
            flags: 0,
            gso_type: HDR_GSO_NONE,
            hdr_len: 0,
            gso_size: 0,
            csum_start: 0,
            csum_offset: 0,
            num_buffers: 1,
        }
    }

    /// The twelve bytes of the header, little-endian.
    ///
    /// A caller writing a header of [`HEADER_LEN_LEGACY`] bytes writes the
    /// first ten of these and drops `num_buffers`, which is exactly what the
    /// legacy layout is.
    #[must_use]
    pub const fn encode(&self) -> [u8; HEADER_LEN as usize] {
        let hdr_len = self.hdr_len.to_le_bytes();
        let gso_size = self.gso_size.to_le_bytes();
        let csum_start = self.csum_start.to_le_bytes();
        let csum_offset = self.csum_offset.to_le_bytes();
        let num_buffers = self.num_buffers.to_le_bytes();
        [
            self.flags,
            self.gso_type,
            hdr_len[0],
            hdr_len[1],
            gso_size[0],
            gso_size[1],
            csum_start[0],
            csum_start[1],
            csum_offset[0],
            csum_offset[1],
            num_buffers[0],
            num_buffers[1],
        ]
    }

    /// Read a header back out of `bytes`, as long as `features` says it is.
    ///
    /// A header of [`HEADER_LEN_LEGACY`] bytes has no `num_buffers`, and it
    /// reads as one: a legacy device without [`FEATURE_MRG_RXBUF`] puts each
    /// frame in exactly one buffer.
    ///
    /// # Errors
    ///
    /// [`NetError::HeaderTruncated`] if `bytes` is shorter than
    /// [`header_len`].
    pub fn decode(bytes: &[u8], features: u64) -> Result<Self, NetError> {
        let needed = header_len(features);
        let have = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        if have < needed {
            return Err(NetError::HeaderTruncated { have, needed });
        }
        Ok(Header {
            flags: byte(bytes, 0),
            gso_type: byte(bytes, 1),
            hdr_len: word(bytes, 2),
            gso_size: word(bytes, 4),
            csum_start: word(bytes, 6),
            csum_offset: word(bytes, 8),
            num_buffers: if needed == HEADER_LEN {
                word(bytes, 10)
            } else {
                1
            },
        })
    }

    /// Whether a device may send this header to a driver that negotiated
    /// `features`.
    ///
    /// Three checks, each of a `MUST` in virtio 1.2 §5.1.6.4, and each one the
    /// stack above cannot make for itself:
    ///
    /// 1. Without [`FEATURE_GUEST_CSUM`] the device must not set
    ///    [`HDR_F_NEEDS_CSUM`], which would mean a frame whose checksum is
    ///    still to be computed.
    /// 2. Without the `GUEST_TSO`/`GUEST_UFO` bits the device must not set a
    ///    `gso_type` other than [`HDR_GSO_NONE`], which would mean a frame
    ///    larger than any buffer offered.
    /// 3. Without [`FEATURE_MRG_RXBUF`] a frame is one buffer, so a
    ///    `num_buffers` above one says the rest of the frame is somewhere the
    ///    driver was never told about.
    ///
    /// # Errors
    ///
    /// [`NetError::UnexpectedOffload`] and [`NetError::MergedBuffers`].
    pub const fn check_received(&self, features: u64) -> Result<(), NetError> {
        let offloads = FEATURE_GUEST_TSO4
            | FEATURE_GUEST_TSO6
            | FEATURE_GUEST_ECN
            | FEATURE_GUEST_UFO
            | FEATURE_GUEST_HDRLEN;
        if (features & FEATURE_GUEST_CSUM == 0 && self.flags & HDR_F_NEEDS_CSUM != 0)
            || (features & offloads == 0 && self.gso_type != HDR_GSO_NONE)
        {
            return Err(NetError::UnexpectedOffload {
                flags: self.flags,
                gso_type: self.gso_type,
            });
        }
        if features & FEATURE_MRG_RXBUF == 0 && self.num_buffers > 1 {
            return Err(NetError::MergedBuffers {
                num_buffers: self.num_buffers,
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Limits: how large a frame is, and how large a buffer has to be.
// ---------------------------------------------------------------------------

/// Bytes of an Ethernet header: two addresses and the type, with no tag.
///
/// A virtio frame carries no frame check sequence — the device strips it — so
/// this and the MTU are the whole of it.
pub const ETHERNET_HEADER_LEN: u32 = 14;

/// The MTU assumed when the device advises none.
pub const DEFAULT_MTU: u16 = 1500;

/// The smallest MTU accepted: Linux's `ETH_MIN_MTU`, the smallest IPv4
/// datagram every host must be able to take.
pub const MIN_MTU: u16 = 68;

/// The most descriptors one frame's bytes may need.
///
/// The largest MTU a `u16` can advise is 65535, whose frame spans seventeen
/// pages, and a frame that does not start on a page boundary straddles one
/// more. A frame is never split across chains, so a device that cannot be
/// given this many segments in one chain cannot be driven at that MTU.
pub const MAX_FRAME_SEGMENTS: usize = 18;

/// The most descriptors one chain has: the header and [`MAX_FRAME_SEGMENTS`].
pub const MAX_CHAIN: usize = MAX_FRAME_SEGMENTS + 1;

/// What the device's configuration and features make a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// The MTU in force: the device's advice, or [`DEFAULT_MTU`].
    pub mtu: u16,
    /// The longest frame, header and payload: `mtu` plus
    /// [`ETHERNET_HEADER_LEN`].
    pub frame_capacity: u32,
    /// Bytes of virtio header before every frame, from [`header_len`].
    pub header_len: u32,
    /// What one receive buffer must hold: the header and a whole frame.
    pub buffer_len: u32,
}

impl Limits {
    /// The limits `config` and `features` set.
    ///
    /// # Errors
    ///
    /// [`NetError::BadMtu`] for an advised MTU below [`MIN_MTU`], which no
    /// Ethernet host may use.
    pub const fn new(config: &Config, features: u64) -> Result<Self, NetError> {
        let mtu = match config.mtu {
            Some(mtu) => mtu,
            None => DEFAULT_MTU,
        };
        if mtu < MIN_MTU {
            return Err(NetError::BadMtu(mtu));
        }
        let frame_capacity = mtu as u32 + ETHERNET_HEADER_LEN;
        let header_len = header_len(features);
        Ok(Limits {
            mtu,
            frame_capacity,
            header_len,
            buffer_len: header_len + frame_capacity,
        })
    }
}

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

/// Why a frame could not be laid out, or a header, completion or configuration
/// cannot be believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetError {
    /// The configuration block ends before a field whose feature was
    /// negotiated.
    ConfigTruncated {
        /// The feature whose field is missing.
        feature: u64,
    },
    /// The advised MTU is below [`MIN_MTU`].
    BadMtu(u16),
    /// There are fewer bytes than the negotiated header needs.
    HeaderTruncated {
        /// Bytes there are.
        have: u32,
        /// Bytes the header needs.
        needed: u32,
    },
    /// A received header claims an offload the driver did not negotiate.
    UnexpectedOffload {
        /// The header's flags.
        flags: u8,
        /// The header's `gso_type`.
        gso_type: u8,
    },
    /// A received header spreads its frame over buffers the driver never
    /// agreed to merge.
    MergedBuffers {
        /// What the header claimed.
        num_buffers: u16,
    },
    /// The device says it wrote more than the chain could hold.
    WrittenTooLong {
        /// What the device claimed.
        written: u32,
        /// What the chain holds.
        capacity: u32,
    },
    /// A frame of no bytes, which no device may be asked to send.
    EmptyFrame,
    /// The frame is longer than [`Limits::frame_capacity`], or reaches past
    /// the pages of its region.
    OutsideRegion,
    /// A device address plus a length does not fit in 64 bits.
    AddressOverflow,
    /// The frame's bytes need more descriptors than [`MAX_FRAME_SEGMENTS`].
    TooManySegments,
    /// The queue refused the chain.
    Queue(QueueError),
}

impl From<QueueError> for NetError {
    fn from(error: QueueError) -> Self {
        NetError::Queue(error)
    }
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            NetError::ConfigTruncated { feature } => write!(
                f,
                "the configuration ends before feature {feature:#x}'s field"
            ),
            NetError::BadMtu(mtu) => write!(f, "an MTU of {mtu} is below the Ethernet minimum"),
            NetError::HeaderTruncated { have, needed } => {
                write!(f, "a header of {have} bytes where {needed} are needed")
            }
            NetError::UnexpectedOffload { flags, gso_type } => write!(
                f,
                "a frame with flags {flags:#x} and gso_type {gso_type:#x}, neither negotiated"
            ),
            NetError::MergedBuffers { num_buffers } => {
                write!(f, "a frame spread over {num_buffers} buffers")
            }
            NetError::WrittenTooLong { written, capacity } => write!(
                f,
                "the device claims {written} bytes in a chain holding {capacity}"
            ),
            NetError::EmptyFrame => f.write_str("a frame of no bytes"),
            NetError::OutsideRegion => f.write_str("the frame is outside its pinned region"),
            NetError::AddressOverflow => f.write_str("a device address overflows"),
            NetError::TooManySegments => f.write_str("the frame needs too many descriptors"),
            NetError::Queue(error) => write!(f, "the queue refused the chain: {error:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Building a chain.
// ---------------------------------------------------------------------------

/// A byte range of a pinned region: `len` bytes from `offset`, in a region
/// whose page `i` the device reaches at `pages[i]`.
///
/// The addresses are what a pin returned, not physical addresses and not
/// necessarily consecutive; see `crate::blk`'s module documentation, which
/// says the same at more length.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Data<'a> {
    /// The device address of each page of the region.
    pub pages: &'a [u64],
    /// Where the frame starts, in bytes from the region's start.
    pub offset: u64,
    /// How many bytes of frame.
    pub len: u32,
}

/// The descriptors one frame's bytes need, as [`plan`] laid them out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segments {
    /// The descriptors; only the first `count` mean anything.
    buffers: [Buffer; MAX_FRAME_SEGMENTS],
    /// How many there are.
    count: usize,
    /// Their total length, which is always the whole frame.
    bytes: u32,
}

impl Segments {
    /// No bytes at all.
    #[must_use]
    pub const fn none() -> Self {
        Segments {
            buffers: [Buffer::readable(0, 0); MAX_FRAME_SEGMENTS],
            count: 0,
            bytes: 0,
        }
    }

    /// The descriptors.
    #[must_use]
    pub fn buffers(&self) -> &[Buffer] {
        self.buffers.get(..self.count).unwrap_or(&[])
    }

    /// How many descriptors.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Bytes of frame the descriptors cover.
    #[must_use]
    pub const fn bytes(&self) -> u32 {
        self.bytes
    }
}

/// The longest run of device-contiguous bytes from `at`, up to `stop`, as a
/// device address and a length.
fn run(pages: &[u64], at: u64, stop: u64) -> Result<(u64, u64), NetError> {
    let page = usize::try_from(at / PAGE_SIZE).map_err(|_| NetError::OutsideRegion)?;
    let within = at % PAGE_SIZE;
    let base = *pages.get(page).ok_or(NetError::OutsideRegion)?;
    let address = base.checked_add(within).ok_or(NetError::AddressOverflow)?;

    let mut len = (PAGE_SIZE - within).min(stop - at);
    let mut next = page;
    let mut expected = base;
    // Each step joins one more page, so the walk ends within the region.
    while at + len < stop {
        next += 1;
        expected = expected
            .checked_add(PAGE_SIZE)
            .ok_or(NetError::AddressOverflow)?;
        match pages.get(next) {
            Some(&address) if address == expected => {
                len += PAGE_SIZE.min(stop - (at + len));
            }
            _ => break,
        }
    }
    let _ = address.checked_add(len).ok_or(NetError::AddressOverflow)?;
    Ok((address, len))
}

/// Lay out the descriptors for the whole of `data`.
///
/// Each descriptor covers bytes whose device addresses are consecutive: one
/// page, or several whose addresses follow on. Unlike a block request a frame
/// is never split across chains — a device given half a frame would put half a
/// frame on the wire — so a frame needing more than [`MAX_FRAME_SEGMENTS`]
/// descriptors is an error rather than a prefix.
///
/// # Errors
///
/// [`NetError::EmptyFrame`] for no bytes, [`NetError::OutsideRegion`] if the
/// frame reaches past the pages, [`NetError::AddressOverflow`] if a page's
/// address plus a length does, and [`NetError::TooManySegments`] if the frame
/// is scattered over more pages than one chain can name.
pub fn plan(data: &Data<'_>, device_writable: bool) -> Result<Segments, NetError> {
    if data.len == 0 {
        return Err(NetError::EmptyFrame);
    }
    let region = u64::try_from(data.pages.len())
        .ok()
        .and_then(|pages| pages.checked_mul(PAGE_SIZE))
        .ok_or(NetError::OutsideRegion)?;
    let stop = data
        .offset
        .checked_add(u64::from(data.len))
        .ok_or(NetError::OutsideRegion)?;
    if stop > region {
        return Err(NetError::OutsideRegion);
    }

    let mut segments = Segments::none();
    let mut at = data.offset;
    while at < stop {
        let (address, len) = run(data.pages, at, stop)?;
        let slot = segments
            .buffers
            .get_mut(segments.count)
            .ok_or(NetError::TooManySegments)?;
        *slot = Buffer {
            address,
            // At most the frame's length, which came from a `u32`.
            len: len as u32,
            device_writable,
        };
        segments.count += 1;
        at += len;
    }
    segments.bytes = data.len;
    Ok(segments)
}

/// A chain [`publish`] put on a queue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chain {
    /// The head descriptor, which the device names on completion.
    pub head: u16,
    /// Descriptors in the chain.
    pub descriptors: u16,
    /// Bytes of frame in the chain, without the header.
    pub frame_len: u32,
    /// Bytes the whole chain covers: the header and the frame.
    pub total: u32,
}

/// Publish one chain: a virtio header of `header_len` bytes at `header`, then
/// the frame `segments` lays out.
///
/// `device_writable` is what tells a receive chain from a transmit one: on the
/// receive queue the device writes both the header and the frame, on the
/// transmit queue it reads both. A caller publishing a transmit chain has
/// already written the header bytes at `header`, because the device may read
/// them the moment this returns.
///
/// # Errors
///
/// [`NetError::AddressOverflow`] if `header` has no room for its bytes below
/// 2^64, and [`NetError::Queue`] if the queue refuses the chain —
/// [`QueueError::OutOfDescriptors`] when it is full.
pub fn publish<M: QueueMemory>(
    queue: &mut SplitQueue<M>,
    header: u64,
    header_len: u32,
    segments: &Segments,
    device_writable: bool,
) -> Result<Chain, NetError> {
    let _ = header
        .checked_add(u64::from(header_len))
        .ok_or(NetError::AddressOverflow)?;
    let total = header_len
        .checked_add(segments.bytes())
        .ok_or(NetError::AddressOverflow)?;

    let mut buffers = [Buffer::readable(0, 0); MAX_CHAIN];
    let frame = segments.buffers();
    let count = frame.len() + 1;
    let mut slots = buffers.iter_mut();
    if let Some(slot) = slots.next() {
        *slot = Buffer {
            address: header,
            len: header_len,
            device_writable,
        };
    }
    for buffer in frame {
        if let Some(slot) = slots.next() {
            *slot = *buffer;
        }
    }

    let chain = buffers.get(..count).ok_or(NetError::TooManySegments)?;
    let head = queue.add_chain(chain)?;
    Ok(Chain {
        head,
        // At most `MAX_CHAIN`.
        descriptors: count as u16,
        frame_len: segments.bytes(),
        total,
    })
}

/// What a received frame is, once its completion has been checked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Receipt {
    /// The header the device wrote.
    pub header: Header,
    /// Bytes of frame after it.
    pub len: u32,
}

/// Read a receive completion: how much of the buffer holds a frame.
///
/// `header_bytes` is what the device wrote where the chain's header
/// descriptor pointed, which the caller has copied out of its own memory.
///
/// # Errors
///
/// [`NetError::WrittenTooLong`] if the device claims more bytes than the chain
/// could hold, [`NetError::HeaderTruncated`] if it claims fewer than the
/// header needs — a frame whose header the device did not finish writing is
/// not a short frame, it is a broken device — and whatever
/// [`Header::check_received`] refuses.
pub fn parse_receipt(
    chain: &Chain,
    written: u32,
    header_bytes: &[u8],
    features: u64,
) -> Result<Receipt, NetError> {
    if written > chain.total {
        return Err(NetError::WrittenTooLong {
            written,
            capacity: chain.total,
        });
    }
    let needed = header_len(features);
    if written < needed {
        return Err(NetError::HeaderTruncated {
            have: written,
            needed,
        });
    }
    let header = Header::decode(header_bytes, features)?;
    header.check_received(features)?;
    Ok(Receipt {
        header,
        len: written - needed,
    })
}

/// Check a transmit completion.
///
/// A transmit chain is read by the device and written by nobody, so virtio 1.2
/// §5.1.6.2 has the device report zero. Devices have not always: some report
/// what they read. What can never be right is a number above what the chain
/// covers at all, which says the device was looking somewhere it was not
/// given, so that — and not equality with zero — is what is refused.
///
/// # Errors
///
/// [`NetError::WrittenTooLong`].
pub const fn parse_sent(chain: &Chain, written: u32) -> Result<(), NetError> {
    if written > chain.total {
        return Err(NetError::WrittenTooLong {
            written,
            capacity: chain.total,
        });
    }
    Ok(())
}
