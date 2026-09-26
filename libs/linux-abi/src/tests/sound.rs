//! `sound`: every number and layout against the probe's output at both
//! widths, every request taken apart and found again by its number, and each
//! structure read and written back.

use super::std::borrow::ToOwned;
use super::std::collections::BTreeMap;
use super::std::format;
use super::std::string::String;
use super::std::vec;
use super::std::vec::Vec;

use crate::input;
use crate::layout::{Field, Layout};
use crate::socket::Width;
use crate::sound::{
    self, Ctl, CtlCardInfo, CtlElemList, HwParams, Interval, Mask, MmapControl, MmapStatus, Pcm,
    PcmInfo, Status, SwParams, SyncPtr, Timespec, Xferi,
};

const PROBE_64: &str = include_str!("../../probe/sound-64.txt");
const PROBE_32: &str = include_str!("../../probe/sound-32.txt");

/// The probe's lines as `name → value`.
fn probe(text: &str) -> BTreeMap<String, u64> {
    let mut lines = BTreeMap::new();
    for line in text.lines() {
        let (name, value) = line.split_once(' ').expect("each line is `name value`");
        let value = value.parse().expect("each value is decimal");
        assert!(
            lines.insert(name.to_owned(), value).is_none(),
            "{name} printed twice"
        );
    }
    lines
}

/// The refine's constants: access, formats, parameters, masks and flags.
fn refine_values() -> Vec<(&'static str, u32)> {
    vec![
        (
            "SNDRV_PCM_ACCESS_MMAP_INTERLEAVED",
            sound::ACCESS_MMAP_INTERLEAVED,
        ),
        (
            "SNDRV_PCM_ACCESS_MMAP_NONINTERLEAVED",
            sound::ACCESS_MMAP_NONINTERLEAVED,
        ),
        ("SNDRV_PCM_ACCESS_MMAP_COMPLEX", sound::ACCESS_MMAP_COMPLEX),
        (
            "SNDRV_PCM_ACCESS_RW_INTERLEAVED",
            sound::ACCESS_RW_INTERLEAVED,
        ),
        (
            "SNDRV_PCM_ACCESS_RW_NONINTERLEAVED",
            sound::ACCESS_RW_NONINTERLEAVED,
        ),
        ("SNDRV_PCM_FORMAT_S8", sound::FORMAT_S8),
        ("SNDRV_PCM_FORMAT_U8", sound::FORMAT_U8),
        ("SNDRV_PCM_FORMAT_S16_LE", sound::FORMAT_S16_LE),
        ("SNDRV_PCM_FORMAT_U16_LE", sound::FORMAT_U16_LE),
        ("SNDRV_PCM_FORMAT_S32_LE", sound::FORMAT_S32_LE),
        ("SNDRV_PCM_FORMAT_U32_LE", sound::FORMAT_U32_LE),
        ("SNDRV_PCM_FORMAT_FLOAT_LE", sound::FORMAT_FLOAT_LE),
        ("SNDRV_PCM_FORMAT_LAST", sound::FORMAT_LAST),
        ("SNDRV_PCM_SUBFORMAT_STD", sound::SUBFORMAT_STD),
        ("SNDRV_PCM_SUBFORMAT_LAST", sound::SUBFORMAT_LAST),
        ("SNDRV_PCM_HW_PARAM_ACCESS", sound::HW_PARAM_ACCESS),
        ("SNDRV_PCM_HW_PARAM_FORMAT", sound::HW_PARAM_FORMAT),
        ("SNDRV_PCM_HW_PARAM_SUBFORMAT", sound::HW_PARAM_SUBFORMAT),
        ("SNDRV_PCM_HW_PARAM_FIRST_MASK", sound::HW_PARAM_FIRST_MASK),
        ("SNDRV_PCM_HW_PARAM_LAST_MASK", sound::HW_PARAM_LAST_MASK),
        (
            "SNDRV_PCM_HW_PARAM_SAMPLE_BITS",
            sound::HW_PARAM_SAMPLE_BITS,
        ),
        ("SNDRV_PCM_HW_PARAM_FRAME_BITS", sound::HW_PARAM_FRAME_BITS),
        ("SNDRV_PCM_HW_PARAM_CHANNELS", sound::HW_PARAM_CHANNELS),
        ("SNDRV_PCM_HW_PARAM_RATE", sound::HW_PARAM_RATE),
        (
            "SNDRV_PCM_HW_PARAM_PERIOD_TIME",
            sound::HW_PARAM_PERIOD_TIME,
        ),
        (
            "SNDRV_PCM_HW_PARAM_PERIOD_SIZE",
            sound::HW_PARAM_PERIOD_SIZE,
        ),
        (
            "SNDRV_PCM_HW_PARAM_PERIOD_BYTES",
            sound::HW_PARAM_PERIOD_BYTES,
        ),
        ("SNDRV_PCM_HW_PARAM_PERIODS", sound::HW_PARAM_PERIODS),
        (
            "SNDRV_PCM_HW_PARAM_BUFFER_TIME",
            sound::HW_PARAM_BUFFER_TIME,
        ),
        (
            "SNDRV_PCM_HW_PARAM_BUFFER_SIZE",
            sound::HW_PARAM_BUFFER_SIZE,
        ),
        (
            "SNDRV_PCM_HW_PARAM_BUFFER_BYTES",
            sound::HW_PARAM_BUFFER_BYTES,
        ),
        ("SNDRV_PCM_HW_PARAM_TICK_TIME", sound::HW_PARAM_TICK_TIME),
        (
            "SNDRV_PCM_HW_PARAM_FIRST_INTERVAL",
            sound::HW_PARAM_FIRST_INTERVAL,
        ),
        (
            "SNDRV_PCM_HW_PARAM_LAST_INTERVAL",
            sound::HW_PARAM_LAST_INTERVAL,
        ),
        ("SNDRV_MASK_MAX", sound::MASK_MAX),
        (
            "SNDRV_PCM_HW_PARAMS_NORESAMPLE",
            sound::HW_PARAMS_NORESAMPLE,
        ),
        (
            "SNDRV_PCM_HW_PARAMS_EXPORT_BUFFER",
            sound::HW_PARAMS_EXPORT_BUFFER,
        ),
        (
            "SNDRV_PCM_HW_PARAMS_NO_PERIOD_WAKEUP",
            sound::HW_PARAMS_NO_PERIOD_WAKEUP,
        ),
        (
            "SNDRV_PCM_HW_PARAMS_NO_DRAIN_SILENCE",
            sound::HW_PARAMS_NO_DRAIN_SILENCE,
        ),
        ("bit.snd_interval.openmin", sound::INTERVAL_OPENMIN),
        ("bit.snd_interval.openmax", sound::INTERVAL_OPENMAX),
        ("bit.snd_interval.integer", sound::INTERVAL_INTEGER),
        ("bit.snd_interval.empty", sound::INTERVAL_EMPTY),
    ]
}

