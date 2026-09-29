//! The names of the types and codes an event can carry.
//!
//! An evdev event is three numbers, and a line saying `1 30 1` proves
//! nothing to a reader. These are the kernel's own names, from
//! `ferrix_linux_abi::input`, for the subset a compositor sees: enough that
//! a test can look for `KEY_A` and a person can read the log.
//!
//! A code with no name is printed as its number, which is what `evtest`
//! does; the table is not meant to be complete.

use ferrix_linux_abi::input::{
    ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID, ABS_X, ABS_Y, BTN_EXTRA,
    BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE, BTN_TOUCH, EV_ABS, EV_FF, EV_KEY, EV_LED, EV_MSC,
    EV_PWR, EV_REL, EV_REP, EV_SND, EV_SW, EV_SYN, KEY_A, KEY_ESC, KEY_RESERVED, MSC_SCAN,
    REL_HWHEEL, REL_WHEEL, REL_X, REL_Y, REP_DELAY, REP_PERIOD, SYN_CONFIG, SYN_DROPPED,
    SYN_MT_REPORT, SYN_REPORT,
};

/// The name of an event type, or `None` for one with no name here.
#[must_use]
pub fn event_type(kind: u16) -> Option<&'static str> {
    Some(match kind {
        EV_SYN => "EV_SYN",
        EV_KEY => "EV_KEY",
        EV_REL => "EV_REL",
        EV_ABS => "EV_ABS",
        EV_MSC => "EV_MSC",
        EV_SW => "EV_SW",
        EV_LED => "EV_LED",
        EV_SND => "EV_SND",
        EV_REP => "EV_REP",
        EV_FF => "EV_FF",
        EV_PWR => "EV_PWR",
        _ => return None,
    })
}

/// The name of a code of `kind`, or `None` for one with no name here.
///
/// Codes are per type: 1 is `KEY_ESC` under `EV_KEY` and `REL_Y` under
/// `EV_REL`.
#[must_use]
pub fn code(kind: u16, code: u16) -> Option<&'static str> {
    Some(match (kind, code) {
        (EV_SYN, SYN_REPORT) => "SYN_REPORT",
        (EV_SYN, SYN_CONFIG) => "SYN_CONFIG",
        (EV_SYN, SYN_MT_REPORT) => "SYN_MT_REPORT",
        (EV_SYN, SYN_DROPPED) => "SYN_DROPPED",
        (EV_KEY, KEY_RESERVED) => "KEY_RESERVED",
        (EV_KEY, KEY_ESC) => "KEY_ESC",
        (EV_KEY, KEY_A) => "KEY_A",
        (EV_KEY, BTN_LEFT) => "BTN_LEFT",
        (EV_KEY, BTN_RIGHT) => "BTN_RIGHT",
        (EV_KEY, BTN_MIDDLE) => "BTN_MIDDLE",
        (EV_KEY, BTN_SIDE) => "BTN_SIDE",
        (EV_KEY, BTN_EXTRA) => "BTN_EXTRA",
        (EV_KEY, BTN_TOUCH) => "BTN_TOUCH",
        (EV_REL, REL_X) => "REL_X",
        (EV_REL, REL_Y) => "REL_Y",
        (EV_REL, REL_HWHEEL) => "REL_HWHEEL",
        (EV_REL, REL_WHEEL) => "REL_WHEEL",
        (EV_ABS, ABS_X) => "ABS_X",
        (EV_ABS, ABS_Y) => "ABS_Y",
        (EV_ABS, ABS_MT_SLOT) => "ABS_MT_SLOT",
        (EV_ABS, ABS_MT_POSITION_X) => "ABS_MT_POSITION_X",
        (EV_ABS, ABS_MT_POSITION_Y) => "ABS_MT_POSITION_Y",
        (EV_ABS, ABS_MT_TRACKING_ID) => "ABS_MT_TRACKING_ID",
        (EV_MSC, MSC_SCAN) => "MSC_SCAN",
        (EV_REP, REP_DELAY) => "REP_DELAY",
        (EV_REP, REP_PERIOD) => "REP_PERIOD",
        _ => return None,
    })
}

/// `kind` as its name, or as a number for one with no name.
#[must_use]
pub fn event_type_text(kind: u16) -> String {
    event_type(kind).map_or_else(|| format!("{kind:#04x}"), str::to_owned)
}

/// `code` of `kind` as its name, or as a number for one with no name.
#[must_use]
pub fn code_text(kind: u16, code_number: u16) -> String {
    code(kind, code_number).map_or_else(|| format!("{code_number:#06x}"), str::to_owned)
}
