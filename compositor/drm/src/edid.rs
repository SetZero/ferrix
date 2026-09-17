//! What a monitor says it is: the `EDID` blob, read into a make, a model and
//! a serial.
//!
//! A person's configuration names monitors by *description* --
//! `monitor = desc:Dell Inc. DELL P2418D MY3ND91J09CT` -- because a
//! connector's name moves when a cable does and a description does not. The
//! description is the three strings here with spaces between them, which is
//! how Hyprland builds its `m_shortDescription`
//! (`CMonitor::onConnect`), so a rule written for Hyprland matches here.
//!
//! # What is read
//!
//! The base block of E-EDID, which is 128 bytes and the only block a
//! monitor is obliged to have:
//!
//! * bytes 0..8, the fixed header `00 FF FF FF FF FF FF 00`, which is what
//!   says the bytes are an EDID at all;
//! * bytes 8..10, the manufacturer: three five-bit letters packed
//!   big-endian, which is the PNP id -- `DEL`, `LEN`, `SAM`;
//! * bytes 10..12, the product code, little-endian, which is the model
//!   where no descriptor names one;
//! * bytes 12..16, the serial number, little-endian, likewise;
//! * the four 18-byte descriptors at 54, 72, 90 and 108. One whose first
//!   three bytes are zero is a *display descriptor*, and its fourth byte
//!   says which: `0xFC` is the monitor's name and `0xFF` its serial, each a
//!   string of up to thirteen characters ended by `0x0A` and padded with
//!   spaces.
//!
//! # The make is the three-letter code, not the company's name
//!
//! Hyprland's make comes from `libdisplay-info`, which looks the PNP id up
//! in `hwdata`'s registry and turns `DEL` into `Dell Inc.`. That registry
//! is a file on a desktop distribution and is not part of EDID; a machine
//! running Ferrix has no copy of it. So the make here is the file's when
//! there is one -- [`Edid::describe`] takes the table it is given -- and the
//! three-letter code when there is not, which is what every tool that has no
//! registry prints.

/// The bytes a monitor's `EDID` property holds, read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edid {
    /// The PNP manufacturer id: three letters, such as `DEL`.
    pub manufacturer: String,
    /// The monitor's name, from the `0xFC` descriptor, or the product code
    /// in hexadecimal where there is none.
    pub model: String,
    /// Its serial, from the `0xFF` descriptor, or the serial number field
    /// in hexadecimal where there is none, or empty where both are zero.
    pub serial: String,
}

/// The fixed eight bytes every EDID begins with.
const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// Where the four 18-byte descriptors start.
const DESCRIPTORS: [usize; 4] = [54, 72, 90, 108];

/// A display descriptor's tag for the monitor's name.
const TAG_NAME: u8 = 0xFC;

/// The same for its serial.
const TAG_SERIAL: u8 = 0xFF;

impl Edid {
    /// Read the base block.
    ///
    /// `None` for bytes that are not an EDID: too short, or without the
    /// header every one begins with. A monitor that answers nonsense is a
    /// monitor with no description, not a compositor that will not start.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let base = bytes.get(..128)?;
        if base.get(..8)? != HEADER {
            return None;
        }
        let manufacturer = manufacturer(base.get(8..10)?)?;
        let product = u16::from_le_bytes([*base.get(10)?, *base.get(11)?]);
        let number = u32::from_le_bytes([
            *base.get(12)?,
            *base.get(13)?,
            *base.get(14)?,
            *base.get(15)?,
        ]);
        let model = descriptor(base, TAG_NAME).unwrap_or_else(|| format!("{product:04X}"));
        let serial = descriptor(base, TAG_SERIAL).unwrap_or_else(|| {
            if number == 0 {
                String::new()
            } else {
                format!("{number:08X}")
            }
        });
        Some(Self {
            manufacturer,
            model,
            serial,
        })
    }

    /// Hyprland's short description: the make, the model and the serial with
    /// single spaces between them, and no comma.
    ///
    /// `named` turns a PNP id into a company's name where the machine has a
    /// registry to look it up in; where it does not, the id itself is the
    /// make. Hyprland strips commas because a `monitor =` line is
    /// comma-separated and a description holding one could never be matched.
    #[must_use]
    pub fn describe(&self, named: impl FnOnce(&str) -> Option<String>) -> String {
        let make = named(&self.manufacturer).unwrap_or_else(|| self.manufacturer.clone());
        let described = [make.as_str(), self.model.as_str(), self.serial.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        described.replace(',', "")
    }
}

/// The three letters bytes 8 and 9 pack.
///
/// Five bits each, most significant first, with 1 standing for `A`. A zero
/// letter is not one, and the whole id is refused rather than a name with a
/// hole in it.
fn manufacturer(bytes: &[u8]) -> Option<String> {
    let packed = u16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]);
    let mut name = String::with_capacity(3);
    for shift in [10, 5, 0] {
        let value = u8::try_from((packed >> shift) & 0x1F).ok()?;
        if value == 0 || value > 26 {
            return None;
        }
        name.push(char::from(b'A' + value - 1));
    }
    Some(name)
}

/// The string of the first display descriptor with this tag.
///
/// A display descriptor is 18 bytes beginning `00 00 00 <tag> 00`, and its
/// text is the thirteen after that: ended by `0x0A` where it is shorter, and
/// padded with spaces.
fn descriptor(base: &[u8], tag: u8) -> Option<String> {
    for &at in &DESCRIPTORS {
        let block = base.get(at..at + 18)?;
        if block.get(..3)? != [0, 0, 0] || block.get(3) != Some(&tag) {
            continue;
        }
        let text = block.get(5..18)?;
        let end = text.iter().position(|&byte| byte == 0x0A).unwrap_or(13);
        let taken = text.get(..end)?;
        let read: String = taken
            .iter()
            .map(|&byte| char::from(byte))
            .filter(|c| !c.is_control())
            .collect();
        let trimmed = read.trim().to_owned();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

/// Look a PNP id up in `hwdata`'s registry, where the machine has one.
///
/// `/usr/share/hwdata/pnp.ids` is `hwdata`'s copy of the registry, and is
/// what `libdisplay-info` -- and so Hyprland -- turns `DEL` into `Dell Inc.`
/// with. Its lines are the three-letter id, a tab, and the company's name.
/// A machine without the file gets `None` and the id is used as it stands.
#[must_use]
pub fn registered(id: &str) -> Option<String> {
    let text = std::fs::read_to_string("/usr/share/hwdata/pnp.ids").ok()?;
    for line in text.lines() {
        let Some((code, name)) = line.split_once('\t') else {
            continue;
        };
        if code.trim() == id {
            return Some(name.trim().to_owned());
        }
    }
    None
}
