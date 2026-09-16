//! `input`: every number and layout against the probe's output, at both
//! widths and in every view of `struct input_event`, request numbers taken
//! apart and put back, and each structure read and written back byte for
//! byte.

use super::std::borrow::ToOwned;
use super::std::collections::BTreeMap;
use super::std::format;
use super::std::string::String;
use super::std::vec;
use super::std::vec::Vec;

use crate::input::{self, AbsInfo, Event, InputId};
use crate::layout::{Field, Layout};
use crate::socket::Width;

const PROBE_64: &str = include_str!("../../probe/input-64.txt");
const PROBE_32: &str = include_str!("../../probe/input-32.txt");

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

/// The request numbers, the sized ones at the probe's sample arguments.
fn ioctls(width: Width) -> Vec<(&'static str, u32)> {
    vec![
        ("EVIOCGVERSION", input::EVIOCGVERSION),
        ("EVIOCGID", input::EVIOCGID),
        ("EVIOCGREP", input::EVIOCGREP),
        ("EVIOCSREP", input::EVIOCSREP),
        ("EVIOCGKEYCODE", input::EVIOCGKEYCODE),
        ("EVIOCGKEYCODE_V2", input::EVIOCGKEYCODE_V2),
        ("EVIOCSKEYCODE", input::EVIOCSKEYCODE),
        ("EVIOCSKEYCODE_V2", input::EVIOCSKEYCODE_V2),
        ("EVIOCSFF", input::eviocsff(width)),
        ("EVIOCRMFF", input::EVIOCRMFF),
        ("EVIOCGEFFECTS", input::EVIOCGEFFECTS),
        ("EVIOCGRAB", input::EVIOCGRAB),
        ("EVIOCREVOKE", input::EVIOCREVOKE),
        ("EVIOCGMASK", input::EVIOCGMASK),
        ("EVIOCSMASK", input::EVIOCSMASK),
        ("EVIOCSCLOCKID", input::EVIOCSCLOCKID),
        ("EVIOCGNAME(256)", input::eviocgname(256)),
        ("EVIOCGNAME(4096)", input::eviocgname(4096)),
        ("EVIOCGPHYS(256)", input::eviocgphys(256)),
        ("EVIOCGUNIQ(256)", input::eviocguniq(256)),
        ("EVIOCGPROP(8)", input::eviocgprop(8)),
        ("EVIOCGMTSLOTS(64)", input::eviocgmtslots(64)),
        ("EVIOCGKEY(96)", input::eviocgkey(96)),
        ("EVIOCGLED(8)", input::eviocgled(8)),
        ("EVIOCGSND(8)", input::eviocgsnd(8)),
        ("EVIOCGSW(8)", input::eviocgsw(8)),
        ("EVIOCGBIT(0,8)", input::eviocgbit(0, 8)),
        ("EVIOCGBIT(EV_KEY,96)", input::eviocgbit(input::EV_KEY, 96)),
        ("EVIOCGBIT(EV_REL,8)", input::eviocgbit(input::EV_REL, 8)),
        ("EVIOCGBIT(EV_ABS,8)", input::eviocgbit(input::EV_ABS, 8)),
        ("EVIOCGBIT(EV_MAX,8)", input::eviocgbit(input::EV_MAX, 8)),
        ("EVIOCGABS(ABS_X)", input::eviocgabs(input::ABS_X)),
        ("EVIOCGABS(ABS_Y)", input::eviocgabs(input::ABS_Y)),
        ("EVIOCGABS(ABS_MAX)", input::eviocgabs(input::ABS_MAX)),
        ("EVIOCSABS(ABS_X)", input::eviocsabs(input::ABS_X)),
        ("EVIOCSABS(ABS_MAX)", input::eviocsabs(input::ABS_MAX)),
    ]
}

