//! Real monitors' EDIDs, what they offer and what the board runs on them.
//!
//! The dumps are public ones from the linuxhw/EDID collection
//! (`Digital/Dell/DELA07A/011E49881443` and `DELA0BA/006195C4C6C8`), and the
//! expected lists are what `edid-decode` printed beside each; the third is
//! the EDID a Dell iDRAC's virtual console presents, read from a server's
//! `/sys/class/drm`. The board's clock is modelled as the kernel rounds it:
//! PLL4's 594 MHz VCO over an integer divider, at most 74.25 MHz.

use std::vec::Vec;

use crate::choice::{self, How, Verdict};
use crate::edid::{self, Offer, RangeLimits, Source, Unusable};
use crate::mode::Mode;
use crate::sii9022;

/// DELL U2412M: 1920x1200, DVI, `DisplayPort` and VGA, EDID 1.3, no
/// extension.
#[rustfmt::skip]
const U2412M: [u8; 128] = [
    0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x10, 0xac, 0x7a, 0xa0, 0x53, 0x46, 0x56, 0x33,
    0x22, 0x17, 0x01, 0x03, 0x80, 0x34, 0x20, 0x78, 0xea, 0xee, 0x95, 0xa3, 0x54, 0x4c, 0x99, 0x26,
    0x0f, 0x50, 0x54, 0xa1, 0x08, 0x00, 0x81, 0x40, 0x81, 0x80, 0xa9, 0x40, 0xb3, 0x00, 0xd1, 0xc0,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x28, 0x3c, 0x80, 0xa0, 0x70, 0xb0, 0x23, 0x40, 0x30, 0x20,
    0x36, 0x00, 0x06, 0x44, 0x21, 0x00, 0x00, 0x1a, 0x00, 0x00, 0x00, 0xff, 0x00, 0x39, 0x57, 0x35,
    0x59, 0x48, 0x33, 0x38, 0x4b, 0x33, 0x56, 0x46, 0x53, 0x0a, 0x00, 0x00, 0x00, 0xfc, 0x00, 0x44,
    0x45, 0x4c, 0x4c, 0x20, 0x55, 0x32, 0x34, 0x31, 0x32, 0x4d, 0x0a, 0x20, 0x00, 0x00, 0x00, 0xfd,
    0x00, 0x32, 0x3d, 0x1e, 0x53, 0x11, 0x00, 0x0a, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x00, 0xe2,
];

/// DELL U2415: 1920x1200 with HDMI, EDID 1.3 and a CTA-861 extension.
#[rustfmt::skip]
const U2415: [[u8; 128]; 2] = [
    [
        0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x10, 0xac, 0xba, 0xa0, 0x55, 0x48, 0x34, 0x33,
        0x33, 0x1d, 0x01, 0x03, 0x80, 0x34, 0x20, 0x78, 0xea, 0x04, 0x95, 0xa9, 0x55, 0x4d, 0x9d, 0x26,
        0x10, 0x50, 0x54, 0xa5, 0x4b, 0x00, 0x71, 0x4f, 0x81, 0x80, 0xa9, 0x40, 0xd1, 0xc0, 0xd1, 0x00,
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x28, 0x3c, 0x80, 0xa0, 0x70, 0xb0, 0x23, 0x40, 0x30, 0x20,
        0x36, 0x00, 0x06, 0x44, 0x21, 0x00, 0x00, 0x1e, 0x00, 0x00, 0x00, 0xff, 0x00, 0x58, 0x4b, 0x56,
        0x30, 0x50, 0x39, 0x43, 0x48, 0x33, 0x34, 0x48, 0x55, 0x0a, 0x00, 0x00, 0x00, 0xfc, 0x00, 0x44,
        0x45, 0x4c, 0x4c, 0x20, 0x55, 0x32, 0x34, 0x31, 0x35, 0x0a, 0x20, 0x20, 0x00, 0x00, 0x00, 0xfd,
        0x00, 0x31, 0x3d, 0x1e, 0x53, 0x11, 0x00, 0x0a, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x01, 0x45,
    ],
    [
        0x02, 0x03, 0x22, 0xf1, 0x4f, 0x90, 0x05, 0x04, 0x03, 0x02, 0x07, 0x16, 0x01, 0x14, 0x1f, 0x12,
        0x13, 0x20, 0x21, 0x22, 0x23, 0x09, 0x07, 0x07, 0x65, 0x03, 0x0c, 0x00, 0x10, 0x00, 0x83, 0x01,
        0x00, 0x00, 0x02, 0x3a, 0x80, 0x18, 0x71, 0x38, 0x2d, 0x40, 0x58, 0x2c, 0x45, 0x00, 0x06, 0x44,
        0x21, 0x00, 0x00, 0x1e, 0x01, 0x1d, 0x80, 0x18, 0x71, 0x1c, 0x16, 0x20, 0x58, 0x2c, 0x25, 0x00,
        0x06, 0x44, 0x21, 0x00, 0x00, 0x9e, 0x01, 0x1d, 0x00, 0x72, 0x51, 0xd0, 0x1e, 0x20, 0x6e, 0x28,
        0x55, 0x00, 0x06, 0x44, 0x21, 0x00, 0x00, 0x1e, 0x8c, 0x0a, 0xd0, 0x8a, 0x20, 0xe0, 0x2d, 0x10,
        0x10, 0x3e, 0x96, 0x00, 0x06, 0x44, 0x21, 0x00, 0x00, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x82,
    ],
];

