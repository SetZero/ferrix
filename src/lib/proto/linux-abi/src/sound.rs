//! ALSA: the requests, constants and structures `/dev/snd/controlC0` and
//! `/dev/snd/pcmC0D0p` answer.
//!
//! This is the ABI `docs/AUDIO.md` §3.4 implements in the audio iteration:
//! every PCM request and every control request the header defines, so that a
//! core can recognise and refuse each by name, and the structures of the ones
//! it answers -- the refine's masks and intervals, `hw_params`, `sw_params`,
//! `status`, `SYNC_PTR`'s halves, a transfer, and what the two nodes say they
//! are. The constants are the ones a playback card and alsa-lib's `hw`
//! plugin use, not the whole of `asound.h`: formats beyond the seven
//! virtio-snd's QEMU offers, the channel maps and the control element types
//! are left out.
//!
//! # Where the numbers come from
//!
//! `include/uapi/sound/asound.h`, which every architecture takes unchanged,
//! and `asm-generic/ioctl.h` for how a request number is put together.
//! `probe/sound.c` prints every number and layout below from the header,
//! natively for 64-bit and under `qemu-arm` for ARMv7-A, into
//! `probe/sound-64.txt` and `probe/sound-32.txt`; the tests read both files
//! and require this module to agree with every line.
//!
//! # Two widths, and one `time_t`
//!
//! A `snd_pcm_uframes_t` is an `unsigned long` and a `snd_pcm_sframes_t` a
//! `long`, so `hw_params`, `sw_params`, `status`, a transfer and the control
//! page have a layout per width, and so do the requests that carry them:
//! [`Pcm::request`] and [`Ctl::request`] take a [`Width`]. The 32-bit view is
//! a program with a 64-bit `time_t`, which is what musl and ferrousli give:
//! `asound.h` then defines `__SND_STRUCT_TIME64`, which makes every timestamp
//! 16 bytes, keeps `SYNC_PTR` at 136 bytes and one request number at both
//! widths, and moves the status and control pages' mmap offsets to their
//! `_NEW` values ([`mmap_offset_status`], [`mmap_offset_control`]). The
//! time32 forms of `STATUS`, `STATUS_EXT` and `SYNC_PTR` are not here: the
//! core never answers them, and musl retries under their numbers only after
//! an `ENOTTY` the core never gives (`docs/AUDIO.md` §3.4).
//!
//! # Reading and writing
//!
//! Every structure is read from and written into bytes, little-endian,
//! returning `None` rather than panicking when the buffer is short, and
//! refusing to write a word that does not fit a 32-bit one. Padding is not a
//! field: `write` leaves it as it was, so an answer is written into zeroes.
//! What the fields mean is the audio core's business; nothing here validates
//! a value.

use crate::input::{
    IOC_DIRSHIFT, IOC_NONE, IOC_NRSHIFT, IOC_READ, IOC_SIZESHIFT, IOC_TYPESHIFT, IOC_WRITE,
};
use crate::layout::{Field, layout, wide_layout};
use crate::socket::Width;

// ---------------------------------------------------------------------------
// Protocol versions
// ---------------------------------------------------------------------------

/// `SNDRV_PROTOCOL_VERSION(major, minor, subminor)`.
#[must_use]
pub const fn protocol_version(major: u32, minor: u32, subminor: u32) -> u32 {
    (major << 16) | (minor << 8) | subminor
}

/// `SNDRV_PCM_VERSION`, 2.0.18: what `PVERSION` on a PCM node answers.
pub const PCM_VERSION: u32 = protocol_version(2, 0, 18);
/// `SNDRV_CTL_VERSION`, 2.0.9: what `PVERSION` on a control node answers.
pub const CTL_VERSION: u32 = protocol_version(2, 0, 9);

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// The type byte of every PCM request, `'A'`.
pub const IOC_TYPE_PCM: u32 = b'A' as u32;
/// The type byte of every control request, `'U'`.
pub const IOC_TYPE_CTL: u32 = b'U' as u32;