/// The event types, codes and properties, all `u16`.
fn codes() -> Vec<(&'static str, u16)> {
    vec![
        ("EV_SYN", input::EV_SYN),
        ("EV_KEY", input::EV_KEY),
        ("EV_REL", input::EV_REL),
        ("EV_ABS", input::EV_ABS),
        ("EV_MSC", input::EV_MSC),
        ("EV_SW", input::EV_SW),
        ("EV_LED", input::EV_LED),
        ("EV_SND", input::EV_SND),
        ("EV_REP", input::EV_REP),
        ("EV_FF", input::EV_FF),
        ("EV_PWR", input::EV_PWR),
        ("EV_FF_STATUS", input::EV_FF_STATUS),
        ("EV_MAX", input::EV_MAX),
        ("EV_CNT", input::EV_CNT),
        ("SYN_REPORT", input::SYN_REPORT),
        ("SYN_CONFIG", input::SYN_CONFIG),
        ("SYN_MT_REPORT", input::SYN_MT_REPORT),
        ("SYN_DROPPED", input::SYN_DROPPED),
        ("SYN_MAX", input::SYN_MAX),
        ("SYN_CNT", input::SYN_CNT),
        ("INPUT_PROP_POINTER", input::INPUT_PROP_POINTER),
        ("INPUT_PROP_DIRECT", input::INPUT_PROP_DIRECT),
        ("INPUT_PROP_MAX", input::INPUT_PROP_MAX),
        ("INPUT_PROP_CNT", input::INPUT_PROP_CNT),
        ("KEY_RESERVED", input::KEY_RESERVED),
        ("KEY_ESC", input::KEY_ESC),
        ("KEY_A", input::KEY_A),
        ("BTN_MISC", input::BTN_MISC),
        ("BTN_MOUSE", input::BTN_MOUSE),
        ("BTN_LEFT", input::BTN_LEFT),
        ("BTN_RIGHT", input::BTN_RIGHT),
        ("BTN_MIDDLE", input::BTN_MIDDLE),
        ("BTN_SIDE", input::BTN_SIDE),
        ("BTN_EXTRA", input::BTN_EXTRA),
        ("BTN_TOUCH", input::BTN_TOUCH),
        ("BTN_GEAR_DOWN", input::BTN_GEAR_DOWN),
        ("BTN_GEAR_UP", input::BTN_GEAR_UP),
        ("KEY_MAX", input::KEY_MAX),
        ("KEY_CNT", input::KEY_CNT),
        ("REL_X", input::REL_X),
        ("REL_Y", input::REL_Y),
        ("REL_HWHEEL", input::REL_HWHEEL),
        ("REL_WHEEL", input::REL_WHEEL),
        ("REL_MAX", input::REL_MAX),
        ("REL_CNT", input::REL_CNT),
        ("ABS_X", input::ABS_X),
        ("ABS_Y", input::ABS_Y),
        ("ABS_MT_SLOT", input::ABS_MT_SLOT),
        ("ABS_MT_POSITION_X", input::ABS_MT_POSITION_X),
        ("ABS_MT_POSITION_Y", input::ABS_MT_POSITION_Y),
        ("ABS_MT_TRACKING_ID", input::ABS_MT_TRACKING_ID),
        ("ABS_MAX", input::ABS_MAX),
        ("ABS_CNT", input::ABS_CNT),
        ("MSC_SCAN", input::MSC_SCAN),
        ("MSC_MAX", input::MSC_MAX),
        ("MSC_CNT", input::MSC_CNT),
        ("SW_MAX", input::SW_MAX),
        ("SW_CNT", input::SW_CNT),
        ("LED_NUML", input::LED_NUML),
        ("LED_CAPSL", input::LED_CAPSL),
        ("LED_SCROLLL", input::LED_SCROLLL),
        ("LED_MAX", input::LED_MAX),
        ("LED_CNT", input::LED_CNT),
        ("SND_MAX", input::SND_MAX),
        ("SND_CNT", input::SND_CNT),
        ("REP_DELAY", input::REP_DELAY),
        ("REP_PERIOD", input::REP_PERIOD),
        ("REP_MAX", input::REP_MAX),
        ("REP_CNT", input::REP_CNT),
        ("FF_MAX", input::FF_MAX),
        ("FF_CNT", input::FF_CNT),
        ("BUS_VIRTUAL", input::BUS_VIRTUAL),
    ]
}