/// The versions, what a card says it can do, timestamps and `SYNC_PTR`'s flags.
fn info_values() -> Vec<(&'static str, u32)> {
    vec![
        ("SNDRV_PCM_VERSION", sound::PCM_VERSION),
        ("SNDRV_CTL_VERSION", sound::CTL_VERSION),
        ("SNDRV_PCM_INFO_MMAP", sound::INFO_MMAP),
        ("SNDRV_PCM_INFO_MMAP_VALID", sound::INFO_MMAP_VALID),
        ("SNDRV_PCM_INFO_DOUBLE", sound::INFO_DOUBLE),
        ("SNDRV_PCM_INFO_BATCH", sound::INFO_BATCH),
        ("SNDRV_PCM_INFO_SYNC_APPLPTR", sound::INFO_SYNC_APPLPTR),
        ("SNDRV_PCM_INFO_PERFECT_DRAIN", sound::INFO_PERFECT_DRAIN),
        ("SNDRV_PCM_INFO_INTERLEAVED", sound::INFO_INTERLEAVED),
        ("SNDRV_PCM_INFO_NONINTERLEAVED", sound::INFO_NONINTERLEAVED),
        ("SNDRV_PCM_INFO_COMPLEX", sound::INFO_COMPLEX),
        ("SNDRV_PCM_INFO_BLOCK_TRANSFER", sound::INFO_BLOCK_TRANSFER),
        ("SNDRV_PCM_INFO_OVERRANGE", sound::INFO_OVERRANGE),
        ("SNDRV_PCM_INFO_RESUME", sound::INFO_RESUME),
        ("SNDRV_PCM_INFO_PAUSE", sound::INFO_PAUSE),
        ("SNDRV_PCM_INFO_HALF_DUPLEX", sound::INFO_HALF_DUPLEX),
        ("SNDRV_PCM_INFO_JOINT_DUPLEX", sound::INFO_JOINT_DUPLEX),
        ("SNDRV_PCM_INFO_SYNC_START", sound::INFO_SYNC_START),
        (
            "SNDRV_PCM_INFO_NO_PERIOD_WAKEUP",
            sound::INFO_NO_PERIOD_WAKEUP,
        ),
        ("SNDRV_PCM_INFO_NO_REWINDS", sound::INFO_NO_REWINDS),
        (
            "SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY",
            sound::TSTAMP_TYPE_GETTIMEOFDAY,
        ),
        (
            "SNDRV_PCM_TSTAMP_TYPE_MONOTONIC",
            sound::TSTAMP_TYPE_MONOTONIC,
        ),
        (
            "SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW",
            sound::TSTAMP_TYPE_MONOTONIC_RAW,
        ),
        ("SNDRV_PCM_SYNC_PTR_HWSYNC", sound::SYNC_PTR_HWSYNC),
        ("SNDRV_PCM_SYNC_PTR_APPL", sound::SYNC_PTR_APPL),
        ("SNDRV_PCM_SYNC_PTR_AVAIL_MIN", sound::SYNC_PTR_AVAIL_MIN),
    ]
}