/// `_IOC(dir, type, nr, size)`.
#[must_use]
pub const fn ioc(dir: u32, r#type: u32, nr: u32, size: usize) -> u32 {
    (dir << IOC_DIRSHIFT)
        | (r#type << IOC_TYPESHIFT)
        | (nr << IOC_NRSHIFT)
        | ((size as u32) << IOC_SIZESHIFT)
}

/// Both directions, `_IOWR`.
const IOC_READ_WRITE: u32 = IOC_READ | IOC_WRITE;

/// `sizeof(int)`, the argument of the simplest requests.
const INT: usize = 4;

/// `sizeof(struct snd_ctl_elem_id)`: the argument of `ELEM_LOCK`, `ELEM_UNLOCK`
/// and `ELEM_REMOVE`.
pub const CTL_ELEM_ID_SIZE: usize = 64;
/// `sizeof(struct snd_ctl_elem_info)`, at both widths.
pub const CTL_ELEM_INFO_SIZE: usize = 272;
/// `sizeof(struct snd_ctl_tlv)` without its data: `numid` and `length`.
pub const CTL_TLV_SIZE: usize = 8;
/// `sizeof(struct snd_hwdep_info)`.
pub const HWDEP_INFO_SIZE: usize = 220;
/// `sizeof(struct snd_rawmidi_info)`.
pub const RAWMIDI_INFO_SIZE: usize = 268;

/// `sizeof(struct snd_ctl_elem_value)` at `width`: its value union holds
/// `long`s.
#[must_use]
pub const fn ctl_elem_value_size(width: Width) -> usize {
    match width {
        Width::Bits64 => 1224,
        Width::Bits32 => 712,
    }
}

/// `sizeof(struct snd_pcm_channel_info)` at `width`: it holds an `off_t`.
#[must_use]
pub const fn channel_info_size(width: Width) -> usize {
    match width {
        Width::Bits64 => 24,
        Width::Bits32 => 16,
    }
}

/// Every request a PCM node defines, `SNDRV_PCM_IOCTL_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pcm {
    /// `PVERSION`: the protocol version, [`PCM_VERSION`].
    Pversion,
    /// `INFO`: a [`PcmInfo`].
    Info,
    /// `TSTAMP`: obsolete since 2.0.12.
    Tstamp,
    /// `TTSTAMP`: which clock `tstamp` is read from.
    Ttstamp,
    /// `USER_PVERSION`: the version the program speaks.
    UserPversion,
    /// `HW_REFINE`: narrow a [`HwParams`] space.
    HwRefine,
    /// `HW_PARAMS`: choose one configuration from it.
    HwParams,
    /// `HW_FREE`: give the configuration up.
    HwFree,
    /// `SW_PARAMS`: a [`SwParams`].
    SwParams,
    /// `STATUS`: a [`Status`], for programs older than 2.0.13.
    Status,
    /// `DELAY`: frames between the program and the speaker.
    Delay,
    /// `HWSYNC`: bring `hw_ptr` up to date.
    Hwsync,
    /// `SYNC_PTR`: a [`SyncPtr`], in place of the mapped pages.
    SyncPtr,
    /// `STATUS_EXT`: a [`Status`], with the audio timestamp asked for.
    StatusExt,
    /// `CHANNEL_INFO`: where a channel lies in the mapped buffer.
    ChannelInfo,
    /// `PREPARE`.
    Prepare,
    /// `RESET`.
    Reset,
    /// `START`.
    Start,
    /// `DROP`: stop at once, discarding what is queued.
    Drop,
    /// `DRAIN`: stop once what is queued has played.
    Drain,
    /// `PAUSE`: pause or resume, by the argument's value.
    Pause,
    /// `REWIND`: move `appl_ptr` back.
    Rewind,
    /// `RESUME`: after a suspend.
    Resume,
    /// `XRUN`: force an underrun.
    Xrun,
    /// `FORWARD`: move `appl_ptr` on.
    Forward,
    /// `WRITEI_FRAMES`: an [`Xferi`] of interleaved frames to play.
    WriteiFrames,
    /// `READI_FRAMES`: an [`Xferi`] of interleaved frames recorded.
    ReadiFrames,
    /// `WRITEN_FRAMES`: frames to play, one buffer per channel.
    WritenFrames,
    /// `READN_FRAMES`: frames recorded, one buffer per channel.
    ReadnFrames,
    /// `LINK`: start and stop with another stream, by descriptor.
    Link,
    /// `UNLINK`.
    Unlink,
}

impl Pcm {
    /// Every request, in the header's order.
    pub const ALL: [Self; 31] = [
        Self::Pversion,
        Self::Info,
        Self::Tstamp,
        Self::Ttstamp,
        Self::UserPversion,
        Self::HwRefine,
        Self::HwParams,
        Self::HwFree,
        Self::SwParams,
        Self::Status,
        Self::Delay,
        Self::Hwsync,
        Self::SyncPtr,
        Self::StatusExt,
        Self::ChannelInfo,
        Self::Prepare,
        Self::Reset,
        Self::Start,
        Self::Drop,
        Self::Drain,
        Self::Pause,
        Self::Rewind,
        Self::Resume,
        Self::Xrun,
        Self::Forward,
        Self::WriteiFrames,
        Self::ReadiFrames,
        Self::WritenFrames,
        Self::ReadnFrames,
        Self::Link,
        Self::Unlink,
    ];

    /// Its name after `SNDRV_PCM_IOCTL_`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pversion => "PVERSION",
            Self::Info => "INFO",
            Self::Tstamp => "TSTAMP",
            Self::Ttstamp => "TTSTAMP",
            Self::UserPversion => "USER_PVERSION",
            Self::HwRefine => "HW_REFINE",
            Self::HwParams => "HW_PARAMS",
            Self::HwFree => "HW_FREE",
            Self::SwParams => "SW_PARAMS",
            Self::Status => "STATUS",
            Self::Delay => "DELAY",
            Self::Hwsync => "HWSYNC",
            Self::SyncPtr => "SYNC_PTR",
            Self::StatusExt => "STATUS_EXT",
            Self::ChannelInfo => "CHANNEL_INFO",
            Self::Prepare => "PREPARE",
            Self::Reset => "RESET",
            Self::Start => "START",
            Self::Drop => "DROP",
            Self::Drain => "DRAIN",
            Self::Pause => "PAUSE",
            Self::Rewind => "REWIND",
            Self::Resume => "RESUME",
            Self::Xrun => "XRUN",
            Self::Forward => "FORWARD",
            Self::WriteiFrames => "WRITEI_FRAMES",
            Self::ReadiFrames => "READI_FRAMES",
            Self::WritenFrames => "WRITEN_FRAMES",
            Self::ReadnFrames => "READN_FRAMES",
            Self::Link => "LINK",
            Self::Unlink => "UNLINK",
        }
    }

    /// Its direction, number and argument size at `width`.
    const fn parts(self, width: Width) -> (u32, u32, usize) {
        let word = width.bytes();
        match self {
            Self::Pversion => (IOC_READ, 0x00, INT),
            Self::Info => (IOC_READ, 0x01, <PcmInfo as Field>::SIZE),
            Self::Tstamp => (IOC_WRITE, 0x02, INT),
            Self::Ttstamp => (IOC_WRITE, 0x03, INT),
            Self::UserPversion => (IOC_WRITE, 0x04, INT),
            Self::HwRefine => (IOC_READ_WRITE, 0x10, HwParams::size(width)),
            Self::HwParams => (IOC_READ_WRITE, 0x11, HwParams::size(width)),
            Self::HwFree => (IOC_NONE, 0x12, 0),
            Self::SwParams => (IOC_READ_WRITE, 0x13, SwParams::size(width)),
            Self::Status => (IOC_READ, 0x20, Status::size(width)),
            Self::Delay => (IOC_READ, 0x21, word),
            Self::Hwsync => (IOC_NONE, 0x22, 0),
            Self::SyncPtr => (IOC_READ_WRITE, 0x23, SyncPtr::size(width)),
            Self::StatusExt => (IOC_READ_WRITE, 0x24, Status::size(width)),
            Self::ChannelInfo => (IOC_READ, 0x32, channel_info_size(width)),
            Self::Prepare => (IOC_NONE, 0x40, 0),
            Self::Reset => (IOC_NONE, 0x41, 0),
            Self::Start => (IOC_NONE, 0x42, 0),
            Self::Drop => (IOC_NONE, 0x43, 0),
            Self::Drain => (IOC_NONE, 0x44, 0),
            Self::Pause => (IOC_WRITE, 0x45, INT),
            Self::Rewind => (IOC_WRITE, 0x46, word),
            Self::Resume => (IOC_NONE, 0x47, 0),
            Self::Xrun => (IOC_NONE, 0x48, 0),
            Self::Forward => (IOC_WRITE, 0x49, word),
            // `struct snd_xfern` holds a pointer where `snd_xferi` does, so
            // the two are the same size.
            Self::WriteiFrames => (IOC_WRITE, 0x50, Xferi::size(width)),
            Self::ReadiFrames => (IOC_READ, 0x51, Xferi::size(width)),
            Self::WritenFrames => (IOC_WRITE, 0x52, Xferi::size(width)),
            Self::ReadnFrames => (IOC_READ, 0x53, Xferi::size(width)),
            Self::Link => (IOC_WRITE, 0x60, INT),
            Self::Unlink => (IOC_NONE, 0x61, 0),
        }
    }

    /// Its request number at `width`.
    #[must_use]
    pub const fn request(self, width: Width) -> u32 {
        let (dir, nr, size) = self.parts(width);
        ioc(dir, IOC_TYPE_PCM, nr, size)
    }

    /// The request `request` is at `width`, if a PCM node defines it.
    #[must_use]
    pub fn from_request(width: Width, request: u32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|pcm| pcm.request(width) == request)
    }
}