/// Every other constant.
fn values() -> Vec<(&'static str, u64)> {
    let mut all: Vec<(&'static str, u64)> = codes()
        .into_iter()
        .map(|(name, value)| (name, u64::from(value)))
        .collect();
    let nonnegative = |value: i32| u64::try_from(value).expect("the probe prints unsigned");
    all.extend([
        ("EV_VERSION", nonnegative(input::EV_VERSION)),
        ("INPUT_MAJOR", input::INPUT_MAJOR.into()),
        ("_IOC_NRBITS", input::IOC_NRBITS.into()),
        ("_IOC_TYPEBITS", input::IOC_TYPEBITS.into()),
        ("_IOC_SIZEBITS", input::IOC_SIZEBITS.into()),
        ("_IOC_DIRBITS", input::IOC_DIRBITS.into()),
        ("_IOC_NRSHIFT", input::IOC_NRSHIFT.into()),
        ("_IOC_TYPESHIFT", input::IOC_TYPESHIFT.into()),
        ("_IOC_SIZESHIFT", input::IOC_SIZESHIFT.into()),
        ("_IOC_DIRSHIFT", input::IOC_DIRSHIFT.into()),
        ("_IOC_NONE", input::IOC_NONE.into()),
        ("_IOC_WRITE", input::IOC_WRITE.into()),
        ("_IOC_READ", input::IOC_READ.into()),
        ("ID_BUS", input::ID_BUS as u64),
        ("ID_VENDOR", input::ID_VENDOR as u64),
        ("ID_PRODUCT", input::ID_PRODUCT as u64),
        ("ID_VERSION", input::ID_VERSION as u64),
        ("CLOCK_REALTIME", nonnegative(input::CLOCK_REALTIME)),
        ("CLOCK_MONOTONIC", nonnegative(input::CLOCK_MONOTONIC)),
        ("CLOCK_BOOTTIME", nonnegative(input::CLOCK_BOOTTIME)),
    ]);
    all
}

/// A layout's `sizeof` and `offsetof` lines as the probe prints them, after
/// `prefix`.
fn layout_lines(
    prefix: &str,
    c_name: &str,
    size: usize,
    fields: &[(&str, usize)],
) -> Vec<(String, u64)> {
    let mut lines = vec![(format!("{prefix}sizeof.{c_name}"), size as u64)];
    for (field, offset) in fields {
        lines.push((format!("{prefix}offsetof.{c_name}.{field}"), *offset as u64));
    }
    lines
}

fn layouts<L: Layout>() -> Vec<(String, u64)> {
    layout_lines("", L::C_NAME, L::SIZE, L::FIELDS)
}

fn event_lines(prefix: &str, width: Width) -> Vec<(String, u64)> {
    layout_lines(
        prefix,
        Event::C_NAME,
        Event::size(width),
        &Event::fields(width),
    )
}

/// The probe's extra builds: a view's name, its `time_t` in bytes, and the
/// width whose [`Event`] layout that view sees.
///
/// With 32-bit or 64-bit `time_t` and `__USE_TIME_BITS64` as the libc sets
/// it, a program sees the kernel's layout. A libc whose `time_t` is 64 bits
/// but which leaves the macro undefined gives the program a `struct timeval`
/// of 16 bytes, which is the 64-bit layout at either width.
fn views(width: Width) -> [(&'static str, u64, Width); 3] {
    let time32 = match width {
        Width::Bits32 => 4,
        Width::Bits64 => 8,
    };
    [
        ("time32", time32, width),
        ("time64", 8, width),
        ("time64-undef", 8, Width::Bits64),
    ]
}

/// Every line this module says the probe prints at `width`.
fn expected(width: Width) -> BTreeMap<String, u64> {
    let mut lines: Vec<(String, u64)> = ioctls(width)
        .into_iter()
        .map(|(name, value)| (name.to_owned(), u64::from(value)))
        .chain(
            values()
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value)),
        )
        .collect();
    lines.extend(event_lines("", width));
    lines.extend(layouts::<InputId>());
    lines.extend(layouts::<AbsInfo>());
    for (view, time_t, seen) in views(width) {
        let prefix = format!("view.{view}.");
        lines.push((format!("{prefix}sizeof.time_t"), time_t));
        lines.extend(event_lines(&prefix, seen));
    }
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

#[test]
fn only_input_event_evioc_sff_and_time_t_differ_between_the_widths() {
    let wide = probe(PROBE_64);
    let narrow = probe(PROBE_32);
    let differing: Vec<&str> = wide
        .iter()
        .filter(|(name, value)| narrow.get(*name) != Some(value))
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(!differing.is_empty());
    for name in differing {
        assert!(
            name == "EVIOCSFF" || name.contains(".input_event") || name.ends_with(".time_t"),
            "{name} differs between widths"
        );
        assert!(
            !name.starts_with("view.time64-undef.") || name.ends_with(".time_t"),
            "{name}: the undefined-macro view is the 64-bit layout at both widths"
        );
    }
    // The hazard the module comment names, as the probe printed it.
    assert_eq!(narrow["sizeof.input_event"], 16);
    assert_eq!(narrow["view.time64-undef.sizeof.input_event"], 24);
}

#[test]
fn requests_come_apart_and_back_together() {
    for width in [Width::Bits32, Width::Bits64] {
        for (name, request) in ioctls(width) {
            assert_eq!(
                input::ioc_type(request),
                input::IOC_TYPE_EVDEV,
                "{name} is not an 'E' request"
            );
            assert_eq!(
                input::ioc(
                    input::ioc_dir(request),
                    input::ioc_nr(request),
                    input::ioc_size(request)
                ),
                request,
                "{name} does not reassemble"
            );
        }
    }
    let name = input::eviocgname(4096);
    assert_eq!(input::ioc_dir(name), input::IOC_READ);
    assert_eq!(input::ioc_nr(name), 0x06);
    assert_eq!(input::ioc_size(name), 4096);
    let bits = input::eviocgbit(input::EV_KEY, 96);
    assert_eq!(input::ioc_nr(bits), 0x20 + u32::from(input::EV_KEY));
    assert_eq!(input::ioc_size(bits), 96);
    let abs = input::eviocsabs(input::ABS_Y);
    assert_eq!(input::ioc_dir(abs), input::IOC_WRITE);
    assert_eq!(input::ioc_size(abs), AbsInfo::SIZE as u32);
    assert_eq!(input::ioc_dir(input::EVIOCGRAB), input::IOC_WRITE);
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
    // Neither structure has padding, so every byte comes back.
    assert_eq!(out, bytes, "{}", L::C_NAME);
    assert_eq!(L::read(&bytes[..L::SIZE - 1]), None, "{} short", L::C_NAME);
    assert_eq!(value.write(&mut out[..L::SIZE - 1]), None);
    assert_eq!(<L as Field>::get(&bytes, 0), Some(value));
    assert_eq!(<L as Field>::get(&bytes, 1), None);
}

#[test]
fn every_structure_reads_and_writes_back() {
    round_trip::<InputId>();
    round_trip::<AbsInfo>();
    for width in [Width::Bits32, Width::Bits64] {
        let bytes = pattern(Event::size(width), 7);
        let event = Event::read(width, &bytes).expect("a whole event reads");
        let mut out = vec![0u8; Event::size(width)];
        event.write(width, &mut out).expect("it fits");
        assert_eq!(out, bytes);
        assert_eq!(Event::read(width, &bytes[1..]), None);
        assert_eq!(event.write(width, &mut out[1..]), None);
    }
}

#[test]
fn an_event_lands_where_the_headers_put_it() {
    let event = Event {
        sec: 5,
        usec: 250_000,
        r#type: input::EV_KEY,
        code: input::KEY_A,
        value: -1,
    };
    let mut wide = [0u8; 24];
    event.write(Width::Bits64, &mut wide).expect("it fits");
    assert_eq!(wide[0..8], 5u64.to_le_bytes());
    assert_eq!(wide[8..16], 250_000u64.to_le_bytes());
    assert_eq!(wide[16..18], 1u16.to_le_bytes());
    assert_eq!(wide[18..20], 30u16.to_le_bytes());
    assert_eq!(wide[20..24], (-1i32).to_le_bytes());

    let mut narrow = [0u8; 16];
    event.write(Width::Bits32, &mut narrow).expect("it fits");
    assert_eq!(narrow[0..4], 5u32.to_le_bytes());
    assert_eq!(narrow[4..8], 250_000u32.to_le_bytes());
    assert_eq!(narrow[8..10], 1u16.to_le_bytes());
    assert_eq!(narrow[10..12], 30u16.to_le_bytes());
    assert_eq!(narrow[12..16], (-1i32).to_le_bytes());
    assert_eq!(Event::read(Width::Bits32, &narrow), Some(event));

    // Seconds past 2106 do not fit a 32-bit word: refused, nothing written.
    let late = Event {
        sec: 1 << 32,
        ..event
    };
    let mut untouched = [0xAAu8; 16];
    assert_eq!(late.write(Width::Bits32, &mut untouched), None);
    assert_eq!(untouched, [0xAA; 16]);
    assert_eq!(late.write(Width::Bits64, &mut wide), Some(()));

    let mut absinfo = [0u8; 24];
    AbsInfo {
        value: 16_384,
        minimum: 0,
        maximum: 32_767,
        fuzz: 0,
        flat: 0,
        resolution: 0,
    }
    .write(&mut absinfo)
    .expect("it fits");
    assert_eq!(absinfo[0..4], 16_384i32.to_le_bytes());
    assert_eq!(absinfo[8..12], 32_767i32.to_le_bytes());
}
