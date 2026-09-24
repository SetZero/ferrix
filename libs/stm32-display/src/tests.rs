//! Tests against models of the hardware: a register file for the LTDC, and
//! for I2C a controller that runs its transfers against a `SiI9022` and a
//! monitor's EDID EEPROM on the bus behind it.

use std::collections::BTreeMap;
use std::vec::Vec;

use crate::edid;
use crate::i2c::{self, I2c};
use crate::ltdc::{self, Frame, Ltdc, LtdcError, Timing};
use crate::mode::Mode;
use crate::sii9022::{self, Bridge, BridgeError, Bus, BusError};
use crate::{Budget, Registers};

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

/// The detailed timing descriptor nearly every HDMI monitor carries for
/// 720p60, as the EDID standard encodes it.
const DTD_720P60: [u8; 18] = [
    0x01, 0x1D, 0x00, 0x72, 0x51, 0xD0, 0x1E, 0x20, 0x6E, 0x28, 0x55, 0x00, 0x40, 0x84, 0x63, 0x00,
    0x00, 0x1E,
];

#[test]
fn the_board_mode_is_consistent_and_runs_at_sixty_hertz() {
    let mode = Mode::CEA_720P60;
    assert!(mode.is_consistent(), "720p60 is a mode");
    assert_eq!(mode.refresh_hz(), 60, "74.25 MHz over 1650 x 750");
}

#[test]
fn a_detailed_timing_reads_back_as_the_cea_mode() {
    let mode = Mode::from_detailed_timing(&DTD_720P60).expect("a timing");
    assert!(mode.same_timing(&Mode::CEA_720P60), "{mode:?}");
    assert!(mode.hsync_high && mode.vsync_high, "both syncs positive");
}

#[test]
fn a_descriptor_that_is_not_a_timing_is_none() {
    let mut name = [0_u8; 18];
    name[3] = 0xFC;
    assert_eq!(Mode::from_detailed_timing(&name), None, "zero clock");
    let mut interlaced = DTD_720P60;
    interlaced[17] |= 0x80;
    assert_eq!(Mode::from_detailed_timing(&interlaced), None, "interlaced");
    let mut analog = DTD_720P60;
    analog[17] &= !0x18;
    assert_eq!(Mode::from_detailed_timing(&analog), None, "analog sync");
}

// ---------------------------------------------------------------------------
// The LTDC
// ---------------------------------------------------------------------------

/// A register file that reads back what was written, starting from the
/// identification an STM32MP15 gives, and keeps every write in order.
///
/// What is written is the shadow; `active` is what the controller is
/// showing, copied from the shadows at once for `SRCR.IMR` and at the next
/// [`RegisterFile::vertical_blank`] for `SRCR.VBR`, which then clears the
/// request and raises the reload interrupt, as RM0436 has it.
#[derive(Debug, Default)]
struct RegisterFile {
    values: BTreeMap<u32, u32>,
    active: BTreeMap<u32, u32>,
    writes: Vec<(u32, u32)>,
}

impl RegisterFile {
    fn stm32mp15() -> Self {
        let mut file = RegisterFile::default();
        let _ = file.values.insert(ltdc::IDR, 0x0001_0300);
        let _ = file.values.insert(ltdc::LCR, 2);
        // A 64-bit bus: log2 of 8 bytes.
        let _ = file.values.insert(ltdc::GC2R, 3 << 4);
        file
    }

    fn value(&self, offset: u32) -> u32 {
        self.values.get(&offset).copied().unwrap_or(0)
    }

    /// What the controller shows from the register at `offset`.
    fn shown(&self, offset: u32) -> u32 {
        self.active.get(&offset).copied().unwrap_or(0)
    }

    /// The shadows copied in.
    fn reload(&mut self) {
        self.active.clone_from(&self.values);
        let _ = self.values.insert(ltdc::SRCR, 0);
        let status = self.value(ltdc::ISR) | ltdc::INT_RR;
        let _ = self.values.insert(ltdc::ISR, status);
    }

    /// The screen's vertical blanking: a reload, if one was asked for.
    fn vertical_blank(&mut self) {
        if self.value(ltdc::SRCR) & ltdc::SRCR_VBR != 0 {
            self.reload();
        }
    }
}

impl Registers for RegisterFile {
    fn read32(&self, offset: u32) -> u32 {
        self.value(offset)
    }

    fn write32(&mut self, offset: u32, value: u32) {
        self.writes.push((offset, value));
        if offset == ltdc::ICR {
            // Write-one-to-clear, as the status register behaves.
            let status = self.value(ltdc::ISR) & !value;
            let _ = self.values.insert(ltdc::ISR, status);
        } else if offset == ltdc::SRCR {
            // Set by software, cleared only by the reload.
            let asked = self.value(ltdc::SRCR) | value;
            let _ = self.values.insert(ltdc::SRCR, asked);
            if value & ltdc::SRCR_IMR != 0 {
                self.reload();
            }
        } else {
            let _ = self.values.insert(offset, value);
        }
    }
}