/// The constants the header declares `int`.
fn signed_values() -> Vec<(&'static str, i32)> {
    vec![
        ("SNDRV_PCM_STREAM_PLAYBACK", sound::STREAM_PLAYBACK),
        ("SNDRV_PCM_STREAM_CAPTURE", sound::STREAM_CAPTURE),
        ("SNDRV_PCM_CLASS_GENERIC", sound::CLASS_GENERIC),
        (
            "SNDRV_PCM_SUBCLASS_GENERIC_MIX",
            sound::SUBCLASS_GENERIC_MIX,
        ),
        ("SNDRV_PCM_STATE_OPEN", sound::STATE_OPEN),
        ("SNDRV_PCM_STATE_SETUP", sound::STATE_SETUP),
        ("SNDRV_PCM_STATE_PREPARED", sound::STATE_PREPARED),
        ("SNDRV_PCM_STATE_RUNNING", sound::STATE_RUNNING),
        ("SNDRV_PCM_STATE_XRUN", sound::STATE_XRUN),
        ("SNDRV_PCM_STATE_DRAINING", sound::STATE_DRAINING),
        ("SNDRV_PCM_STATE_PAUSED", sound::STATE_PAUSED),
        ("SNDRV_PCM_STATE_SUSPENDED", sound::STATE_SUSPENDED),
        ("SNDRV_PCM_STATE_DISCONNECTED", sound::STATE_DISCONNECTED),
        ("SNDRV_PCM_TSTAMP_NONE", sound::TSTAMP_NONE),
        ("SNDRV_PCM_TSTAMP_ENABLE", sound::TSTAMP_ENABLE),
    ]
}