/// Every request a control node defines, `SNDRV_CTL_IOCTL_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctl {
    /// `PVERSION`: the protocol version, [`CTL_VERSION`].
    Pversion,
    /// `CARD_INFO`: a [`CtlCardInfo`].
    CardInfo,
    /// `ELEM_LIST`: a [`CtlElemList`].
    ElemList,
    /// `ELEM_INFO`: one element's description.
    ElemInfo,
    /// `ELEM_READ`: one element's value.
    ElemRead,
    /// `ELEM_WRITE`: set one element's value.
    ElemWrite,
    /// `ELEM_LOCK`.
    ElemLock,
    /// `ELEM_UNLOCK`.
    ElemUnlock,
    /// `SUBSCRIBE_EVENTS`: read events from the node, or stop.
    SubscribeEvents,
    /// `ELEM_ADD`: a user element.
    ElemAdd,
    /// `ELEM_REPLACE`.
    ElemReplace,
    /// `ELEM_REMOVE`.
    ElemRemove,
    /// `TLV_READ`: an element's dB scale or channel map.
    TlvRead,
    /// `TLV_WRITE`.
    TlvWrite,
    /// `TLV_COMMAND`.
    TlvCommand,
    /// `HWDEP_NEXT_DEVICE`.
    HwdepNextDevice,
    /// `HWDEP_INFO`.
    HwdepInfo,
    /// `PCM_NEXT_DEVICE`: the next PCM device after the argument, or -1.
    PcmNextDevice,
    /// `PCM_INFO`: a [`PcmInfo`] for the device, subdevice and stream asked.
    PcmInfo,
    /// `PCM_PREFER_SUBDEVICE`: the subdevice the next open should take.
    PcmPreferSubdevice,
    /// `RAWMIDI_NEXT_DEVICE`.
    RawmidiNextDevice,
    /// `RAWMIDI_INFO`.
    RawmidiInfo,
    /// `RAWMIDI_PREFER_SUBDEVICE`.
    RawmidiPreferSubdevice,
    /// `UMP_NEXT_DEVICE`.
    UmpNextDevice,
    /// `POWER`.
    Power,
    /// `POWER_STATE`.
    PowerState,
}

impl Ctl {
    /// Every request the tests pin, in the header's order. The header's
    /// `UMP_ENDPOINT_INFO` and `UMP_BLOCK_INFO` are left out: a card without
    /// UMP devices never reaches them.
    pub const ALL: [Self; 26] = [
        Self::Pversion,
        Self::CardInfo,
        Self::ElemList,
        Self::ElemInfo,
        Self::ElemRead,
        Self::ElemWrite,
        Self::ElemLock,
        Self::ElemUnlock,
        Self::SubscribeEvents,
        Self::ElemAdd,
        Self::ElemReplace,
        Self::ElemRemove,
        Self::TlvRead,
        Self::TlvWrite,
        Self::TlvCommand,
        Self::HwdepNextDevice,
        Self::HwdepInfo,
        Self::PcmNextDevice,
        Self::PcmInfo,
        Self::PcmPreferSubdevice,
        Self::RawmidiNextDevice,
        Self::RawmidiInfo,
        Self::RawmidiPreferSubdevice,
        Self::UmpNextDevice,
        Self::Power,
        Self::PowerState,
    ];