/// DELL IDRAC: a server's virtual console, with no detailed timing at all.
#[rustfmt::skip]
const IDRAC: [u8; 128] = [
    0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x10, 0xac, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00,
    0x01, 0x11, 0x01, 0x03, 0x80, 0x22, 0x1b, 0xff, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0xad, 0xce, 0x07, 0x81, 0x80, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0xff, 0x00, 0x30, 0x30, 0x30, 0x30, 0x30,
    0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x00, 0x00, 0x00, 0xff, 0x00, 0x30, 0x30, 0x30,
    0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x00, 0x00, 0x00, 0xfd, 0x00, 0x38,
    0x4c, 0x1f, 0x53, 0x0b, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfc,
    0x00, 0x44, 0x45, 0x4c, 0x4c, 0x20, 0x49, 0x44, 0x52, 0x41, 0x43, 0x0a, 0x20, 0x20, 0x00, 0x0a,
];

/// The kernel's rounding on a DK board: 594 MHz over 1 to 128, the nearest
/// rate at most 74.25 MHz, in kHz as the driver sees it.
fn board(khz: u32) -> Option<u32> {
    (1..=128_u64)
        .map(|q| 594_000_000 / q)
        .filter(|&hz| hz <= 74_250_000)
        .min_by_key(|&hz| hz.abs_diff(u64::from(khz) * 1000))
        .map(|hz| (hz / 1000) as u32)
}

/// `block` with its checksum made right again after an edit.
fn summed(mut block: [u8; 128]) -> [u8; 128] {
    let sum = block[..127].iter().fold(0_u8, |s, b| s.wrapping_add(*b));
    block[127] = 0_u8.wrapping_sub(sum);
    block
}

/// Each offer as `(width, height, refresh millihertz, clock kHz)`, or the
/// reason it has none.
fn listed(offers: &[Offer]) -> Vec<Result<(u16, u16, u64, u32), Unusable>> {
    offers
        .iter()
        .map(|offer| {
            offer
                .mode
                .map(|m| (m.hdisplay, m.vdisplay, m.refresh_millihz(), m.clock_khz))
        })
        .collect()
}

fn size(mode: &Mode) -> (u16, u16) {
    (mode.hdisplay, mode.vdisplay)
}

#[test]
fn the_dumps_are_whole_blocks() {
    assert!(edid::is_base(&U2412M), "U2412M");
    assert!(edid::is_base(&U2415[0]), "U2415");
    assert!(edid::checksum_ok(&U2415[1]), "U2415's extension");
    assert!(edid::is_base(&IDRAC), "iDRAC");
    assert_eq!(edid::extensions(&U2415[0]), 1, "one extension");
    assert!(edid::is_hdmi(&U2415[1]), "the U2415 speaks HDMI");
    let (name, len) = edid::name(&U2415[0]).expect("a name");
    assert_eq!(&name[..len], b"DELL U2415");
}

