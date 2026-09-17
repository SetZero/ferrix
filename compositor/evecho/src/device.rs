//! One open `/dev/input/eventN`.
//!
//! What the `evdev` crate's `Device::open` asks for, in the order it asks,
//! because that is the shape of the demand a compositor's input backend puts
//! on the kernel (`docs/INPUT.md` §2.4): the driver version, the ids, the
//! event-type bitmap, the properties, the name, then the code bitmap of each
//! type the device reports, and the repeat settings when it reports
//! `EV_REP`. The clock is then set to `CLOCK_MONOTONIC`, as libinput does,
//! so a timestamp can be compared with a frame's.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use ferrix_linux_abi::input::{self, CLOCK_MONOTONIC, EV_ABS, EV_CNT, EV_REP, Event, InputId};
use ferrix_linux_abi::layout::{Field as _, Layout as _};
use ferrix_linux_abi::socket::Width;

/// The directory the event nodes are in.
pub const INPUT_DIR: &str = "/dev/input";

/// How many bytes hold `count` bits, as evdev's `BITS_TO_LONGS` rounds.
const fn bitmap_bytes(count: u16) -> usize {
    (count as usize).div_ceil(8)
}

/// The longest name a device is asked for, as the `evdev` crate asks.
const MAX_NAME: usize = 256;

/// The width an `input_event` has for this program, which is its own.
const WIDTH: Width = if usize::BITS == 32 {
    Width::Bits32
} else {
    Width::Bits64
};

/// How many events one `read` asks for. libinput reads in batches; a report
/// from a tablet is a handful of events, so this is generous.
const BATCH: usize = 64;

/// What a device says it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Description {
    /// The device's name, as `EVIOCGNAME` gave it.
    pub name: String,
    /// The driver version, as `EVIOCGVERSION` gave it.
    pub version: i32,
    /// Bus, vendor, product and version.
    pub id: InputId,
    /// Every `EV_*` the device reports, in order, `EV_SYN` included.
    pub types: Vec<u16>,
    /// The `INPUT_PROP_*` bitmap.
    pub props: Vec<u8>,
    /// The repeat delay and period, for a device that reports `EV_REP`.
    pub repeat: Option<(i32, i32)>,
}

impl Description {
    /// One line naming the device and the types it reports.
    #[must_use]
    pub fn line(&self) -> String {
        let types: Vec<String> = self
            .types
            .iter()
            .map(|&kind| crate::names::event_type_text(kind))
            .collect();
        let InputId {
            bustype,
            vendor,
            product,
            version,
        } = self.id;
        format!(
            "{} [{bustype:#06x}:{vendor:#06x}:{product:#06x}:{version:#06x}] {}",
            self.name,
            types.join(" ")
        )
    }
}

/// An open event device.
#[derive(Debug)]
pub struct Device {
    fd: OwnedFd,
    path: PathBuf,
    description: Description,
}