    /// Its name after `SNDRV_CTL_IOCTL_`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pversion => "PVERSION",
            Self::CardInfo => "CARD_INFO",
            Self::ElemList => "ELEM_LIST",
            Self::ElemInfo => "ELEM_INFO",
            Self::ElemRead => "ELEM_READ",
            Self::ElemWrite => "ELEM_WRITE",
            Self::ElemLock => "ELEM_LOCK",
            Self::ElemUnlock => "ELEM_UNLOCK",
            Self::SubscribeEvents => "SUBSCRIBE_EVENTS",
            Self::ElemAdd => "ELEM_ADD",
            Self::ElemReplace => "ELEM_REPLACE",
            Self::ElemRemove => "ELEM_REMOVE",
            Self::TlvRead => "TLV_READ",
            Self::TlvWrite => "TLV_WRITE",
            Self::TlvCommand => "TLV_COMMAND",
            Self::HwdepNextDevice => "HWDEP_NEXT_DEVICE",
            Self::HwdepInfo => "HWDEP_INFO",
            Self::PcmNextDevice => "PCM_NEXT_DEVICE",
            Self::PcmInfo => "PCM_INFO",
            Self::PcmPreferSubdevice => "PCM_PREFER_SUBDEVICE",
            Self::RawmidiNextDevice => "RAWMIDI_NEXT_DEVICE",
            Self::RawmidiInfo => "RAWMIDI_INFO",
            Self::RawmidiPreferSubdevice => "RAWMIDI_PREFER_SUBDEVICE",
            Self::UmpNextDevice => "UMP_NEXT_DEVICE",
            Self::Power => "POWER",
            Self::PowerState => "POWER_STATE",
        }
    }

    /// Its direction, number and argument size at `width`.
    const fn parts(self, width: Width) -> (u32, u32, usize) {
        match self {
            Self::Pversion => (IOC_READ, 0x00, INT),
            Self::CardInfo => (IOC_READ, 0x01, <CtlCardInfo as Field>::SIZE),
            Self::ElemList => (IOC_READ_WRITE, 0x10, CtlElemList::size(width)),
            Self::ElemInfo => (IOC_READ_WRITE, 0x11, CTL_ELEM_INFO_SIZE),
            Self::ElemRead => (IOC_READ_WRITE, 0x12, ctl_elem_value_size(width)),
            Self::ElemWrite => (IOC_READ_WRITE, 0x13, ctl_elem_value_size(width)),
            Self::ElemLock => (IOC_WRITE, 0x14, CTL_ELEM_ID_SIZE),
            Self::ElemUnlock => (IOC_WRITE, 0x15, CTL_ELEM_ID_SIZE),
            Self::SubscribeEvents => (IOC_READ_WRITE, 0x16, INT),
            Self::ElemAdd => (IOC_READ_WRITE, 0x17, CTL_ELEM_INFO_SIZE),
            Self::ElemReplace => (IOC_READ_WRITE, 0x18, CTL_ELEM_INFO_SIZE),
            Self::ElemRemove => (IOC_READ_WRITE, 0x19, CTL_ELEM_ID_SIZE),
            Self::TlvRead => (IOC_READ_WRITE, 0x1a, CTL_TLV_SIZE),
            Self::TlvWrite => (IOC_READ_WRITE, 0x1b, CTL_TLV_SIZE),
            Self::TlvCommand => (IOC_READ_WRITE, 0x1c, CTL_TLV_SIZE),
            Self::HwdepNextDevice => (IOC_READ_WRITE, 0x20, INT),
            Self::HwdepInfo => (IOC_READ, 0x21, HWDEP_INFO_SIZE),
            Self::PcmNextDevice => (IOC_READ, 0x30, INT),
            Self::PcmInfo => (IOC_READ_WRITE, 0x31, <PcmInfo as Field>::SIZE),
            Self::PcmPreferSubdevice => (IOC_WRITE, 0x32, INT),
            Self::RawmidiNextDevice => (IOC_READ_WRITE, 0x40, INT),
            Self::RawmidiInfo => (IOC_READ_WRITE, 0x41, RAWMIDI_INFO_SIZE),
            Self::RawmidiPreferSubdevice => (IOC_WRITE, 0x42, INT),
            Self::UmpNextDevice => (IOC_READ_WRITE, 0x43, INT),
            Self::Power => (IOC_READ_WRITE, 0xd0, INT),
            Self::PowerState => (IOC_READ, 0xd1, INT),
        }
    }

    /// Its request number at `width`.
    #[must_use]
    pub const fn request(self, width: Width) -> u32 {
        let (dir, nr, size) = self.parts(width);
        ioc(dir, IOC_TYPE_CTL, nr, size)
    }

    /// The request `request` is at `width`, if this module names it.
    #[must_use]
    pub fn from_request(width: Width, request: u32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|ctl| ctl.request(width) == request)
    }
}

// ---------------------------------------------------------------------------
// Streams, access, formats
// ---------------------------------------------------------------------------

/// `SNDRV_PCM_STREAM_PLAYBACK`.
pub const STREAM_PLAYBACK: i32 = 0;
/// `SNDRV_PCM_STREAM_CAPTURE`.
pub const STREAM_CAPTURE: i32 = 1;
/// `SNDRV_PCM_CLASS_GENERIC`: a plain mono or stereo device.
pub const CLASS_GENERIC: i32 = 0;
/// `SNDRV_PCM_SUBCLASS_GENERIC_MIX`.
pub const SUBCLASS_GENERIC_MIX: i32 = 0;

/// `SNDRV_PCM_ACCESS_MMAP_INTERLEAVED`.
pub const ACCESS_MMAP_INTERLEAVED: u32 = 0;
/// `SNDRV_PCM_ACCESS_MMAP_NONINTERLEAVED`.
pub const ACCESS_MMAP_NONINTERLEAVED: u32 = 1;
/// `SNDRV_PCM_ACCESS_MMAP_COMPLEX`.
pub const ACCESS_MMAP_COMPLEX: u32 = 2;
/// `SNDRV_PCM_ACCESS_RW_INTERLEAVED`: `WRITEI_FRAMES`, the one version 1
/// offers.
pub const ACCESS_RW_INTERLEAVED: u32 = 3;
/// `SNDRV_PCM_ACCESS_RW_NONINTERLEAVED`.
pub const ACCESS_RW_NONINTERLEAVED: u32 = 4;

/// `SNDRV_PCM_FORMAT_S8`.
pub const FORMAT_S8: u32 = 0;
/// `SNDRV_PCM_FORMAT_U8`.
pub const FORMAT_U8: u32 = 1;
/// `SNDRV_PCM_FORMAT_S16_LE`: the one version 1 offers.
pub const FORMAT_S16_LE: u32 = 2;
/// `SNDRV_PCM_FORMAT_U16_LE`.
pub const FORMAT_U16_LE: u32 = 4;
/// `SNDRV_PCM_FORMAT_S32_LE`.
pub const FORMAT_S32_LE: u32 = 10;
/// `SNDRV_PCM_FORMAT_U32_LE`.
pub const FORMAT_U32_LE: u32 = 12;
/// `SNDRV_PCM_FORMAT_FLOAT_LE`.
pub const FORMAT_FLOAT_LE: u32 = 14;
/// `SNDRV_PCM_FORMAT_LAST`: the highest format bit a mask can carry.
pub const FORMAT_LAST: u32 = 52;
/// `SNDRV_PCM_SUBFORMAT_STD`.
pub const SUBFORMAT_STD: u32 = 0;
/// `SNDRV_PCM_SUBFORMAT_LAST`.
pub const SUBFORMAT_LAST: u32 = 3;