#[test]
fn the_timing_words_are_linux_arithmetic_for_720p() {
    let timing = Timing::of(&Mode::CEA_720P60).expect("fits");
    assert_eq!(timing.sscr, (39 << 16) | 4, "sync widths less one");
    assert_eq!(
        timing.bpcr,
        (259 << 16) | 24,
        "sync plus back porch, less one"
    );
    assert_eq!(timing.awcr, (1539 << 16) | 744, "plus the active area");
    assert_eq!(timing.twcr, (1649 << 16) | 749, "the totals less one");
    assert_eq!(
        timing.polarity,
        ltdc::GCR_HSPOL | ltdc::GCR_VSPOL,
        "positive syncs"
    );
}

#[test]
fn start_programs_the_timing_and_turns_the_controller_on_last() {
    let mut ltdc = Ltdc::new(RegisterFile::stm32mp15()).expect("an STM32MP15 LTDC");
    ltdc.start(&Mode::CEA_720P60).expect("720p fits");
    let r = ltdc.registers();
    assert_eq!(r.value(ltdc::SSCR), (39 << 16) | 4, "SSCR");
    assert_eq!(r.value(ltdc::TWCR), (1649 << 16) | 749, "TWCR");
    assert_eq!(r.value(ltdc::BCCR), 0, "black background");
    assert_eq!(r.value(ltdc::LAYER + ltdc::L_CR), 0, "layer 1 off");
    assert_eq!(
        r.value(ltdc::LAYER + ltdc::LAYER_STRIDE + ltdc::L_CR),
        0,
        "layer 2 off"
    );
    assert_eq!(
        r.value(ltdc::IER),
        ltdc::INT_RR | ltdc::INT_FUE | ltdc::INT_TERR,
        "reload, underrun and error interrupts"
    );
    assert_eq!(
        r.writes.last(),
        Some(&(
            ltdc::GCR,
            ltdc::GCR_HSPOL | ltdc::GCR_VSPOL | ltdc::GCR_LTDCEN
        )),
        "enabled last, with the polarities"
    );
    assert_eq!(
        r.writes.first(),
        Some(&(ltdc::GCR, 0)),
        "off while the timing changes"
    );
}

#[test]
fn show_puts_a_full_screen_layer_on_at_the_next_vertical_blanking() {
    let mut ltdc = Ltdc::new(RegisterFile::stm32mp15()).expect("an STM32MP15 LTDC");
    ltdc.start(&Mode::CEA_720P60).expect("720p fits");
    let frame = Frame {
        address: 0xC800_0000,
        pitch: 5120,
        width: 1280,
        height: 720,
    };
    ltdc.show(&frame).expect("the mode's size");
    let r = ltdc.registers();
    let at = |offset| r.value(ltdc::LAYER + offset);
    assert_eq!(at(ltdc::L_WHPCR), (1539 << 16) | 260, "columns 260 to 1539");
    assert_eq!(at(ltdc::L_WVPCR), (744 << 16) | 25, "rows 25 to 744");
    assert_eq!(at(ltdc::L_PFCR), ltdc::PF_ARGB8888, "XRGB8888 as ARGB8888");
    assert_eq!(at(ltdc::L_CACR), 0xFF, "opaque");
    assert_eq!(
        at(ltdc::L_BFCR),
        ltdc::BFCR_CONSTANT,
        "the pixel alpha ignored"
    );
    assert_eq!(at(ltdc::L_CFBAR), 0xC800_0000, "the buffer");
    assert_eq!(
        at(ltdc::L_CFBLR),
        (5120 << 16) | (5120 + 7),
        "pitch, row plus bus width less one"
    );
    assert_eq!(at(ltdc::L_CFBLNR), 720, "rows");
    assert_eq!(at(ltdc::L_CR), ltdc::L_CR_LEN, "shown");
    assert_eq!(
        r.writes.last(),
        Some(&(ltdc::SRCR, ltdc::SRCR_VBR)),
        "reloaded at blanking"
    );
    assert!(ltdc.reload_pending(), "the model keeps VBR set");
}

#[test]
fn a_frame_the_mode_cannot_show_is_refused_and_nothing_is_written() {
    let mut ltdc = Ltdc::new(RegisterFile::stm32mp15()).expect("an STM32MP15 LTDC");
    let good = Frame {
        address: 0xC800_0000,
        pitch: 5120,
        width: 1280,
        height: 720,
    };
    assert_eq!(ltdc.show(&good), Err(LtdcError::Mode), "not started");
    ltdc.start(&Mode::CEA_720P60).expect("720p fits");
    let written = ltdc.registers().writes.len();
    for bad in [
        Frame {
            width: 1920,
            ..good
        },
        Frame {
            height: 721,
            ..good
        },
        Frame {
            pitch: 5116,
            ..good
        },
        Frame {
            address: 0xC800_0002,
            ..good
        },
        Frame {
            pitch: 0x1_0000,
            ..good
        },
    ] {
        assert_eq!(ltdc.show(&bad), Err(LtdcError::Buffer), "{bad:?}");
    }
    assert_eq!(
        ltdc.registers().writes.len(),
        written,
        "nothing written for a refusal"
    );
}

/// A 64 x 64 pointer image, rows packed, as the display core's cursor
/// buffers are.
const ARROW: ltdc::CursorImage = ltdc::CursorImage {
    address: 0xC900_0000,
    pitch: 256,
    width: 64,
    height: 64,
};

