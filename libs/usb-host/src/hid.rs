//! The HID boot protocol: a keyboard's eight-byte report and a mouse's three
//! or four, turned into the events Linux's HID driver makes of them, and the
//! HELLO that declares those events.
//!
//! A boot report's layout is fixed by the HID specification's appendix B,
//! which is what lets a driver read one without the device's report
//! descriptor. The keyboard's usages become key codes by Linux's own table,
//! `hid_keyboard` in `drivers/hid/hid-input.c`, so a key reads the same here
//! as it does there; its events come in Linux's order too: the modifiers,
//! then keys let go, then keys pressed, then `SYN_REPORT`.

use ferrix_inputctl::message::{Bitmaps, DeviceId, Hello, RawEvent, Text, VERSION};
use ferrix_linux_abi::input::{
    BTN_LEFT, BUS_USB, EV_KEY, EV_REL, EV_SYN, REL_WHEEL, REL_X, REL_Y, SYN_REPORT,
};

/// What a boot interface is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A keyboard.
    Keyboard,
    /// A mouse.
    Mouse,
}

/// `hid_keyboard`: each keyboard usage's key code, zero for none.
pub const KEYBOARD_CODES: [u8; 256] = [
    0, 0, 0, 0, 30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, //
    50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44, 2, 3, //
    4, 5, 6, 7, 8, 9, 10, 11, 28, 1, 14, 15, 57, 12, 13, 26, //
    27, 43, 43, 39, 40, 41, 51, 52, 53, 58, 59, 60, 61, 62, 63, 64, //
    65, 66, 67, 68, 87, 88, 99, 70, 119, 110, 102, 104, 111, 107, 109, 106, //
    105, 108, 103, 69, 98, 55, 74, 78, 96, 79, 80, 81, 75, 76, 77, 71, //
    72, 73, 82, 83, 86, 127, 116, 117, 183, 184, 185, 186, 187, 188, 189, 190, //
    191, 192, 193, 194, 134, 138, 130, 132, 128, 129, 131, 137, 133, 135, 136, 113, //
    115, 114, 0, 0, 0, 121, 0, 89, 93, 124, 92, 94, 95, 0, 0, 0, //
    122, 123, 90, 91, 85, 0, 0, 0, 0, 0, 0, 0, 111, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 179, 180, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 111, 0, 0, 0, 0, 0, 0, 0, //
    29, 42, 56, 125, 97, 54, 100, 126, 164, 166, 165, 163, 161, 115, 114, 113, //
    150, 158, 159, 128, 136, 177, 178, 176, 142, 152, 173, 140, 0, 0, 0, 0, //
];

/// The usage of the first modifier, left control: bit 0 of the report's
/// first byte, and bit `n` is usage `0xE0 + n`.
const FIRST_MODIFIER: usize = 0xE0;

/// A report's key slots: bytes 2 to 7.
const KEY_SLOTS: usize = 6;

/// `ErrorRollOver`: every slot says it when more keys are down than fit.
const ROLL_OVER: u8 = 0x01;

/// The mouse buttons a boot report's first byte may carry, bit by bit:
/// left, right, middle, side, extra.
const MOUSE_BUTTONS: u16 = 5;

/// The most events one report makes: eight modifiers, six keys let go and
/// six pressed, and `SYN_REPORT`.
pub const MAX_REPORT_EVENTS: usize = 8 + KEY_SLOTS * 2 + 1;

/// The events one report made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EventBuf {
    events: [RawEvent; MAX_REPORT_EVENTS],
    len: usize,
}

impl Default for EventBuf {
    fn default() -> Self {
        EventBuf {
            events: [RawEvent::default(); MAX_REPORT_EVENTS],
            len: 0,
        }
    }
}

impl EventBuf {
    fn push(&mut self, kind: u16, code: u16, value: i32) {
        if let Some(slot) = self.events.get_mut(self.len) {
            *slot = RawEvent::new(kind, code, value);
            self.len += 1;
        }
    }

    /// Close the report with `SYN_REPORT`, if it holds anything.
    fn finish(&mut self) {
        if self.len > 0 {
            self.push(EV_SYN, SYN_REPORT, 0);
        }
    }

    /// The events, empty for a report that changed nothing.
    #[must_use]
    pub fn as_slice(&self) -> &[RawEvent] {
        self.events.get(..self.len).unwrap_or(&[])
    }
}

/// What a keyboard last reported.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KeyboardState {
    modifiers: u8,
    keys: [u8; KEY_SLOTS],
}

/// What a mouse last reported.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MouseState {
    buttons: u8,
}

/// A boot interface's state between reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// A keyboard's.
    Keyboard(KeyboardState),
    /// A mouse's.
    Mouse(MouseState),
}

impl State {
    /// A fresh state for `kind`: nothing pressed.
    #[must_use]
    pub const fn new(kind: Kind) -> State {
        match kind {
            Kind::Keyboard => State::Keyboard(KeyboardState {
                modifiers: 0,
                keys: [0; KEY_SLOTS],
            }),
            Kind::Mouse => State::Mouse(MouseState { buttons: 0 }),
        }
    }

    /// The events `report` makes, given what came before it.
    pub fn report(&mut self, report: &[u8]) -> EventBuf {
        match self {
            State::Keyboard(state) => keyboard_report(state, report),
            State::Mouse(state) => mouse_report(state, report),
        }
    }
}