/// The constants of one value at both widths, by their names in the header.
fn values() -> Vec<(&'static str, u64)> {
    let mut values: Vec<(&'static str, u64)> = refine_values()
        .into_iter()
        .chain(info_values())
        .map(|(name, value)| (name, u64::from(value)))
        .collect();
    values.extend(signed_values().into_iter().map(|(name, value)| {
        (
            name,
            u64::try_from(value).expect("no negative constant here"),
        )
    }));
    values.extend([
        ("SNDRV_PCM_MMAP_OFFSET_DATA", sound::MMAP_OFFSET_DATA),
        (
            "SNDRV_PCM_MMAP_OFFSET_STATUS_OLD",
            sound::MMAP_OFFSET_STATUS_OLD,
        ),
        (
            "SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD",
            sound::MMAP_OFFSET_CONTROL_OLD,
        ),
        (
            "SNDRV_PCM_MMAP_OFFSET_STATUS_NEW",
            sound::MMAP_OFFSET_STATUS_NEW,
        ),
        (
            "SNDRV_PCM_MMAP_OFFSET_CONTROL_NEW",
            sound::MMAP_OFFSET_CONTROL_NEW,
        ),
        ("sizeof.time_t", 8),
        ("sizeof.snd_ctl_elem_id", sound::CTL_ELEM_ID_SIZE as u64),
    ]);
    values
}

/// A structure's `sizeof` line and one `offsetof` line per field.
fn layout_lines(c_name: &str, size: usize, fields: &[(&str, usize)]) -> Vec<(String, u64)> {
    let mut lines = vec![(format!("sizeof.{c_name}"), size as u64)];
    lines.extend(
        fields
            .iter()
            .map(|(field, at)| (format!("offsetof.{c_name}.{field}"), *at as u64)),
    );
    lines
}

fn layouts<L: Layout>() -> Vec<(String, u64)> {
    layout_lines(L::C_NAME, L::SIZE, L::FIELDS)
}

/// The fields of a structure nested in `SYNC_PTR` at `at`, named by the path
/// the probe prints.
fn nested(path: &str, at: usize, fields: &[(&str, usize)]) -> Vec<(String, u64)> {
    fields
        .iter()
        .map(|(field, offset)| {
            (
                format!("offsetof.snd_pcm_sync_ptr.{path}.{field}"),
                (at + offset) as u64,
            )
        })
        .collect()
}

/// Where `field` is in `fields`.
fn offset(fields: &[(&str, usize)], field: &str) -> usize {
    fields
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, at)| *at)
        .expect("the field is there")
}

/// What this module says the probe printed at `width`.
fn expected(width: Width) -> BTreeMap<String, u64> {
    let mut lines: Vec<(String, u64)> = values()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    lines.push(("sizeof.long".to_owned(), width.bytes() as u64));
    lines.push((
        "SNDRV_PCM_MMAP_OFFSET_STATUS".to_owned(),
        sound::mmap_offset_status(width),
    ));
    lines.push((
        "SNDRV_PCM_MMAP_OFFSET_CONTROL".to_owned(),
        sound::mmap_offset_control(width),
    ));
    lines.push((
        "sizeof.snd_pcm_channel_info".to_owned(),
        sound::channel_info_size(width) as u64,
    ));
    for pcm in Pcm::ALL {
        lines.push((
            format!("SNDRV_PCM_IOCTL_{}", pcm.name()),
            u64::from(pcm.request(width)),
        ));
    }
    for ctl in Ctl::ALL {
        lines.push((
            format!("SNDRV_CTL_IOCTL_{}", ctl.name()),
            u64::from(ctl.request(width)),
        ));
    }
    lines.extend(layouts::<Interval>());
    lines.push(("sizeof.snd_mask".to_owned(), Mask::SIZE as u64));
    lines.extend(layouts::<Timespec>());
    lines.extend(layouts::<MmapStatus>());
    lines.extend(layouts::<PcmInfo>());
    lines.extend(layouts::<CtlCardInfo>());
    lines.extend(layout_lines(
        HwParams::C_NAME,
        HwParams::size(width),
        HwParams::fields(width),
    ));
    lines.extend(layout_lines(
        SwParams::C_NAME,
        SwParams::size(width),
        SwParams::fields(width),
    ));
    lines.extend(layout_lines(
        Status::C_NAME,
        Status::size(width),
        Status::fields(width),
    ));
    lines.extend(layout_lines(
        MmapControl::C_NAME,
        MmapControl::size(width),
        MmapControl::fields(width),
    ));
    lines.extend(layout_lines(
        SyncPtr::C_NAME,
        SyncPtr::size(width),
        SyncPtr::fields(width),
    ));
    lines.extend(layout_lines(
        Xferi::C_NAME,
        Xferi::size(width),
        Xferi::fields(width),
    ));
    lines.extend(layout_lines(
        CtlElemList::C_NAME,
        CtlElemList::size(width),
        CtlElemList::fields(width),
    ));

    // SYNC_PTR's halves, where the probe names each field by its path.
    let status = offset(SyncPtr::fields(width), "s");
    let control = offset(SyncPtr::fields(width), "c");
    lines.extend(nested("s.status", status, MmapStatus::FIELDS));
    let tstamp = status + offset(MmapStatus::FIELDS, "tstamp");
    lines.extend(nested("s.status.tstamp", tstamp, Timespec::FIELDS));
    lines.extend(nested("c.control", control, MmapControl::fields(width)));

    let count = lines.len();
    let map: BTreeMap<String, u64> = lines.into_iter().collect();
    assert_eq!(map.len(), count, "a line is defined twice");
    map
}