#[test]
fn the_u2412m_offers_what_edid_decode_lists() {
    let offers = edid::offers(&U2412M, &[]);
    assert_eq!(
        listed(offers.as_slice()),
        [
            // DTD 1: 1920x1200 59.950171 Hz, 154 MHz.
            Ok((1920, 1200, 59_950, 154_000)),
            // Established, by bit: 800x600@60, 640x480@60, 720x400@70,
            // 1024x768@60.
            Ok((800, 600, 60_317, 40_000)),
            Ok((640, 480, 59_940, 25_175)),
            Ok((720, 400, 70_082, 28_320)),
            Ok((1024, 768, 60_004, 65_000)),
            // Standard: DMT 0x20, 0x23, 0x33, 0x3a, 0x52.
            Ok((1280, 960, 60_000, 108_000)),
            Ok((1280, 1024, 60_020, 108_000)),
            Ok((1600, 1200, 60_000, 162_000)),
            Ok((1680, 1050, 59_954, 146_250)),
            Ok((1920, 1080, 60_000, 148_500)),
        ]
    );
    let first = offers.as_slice()[0];
    assert_eq!(first.source, Source::Detailed { block: 0, index: 0 });
    let dtd = first.mode.expect("a timing");
    // Hfront 48 Hsync 32 Hback 80 Hpol P, Vfront 3 Vsync 6 Vback 26 Vpol N.
    assert_eq!(
        (dtd.hsync_start, dtd.hsync_end, dtd.htotal),
        (1968, 2000, 2080)
    );
    assert_eq!(
        (dtd.vsync_start, dtd.vsync_end, dtd.vtotal),
        (1203, 1209, 1235)
    );
    assert!(dtd.hsync_high && !dtd.vsync_high, "P, N");
    assert_eq!(
        edid::range_limits(&U2412M),
        Some(RangeLimits {
            vertical_hz: (50, 61),
            horizontal_khz: (30, 83),
            max_clock_khz: 170_000,
        })
    );
    assert!(!edid::continuous(&U2412M), "feature byte 0xea: bit 0 clear");
}

#[test]
fn the_u2415_offers_its_cta_formats_too() {
    let offers = edid::offers(&U2415[0], &U2415[1..]);
    let all = offers.as_slice();
    type Coded = (u8, bool, Result<(u16, u16), Unusable>);
    let vics: Vec<Coded> = all
        .iter()
        .filter_map(|offer| match offer.source {
            Source::Vic { vic, native } => Some((vic, native, offer.mode.map(|m| size(&m)))),
            _ => None,
        })
        .collect();
    let unknown = Err(Unusable::Unknown);
    // Header 0x4f: a video data block of fifteen codes. The 0x23 after them
    // is the audio block's header, not VIC 35.
    assert_eq!(
        vics,
        [
            (16, true, Ok((1920, 1080))),
            (5, false, unknown),
            (4, false, Ok((1280, 720))),
            (3, false, Ok((720, 480))),
            (2, false, Ok((720, 480))),
            (7, false, unknown),
            (22, false, unknown),
            (1, false, Ok((640, 480))),
            (20, false, unknown),
            (31, false, Ok((1920, 1080))),
            (18, false, Ok((720, 576))),
            (19, false, Ok((1280, 720))),
            (32, false, Ok((1920, 1080))),
            (33, false, Ok((1920, 1080))),
            (34, false, Ok((1920, 1080))),
        ]
    );
    // VICs 32 to 34 are 24, 25 and 30 Hz at 74.25 MHz.
    let rates: Vec<(u64, u32)> = all
        .iter()
        .filter(|o| matches!(o.source, Source::Vic { vic: 32..=34, .. }))
        .filter_map(|o| o.mode.ok())
        .map(|m| (m.refresh_millihz(), m.clock_khz))
        .collect();
    assert_eq!(
        rates,
        [(24_000, 74_250), (25_000, 74_250), (30_000, 74_250)]
    );
    // The extension's four timings, the interlaced one refused and the two
    // that are CTA formats named so.
    let extension: Vec<Result<(u16, u16, u8), Unusable>> = all
        .iter()
        .filter(|o| matches!(o.source, Source::Detailed { block: 1, .. }))
        .map(|o| o.mode.map(|m| (m.hdisplay, m.vdisplay, m.vic)))
        .collect();
    assert_eq!(
        extension,
        [
            Ok((1920, 1080, 16)),
            Err(Unusable::Interlaced),
            Ok((1280, 720, 4)),
            Ok((720, 480, 2)),
        ]
    );
    // Standard 0xd100 is DMT 0x45, 1920x1200 at 193.25 MHz.
    let standard = all
        .iter()
        .find(|o| o.source == Source::Standard(0xd100))
        .and_then(|o| o.mode.ok())
        .expect("1920x1200 at 60");
    assert_eq!(
        (size(&standard), standard.clock_khz),
        ((1920, 1200), 193_250)
    );
    assert_eq!(
        edid::range_limits(&U2415[0]).map(|l| l.vertical_hz),
        Some((49, 61))
    );
}