/// The screen's frame, 720p.
const SCREEN: Frame = Frame {
    address: 0xC800_0000,
    pitch: 5120,
    width: 1280,
    height: 720,
};

/// The second layer's register at `offset`.
const fn cursor(offset: u32) -> u32 {
    ltdc::LAYER + ltdc::CURSOR_LAYER * ltdc::LAYER_STRIDE + offset
}

fn started() -> Ltdc<RegisterFile> {
    let mut ltdc = Ltdc::new(RegisterFile::stm32mp15()).expect("an STM32MP15 LTDC");
    ltdc.start(&Mode::CEA_720P60).expect("720p fits");
    ltdc
}

#[test]
fn the_pointer_goes_on_the_second_layer_blended_as_premultiplied() {
    let mut ltdc = started();
    assert!(ltdc.has_cursor_layer(), "LCR says two");
    assert_eq!(ltdc.show_cursor(&ARROW, (100, 200)), Ok(true));
    let r = ltdc.registers();
    // 720p's back porches end at column 259 and row 24.
    assert_eq!(
        r.value(cursor(ltdc::L_WHPCR)),
        ((259 + 100 + 64) << 16) | (259 + 101),
        "columns 100 to 163 of the screen"
    );
    assert_eq!(
        r.value(cursor(ltdc::L_WVPCR)),
        ((24 + 200 + 64) << 16) | (24 + 201),
        "rows 200 to 263"
    );
    assert_eq!(r.value(cursor(ltdc::L_PFCR)), ltdc::PF_ARGB8888);
    assert_eq!(r.value(cursor(ltdc::L_CACR)), 0xFF, "constant alpha one");
    assert_eq!(
        r.value(cursor(ltdc::L_BFCR)),
        (0b100 << 8) | 0b111,
        "BF1 constant alpha, BF2 one less pixel alpha times constant alpha"
    );
    assert_eq!(
        r.value(cursor(ltdc::L_DCCR)),
        0,
        "transparent outside the window"
    );
    assert_eq!(r.value(cursor(ltdc::L_CFBAR)), 0xC900_0000, "the image");
    assert_eq!(
        r.value(cursor(ltdc::L_CFBLR)),
        (256 << 16) | (256 + 7),
        "its pitch, and a row plus the bus width less one"
    );
    assert_eq!(r.value(cursor(ltdc::L_CFBLNR)), 64, "rows");
    assert_eq!(r.value(cursor(ltdc::L_CR)), ltdc::L_CR_LEN, "shown");
    assert_eq!(
        r.writes.last(),
        Some(&(ltdc::SRCR, ltdc::SRCR_VBR)),
        "at the next blanking, never at once"
    );
    assert!(
        r.writes.iter().all(
            |&(offset, _)| !(ltdc::LAYER..ltdc::LAYER + ltdc::LAYER_STRIDE).contains(&offset)
                || offset == ltdc::LAYER + ltdc::L_CR
        ),
        "the first layer's registers were not touched after start"
    );
}

#[test]
fn a_pointer_across_an_edge_is_shown_in_part() {
    let screen = (1280, 720);
    let clip = |at| ltdc::Clip::of(&ARROW, at, screen);
    // Off the left and the top: the window starts at the screen's corner,
    // and the buffer is read from as far into it as was cut off.
    assert_eq!(
        clip((-10, -3)),
        Some(ltdc::Clip {
            address: 0xC900_0000 + 3 * 256 + 10 * 4,
            x: 0,
            y: 0,
            width: 54,
            height: 61,
        })
    );
    // Off the right and the bottom: only a narrower, shorter window.
    assert_eq!(
        clip((1250, 700)),
        Some(ltdc::Clip {
            address: 0xC900_0000,
            x: 1250,
            y: 700,
            width: 30,
            height: 20,
        })
    );
    // One pixel of it in each corner, and none past them.
    assert_eq!(clip((-63, -63)).map(|c| (c.width, c.height)), Some((1, 1)));
    assert_eq!(clip((1279, 719)).map(|c| (c.width, c.height)), Some((1, 1)));
    for off in [
        (-64, 0),
        (0, -64),
        (1280, 0),
        (0, 720),
        (i32::MIN, i32::MAX),
    ] {
        assert_eq!(clip(off), None, "{off:?}");
    }

    // And the registers made from a clip off the left.
    let mut ltdc = started();
    assert_eq!(ltdc.show_cursor(&ARROW, (-10, 5)), Ok(true));
    let r = ltdc.registers();
    assert_eq!(
        r.value(cursor(ltdc::L_WHPCR)),
        ((259 + 54) << 16) | 260,
        "the first 54 columns"
    );
    assert_eq!(r.value(cursor(ltdc::L_CFBAR)), 0xC900_0000 + 40);
    assert_eq!(
        r.value(cursor(ltdc::L_CFBLR)),
        (256 << 16) | (54 * 4 + 7),
        "the pitch as before, the line as long as what is shown"
    );
}

