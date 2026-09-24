//! The input devices, and what their events mean.
//!
//! `compositor/evecho` opens a `/dev/input/eventN` and reads whole
//! `input_event`s. This turns those into the [`Input`]s the seat understands:
//! evdev's relative axes into pixels, its absolute axes into a fraction of
//! the device's own range, its keys and buttons apart, and its `SYN_REPORT`
//! into the end of a group.
//!
//! # Why absolute axes are a fraction
//!
//! A tablet reports where it is in its own units -- QEMU's virtio-tablet in
//! 0 to 32767 -- and only the device knows what its range is, through
//! `EVIOCGABS`. The seat knows how big the screen is. So the conversion is
//! here, where the range is, and the seat multiplies by the screen: a device
//! with another range then needs no change anywhere else.
//!
//! # What is left out
//!
//! Touch is not forwarded: `wl_touch` would need the whole multi-touch
//! protocol, and the seat announces no touch capability, so a client would
//! never be given an object to send it on. `EV_MSC` scan codes, `EV_SW`
//! switches and `EV_LED` are dropped, which is what libinput does with them
//! for a Wayland seat.

use std::io;
use std::path::Path;

use compositor_evecho::{Device, event_nodes};
use ferrix_linux_abi::input::{
    ABS_X, ABS_Y, BTN_MISC, EV_ABS, EV_KEY, EV_LED, EV_REL, EV_SYN, Event, KEY_MAX, LED_CAPSL,
    LED_NUML, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y, SYN_REPORT,
};

use crate::seat::Input;

/// How far one wheel click scrolls, in surface coordinates.
///
/// libinput reports a wheel click as 15 units, which is what every toolkit
/// expects one to be, and `wl_pointer.axis` carries that distance.
const WHEEL_STEP: f64 = 15.0;

/// A device's absolute axes: what its range is, and where it last said it
/// was.
///
/// Its own value, and not part of [`Open`], so that the conversion from a
/// device's units to a fraction of the screen is tested without a device.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Axes {
    /// The range of `ABS_X` and `ABS_Y`, for a device that has them.
    range: Option<((i32, i32), (i32, i32))>,
    /// Where it last said it was: a report may carry one axis and not the
    /// other, and the pointer has to go somewhere in both.
    at: (i32, i32),
}

/// One open device, with what its absolute axes are worth.
#[derive(Debug)]
struct Open {
    device: Device,
    node: String,
    axes: Axes,
}

/// Every device the compositor reads.
#[derive(Debug, Default)]
pub struct Devices {
    open: Vec<Open>,
    events: Vec<Event>,
    lights: Lights,
    /// Nodes that could not be opened, so that a rescan says why once and
    /// not every time it looks.
    refused: Vec<String>,
}

/// What the keyboards' LEDs were last told: Caps Lock and Num Lock, from the
/// keymap's *locked* modifiers, as libxkbcommon's LED indicators follow
/// them -- a Caps Lock held down lights nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lights {
    shown: Option<(bool, bool)>,
}

impl Lights {
    /// The LEDs to write for `locked`, the keymap's locked modifier mask:
    /// `None` when they already show it. The first call always writes, which
    /// clears whatever a keyboard was left showing.
    pub fn change(&mut self, locked: u32) -> Option<[(u16, bool); 2]> {
        let now = (
            locked & compositor_xkb::generated::LOCK != 0,
            locked & compositor_xkb::generated::MOD2 != 0,
        );
        if self.shown == Some(now) {
            return None;
        }
        self.shown = Some(now);
        Some([(LED_CAPSL, now.0), (LED_NUML, now.1)])
    }
}

impl Devices {
    /// Open every `/dev/input/eventN` and take it for this compositor.
    ///
    /// A device that cannot be opened is one device, not a failure: a machine
    /// with a keyboard and a device the compositor cannot read should still
    /// have a keyboard. The reasons are returned so the caller can say them.
    ///
    /// # Errors
    ///
    /// Only a `/dev/input` that cannot be read at all, which is a machine
    /// whose kernel published no input devices.
    pub fn open() -> io::Result<(Self, Vec<String>)> {
        let mut devices = Self::default();
        let mut refused = Vec::new();
        for path in event_nodes()? {
            match Self::take(&path) {
                Ok(open) => devices.open.push(open),
                Err(error) => {
                    devices.refused.push(node_name(&path));
                    refused.push(format!("{}: {error}", path.display()));
                }
            }
        }
        Ok((devices, refused))
    }