impl Device {
    /// Open `path` and ask it everything a compositor's backend asks.
    ///
    /// The device is opened read-write and, if that is refused, read-only:
    /// the `evdev` crate's order, since writing is only for LEDs and force
    /// feedback.
    ///
    /// # Errors
    ///
    /// Whatever `open` said, or whichever of the required requests the
    /// device refused. `EVIOCGNAME` failing is not an error -- a device
    /// without a name is nameless, not broken -- and neither is
    /// `EVIOCSCLOCKID`, which only changes which clock the stamps are in.
    pub fn open(path: &Path) -> io::Result<Self> {
        let name = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::other("a path with a NUL in it"))?;
        let fd = open_rw_then_ro(&name)?;
        let device = Self {
            fd,
            path: path.to_owned(),
            description: Description {
                name: String::new(),
                version: 0,
                id: InputId {
                    bustype: 0,
                    vendor: 0,
                    product: 0,
                    version: 0,
                },
                types: Vec::new(),
                props: Vec::new(),
                repeat: None,
            },
        };
        let description = device.interrogate()?;
        let _ = device.set_clock(CLOCK_MONOTONIC);
        Ok(Self {
            description,
            ..device
        })
    }

    /// The path it was opened from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What it said it is.
    #[must_use]
    pub const fn description(&self) -> &Description {
        &self.description
    }

    /// Its descriptor, for a poll.
    #[must_use]
    pub fn raw_fd(&self) -> libc::c_int {
        self.fd.as_raw_fd()
    }

    fn interrogate(&self) -> io::Result<Description> {
        let mut version: i32 = 0;
        self.ioctl_ptr(
            input::EVIOCGVERSION,
            std::ptr::from_mut(&mut version).cast(),
        )?;

        let mut id_bytes = [0u8; InputId::SIZE];
        self.ioctl_ptr(input::EVIOCGID, id_bytes.as_mut_ptr())?;
        let id = InputId::read(&id_bytes).ok_or_else(|| io::Error::other("a short input_id"))?;

        let types_map = self.bitmap(
            input::eviocgbit(0, u32::try_from(bitmap_bytes(EV_CNT)).unwrap_or(u32::MAX)),
            bitmap_bytes(EV_CNT),
        )?;
        let types: Vec<u16> = (0..EV_CNT).filter(|&kind| bit(&types_map, kind)).collect();

        let props = self.bitmap(
            input::eviocgprop(u32::try_from(bitmap_bytes(input::INPUT_PROP_CNT)).unwrap_or(0)),
            bitmap_bytes(input::INPUT_PROP_CNT),
        )?;

        // A device the kernel does not name is nameless; the `evdev` crate
        // tolerates the same refusal.
        let name = self.name().unwrap_or_default();

        // Every type's code bitmap, which the crate reads and a backend
        // needs to know what the device can send. Only the types that have
        // one: `evdev_handle_get_bits` switches on the type and answers
        // `EINVAL` for a type with no bitmap of its own, and `EV_REP` --
        // which QEMU's keyboard and a real PS/2 one both report -- is such a
        // type. Asking anyway costs a working keyboard.
        for &kind in &types {
            let Some(count) = bitmap_of(kind) else {
                continue;
            };
            let len = bitmap_bytes(count);
            let _ = self.bitmap(input::eviocgbit(kind, u32::try_from(len).unwrap_or(0)), len)?;
        }

        let repeat = types.contains(&EV_REP).then(|| self.repeat()).transpose()?;

        Ok(Description {
            name,
            version,
            id,
            types,
            props,
            repeat,
        })
    }

    /// The range of an absolute axis, for a device that reports one.
    ///
    /// # Errors
    ///
    /// Whatever `EVIOCGABS` said.
    pub fn axis(&self, axis: u16) -> io::Result<input::AbsInfo> {
        let mut bytes = [0u8; input::AbsInfo::SIZE];
        self.ioctl_ptr(input::eviocgabs(axis), bytes.as_mut_ptr())?;
        input::AbsInfo::read(&bytes).ok_or_else(|| io::Error::other("a short input_absinfo"))
    }

    /// Whether the device reports `kind`.
    #[must_use]
    pub fn reports(&self, kind: u16) -> bool {
        self.description.types.contains(&kind)
    }

    /// Take the device for this program alone, as a compositor does so that
    /// a key it handles does not also reach whatever else is reading.
    ///
    /// # Errors
    ///
    /// Whatever `EVIOCGRAB` said; `EBUSY` when another open holds it.
    pub fn grab(&self, held: bool) -> io::Result<()> {
        let on = i32::from(held);
        self.ioctl_value(input::EVIOCGRAB, on)
    }

    fn set_clock(&self, clock: i32) -> io::Result<()> {
        self.ioctl_ptr(
            input::EVIOCSCLOCKID,
            std::ptr::from_ref(&clock).cast_mut().cast(),
        )
    }

    fn repeat(&self) -> io::Result<(i32, i32)> {
        let mut settings = [0i32; 2];
        self.ioctl_ptr(input::EVIOCGREP, settings.as_mut_ptr().cast())?;
        let [delay, period] = settings;
        Ok((delay, period))
    }

    fn name(&self) -> io::Result<String> {
        let mut bytes = [0u8; MAX_NAME];
        let len = self.ioctl_len(
            input::eviocgname(u32::try_from(MAX_NAME).unwrap_or(0)),
            bytes.as_mut_ptr(),
        )?;
        // The answer's length counts the NUL, which the name does not.
        let text = bytes.get(..len.saturating_sub(1)).unwrap_or_default();
        Ok(String::from_utf8_lossy(text).into_owned())
    }

    /// Read whatever events are waiting, appending them to `out`.
    ///
    /// A read returns whole events; a partial one is the kernel breaking its
    /// own contract and is refused rather than guessed at.
    ///
    /// # Errors
    ///
    /// Whatever `read` said, `EAGAIN` included when the device is
    /// non-blocking and has nothing.
    pub fn read_events(&self, out: &mut Vec<Event>) -> io::Result<usize> {
        let size = Event::size(WIDTH);
        let mut bytes = vec![0u8; size * BATCH];
        // SAFETY: `bytes` is a live buffer of the length passed.
        let read =
            unsafe { libc::read(self.fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
        if read < 0 {
            return Err(io::Error::last_os_error());
        }
        let read = usize::try_from(read).unwrap_or(0);
        if read % size != 0 {
            return Err(io::Error::other("a read that was not whole events"));
        }
        let mut at = 0;
        while at < read {
            let event = bytes
                .get(at..)
                .and_then(|rest| Event::read(WIDTH, rest))
                .ok_or_else(|| io::Error::other("a short event"))?;
            out.push(event);
            at += size;
        }
        Ok(read / size)
    }

    fn bitmap(&self, request: u32, len: usize) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0u8; len];
        let got = self.ioctl_len(request, bytes.as_mut_ptr())?;
        bytes.truncate(got.min(len));
        bytes.resize(len, 0);
        Ok(bytes)
    }

    /// Run `request` with `pointer` as its argument, refusing a failure.
    fn ioctl_ptr(&self, request: u32, pointer: *mut u8) -> io::Result<()> {
        self.ioctl_len(request, pointer).map(drop)
    }

    /// Run `request` with `pointer` as its argument, giving what it answered:
    /// for the length-encoded requests, the bytes it wrote.
    fn ioctl_len(&self, request: u32, pointer: *mut u8) -> io::Result<usize> {
        // SAFETY: `pointer` is a live buffer of at least the size the
        // request's number encodes, which the kernel reads and writes within.
        let result = unsafe { libc::ioctl(self.fd.as_raw_fd(), request as _, pointer) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(usize::try_from(result).unwrap_or(0))
    }

    /// Run `request` with `value` itself as its argument, not a pointer.
    fn ioctl_value(&self, request: u32, value: i32) -> io::Result<()> {
        // SAFETY: the request takes its argument by value, not by reference.
        let result = unsafe { libc::ioctl(self.fd.as_raw_fd(), request as _, value) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// How many codes a type's bitmap holds, or `None` for a type that has no
/// bitmap.
///
/// The list is `evdev_handle_get_bits`' `switch`, which is what decides
/// whether `EVIOCGBIT(type)` answers or refuses: the types with a
/// `dev->*bit` array, and no others. `EV_SYN` is not one of them -- type 0
/// is the map of the types themselves -- and neither are `EV_REP`, whose
/// settings are `EVIOCGREP`, or `EV_PWR` and `EV_FF_STATUS`, which no driver
/// declares codes for.
const fn bitmap_of(kind: u16) -> Option<u16> {
    Some(match kind {
        input::EV_KEY => input::KEY_CNT,
        input::EV_REL => input::REL_CNT,
        EV_ABS => input::ABS_CNT,
        input::EV_MSC => input::MSC_CNT,
        input::EV_SW => input::SW_CNT,
        input::EV_LED => input::LED_CNT,
        input::EV_SND => input::SND_CNT,
        input::EV_FF => input::FF_CNT,
        _ => return None,
    })
}

/// Whether bit `index` is set.
fn bit(bits: &[u8], index: u16) -> bool {
    let at = usize::from(index / 8);
    bits.get(at)
        .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
}

fn open_rw_then_ro(path: &CString) -> io::Result<OwnedFd> {
    for flags in [libc::O_RDWR, libc::O_RDONLY] {
        // SAFETY: `path` is NUL-terminated and the flags are constants.
        let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC) };
        if fd >= 0 {
            // SAFETY: `fd` is a descriptor `open` just gave and nothing else
            // owns.
            return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
        }
    }
    Err(io::Error::last_os_error())
}

/// Every `/dev/input/eventN` there is, in the order of their numbers.
///
/// # Errors
///
/// Whatever reading the directory said. A machine with no input devices has
/// an empty directory, not an error; a machine with no `/dev/input` at all
/// is one whose kernel published none, and that is the error.
pub fn event_nodes() -> io::Result<Vec<PathBuf>> {
    let mut found: Vec<(u32, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(INPUT_DIR)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(text) = name.to_str() else { continue };
        let Some(number) = text.strip_prefix("event") else {
            continue;
        };
        let Ok(number) = number.parse::<u32>() else {
            continue;
        };
        found.push((number, entry.path()));
    }
    found.sort_by_key(|&(number, _)| number);
    Ok(found.into_iter().map(|(_, path)| path).collect())
}

/// The bitmap helpers, which the tests reach.
#[cfg(test)]
mod tests {
    use super::{bit, bitmap_bytes, bitmap_of};
    use ferrix_linux_abi::input::{
        ABS_CNT, EV_ABS, EV_CNT, EV_KEY, EV_PWR, EV_REP, EV_SYN, KEY_A, KEY_CNT,
    };

    #[test]
    fn a_bitmap_is_as_long_as_evdev_makes_it() {
        assert_eq!(bitmap_bytes(EV_CNT), 4);
        assert_eq!(bitmap_bytes(KEY_CNT), 96);
        assert_eq!(bitmap_of(EV_KEY), Some(KEY_CNT));
        assert_eq!(bitmap_of(EV_ABS), Some(ABS_CNT));
    }

    /// `evdev_handle_get_bits` refuses a type with no bitmap, and a device
    /// that reports `EV_REP` -- QEMU's keyboard does, and so does a PS/2 one
    /// -- would be lost if its bitmap were asked for anyway.
    #[test]
    fn a_type_with_no_bitmap_of_its_own_is_not_asked_for_one() {
        assert_eq!(bitmap_of(EV_REP), None);
        assert_eq!(bitmap_of(EV_SYN), None);
        assert_eq!(bitmap_of(EV_PWR), None);
    }

    #[test]
    fn a_bit_is_read_from_its_byte_and_a_short_map_has_none() {
        let mut bits = vec![0u8; 96];
        if let Some(byte) = bits.get_mut(usize::from(KEY_A / 8)) {
            *byte = 1 << (KEY_A % 8);
        }
        assert!(bit(&bits, KEY_A));
        assert!(!bit(&bits, KEY_A + 1));
        assert!(!bit(&[], KEY_A));
    }
}