#[test]
fn a_pointer_off_the_screen_takes_the_layer_off() {
    let mut ltdc = started();
    assert_eq!(ltdc.show_cursor(&ARROW, (10, 10)), Ok(true));
    ltdc.registers_mut().vertical_blank();
    assert_eq!(ltdc.show_cursor(&ARROW, (-200, 10)), Ok(false));
    ltdc.registers_mut().vertical_blank();
    assert_eq!(ltdc.registers().shown(cursor(ltdc::L_CR)), 0, "off");
    // And back on when it comes back.
    assert_eq!(ltdc.show_cursor(&ARROW, (0, 0)), Ok(true));
    ltdc.registers_mut().vertical_blank();
    assert_eq!(ltdc.registers().shown(cursor(ltdc::L_CR)), ltdc::L_CR_LEN);
    ltdc.hide_cursor();
    assert!(ltdc.reload_pending(), "hidden at the next blanking");
    ltdc.registers_mut().vertical_blank();
    assert_eq!(ltdc.registers().shown(cursor(ltdc::L_CR)), 0, "hidden");
}

#[test]
fn a_move_and_a_flip_in_one_frame_both_land_at_its_blanking() {
    let mut ltdc = started();
    ltdc.show(&SCREEN).expect("the mode's size");
    assert_eq!(ltdc.show_cursor(&ARROW, (10, 10)), Ok(true));
    ltdc.registers_mut().vertical_blank();
    let _ = ltdc.take_events();

    // The compositor flips to its other buffer, and before the blanking
    // the pointer moves.
    let other = Frame {
        address: 0xC840_0000,
        ..SCREEN
    };
    ltdc.show(&other).expect("the mode's size");
    assert_eq!(ltdc.show_cursor(&ARROW, (20, 30)), Ok(true));
    let r = ltdc.registers();
    assert_eq!(
        r.shown(ltdc::LAYER + ltdc::L_CFBAR),
        0xC800_0000,
        "nothing lands before the blanking"
    );
    assert_eq!(r.shown(cursor(ltdc::L_WHPCR)) & 0xFFF, 259 + 11);
    ltdc.registers_mut().vertical_blank();
    let r = ltdc.registers();
    assert_eq!(
        r.shown(ltdc::LAYER + ltdc::L_CFBAR),
        0xC840_0000,
        "the flip"
    );
    assert_eq!(
        r.shown(cursor(ltdc::L_WHPCR)) & 0xFFF,
        259 + 21,
        "and the move"
    );
    assert_eq!(r.shown(cursor(ltdc::L_WVPCR)) & 0x7FF, 24 + 31);
    assert!(ltdc.take_events().reloaded, "one reload, one interrupt");
    assert!(!ltdc.reload_pending());

    // A move alone asks for a reload and writes none of the first layer's
    // registers, so a flip waiting for its blanking is left as it was.
    ltdc.show(&SCREEN).expect("the mode's size");
    let before = ltdc.registers().writes.len();
    assert_eq!(ltdc.show_cursor(&ARROW, (21, 30)), Ok(true));
    let first = ltdc::LAYER..ltdc::LAYER + ltdc::LAYER_STRIDE;
    assert!(
        ltdc.registers().writes[before..]
            .iter()
            .all(|(offset, _)| !first.contains(offset)),
        "a move touches the second layer only"
    );
    ltdc.registers_mut().vertical_blank();
    assert_eq!(
        ltdc.registers().shown(ltdc::LAYER + ltdc::L_CFBAR),
        0xC800_0000,
        "the flip waiting under the move landed"
    );
}

#[test]
fn the_pointer_comes_back_after_a_mode_switch_only_when_shown_again() {
    let mut ltdc = started();
    assert_eq!(ltdc.show_cursor(&ARROW, (1000, 600)), Ok(true));
    ltdc.registers_mut().vertical_blank();
    ltdc.stop();
    assert_eq!(
        ltdc.registers().shown(cursor(ltdc::L_CR)),
        0,
        "stopped: nothing read from memory, the pointer's buffer included"
    );
    // 720x480 at 60 Hz: the old place is off this screen, and the image is
    // clipped against the new mode's size.
    let small = Mode {
        clock_khz: 27_000,
        hdisplay: 720,
        hsync_start: 736,
        hsync_end: 798,
        htotal: 858,
        vdisplay: 480,
        vsync_start: 489,
        vsync_end: 495,
        vtotal: 525,
        hsync_high: false,
        vsync_high: false,
        vic: 2,
    };
    ltdc.start(&small).expect("fits");
    assert_eq!(ltdc.registers().shown(cursor(ltdc::L_CR)), 0, "off");
    assert_eq!(ltdc.show_cursor(&ARROW, (1000, 600)), Ok(false));
    assert_eq!(ltdc.show_cursor(&ARROW, (700, 470)), Ok(true));
    let r = ltdc.registers();
    let hbp = r.value(ltdc::BPCR) >> 16;
    assert_eq!(
        r.value(cursor(ltdc::L_WHPCR)),
        ((hbp + 720) << 16) | (hbp + 701),
        "clipped at the new right edge"
    );
    assert_eq!(r.value(cursor(ltdc::L_CFBLNR)), 10, "and the new bottom");
}