/// Every line on which this module and the probe disagree, one per line.
fn disagreements(width: Width, text: &str) -> Vec<String> {
    let ours = expected(width);
    let theirs = probe(text);
    let mut names: Vec<&String> = ours.keys().chain(theirs.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|name| ours.get(*name) != theirs.get(*name))
        .map(|name| {
            format!(
                "{name}: this module says {:?}, the probe printed {:?}",
                ours.get(name),
                theirs.get(name)
            )
        })
        .collect()
}

#[test]
fn every_number_and_layout_matches_the_probe_at_64_bits() {
    assert_eq!(disagreements(Width::Bits64, PROBE_64), Vec::<String>::new());
}

#[test]
fn every_number_and_layout_matches_the_probe_at_32_bits() {
    assert_eq!(disagreements(Width::Bits32, PROBE_32), Vec::<String>::new());
}

/// The hazards the module comment and `docs/AUDIO.md` §3.4 name, as the
/// probe printed them.
#[test]
fn sync_ptr_is_one_request_and_the_control_page_is_not_one_layout() {
    let wide = probe(PROBE_64);
    let narrow = probe(PROBE_32);
    assert_eq!(
        wide["SNDRV_PCM_IOCTL_SYNC_PTR"],
        narrow["SNDRV_PCM_IOCTL_SYNC_PTR"]
    );
    assert_eq!(narrow["offsetof.snd_pcm_sync_ptr.c.control.avail_min"], 76);
    assert_eq!(wide["offsetof.snd_pcm_sync_ptr.c.control.avail_min"], 80);
    assert_ne!(
        wide["SNDRV_PCM_MMAP_OFFSET_STATUS"],
        narrow["SNDRV_PCM_MMAP_OFFSET_STATUS"]
    );
    assert_ne!(
        wide["SNDRV_PCM_IOCTL_STATUS_EXT"],
        narrow["SNDRV_PCM_IOCTL_STATUS_EXT"]
    );
}

#[test]
fn every_request_comes_apart_and_is_found_by_its_number() {
    for width in [Width::Bits32, Width::Bits64] {
        for pcm in Pcm::ALL {
            let request = pcm.request(width);
            assert_eq!(
                input::ioc_type(request),
                sound::IOC_TYPE_PCM,
                "{}",
                pcm.name()
            );
            assert_eq!(
                Pcm::from_request(width, request),
                Some(pcm),
                "{}",
                pcm.name()
            );
            assert_eq!(Ctl::from_request(width, request), None, "{}", pcm.name());
        }
        for ctl in Ctl::ALL {
            let request = ctl.request(width);
            assert_eq!(
                input::ioc_type(request),
                sound::IOC_TYPE_CTL,
                "{}",
                ctl.name()
            );
            assert_eq!(
                Ctl::from_request(width, request),
                Some(ctl),
                "{}",
                ctl.name()
            );
            assert_eq!(Pcm::from_request(width, request), None, "{}", ctl.name());
        }
    }
    // A request of the other width is not taken for this one's.
    let narrow = Pcm::HwParams.request(Width::Bits32);
    assert_eq!(Pcm::from_request(Width::Bits64, narrow), None);
    assert_eq!(input::ioc_size(narrow), 604);
    assert_eq!(Pcm::from_request(Width::Bits64, 0), None);
}

/// Bytes counting up from `seed`, so every field reads a different value.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| seed.wrapping_add((index % 251) as u8))
        .collect()
}

fn round_trip<L: Layout + core::fmt::Debug + PartialEq>() {
    let bytes = pattern(L::SIZE, 7);
    let value = L::read(&bytes).expect("a whole structure reads");
    let mut out = vec![0u8; L::SIZE];
    value.write(&mut out).expect("it fits");
    assert_eq!(L::read(&out), Some(value), "{}", L::C_NAME);
    assert_eq!(L::read(&bytes[..L::SIZE - 1]), None, "{} short", L::C_NAME);
    assert_eq!(value.write(&mut out[..L::SIZE - 1]), None);
}