/// A keyboard's report: the modifiers that changed, the keys no longer in
/// the report, the keys new to it. A report short of eight bytes, or one
/// saying `ErrorRollOver`, changes nothing, as Linux ignores it.
fn keyboard_report(state: &mut KeyboardState, report: &[u8]) -> EventBuf {
    let mut out = EventBuf::default();
    let (Some(&modifiers), Some(keys)) = (report.first(), report.get(2..2 + KEY_SLOTS)) else {
        return out;
    };
    if keys.iter().all(|&key| key == ROLL_OVER) {
        return out;
    }
    let mut now = [0_u8; KEY_SLOTS];
    now.copy_from_slice(keys);

    let changed = modifiers ^ state.modifiers;
    for bit in 0..8_usize {
        if changed & (1 << bit) != 0 {
            let down = modifiers & (1 << bit) != 0;
            push_key(&mut out, FIRST_MODIFIER + bit, down);
        }
    }
    for &key in &state.keys {
        if key > ROLL_OVER && !now.contains(&key) {
            push_key(&mut out, usize::from(key), false);
        }
    }
    for &key in &now {
        if key > ROLL_OVER && !state.keys.contains(&key) {
            push_key(&mut out, usize::from(key), true);
        }
    }
    state.modifiers = modifiers;
    state.keys = now;
    out.finish();
    out
}

/// A key event for `usage`, if the usage has a code.
fn push_key(out: &mut EventBuf, usage: usize, down: bool) {
    match KEYBOARD_CODES.get(usage) {
        Some(&code) if code != 0 => out.push(EV_KEY, u16::from(code), i32::from(down)),
        _ => {}
    }
}

/// A mouse's report: the buttons that changed, then the motion, then the
/// wheel, which a boot report may carry as a fourth byte.
fn mouse_report(state: &mut MouseState, report: &[u8]) -> EventBuf {
    let mut out = EventBuf::default();
    let (Some(&buttons), Some(&x), Some(&y)) = (report.first(), report.get(1), report.get(2))
    else {
        return out;
    };
    let changed = buttons ^ state.buttons;
    for bit in 0..MOUSE_BUTTONS {
        if changed & (1 << bit) != 0 {
            out.push(EV_KEY, BTN_LEFT + bit, i32::from(buttons & (1 << bit) != 0));
        }
    }
    state.buttons = buttons;
    let signed = |byte: u8| i32::from(byte.cast_signed());
    if x != 0 {
        out.push(EV_REL, REL_X, signed(x));
    }
    if y != 0 {
        out.push(EV_REL, REL_Y, signed(y));
    }
    if let Some(&wheel) = report.get(3)
        && wheel != 0
    {
        out.push(EV_REL, REL_WHEEL, signed(wheel));
    }
    out.finish();
    out
}

/// Who a device says it is, for its HELLO.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Identity {
    /// `idVendor`.
    pub vendor: u16,
    /// `idProduct`.
    pub product: u16,
    /// `bcdDevice`.
    pub release: u16,
    /// Its name: the manufacturer's and product's strings, as Linux joins
    /// them.
    pub name: Text,
}

/// The HELLO for a boot interface of `kind`, at `location` -- which for a
/// device tree node is `DEVICE_NOT_PCI`, what START said.
#[must_use]
pub fn hello(kind: Kind, identity: &Identity, location: u32) -> Hello {
    let mut hello = Hello::EMPTY;
    hello.version = VERSION;
    hello.location = location;
    hello.id = DeviceId {
        bustype: BUS_USB,
        vendor: identity.vendor,
        product: identity.product,
        version: identity.release,
    };
    hello.name = identity.name;
    hello.bits = bits(kind);
    hello
}

/// What a boot interface of `kind` declares.
#[must_use]
pub fn bits(kind: Kind) -> Bitmaps {
    let mut bits = Bitmaps::EMPTY;
    set(&mut bits.types, EV_SYN);
    set(&mut bits.types, EV_KEY);
    match kind {
        Kind::Keyboard => {
            for &code in KEYBOARD_CODES.iter().filter(|&&code| code != 0) {
                set(&mut bits.keys, u16::from(code));
            }
        }
        Kind::Mouse => {
            set(&mut bits.types, EV_REL);
            for bit in 0..MOUSE_BUTTONS {
                set(&mut bits.keys, BTN_LEFT + bit);
            }
            for code in [REL_X, REL_Y, REL_WHEEL] {
                set(&mut bits.rels, code);
            }
        }
    }
    bits
}

fn set(bits: &mut [u8], code: u16) {
    if let Some(byte) = bits.get_mut(usize::from(code / 8)) {
        *byte |= 1 << (code % 8);
    }
}

/// A device's name from its manufacturer's and product's strings, as Linux's
/// HID core joins them: the product alone if it already starts with the
/// manufacturer, either alone if the other is missing, and `fallback` if
/// both are.
#[must_use]
pub fn name(manufacturer: &[u8], product: &[u8], fallback: &[u8]) -> Text {
    let mut joined = [0_u8; 128];
    let mut len = 0_usize;
    let mut append = |part: &[u8]| {
        for &byte in part {
            if let Some(slot) = joined.get_mut(len) {
                *slot = byte;
                len += 1;
            }
        }
    };
    match (manufacturer.is_empty(), product.is_empty()) {
        (true, true) => append(fallback),
        (true, false) => append(product),
        (false, true) => append(manufacturer),
        (false, false) if product.starts_with(manufacturer) => append(product),
        (false, false) => {
            append(manufacturer);
            append(b" ");
            append(product);
        }
    }
    Text::new(joined.get(..len).unwrap_or(&[])).unwrap_or(Text::NONE)
}