#[test]
fn a_cursor_the_controller_cannot_show_is_refused_and_nothing_is_written() {
    let mut ltdc = Ltdc::new(RegisterFile::stm32mp15()).expect("an STM32MP15 LTDC");
    assert_eq!(
        ltdc.show_cursor(&ARROW, (0, 0)),
        Err(LtdcError::Mode),
        "not started"
    );
    ltdc.start(&Mode::CEA_720P60).expect("720p fits");
    let written = ltdc.registers().writes.len();
    for bad in [
        ltdc::CursorImage {
            pitch: 252,
            ..ARROW
        },
        ltdc::CursorImage {
            address: 0xC900_0002,
            ..ARROW
        },
        ltdc::CursorImage {
            width: 0x1000,
            pitch: 0x4000,
            ..ARROW
        },
    ] {
        assert_eq!(
            ltdc.show_cursor(&bad, (0, 0)),
            Err(LtdcError::Buffer),
            "{bad:?}"
        );
    }
    assert_eq!(ltdc.registers().writes.len(), written, "nothing written");

    // A controller with one layer has no plane to offer.
    let mut one = RegisterFile::stm32mp15();
    let _ = one.values.insert(ltdc::LCR, 1);
    let mut one = Ltdc::new(one).expect("an LTDC all the same");
    one.start(&Mode::CEA_720P60).expect("720p fits");
    assert!(!one.has_cursor_layer());
    assert_eq!(one.show_cursor(&ARROW, (0, 0)), Err(LtdcError::Mode));
}

#[test]
fn another_ltdc_version_is_refused() {
    let mut file = RegisterFile::stm32mp15();
    let _ = file.values.insert(ltdc::IDR, 0x0004_0100);
    assert_eq!(
        Ltdc::new(file).map(|_| ()),
        Err(LtdcError::Version(0x0004_0100)),
        "the STM32MP25's layout is another"
    );
}

#[test]
fn events_are_taken_and_cleared() {
    let mut file = RegisterFile::stm32mp15();
    let _ = file.values.insert(ltdc::ISR, ltdc::INT_RR | ltdc::INT_FUE);
    let mut ltdc = Ltdc::new(file).expect("an STM32MP15 LTDC");
    let events = ltdc.take_events();
    assert!(
        events.reloaded && events.underrun && !events.transfer_error,
        "{events:?}"
    );
    assert_eq!(ltdc.registers().value(ltdc::ISR), 0, "cleared");
    assert_eq!(ltdc.take_events(), ltdc::Events::default(), "nothing twice");
}

// ---------------------------------------------------------------------------
// The I2C controller, and what is on its bus
// ---------------------------------------------------------------------------

/// A target on the modelled bus.
trait Target {
    fn start(&mut self, read: bool);
    fn write(&mut self, byte: u8);
    fn read(&mut self) -> u8;
}

/// A register file behind a pointer, as the `SiI9022` and an EEPROM are.
#[derive(Debug, Clone)]
struct Pointer {
    bytes: [u8; 256],
    at: u8,
    first: bool,
    written: Vec<(u8, u8)>,
}

impl Pointer {
    fn new(bytes: [u8; 256]) -> Self {
        Pointer {
            bytes,
            at: 0,
            first: false,
            written: Vec::new(),
        }
    }
}

/// The `SiI9022`: its TPI registers, the chip id visible only in TPI mode,
/// hotplug, and the DDC switch.
#[derive(Debug, Clone)]
struct FakeBridge {
    registers: Pointer,
    tpi: bool,
    granted: bool,
}

impl FakeBridge {
    fn new(plugged: bool) -> Self {
        let mut bytes = [0_u8; 256];
        bytes[usize::from(sii9022::SYS_CTRL)] = sii9022::SYS_POWER_DOWN;
        bytes[usize::from(sii9022::INT_STATUS)] = if plugged { sii9022::PLUGGED | 1 } else { 1 };
        bytes[usize::from(sii9022::POWER_STATE)] = 0x02;
        FakeBridge {
            registers: Pointer::new(bytes),
            tpi: false,
            granted: false,
        }
    }

    fn register(&self, register: u8) -> u8 {
        self.registers.bytes[usize::from(register)]
    }

    /// Whether the switch onto the monitor's bus is closed: both DDC bits
    /// written back after the grant.
    fn switch_closed(&self) -> bool {
        let control = self.register(sii9022::SYS_CTRL);
        control & (sii9022::SYS_DDC_REQUEST | sii9022::SYS_DDC_GRANTED)
            == sii9022::SYS_DDC_REQUEST | sii9022::SYS_DDC_GRANTED
    }
}

impl Target for FakeBridge {
    fn start(&mut self, read: bool) {
        self.registers.first = !read;
    }

    fn write(&mut self, byte: u8) {
        let r = &mut self.registers;
        if r.first {
            r.at = byte;
            r.first = false;
            return;
        }
        r.written.push((r.at, byte));
        match r.at {
            sii9022::TPI_REQUEST if byte == 0 => self.tpi = true,
            // Write-one-to-clear.
            sii9022::INT_STATUS => {
                let index = usize::from(sii9022::INT_STATUS);
                r.bytes[index] &= !(byte & 0x03);
            }
            sii9022::SYS_CTRL => {
                if byte & sii9022::SYS_DDC_REQUEST == 0 {
                    self.granted = false;
                    r.bytes[usize::from(r.at)] = byte & !sii9022::SYS_DDC_GRANTED;
                } else {
                    r.bytes[usize::from(r.at)] = byte;
                }
            }
            _ => r.bytes[usize::from(r.at)] = byte,
        }
        r.at = r.at.wrapping_add(1);
    }