#[test]
fn the_board_runs_1080p30_on_a_u2415() {
    let offers = edid::offers(&U2415[0], &U2415[1..]);
    let limits = edid::range_limits(&U2415[0]);
    let runnable = choice::runnable(&offers, limits, edid::continuous(&U2415[0]), board);
    let chosen = runnable.chosen().expect("something runs");
    assert_eq!(chosen.how, How::Listed);
    assert_eq!(size(&chosen.mode), (1920, 1080));
    assert_eq!(
        chosen.mode.vic, 34,
        "the fastest 1080p it lists that 74.25 MHz makes"
    );
    assert_eq!(chosen.mode.refresh_hz(), 30);
    let sizes: Vec<((u16, u16), u32)> = runnable
        .as_slice()
        .iter()
        .map(|c| (size(&c.mode), c.mode.refresh_hz()))
        .collect();
    assert_eq!(
        sizes,
        [
            ((1920, 1080), 30),
            ((1280, 720), 60),
            ((800, 600), 75),
            ((720, 576), 50),
            ((720, 480), 60),
            ((720, 400), 70),
        ],
        "one a size, largest first"
    );
    // 720p60 is the monitor's own VIC 4, not the assumed fallback.
    assert_eq!(
        runnable.of_size(1280, 720).map(|c| c.how),
        Some(How::Listed)
    );
}

#[test]
fn a_monitor_with_nothing_in_reach_keeps_720p60() {
    for (name, base) in [("U2412M", U2412M), ("iDRAC", IDRAC)] {
        let offers = edid::offers(&base, &[]);
        let runnable = choice::runnable(
            &offers,
            edid::range_limits(&base),
            edid::continuous(&base),
            board,
        );
        let chosen = runnable.chosen().expect("720p60 at least");
        assert_eq!(chosen.how, How::Assumed, "{name}");
        assert!(chosen.mode.same_timing(&Mode::CEA_720P60), "{name}");
    }
}

#[test]
fn verdicts_say_why_a_mode_does_not_run() {
    let offers = edid::offers(&U2412M, &[]);
    let verdicts: Vec<Verdict> = offers
        .as_slice()
        .iter()
        .filter_map(|o| o.mode.ok())
        .map(|m| choice::verdict(&m, &mut board))
        .collect();
    assert_eq!(verdicts[0], Verdict::TooFast, "154 MHz");
    assert_eq!(
        verdicts[1],
        Verdict::Clock {
            nearest_khz: 39_600
        },
        "40 MHz is 1% from 594/15"
    );
    assert_eq!(
        verdicts[3],
        Verdict::Runs { clock_khz: 28_285 },
        "720x400: 594/21 is 0.12% off"
    );
    let mut none = |_| None;
    assert_eq!(
        choice::verdict(&Mode::CEA_720P60, &mut none),
        Verdict::NoClock
    );
    let slow = Mode {
        clock_khz: 20_000,
        ..Mode::CEA_720P60
    };
    assert_eq!(choice::verdict(&slow, &mut board), Verdict::TooSlow);
}

/// The U2412M's base block made continuous frequency, with the lowest
/// vertical rate its range limits give set to `v_min`: no real dump of a
/// 1920x1200 monitor that says so was at hand, and the arithmetic is the
/// same.
fn continuous_u2412m(v_min: u8) -> [u8; 128] {
    let mut block = U2412M;
    block[24] |= 1;
    block[113] = v_min;
    summed(block)
}

#[test]
fn a_continuous_monitor_gets_its_own_size_retimed() {
    let base = continuous_u2412m(24);
    assert!(edid::continuous(&base));
    let offers = edid::offers(&base, &[]);
    let runnable = choice::runnable(&offers, edid::range_limits(&base), true, board);
    let chosen = runnable.chosen().expect("something");
    assert_eq!(chosen.how, How::Retimed);
    assert_eq!(size(&chosen.mode), (1920, 1200));
    // Reduced blanking at 74.25 MHz: 2080 x 1217, 29.33 Hz, which beats
    // the DTD's own blanking (2080 x 1235, 28.90 Hz).
    assert_eq!(chosen.mode.clock_khz, 74_250);
    assert_eq!((chosen.mode.htotal, chosen.mode.vtotal), (2080, 1217));
    assert_eq!(chosen.mode.refresh_millihz(), 29_332);
    assert_eq!(chosen.mode.vic, 0);
}