// ---------------------------------------------------------------------------
// The refine
// ---------------------------------------------------------------------------

/// `SNDRV_PCM_HW_PARAM_ACCESS`: a mask of `ACCESS_*`.
pub const HW_PARAM_ACCESS: u32 = 0;
/// `SNDRV_PCM_HW_PARAM_FORMAT`: a mask of `FORMAT_*`.
pub const HW_PARAM_FORMAT: u32 = 1;
/// `SNDRV_PCM_HW_PARAM_SUBFORMAT`: a mask of `SUBFORMAT_*`.
pub const HW_PARAM_SUBFORMAT: u32 = 2;
/// `SNDRV_PCM_HW_PARAM_FIRST_MASK`.
pub const HW_PARAM_FIRST_MASK: u32 = HW_PARAM_ACCESS;
/// `SNDRV_PCM_HW_PARAM_LAST_MASK`.
pub const HW_PARAM_LAST_MASK: u32 = HW_PARAM_SUBFORMAT;
/// `SNDRV_PCM_HW_PARAM_SAMPLE_BITS`: an interval, as are all that follow.
pub const HW_PARAM_SAMPLE_BITS: u32 = 8;
/// `SNDRV_PCM_HW_PARAM_FRAME_BITS`.
pub const HW_PARAM_FRAME_BITS: u32 = 9;
/// `SNDRV_PCM_HW_PARAM_CHANNELS`.
pub const HW_PARAM_CHANNELS: u32 = 10;
/// `SNDRV_PCM_HW_PARAM_RATE`.
pub const HW_PARAM_RATE: u32 = 11;
/// `SNDRV_PCM_HW_PARAM_PERIOD_TIME`, in microseconds.
pub const HW_PARAM_PERIOD_TIME: u32 = 12;
/// `SNDRV_PCM_HW_PARAM_PERIOD_SIZE`, in frames.
pub const HW_PARAM_PERIOD_SIZE: u32 = 13;
/// `SNDRV_PCM_HW_PARAM_PERIOD_BYTES`.
pub const HW_PARAM_PERIOD_BYTES: u32 = 14;
/// `SNDRV_PCM_HW_PARAM_PERIODS`.
pub const HW_PARAM_PERIODS: u32 = 15;
/// `SNDRV_PCM_HW_PARAM_BUFFER_TIME`, in microseconds.
pub const HW_PARAM_BUFFER_TIME: u32 = 16;
/// `SNDRV_PCM_HW_PARAM_BUFFER_SIZE`, in frames.
pub const HW_PARAM_BUFFER_SIZE: u32 = 17;
/// `SNDRV_PCM_HW_PARAM_BUFFER_BYTES`.
pub const HW_PARAM_BUFFER_BYTES: u32 = 18;
/// `SNDRV_PCM_HW_PARAM_TICK_TIME`, in microseconds.
pub const HW_PARAM_TICK_TIME: u32 = 19;
/// `SNDRV_PCM_HW_PARAM_FIRST_INTERVAL`.
pub const HW_PARAM_FIRST_INTERVAL: u32 = HW_PARAM_SAMPLE_BITS;
/// `SNDRV_PCM_HW_PARAM_LAST_INTERVAL`.
pub const HW_PARAM_LAST_INTERVAL: u32 = HW_PARAM_TICK_TIME;
/// `SNDRV_MASK_MAX`: bits in a [`Mask`].
pub const MASK_MAX: u32 = 256;

/// The masks in [`HwParams::masks`], `LAST_MASK - FIRST_MASK + 1`.
pub const MASKS: usize = 3;
/// The intervals in [`HwParams::intervals`], `LAST_INTERVAL - FIRST_INTERVAL + 1`.
pub const INTERVALS: usize = 12;

/// `SNDRV_PCM_HW_PARAMS_NORESAMPLE`, in [`HwParams::flags`].
pub const HW_PARAMS_NORESAMPLE: u32 = 1 << 0;
/// `SNDRV_PCM_HW_PARAMS_EXPORT_BUFFER`.
pub const HW_PARAMS_EXPORT_BUFFER: u32 = 1 << 1;
/// `SNDRV_PCM_HW_PARAMS_NO_PERIOD_WAKEUP`.
pub const HW_PARAMS_NO_PERIOD_WAKEUP: u32 = 1 << 2;
/// `SNDRV_PCM_HW_PARAMS_NO_DRAIN_SILENCE`.
pub const HW_PARAMS_NO_DRAIN_SILENCE: u32 = 1 << 3;

/// `openmin`, in [`Interval::flags`]: `min` itself is not in the interval.
pub const INTERVAL_OPENMIN: u32 = 1 << 0;
/// `openmax`: `max` itself is not in the interval.
pub const INTERVAL_OPENMAX: u32 = 1 << 1;
/// `integer`: only whole numbers are in it.
pub const INTERVAL_INTEGER: u32 = 1 << 2;
/// `empty`: nothing is in it.
pub const INTERVAL_EMPTY: u32 = 1 << 3;

// ---------------------------------------------------------------------------
// What a card can do: `hw_params.info`
// ---------------------------------------------------------------------------