    fn read(&mut self) -> u8 {
        let at = self.registers.at;
        self.registers.at = at.wrapping_add(1);
        match at {
            0x1B..=0x1E if !self.tpi => 0xFF,
            0x1B => 0xB0,
            0x1C => 0x02,
            0x1D => 0x03,
            sii9022::SYS_CTRL => {
                let value = self.register(at);
                // Granted a read after it was asked for.
                if value & sii9022::SYS_DDC_REQUEST != 0 && !self.granted {
                    self.granted = true;
                    value
                } else if self.granted {
                    value | sii9022::SYS_DDC_GRANTED
                } else {
                    value
                }
            }
            _ => self.register(at),
        }
    }
}

impl Target for Pointer {
    fn start(&mut self, read: bool) {
        self.first = !read;
    }

    fn write(&mut self, byte: u8) {
        if self.first {
            self.at = byte;
            self.first = false;
        } else {
            self.written.push((self.at, byte));
            self.at = self.at.wrapping_add(1);
        }
    }

    fn read(&mut self) -> u8 {
        let value = self.bytes[usize::from(self.at)];
        self.at = self.at.wrapping_add(1);
        value
    }
}

/// Where a transfer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Writing {
        address: u8,
        left: u32,
        autoend: bool,
    },
    Reading {
        address: u8,
        left: u32,
        autoend: bool,
    },
}

/// The I2C controller, running transfers against the bridge and, while the
/// bridge's switch is closed, the monitor's EEPROM.
#[derive(Debug, Clone)]
struct FakeI2c {
    registers: BTreeMap<u32, u32>,
    status: u32,
    phase: Phase,
    bridge: FakeBridge,
    eeprom: Pointer,
    /// Every START as (address, read).
    starts: Vec<(u8, bool)>,
}

impl FakeI2c {
    fn new(bridge: FakeBridge, eeprom: [u8; 256]) -> Self {
        FakeI2c {
            registers: BTreeMap::new(),
            status: 0,
            phase: Phase::Idle,
            bridge,
            eeprom: Pointer::new(eeprom),
            starts: Vec::new(),
        }
    }

    fn target(&mut self, address: u8) -> Option<&mut dyn Target> {
        if address == sii9022::ADDRESS {
            Some(&mut self.bridge)
        } else if address == sii9022::EDID_ADDRESS && self.bridge.switch_closed() {
            Some(&mut self.eeprom)
        } else {
            None
        }
    }

    fn finish(&mut self, autoend: bool) {
        self.phase = Phase::Idle;
        self.status |= if autoend { i2c::ISR_STOPF } else { i2c::ISR_TC };
    }
}

/// The model sits behind a `RefCell` so a read of `ISR` or `RXDR` can move
/// the transfer on, as the hardware's does.
#[derive(Debug)]
struct Shared(std::cell::RefCell<FakeI2c>);

impl Registers for Shared {
    fn read32(&self, offset: u32) -> u32 {
        let mut i2c = self.0.borrow_mut();
        match offset {
            i2c::ISR => i2c.status,
            i2c::RXDR => {
                let Phase::Reading {
                    address,
                    left,
                    autoend,
                } = i2c.phase
                else {
                    return 0;
                };
                let byte = i2c.target(address).map_or(0xFF, Target::read);
                i2c.status &= !i2c::ISR_RXNE;
                if left > 1 {
                    i2c.phase = Phase::Reading {
                        address,
                        left: left - 1,
                        autoend,
                    };
                    i2c.status |= i2c::ISR_RXNE;
                } else {
                    i2c.finish(autoend);
                }
                u32::from(byte)
            }
            _ => i2c.registers.get(&offset).copied().unwrap_or(0),
        }
    }

    fn write32(&mut self, offset: u32, value: u32) {
        let i2c = self.0.get_mut();
        let _ = i2c.registers.insert(offset, value);
        match offset {
            i2c::ICR => i2c.status &= !value,
            i2c::CR2 if value & i2c::CR2_START != 0 => {
                let address = ((value >> 1) & 0x7F) as u8;
                let read = value & i2c::CR2_RD_WRN != 0;
                let left = (value >> 16) & 0xFF;
                let autoend = value & i2c::CR2_AUTOEND != 0;
                i2c.starts.push((address, read));
                i2c.status &= !i2c::ISR_TC;
                match i2c.target(address) {
                    None => {
                        // Not acknowledged: the controller sends a STOP.
                        i2c.phase = Phase::Idle;
                        i2c.status |= i2c::ISR_NACKF | i2c::ISR_STOPF;
                    }
                    Some(target) => {
                        target.start(read);
                        if read {
                            i2c.phase = Phase::Reading {
                                address,
                                left,
                                autoend,
                            };
                            i2c.status |= i2c::ISR_RXNE;
                        } else {
                            i2c.phase = Phase::Writing {
                                address,
                                left,
                                autoend,
                            };
                            i2c.status |= i2c::ISR_TXIS;
                        }
                    }
                }
            }
            i2c::TXDR => {
                let Phase::Writing {
                    address,
                    left,
                    autoend,
                } = i2c.phase
                else {
                    return;
                };
                if let Some(target) = i2c.target(address) {
                    target.write((value & 0xFF) as u8);
                }
                i2c.status &= !i2c::ISR_TXIS;
                if left > 1 {
                    i2c.phase = Phase::Writing {
                        address,
                        left: left - 1,
                        autoend,
                    };
                    i2c.status |= i2c::ISR_TXIS;
                } else {
                    i2c.finish(autoend);
                }
            }
            _ => {}
        }
    }
}