    /// Open every device that has appeared since the last look, and let go
    /// of every one whose node has gone: what each new one is, and why any
    /// could not be opened.
    ///
    /// A USB keyboard behind a hub is found a second or so after the host
    /// controller starts, and on a board whose boot is quick that is after
    /// the compositor opened `/dev/input` (`docs/INPUT.md` §7.3); one
    /// plugged in later is found later still. So the compositor looks again
    /// every so often rather than only once, as a compositor that has udev
    /// is told.
    pub fn rescan(&mut self) -> (Vec<String>, Vec<String>) {
        let Ok(nodes) = event_nodes() else {
            return (Vec::new(), Vec::new());
        };
        let names: Vec<String> = nodes.iter().map(|path| node_name(path)).collect();
        self.open.retain(|open| names.contains(&open.node));
        self.refused.retain(|node| names.contains(node));
        let mut added = Vec::new();
        let mut refused = Vec::new();
        for (path, name) in nodes.iter().zip(names) {
            if self.open.iter().any(|open| open.node == name) || self.refused.contains(&name) {
                continue;
            }
            match Self::take(path) {
                Ok(open) => {
                    added.push(format!("{} {}", open.node, open.device.description().name));
                    self.open.push(open);
                }
                Err(error) => {
                    refused.push(format!("{}: {error}", path.display()));
                    self.refused.push(name);
                }
            }
        }
        (added, refused)
    }

    fn take(path: &Path) -> io::Result<Open> {
        let device = Device::open(path)?;
        // A compositor grabs its devices so that a key it acts on does not
        // also reach whatever else is reading the node. A refusal is not
        // fatal: another reader having the grab is a machine where both see
        // the keys, which is worse than one but better than none.
        let _ = device.grab(true);
        let range = if device.reports(EV_ABS) {
            match (device.axis(ABS_X), device.axis(ABS_Y)) {
                (Ok(x), Ok(y)) => Some(((x.minimum, x.maximum), (y.minimum, y.maximum))),
                _ => None,
            }
        } else {
            None
        };
        let node = node_name(path);
        Ok(Open {
            device,
            node,
            axes: Axes { range, at: (0, 0) },
        })
    }

    /// Show the keymap's locks on every keyboard that has LEDs, when they
    /// changed. A keyboard that will not take the write keeps what it shows.
    pub fn show_locks(&mut self, locked: u32) {
        let Some(leds) = self.lights.change(locked) else {
            return;
        };
        for open in self.open.iter().filter(|open| open.device.reports(EV_LED)) {
            let _ = open.device.write_leds(&leds);
        }
    }

    /// What each device is, for the compositor's log line.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        self.open
            .iter()
            .map(|open| format!("{} {}", open.node, open.device.description().name))
            .collect()
    }

    /// What each device is, for `hyprctl devices`: the node's number, its
    /// name, and which of Hyprland's five groups it belongs in.
    ///
    /// The grouping is [`Self::capabilities`]'s, which is libinput's for a
    /// device with no `INPUT_PROP_*` to go on: keys and no axes is a
    /// keyboard, axes is a pointer. A tablet reports absolute axes and is a
    /// pointer here, as it is everywhere else in this compositor -- Hyprland
    /// has a group of its own for one because it drives a stylus, which
    /// nothing here does.
    #[must_use]
    pub fn listed(&self) -> Vec<(u64, String, bool)> {
        self.open
            .iter()
            .enumerate()
            .map(|(at, open)| {
                let description = open.device.description();
                let axes =
                    description.types.contains(&EV_REL) || description.types.contains(&EV_ABS);
                (
                    u64::try_from(at).unwrap_or(0),
                    description.name.clone(),
                    !axes && description.types.contains(&EV_KEY),
                )
            })
            .collect()
    }

    /// How many devices are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.open.len()
    }

    /// Whether none is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// The device descriptors that wake the compositor when input arrives.
    pub fn raw_fds(&self) -> impl Iterator<Item = libc::c_int> + '_ {
        self.open.iter().map(|open| open.device.raw_fd())
    }

    /// Whether any device reports keys, and whether any reports a pointer:
    /// the two `wl_seat.capability` bits.
    #[must_use]
    pub fn capabilities(&self) -> (bool, bool) {
        let mut keyboard = false;
        let mut pointer = false;
        for open in &self.open {
            let description = open.device.description();
            // A device with keys and no axes is a keyboard; one with axes is
            // a pointer, and its buttons are the pointer's. libinput decides
            // the same way for a device with no `INPUT_PROP_*` to go on.
            let axes = description.types.contains(&EV_REL) || description.types.contains(&EV_ABS);
            if axes {
                pointer = true;
            } else if description.types.contains(&EV_KEY) {
                keyboard = true;
            }
        }
        (keyboard, pointer)
    }

    /// Whether any device is a pointer, which is whether there is a pointer
    /// to draw.
    #[must_use]
    pub fn has_pointer(&self) -> bool {
        self.capabilities().1
    }

    /// Read whatever is waiting on every device, in the order it arrived.
    ///
    /// Nothing waiting is an empty answer, not an error. The first pass uses
    /// this to sweep devices that were open before the event loop started;
    /// subsequent passes use [`Self::read_ready`].
    pub fn read(&mut self) -> Vec<Input> {
        self.read_ready(&self.raw_fds().collect::<Vec<_>>())
    }

    /// Read only devices whose descriptors `poll` reported ready.
    ///
    /// A ready device may have more than one input report waiting, so its
    /// whole non-blocking queue is drained. Other devices are left alone.
    pub fn read_ready(&mut self, ready: &[libc::c_int]) -> Vec<Input> {
        let mut inputs = Vec::new();
        for open in &mut self.open {
            if !ready.contains(&open.device.raw_fd()) {
                continue;
            }
            self.events.clear();
            // Nothing waiting is `Ok(0)`; an error is a device that has gone,
            // and the next pass will find it gone too.
            if open.device.read_events(&mut self.events).is_err() {
                continue;
            }
            for event in &self.events {
                if let Some(input) = translate(&mut open.axes, *event) {
                    inputs.push(input);
                }
            }
        }
        inputs
    }
}