/// `SNDRV_PCM_INFO_MMAP`.
pub const INFO_MMAP: u32 = 0x0000_0001;
/// `SNDRV_PCM_INFO_MMAP_VALID`.
pub const INFO_MMAP_VALID: u32 = 0x0000_0002;
/// `SNDRV_PCM_INFO_DOUBLE`.
pub const INFO_DOUBLE: u32 = 0x0000_0004;
/// `SNDRV_PCM_INFO_BATCH`: `hw_ptr` moves in periods.
pub const INFO_BATCH: u32 = 0x0000_0010;
/// `SNDRV_PCM_INFO_SYNC_APPLPTR`.
pub const INFO_SYNC_APPLPTR: u32 = 0x0000_0020;
/// `SNDRV_PCM_INFO_PERFECT_DRAIN`: no silence is needed at the end.
pub const INFO_PERFECT_DRAIN: u32 = 0x0000_0040;
/// `SNDRV_PCM_INFO_INTERLEAVED`.
pub const INFO_INTERLEAVED: u32 = 0x0000_0100;
/// `SNDRV_PCM_INFO_NONINTERLEAVED`.
pub const INFO_NONINTERLEAVED: u32 = 0x0000_0200;
/// `SNDRV_PCM_INFO_COMPLEX`.
pub const INFO_COMPLEX: u32 = 0x0000_0400;
/// `SNDRV_PCM_INFO_BLOCK_TRANSFER`.
pub const INFO_BLOCK_TRANSFER: u32 = 0x0001_0000;
/// `SNDRV_PCM_INFO_OVERRANGE`.
pub const INFO_OVERRANGE: u32 = 0x0002_0000;
/// `SNDRV_PCM_INFO_RESUME`.
pub const INFO_RESUME: u32 = 0x0004_0000;
/// `SNDRV_PCM_INFO_PAUSE`.
pub const INFO_PAUSE: u32 = 0x0008_0000;
/// `SNDRV_PCM_INFO_HALF_DUPLEX`.
pub const INFO_HALF_DUPLEX: u32 = 0x0010_0000;
/// `SNDRV_PCM_INFO_JOINT_DUPLEX`.
pub const INFO_JOINT_DUPLEX: u32 = 0x0020_0000;
/// `SNDRV_PCM_INFO_SYNC_START`.
pub const INFO_SYNC_START: u32 = 0x0040_0000;
/// `SNDRV_PCM_INFO_NO_PERIOD_WAKEUP`.
pub const INFO_NO_PERIOD_WAKEUP: u32 = 0x0080_0000;
/// `SNDRV_PCM_INFO_NO_REWINDS`.
pub const INFO_NO_REWINDS: u32 = 0x2000_0000;

// ---------------------------------------------------------------------------
// States, timestamps, SYNC_PTR, mmap
// ---------------------------------------------------------------------------

/// `SNDRV_PCM_STATE_OPEN`: opened, not configured.
pub const STATE_OPEN: i32 = 0;
/// `SNDRV_PCM_STATE_SETUP`: configured, not prepared.
pub const STATE_SETUP: i32 = 1;
/// `SNDRV_PCM_STATE_PREPARED`.
pub const STATE_PREPARED: i32 = 2;
/// `SNDRV_PCM_STATE_RUNNING`.
pub const STATE_RUNNING: i32 = 3;
/// `SNDRV_PCM_STATE_XRUN`: an underrun stopped it.
pub const STATE_XRUN: i32 = 4;
/// `SNDRV_PCM_STATE_DRAINING`.
pub const STATE_DRAINING: i32 = 5;
/// `SNDRV_PCM_STATE_PAUSED`.
pub const STATE_PAUSED: i32 = 6;
/// `SNDRV_PCM_STATE_SUSPENDED`.
pub const STATE_SUSPENDED: i32 = 7;
/// `SNDRV_PCM_STATE_DISCONNECTED`: its card went away.
pub const STATE_DISCONNECTED: i32 = 8;

/// `SNDRV_PCM_TSTAMP_NONE`, in [`SwParams::tstamp_mode`].
pub const TSTAMP_NONE: i32 = 0;
/// `SNDRV_PCM_TSTAMP_ENABLE`.
pub const TSTAMP_ENABLE: i32 = 1;
/// `SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY`: what `TTSTAMP` and
/// [`SwParams::tstamp_type`] choose.
pub const TSTAMP_TYPE_GETTIMEOFDAY: u32 = 0;
/// `SNDRV_PCM_TSTAMP_TYPE_MONOTONIC`: alsa-lib's choice.
pub const TSTAMP_TYPE_MONOTONIC: u32 = 1;
/// `SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW`.
pub const TSTAMP_TYPE_MONOTONIC_RAW: u32 = 2;

/// `SNDRV_PCM_SYNC_PTR_HWSYNC`, in [`SyncPtr::flags`]: bring `hw_ptr` up to
/// date first.
pub const SYNC_PTR_HWSYNC: u32 = 1 << 0;
/// `SNDRV_PCM_SYNC_PTR_APPL`: report `appl_ptr` rather than take it.
pub const SYNC_PTR_APPL: u32 = 1 << 1;
/// `SNDRV_PCM_SYNC_PTR_AVAIL_MIN`: report `avail_min` rather than take it.
pub const SYNC_PTR_AVAIL_MIN: u32 = 1 << 2;

/// `SNDRV_PCM_MMAP_OFFSET_DATA`: the buffer.
pub const MMAP_OFFSET_DATA: u64 = 0;
/// `SNDRV_PCM_MMAP_OFFSET_STATUS_OLD`: the status page, 64-bit and time32.
pub const MMAP_OFFSET_STATUS_OLD: u64 = 0x8000_0000;
/// `SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD`.
pub const MMAP_OFFSET_CONTROL_OLD: u64 = 0x8100_0000;
/// `SNDRV_PCM_MMAP_OFFSET_STATUS_NEW`: the status page of a 32-bit program
/// with a 64-bit `time_t`.
pub const MMAP_OFFSET_STATUS_NEW: u64 = 0x8200_0000;
/// `SNDRV_PCM_MMAP_OFFSET_CONTROL_NEW`.
pub const MMAP_OFFSET_CONTROL_NEW: u64 = 0x8300_0000;

/// `SNDRV_PCM_MMAP_OFFSET_STATUS` as a program at `width` sees it.
#[must_use]
pub const fn mmap_offset_status(width: Width) -> u64 {
    match width {
        Width::Bits64 => MMAP_OFFSET_STATUS_OLD,
        Width::Bits32 => MMAP_OFFSET_STATUS_NEW,
    }
}