/// A monitor's EEPROM: a base block naming it, preferring 720p60, and a
/// CEA extension with an HDMI vendor block.
fn eeprom(hdmi: bool) -> [u8; 256] {
    let mut bytes = [0_u8; 256];
    bytes[..8].copy_from_slice(&edid::HEADER);
    bytes[54..72].copy_from_slice(&DTD_720P60);
    bytes[72..77].copy_from_slice(&[0, 0, 0, 0xFC, 0]);
    bytes[77..90].copy_from_slice(b"FERRIX TEST\n ");
    bytes[126] = 1;
    let sum = bytes[..127].iter().fold(0_u8, |s, b| s.wrapping_add(*b));
    bytes[127] = 0_u8.wrapping_sub(sum);

    let ext = &mut bytes[128..];
    ext[0] = edid::CEA_EXTENSION;
    ext[1] = 3;
    ext[2] = 12;
    if hdmi {
        // Vendor-specific block, five bytes: HDMI's OUI and a source address.
        ext[4..10].copy_from_slice(&[(3 << 5) | 5, 0x03, 0x0C, 0x00, 0x10, 0x00]);
    }
    let sum = ext[..127].iter().fold(0_u8, |s, b| s.wrapping_add(*b));
    ext[127] = 0_u8.wrapping_sub(sum);
    bytes
}

fn bus(plugged: bool, hdmi: bool) -> I2c<Shared> {
    let fake = FakeI2c::new(FakeBridge::new(plugged), eeprom(hdmi));
    I2c::new(
        Shared(std::cell::RefCell::new(fake)),
        i2c::TIMING_100KHZ_AT_64MHZ,
    )
}

fn model(bridge: &Bridge<I2c<Shared>>) -> std::cell::Ref<'_, FakeI2c> {
    bridge.bus().registers().0.borrow()
}

#[test]
fn the_timing_is_a_prescaler_of_sixteen_and_the_standard_mode_counts() {
    assert_eq!(i2c::TIMING_100KHZ_AT_64MHZ, 0xF042_0F13, "the word itself");
    let bus = bus(true, true);
    let fake = bus.registers().0.borrow();
    assert_eq!(
        fake.registers.get(&i2c::TIMINGR),
        Some(&0xF042_0F13),
        "programmed"
    );
    assert_eq!(
        fake.registers.get(&i2c::CR1),
        Some(&i2c::CR1_PE),
        "and enabled after"
    );
}

#[test]
fn probe_turns_tpi_on_before_it_reads_the_chip_id() {
    let bridge = Bridge::probe(bus(true, true), sii9022::ADDRESS).expect("a SiI9022");
    let [first, second, third, _] = bridge.id();
    assert_eq!(
        [first, second, third],
        [0xB0, 0x02, 0x03],
        "the id a SiI9022A answers"
    );
    let fake = model(&bridge);
    assert_eq!(
        fake.bridge.registers.written.first(),
        Some(&(sii9022::TPI_REQUEST, 0)),
        "TPI first"
    );
    assert_eq!(
        fake.bridge.register(sii9022::INT_STATUS) & 1,
        0,
        "the pending hotplug event cleared"
    );
}

#[test]
fn nothing_on_the_bus_is_a_nack_not_a_hang() {
    let mut bus = bus(true, true);
    assert_eq!(
        bus.write(0x22, &[0]),
        Err(BusError::Nack),
        "no device at 0x22"
    );
    // And the bus still works after it.
    let mut id = [0_u8; 1];
    bus.write(sii9022::ADDRESS, &[sii9022::TPI_REQUEST, 0])
        .expect("the bridge acks");
    bus.write_read(sii9022::ADDRESS, &[sii9022::CHIP_ID], &mut id)
        .expect("a read");
    assert_eq!(id, [0xB0], "the chip id");
}

#[test]
fn hotplug_is_the_plugged_bit() {
    let mut plugged = Bridge::probe(bus(true, true), sii9022::ADDRESS).expect("a SiI9022");
    assert_eq!(plugged.plugged(), Ok(true), "a monitor");
    let mut empty = Bridge::probe(bus(false, true), sii9022::ADDRESS).expect("a SiI9022");
    assert_eq!(empty.plugged(), Ok(false), "nothing in the socket");
}