#[test]
fn a_retimed_size_has_to_land_inside_the_range_limits() {
    // The U2412M's own 50 Hz floor: 1920x1200 would be 29 Hz, and the
    // largest size that lands inside is 1280x960 with reduced blanking,
    // 52.4 Hz; 1280x1024 comes to 49.2.
    let base = continuous_u2412m(50);
    let offers = edid::offers(&base, &[]);
    let runnable = choice::runnable(&offers, edid::range_limits(&base), true, board);
    let chosen = runnable.chosen().expect("something");
    assert_eq!(
        (size(&chosen.mode), chosen.how),
        ((1280, 960), How::Retimed)
    );
    assert_eq!(chosen.mode.refresh_millihz(), 52_401);
    assert!(runnable.of_size(1280, 1024).is_none());
    assert!(runnable.of_size(1920, 1200).is_none());
    // Listed modes run whatever the limits say -- the U2415 lists 24 Hz
    // under a 49 Hz floor -- so only the retimed ones are held to them.
    for candidate in runnable.as_slice().iter().filter(|c| c.how == How::Retimed) {
        let millihz = candidate.mode.refresh_millihz();
        assert!((50_000..=61_000).contains(&millihz), "{candidate:?}");
    }
}

#[test]
fn reduced_blanking_is_cvts_at_the_standards_own_clock() {
    // CVT-RB for 1920x1200 at 60 Hz is 154 MHz with exactly the U2412M's
    // preferred timing, and for 1280x800 71 MHz with 23 lines of blanking
    // (libdisplay-info's `di_cvt_compute`, reduced blanking version 1).
    let wuxga = choice::reduced_blanking(1920, 1200, 154_000).expect("a mode");
    let dtd = edid::preferred(&U2412M).expect("a timing");
    assert!(wuxga.same_timing(&dtd), "{wuxga:?} vs {dtd:?}");
    let wxga = choice::reduced_blanking(1280, 800, 71_000).expect("a mode");
    assert_eq!((wxga.htotal, wxga.vtotal), (1440, 823));
    assert_eq!(
        (wxga.vsync_start, wxga.vsync_end),
        (803, 809),
        "16:10: 6 lines"
    );
}

#[test]
fn range_limits_offsets_and_nonsense() {
    let mut base = U2412M;
    // EDID 1.4's offsets: both vertical rates +255 would put the maximum
    // at 316 Hz.
    base[112] = 0x03;
    let limits = edid::range_limits(&summed(base)).expect("limits");
    assert_eq!(limits.vertical_hz, (305, 316));
    let mut base = U2412M;
    base[113] = 70;
    assert_eq!(edid::range_limits(&summed(base)), None, "min above max");
}

#[test]
fn an_extension_with_no_room_and_other_tags_is_passed_over() {
    let mut other = [0_u8; 128];
    other[0] = 0x70;
    other[2] = 4;
    let other = summed(other);
    let offers = edid::offers(&U2412M, &[other]);
    assert_eq!(offers.as_slice().len(), 10, "only the base block's");
    // A CTA block whose timings would run into the checksum stops before.
    let mut cta = [0_u8; 128];
    cta[0] = edid::CEA_EXTENSION;
    cta[1] = 3;
    cta[2] = 110;
    cta[110..128].copy_from_slice(&[0x01; 18]);
    let offers = edid::offers(&U2412M, &[summed(cta)]);
    assert_eq!(offers.as_slice().len(), 10, "no timing across the checksum");
}

#[test]
fn a_16_10_mode_has_no_picture_aspect_and_a_vic_its_own() {
    assert_eq!(
        sii9022::avi_infoframe(0, 1920, 1200)[2],
        0x08,
        "no data, as the picture"
    );
    assert_eq!(sii9022::avi_infoframe(2, 720, 480)[2], 0x18, "VIC 2 is 4:3");
    assert_eq!(
        sii9022::avi_infoframe(3, 720, 480)[2],
        0x28,
        "VIC 3 is 16:9"
    );
    assert_eq!(sii9022::avi_infoframe(34, 1920, 1080)[4], 34);
}