/// `SNDRV_PCM_MMAP_OFFSET_CONTROL` as a program at `width` sees it.
#[must_use]
pub const fn mmap_offset_control(width: Width) -> u64 {
    match width {
        Width::Bits64 => MMAP_OFFSET_CONTROL_OLD,
        Width::Bits32 => MMAP_OFFSET_CONTROL_NEW,
    }
}

// ---------------------------------------------------------------------------
// Layouts of one width
// ---------------------------------------------------------------------------

layout! {
    /// `struct snd_interval`: one parameter's range in a refine.
    Interval = "snd_interval", 12 {
        /// Smallest value.
        min: u32 = 0 / "min",
        /// Largest value.
        max: u32 = 4 / "max",
        /// The bit-fields `openmin`, `openmax`, `integer` and `empty`, as
        /// `INTERVAL_*`.
        flags: u32 = 8 / "flags",
    }
}

layout! {
    /// `struct snd_mask`: one parameter's set of values in a refine, bit
    /// `n` of word `n / 32` for value `n`.
    Mask = "snd_mask", 32 {
        /// The bits.
        bits: [u32; 8] = 0 / "bits",
    }
}

layout! {
    /// `struct timespec` with a 64-bit `time_t`, as the program's libc lays
    /// it out in `status`, and `__snd_timespec64` in the status page. On
    /// ARMv7-A `tv_nsec` is a `long` with four bytes of padding after it; a
    /// value below 10⁹ written as eight bytes leaves that padding zero, which
    /// is how the kernel writes it too.
    Timespec = "timespec", 16 {
        /// Seconds.
        sec: i64 = 0 / "tv_sec",
        /// Nanoseconds.
        nsec: i64 = 8 / "tv_nsec",
    }
}

layout! {
    /// `struct snd_pcm_mmap_status`: the status page, and the status half of
    /// [`SyncPtr`]. One layout at both widths: on ARMv7-A `hw_ptr` is four
    /// bytes followed by four of padding, which [`MmapStatus::hw_ptr`] reads
    /// as one eight-byte word, so a value the kernel wrote reads back as it
    /// was.
    MmapStatus = "snd_pcm_mmap_status", 56 {
        /// `STATE_*`.
        state: i32 = 0 / "state",
        /// The device's position, in frames modulo `boundary`.
        hw_ptr: u64 = 8 / "hw_ptr",
        /// When `hw_ptr` last moved.
        tstamp: Timespec = 16 / "tstamp",
        /// The state before a suspend.
        suspended_state: i32 = 32 / "suspended_state",
        /// The audio timestamp.
        audio_tstamp: Timespec = 40 / "audio_tstamp",
    }
}

layout! {
    /// `struct snd_pcm_info`: what `INFO` and the control node's `PCM_INFO`
    /// answer.
    PcmInfo = "snd_pcm_info", 288 {
        /// The device number, `D` in `pcmCcDd`.
        device: u32 = 0 / "device",
        /// The subdevice.
        subdevice: u32 = 4 / "subdevice",
        /// `STREAM_*`.
        stream: i32 = 8 / "stream",
        /// The card number.
        card: i32 = 12 / "card",
        /// Its id, NUL-terminated.
        id: [u8; 64] = 16 / "id",
        /// Its name, NUL-terminated.
        name: [u8; 80] = 80 / "name",
        /// The subdevice's name, NUL-terminated.
        subname: [u8; 32] = 160 / "subname",
        /// `CLASS_*`.
        dev_class: i32 = 192 / "dev_class",
        /// `SUBCLASS_*`.
        dev_subclass: i32 = 196 / "dev_subclass",
        /// How many subdevices there are.
        subdevices_count: u32 = 200 / "subdevices_count",
        /// How many are free.
        subdevices_avail: u32 = 204 / "subdevices_avail",
        /// Formerly the hardware synchronisation id; zero.
        pad1: [u8; 16] = 208 / "pad1",
        /// Zero.
        reserved: [u8; 64] = 224 / "reserved",
    }
}

layout! {
    /// `struct snd_ctl_card_info`: what `CARD_INFO` answers. alsa-lib picks
    /// the card's configuration file by `driver`.
    CtlCardInfo = "snd_ctl_card_info", 376 {
        /// The card number.
        card: i32 = 0 / "card",
        /// Zero.
        pad: i32 = 4 / "pad",
        /// Its id, NUL-terminated.
        id: [u8; 16] = 8 / "id",
        /// The driver's name, NUL-terminated.
        driver: [u8; 16] = 24 / "driver",
        /// Its short name, NUL-terminated.
        name: [u8; 32] = 40 / "name",
        /// Its long name, NUL-terminated.
        longname: [u8; 80] = 72 / "longname",
        /// Zero.
        reserved_: [u8; 16] = 152 / "reserved_",
        /// The mixer's name, NUL-terminated.
        mixername: [u8; 80] = 168 / "mixername",
        /// Components, space-separated.
        components: [u8; 128] = 248 / "components",
    }
}

// ---------------------------------------------------------------------------
// Layouts per width
// ---------------------------------------------------------------------------

wide_layout! {
    /// `struct snd_pcm_hw_params`: a configuration space, which `HW_REFINE`
    /// narrows and `HW_PARAMS` settles.
    HwParams = "snd_pcm_hw_params", 608 / 604 {
        /// `HW_PARAMS_*`.
        flags: u32 = 0 / 0 / "flags",
        /// Access, format and subformat, indexed from `HW_PARAM_FIRST_MASK`.
        masks: [Mask; MASKS] = 4 / 4 / "masks",
        /// Reserved masks.
        mres: [Mask; 5] = 100 / 100 / "mres",
        /// The intervals, indexed from `HW_PARAM_FIRST_INTERVAL`.
        intervals: [Interval; INTERVALS] = 260 / 260 / "intervals",
        /// Reserved intervals.
        ires: [Interval; 9] = 404 / 404 / "ires",
        /// In: the parameters to refine, one bit per `HW_PARAM_*`.
        rmask: u32 = 512 / 512 / "rmask",
        /// Out: the parameters the refine changed.
        cmask: u32 = 516 / 516 / "cmask",
        /// Out: `INFO_*`.
        info: u32 = 520 / 520 / "info",
        /// Out: significant bits in a sample.
        msbits: u32 = 524 / 524 / "msbits",
        /// Out: the rate's numerator.
        rate_num: u32 = 528 / 528 / "rate_num",
        /// Out: the rate's denominator.
        rate_den: u32 = 532 / 532 / "rate_den",
        /// Out: the chip's FIFO, in frames.
        fifo_size: ulong = 536 / 536 / "fifo_size",
        /// Out: the synchronisation id.
        sync: [u8; 16] = 544 / 540 / "sync",
        /// Zero.
        reserved: [u8; 48] = 560 / 556 / "reserved",
    }
}