/// What a device node is called in the log and in `hyprctl devices`: its
/// file name, `event0`.
fn node_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// One evdev event as an [`Input`], or nothing for one the seat has no use
/// for.
fn translate(axes: &mut Axes, event: Event) -> Option<Input> {
    match event.r#type {
        EV_KEY => Some(key_or_button(event)),
        EV_REL => relative(event),
        EV_ABS => axes.absolute(event),
        // The end of a report. `wl_pointer.frame` groups the events the
        // compositor sent, and the compositor sends one after each read, so
        // the sync itself carries nothing up.
        EV_SYN if event.code == SYN_REPORT => None,
        _ => None,
    }
}

/// A key, or a pointer button.
///
/// evdev puts both under `EV_KEY` and tells them apart by the code:
/// `BTN_MISC` (0x100) and above are buttons, which is where
/// `input.h`'s own comment draws the line, and what `wl_pointer.button`
/// carries.
fn key_or_button(event: Event) -> Input {
    let pressed = event.value != 0;
    if event.code >= BTN_MISC && event.code <= KEY_MAX && is_pointer_button(event.code) {
        return Input::Button {
            button: u32::from(event.code),
            pressed,
        };
    }
    Input::Key {
        code: event.code,
        pressed,
        // evdev repeats a held key as value 2.
        repeat: event.value == 2,
    }
}

/// Whether a code above `BTN_MISC` is a button a pointer has.
///
/// The mouse buttons and the extra ones beside them, which is the range
/// libinput forwards to `wl_pointer`. `BTN_TOUCH` and the tool buttons a
/// tablet reports are not pointer buttons and would click a window on a pen
/// coming near it.
const fn is_pointer_button(code: u16) -> bool {
    // BTN_MISC..BTN_JOYSTICK: the mouse and the generic buttons.
    code >= BTN_MISC && code < 0x120
}

fn relative(event: Event) -> Option<Input> {
    let value = f64::from(event.value);
    match event.code {
        REL_X => Some(Input::Motion { dx: value, dy: 0.0 }),
        REL_Y => Some(Input::Motion { dx: 0.0, dy: value }),
        REL_WHEEL => Some(Input::Axis {
            axis: compositor_protocol::core::wl_pointer::axis::VERTICAL_SCROLL,
            // A wheel click up is a negative movement of the surface's
            // content, which is the opposite sign from evdev's.
            value: -value * WHEEL_STEP,
        }),
        REL_HWHEEL => Some(Input::Axis {
            axis: compositor_protocol::core::wl_pointer::axis::HORIZONTAL_SCROLL,
            value: value * WHEEL_STEP,
        }),
        _ => None,
    }
}

impl Axes {
    /// One `EV_ABS` event as a fraction of the device's own range.
    fn absolute(&mut self, event: Event) -> Option<Input> {
        let ((min_x, max_x), (min_y, max_y)) = self.range?;
        match event.code {
            ABS_X => self.at.0 = event.value,
            ABS_Y => self.at.1 = event.value,
            // A multi-touch axis, or one the compositor has no use for.
            _ => return None,
        }
        Some(Input::Absolute {
            x: fraction(self.at.0, min_x, max_x),
            y: fraction(self.at.1, min_y, max_y),
        })
    }
}

/// Where `value` lies in `minimum..=maximum`, as 0 to 1.
///
/// A device whose range is empty -- which QEMU's keyboard reports for an axis
/// it does not have -- is the left edge rather than a division by zero.
fn fraction(value: i32, minimum: i32, maximum: i32) -> f64 {
    let span = f64::from(maximum) - f64::from(minimum);
    if span <= 0.0 {
        return 0.0;
    }
    ((f64::from(value) - f64::from(minimum)) / span).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests;