/// A structure of two layouts: read at `width` from a pattern, write it into
/// zeroes and read it back, and refuse a short buffer either way. At 32 bits
/// every word read fits, so every write succeeds.
macro_rules! wide_round_trip {
    ($type:ty) => {
        for width in [Width::Bits32, Width::Bits64] {
            let size = <$type>::size(width);
            let bytes = pattern(size, 11);
            let value = <$type>::read(width, &bytes).expect("a whole structure reads");
            let mut out = vec![0u8; size];
            value.write(width, &mut out).expect("it fits");
            assert_eq!(
                <$type>::read(width, &out),
                Some(value),
                "{}",
                <$type>::C_NAME
            );
            assert_eq!(<$type>::read(width, &bytes[..size - 1]), None);
            assert_eq!(value.write(width, &mut out[..size - 1]), None);
        }
    };
}

#[test]
fn every_structure_reads_and_writes_back() {
    round_trip::<Interval>();
    round_trip::<Mask>();
    round_trip::<Timespec>();
    round_trip::<MmapStatus>();
    round_trip::<PcmInfo>();
    round_trip::<CtlCardInfo>();
    wide_round_trip!(HwParams);
    wide_round_trip!(SwParams);
    wide_round_trip!(Status);
    wide_round_trip!(MmapControl);
    wide_round_trip!(SyncPtr);
    wide_round_trip!(Xferi);
    wide_round_trip!(CtlElemList);
}

#[test]
fn a_word_lands_where_the_headers_put_it_and_a_wide_one_is_refused_narrow() {
    let control = MmapControl {
        appl_ptr: 3840,
        avail_min: 960,
    };
    let sync = SyncPtr {
        flags: sound::SYNC_PTR_APPL,
        status: MmapStatus {
            state: sound::STATE_RUNNING,
            hw_ptr: 1920,
            tstamp: Timespec {
                sec: 1 << 33,
                nsec: 999_999_999,
            },
            suspended_state: 0,
            audio_tstamp: Timespec { sec: 0, nsec: 0 },
        },
        control,
    };
    let mut narrow = [0u8; 136];
    sync.write(Width::Bits32, &mut narrow).expect("it fits");
    assert_eq!(narrow[0..4], 2u32.to_le_bytes());
    assert_eq!(narrow[8..12], 3i32.to_le_bytes());
    assert_eq!(narrow[16..24], 1920u64.to_le_bytes());
    // A 64-bit `time_t` at both widths: past 2038 is not cut.
    assert_eq!(narrow[24..32], (1i64 << 33).to_le_bytes());
    assert_eq!(narrow[72..76], 3840u32.to_le_bytes());
    assert_eq!(narrow[76..80], 960u32.to_le_bytes());
    assert_eq!(SyncPtr::read(Width::Bits32, &narrow), Some(sync));

    let mut wide = [0u8; 136];
    sync.write(Width::Bits64, &mut wide).expect("it fits");
    assert_eq!(wide[72..80], 3840u64.to_le_bytes());
    assert_eq!(wide[80..88], 960u64.to_le_bytes());

    // A delay that does not fit a 32-bit `long`: refused, nothing written.
    let transfer = Xferi {
        result: -(1i64 << 40),
        buf: 0x1000,
        frames: 700,
    };
    let mut untouched = [0xAAu8; 12];
    assert_eq!(transfer.write(Width::Bits32, &mut untouched), None);
    assert_eq!(untouched, [0xAA; 12]);
    let negative = Xferi {
        result: -32,
        ..transfer
    };
    negative
        .write(Width::Bits32, &mut untouched)
        .expect("-EPIPE fits");
    assert_eq!(untouched[0..4], (-32i32).to_le_bytes());
    assert_eq!(Xferi::read(Width::Bits32, &untouched), Some(negative));

    // The interval flags are one word after `max`, bit by bit.
    let interval = Interval {
        min: 48_000,
        max: 48_000,
        flags: sound::INTERVAL_INTEGER,
    };
    let mut bytes = [0u8; 12];
    interval.write(&mut bytes).expect("it fits");
    assert_eq!(bytes[8..12], 4u32.to_le_bytes());
    assert_eq!(<Interval as Field>::get(&bytes, 0), Some(interval));
}