wide_layout! {
    /// `struct snd_pcm_sw_params`: thresholds and the pointers' wrap.
    SwParams = "snd_pcm_sw_params", 136 / 104 {
        /// `TSTAMP_*`.
        tstamp_mode: i32 = 0 / 0 / "tstamp_mode",
        /// Obsolete.
        period_step: u32 = 4 / 4 / "period_step",
        /// Obsolete.
        sleep_min: u32 = 8 / 8 / "sleep_min",
        /// Frames free before a writer wakes.
        avail_min: ulong = 16 / 12 / "avail_min",
        /// Obsolete.
        xfer_align: ulong = 24 / 16 / "xfer_align",
        /// Frames queued before a write starts the stream.
        start_threshold: ulong = 32 / 20 / "start_threshold",
        /// Frames free at which the stream stops with an underrun.
        stop_threshold: ulong = 40 / 24 / "stop_threshold",
        /// Frames of silence to keep ahead of the device.
        silence_threshold: ulong = 48 / 28 / "silence_threshold",
        /// Most silence written at once.
        silence_size: ulong = 56 / 32 / "silence_size",
        /// Where the pointers wrap.
        boundary: ulong = 64 / 36 / "boundary",
        /// The program's protocol version.
        proto: u32 = 72 / 40 / "proto",
        /// `TSTAMP_TYPE_*`.
        tstamp_type: u32 = 76 / 44 / "tstamp_type",
        /// Zero.
        reserved: [u8; 56] = 80 / 48 / "reserved",
    }
}

wide_layout! {
    /// `struct snd_pcm_status`: what `STATUS` and `STATUS_EXT` answer.
    Status = "snd_pcm_status", 152 / 128 {
        /// `STATE_*`.
        state: i32 = 0 / 0 / "state",
        /// When it last started, stopped or paused.
        trigger_tstamp: Timespec = 8 / 8 / "trigger_tstamp",
        /// When this status was taken.
        tstamp: Timespec = 24 / 24 / "tstamp",
        /// The program's position.
        appl_ptr: ulong = 40 / 40 / "appl_ptr",
        /// The device's position.
        hw_ptr: ulong = 48 / 44 / "hw_ptr",
        /// Frames between the program and the speaker.
        delay: long = 56 / 48 / "delay",
        /// Frames free.
        avail: ulong = 64 / 52 / "avail",
        /// Most frames free since the last status.
        avail_max: ulong = 72 / 56 / "avail_max",
        /// Capture overrange detections.
        overrange: ulong = 80 / 60 / "overrange",
        /// The state before a suspend.
        suspended_state: i32 = 88 / 64 / "suspended_state",
        /// In and out: which audio timestamp.
        audio_tstamp_data: u32 = 92 / 68 / "audio_tstamp_data",
        /// The audio timestamp.
        audio_tstamp: Timespec = 96 / 72 / "audio_tstamp",
        /// When the driver read the position.
        driver_tstamp: Timespec = 112 / 88 / "driver_tstamp",
        /// The audio timestamp's accuracy, in nanoseconds.
        audio_tstamp_accuracy: u32 = 128 / 104 / "audio_tstamp_accuracy",
        /// Zero.
        reserved: [u8; 20] = 132 / 108 / "reserved",
    }
}

wide_layout! {
    /// `struct snd_pcm_mmap_control`: the control page, and the control half
    /// of [`SyncPtr`]. On ARMv7-A `avail_min` follows `appl_ptr` directly,
    /// the padding the header calls "messed up" but cannot move.
    MmapControl = "snd_pcm_mmap_control", 16 / 12 {
        /// The program's position.
        appl_ptr: ulong = 0 / 0 / "appl_ptr",
        /// Frames free before a writer wakes.
        avail_min: ulong = 8 / 4 / "avail_min",
    }
}

wide_layout! {
    /// `struct snd_pcm_sync_ptr`: `SYNC_PTR`'s argument, both pages in one
    /// request. 136 bytes and one request number at both widths.
    SyncPtr = "snd_pcm_sync_ptr", 136 / 136 {
        /// `SYNC_PTR_*`.
        flags: u32 = 0 / 0 / "flags",
        /// Out: the status page.
        status: MmapStatus = 8 / 8 / "s",
        /// In, or out by the flags: the control page.
        control: MmapControl = 72 / 72 / "c",
    }
}

wide_layout! {
    /// `struct snd_xferi`: `WRITEI_FRAMES`' and `READI_FRAMES`' argument.
    Xferi = "snd_xferi", 24 / 12 {
        /// Out: frames moved, or `-errno`.
        result: long = 0 / 0 / "result",
        /// The program's buffer.
        buf: ulong = 8 / 4 / "buf",
        /// Frames asked for.
        frames: ulong = 16 / 8 / "frames",
    }
}

wide_layout! {
    /// `struct snd_ctl_elem_list`: `ELEM_LIST`'s argument.
    CtlElemList = "snd_ctl_elem_list", 80 / 72 {
        /// In: the first element to list.
        offset: u32 = 0 / 0 / "offset",
        /// In: room at `pids`, in element ids.
        space: u32 = 4 / 4 / "space",
        /// Out: ids written.
        used: u32 = 8 / 8 / "used",
        /// Out: elements there are.
        count: u32 = 12 / 12 / "count",
        /// In: where to write the ids.
        pids: ulong = 16 / 16 / "pids",
        /// Zero.
        reserved: [u8; 50] = 24 / 20 / "reserved",
    }
}