#[test]
fn the_edid_is_read_through_the_switch_and_the_bus_given_back() {
    let mut bridge = Bridge::probe(bus(true, true), sii9022::ADDRESS).expect("a SiI9022");
    let mut base = [0_u8; 128];
    bridge.read_edid(0, &mut base).expect("the base block");
    assert!(edid::is_base(&base), "whole");
    assert_eq!(edid::extensions(&base), 1, "one extension");
    let preferred = edid::preferred(&base).expect("a preferred timing");
    assert!(preferred.same_timing(&Mode::CEA_720P60), "{preferred:?}");
    let (name, len) = edid::name(&base).expect("a name");
    assert_eq!(&name[..len], b"FERRIX TEST", "up to the newline");

    let mut extension = [0_u8; 128];
    bridge.read_edid(1, &mut extension).expect("the extension");
    assert!(edid::is_hdmi(&extension), "an HDMI sink");

    let fake = model(&bridge);
    assert!(!fake.bridge.switch_closed(), "the switch opened again");
    assert_eq!(
        fake.bridge.register(sii9022::SYS_CTRL)
            & (sii9022::SYS_DDC_REQUEST | sii9022::SYS_DDC_GRANTED),
        0,
        "the bus given back"
    );
    assert!(
        fake.starts.contains(&(sii9022::EDID_ADDRESS, true)),
        "the EEPROM was read"
    );
}

#[test]
fn a_dvi_sink_has_no_hdmi_block() {
    let mut bridge = Bridge::probe(bus(true, false), sii9022::ADDRESS).expect("a SiI9022");
    let mut extension = [0_u8; 128];
    bridge.read_edid(1, &mut extension).expect("the extension");
    assert!(!edid::is_hdmi(&extension), "DVI");
}

#[test]
fn set_mode_then_enable_is_linux_order_with_tmds_on_last() {
    let mut bridge = Bridge::probe(bus(true, true), sii9022::ADDRESS).expect("a SiI9022");
    bridge
        .set_mode(&Mode::CEA_720P60, true)
        .expect("720p is carried");
    bridge.enable().expect("on");
    let fake = model(&bridge);
    let written: Vec<(u8, u8)> = fake.bridge.registers.written.clone();
    drop(fake);
    let position = |pair: (u8, u8)| written.iter().position(|&w| w == pair);

    // TMDS off and HDMI chosen before the mode goes in.
    let off = position((
        sii9022::SYS_CTRL,
        sii9022::SYS_POWER_DOWN | sii9022::SYS_OUTPUT_HDMI,
    ))
    .expect("TMDS off, HDMI");
    // 7425 x 10 kHz, 60 Hz, 1280 x 720, 24-bit 1x, RGB.
    let video = [
        0x01,
        0x1D,
        60,
        0,
        0x00,
        0x05,
        0xD0,
        0x02,
        sii9022::PIXEL_24BIT_1X,
        sii9022::INPUT_RGB,
    ];
    for (offset, byte) in video.iter().enumerate() {
        let at = position((offset as u8, *byte)).expect("the video data");
        assert!(at > off, "after TMDS went off");
    }
    let infoframe = sii9022::avi_infoframe(4, 1280, 720);
    for (offset, byte) in infoframe.iter().enumerate() {
        assert!(
            position((sii9022::AVI_INFOFRAME + offset as u8, *byte)).is_some(),
            "infoframe byte {offset}"
        );
    }
    let on = written
        .iter()
        .rposition(|&(register, _)| register == sii9022::SYS_CTRL)
        .expect("SYS_CTRL last written");
    assert_eq!(
        written
            .get(on)
            .map(|&(_, value)| value & sii9022::SYS_POWER_DOWN),
        Some(0),
        "TMDS on"
    );
    assert_eq!(
        position((sii9022::POWER_STATE, 0x00)).map(|p| p < on),
        Some(true),
        "D0 before TMDS"
    );
}

#[test]
fn a_clock_the_bridge_cannot_carry_is_refused() {
    let mut bridge = Bridge::probe(bus(true, true), sii9022::ADDRESS).expect("a SiI9022");
    let slow = Mode {
        clock_khz: 20_000,
        ..Mode::CEA_720P60
    };
    assert_eq!(
        bridge.set_mode(&slow, false),
        Err(BridgeError::Mode),
        "under 25 MHz"
    );
}

#[test]
fn the_avi_infoframe_sums_to_zero_with_its_header() {
    let frame = sii9022::avi_infoframe(4, 1280, 720);
    let sum = [0x82_u8, 0x02, 0x0D]
        .iter()
        .chain(frame.iter())
        .fold(0_u8, |s, b| s.wrapping_add(*b));
    assert_eq!(sum, 0, "checksum");
    assert_eq!(frame[1], 0x10, "RGB, active format present");
    assert_eq!(frame[2], 0x28, "16:9, format as the picture");
    assert_eq!(frame[4], 4, "VIC 4");
    assert_eq!(
        sii9022::avi_infoframe(0, 1024, 768)[2],
        0x18,
        "4:3 for a square one"
    );
}

#[test]
fn a_budget_looks_at_most_its_count_and_once_more() {
    let mut looks = 0;
    assert!(
        !Budget(5).wait(|| {
            looks += 1;
            false
        }),
        "never true"
    );
    assert_eq!(looks, 6, "five and the last look");
}
