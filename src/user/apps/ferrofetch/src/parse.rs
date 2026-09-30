//! The text `ferrofetch` reads, parsed.
//!
//! Each parser takes one file's contents as the kernel's `/proc` and `/sys`
//! write them (`src/lib/fs/procfs`, `src/lib/fs/sysfs`), and answers `None`
//! for text it does not recognise rather than a guess: a line `ferrofetch`
//! cannot fill is left out, never filled wrongly.

/// The parent's pid, from `/proc/<pid>/stat`.
///
/// The fourth field, counted after the *last* `)`: the second field is the
/// command's name in parentheses, and a name may itself hold a `)` or a
/// space.
#[must_use]
pub fn stat_parent(stat: &str) -> Option<u32> {
    let (_, after) = stat.rsplit_once(')')?;
    let mut fields = after.split_ascii_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// The real uid, from `/proc/<pid>/status`'s `Uid:` line: the first of its
/// four ids.
#[must_use]
pub fn status_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_ascii_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The name `/etc/passwd` gives `uid`: the first line whose third field is
/// it.
#[must_use]
pub fn passwd_name(passwd: &str, uid: u32) -> Option<&str> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _password = fields.next()?;
        let id: u32 = fields.next()?.parse().ok()?;
        (id == uid && !name.is_empty()).then_some(name)
    })
}

/// Whole seconds since boot, from `/proc/uptime`'s first field.
#[must_use]
pub fn uptime_seconds(uptime: &str) -> Option<u64> {
    let first = uptime.split_ascii_whitespace().next()?;
    let whole = first.split_once('.').map_or(first, |(whole, _)| whole);
    whole.parse().ok()
}

/// What `/proc/meminfo` says of memory, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    /// `MemTotal`.
    pub total_kib: u64,
    /// `MemAvailable`: what could be had without swapping.
    pub available_kib: u64,
}

impl Memory {
    /// What is in use: all of it less what is available.
    #[must_use]
    pub const fn used_kib(&self) -> u64 {
        self.total_kib.saturating_sub(self.available_kib)
    }
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`, or `None` without
/// both.
#[must_use]
pub fn meminfo(text: &str) -> Option<Memory> {
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let value = line.strip_prefix(name)?.strip_prefix(':')?;
            value.split_ascii_whitespace().next()?.parse::<u64>().ok()
        })
    };
    Some(Memory {
        total_kib: field("MemTotal")?,
        available_kib: field("MemAvailable")?,
    })
}

/// What `/proc/loadavg` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loadavg<'a> {
    /// The three averages, as the kernel wrote them.
    pub averages: [&'a str; 3],
    /// The processes that exist, the denominator of the fourth field.
    pub processes: u64,
}

/// `/proc/loadavg`: `0.00 0.01 0.05 1/42 1234`.
#[must_use]
pub fn loadavg(text: &str) -> Option<Loadavg<'_>> {
    let mut fields = text.split_ascii_whitespace();
    let averages = [fields.next()?, fields.next()?, fields.next()?];
    let (_, processes) = fields.next()?.split_once('/')?;
    Some(Loadavg {
        averages,
        processes: processes.parse().ok()?,
    })
}

/// What `/proc/cpuinfo` says of the processors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu {
    /// The online processors: one `processor` line each.
    pub count: u32,
    /// The first processor's `cpu MHz`, in kHz, where the kernel knows it:
    /// only on x86-64, and only when its counter is the TSC.
    pub khz: Option<u64>,
}

/// `/proc/cpuinfo`: how many processors, and how fast the first says it is.
#[must_use]
pub fn cpuinfo(text: &str) -> Cpu {
    let mut cpu = Cpu {
        count: 0,
        khz: None,
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim_end() {
            "processor" => cpu.count = cpu.count.saturating_add(1),
            "cpu MHz" if cpu.khz.is_none() => cpu.khz = megahertz_as_khz(value.trim()),
            _ => {}
        }
    }
    cpu
}

/// `2995.198` as kHz: whole megahertz and up to three places after the point.
fn megahertz_as_khz(text: &str) -> Option<u64> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let whole: u64 = whole.parse().ok()?;
    let mut khz = whole.checked_mul(1000)?;
    let mut scale = 100;
    for digit in fraction.bytes().take(3) {
        if !digit.is_ascii_digit() {
            return None;
        }
        khz = khz.saturating_add(u64::from(digit - b'0') * scale);
        scale /= 10;
    }
    Some(khz)
}

/// A DRM connector's first mode, from its `modes`: `1280x800`, the preferred
/// mode first, one a line.
#[must_use]
pub fn first_mode(modes: &str) -> Option<(u32, u32)> {
    let (width, height) = modes.lines().next()?.trim().split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

/// The connector a `/sys/class/drm` entry is, if it is one: `card0-Virtual-1`
/// is `Virtual-1`, and `card0` itself, the card, is none.
#[must_use]
pub fn connector(entry: &str) -> Option<&str> {
    let (card, connector) = entry.split_once('-')?;
    let number = card.strip_prefix("card")?;
    (!number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && !connector.is_empty())
    .then_some(connector)
}

/// The names in a buffer of `struct linux_dirent64` records, as
/// `getdents64` fills it: a 64-bit inode, a 64-bit offset, a 16-bit record
/// length, a type byte, then the name and its NUL.
#[derive(Debug, Clone)]
pub struct Dirents<'a> {
    bytes: &'a [u8],
}

/// The offset of a record's length.
const RECORD_LENGTH: usize = 16;
/// The offset of a record's name.
const RECORD_NAME: usize = 19;

impl<'a> Dirents<'a> {
    /// The records in `bytes`, which is what one `getdents64` wrote.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl<'a> Iterator for Dirents<'a> {
    type Item = &'a [u8];

    /// The next name, or `None` at the end or at a record that does not fit
    /// in what is left.
    fn next(&mut self) -> Option<&'a [u8]> {
        let length = self.bytes.get(RECORD_LENGTH..RECORD_NAME - 1)?;
        let length = usize::from(u16::from_ne_bytes([*length.first()?, *length.get(1)?]));
        let record = self.bytes.get(..length).filter(|_| length > RECORD_NAME)?;
        self.bytes = self.bytes.get(length..).unwrap_or_default();
        Some(c_bytes(record.get(RECORD_NAME..)?))
    }
}

/// `bytes` up to its first NUL, or all of it without one.
#[must_use]
pub fn c_bytes(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    bytes.get(..end).unwrap_or_default()
}

/// `bytes` up to its first NUL as text, or `None` for an empty field or one
/// that is not UTF-8: a `uname` field, a `comm`.
#[must_use]
pub fn c_str(bytes: &[u8]) -> Option<&str> {
    let text = core::str::from_utf8(c_bytes(bytes)).ok()?.trim_end();
    (!text.is_empty()).then_some(text)
}

/// Bytes of one `struct new_utsname` field: `__NEW_UTS_LEN` and its NUL.
const UTS_FIELD: usize = 65;

/// Bytes of a `struct new_utsname`: six fields, alike on every architecture.
pub const UTSNAME: usize = 6 * UTS_FIELD;

/// A field of `struct new_utsname`, in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uts {
    /// `sysname`: `Ferrix`.
    Sysname,
    /// `nodename`: the host's name.
    Nodename,
    /// `release`.
    Release,
    /// `version`: `#1 Ferrix 0.1.0`.
    Version,
    /// `machine`: `x86_64`.
    Machine,
}

/// `field` of the `struct new_utsname` in `names`, or `None` for one that
/// is empty or not UTF-8.
#[must_use]
pub fn uts_field(names: &[u8], field: Uts) -> Option<&str> {
    let at = field as usize * UTS_FIELD;
    c_str(names.get(at..at + UTS_FIELD)?)
}

/// The system's own version, from `uname`'s `version`: `#1 Ferrix 0.1.0` is
/// `Ferrix 0.1.0`, the build number in front taken off.
#[must_use]
pub fn os_version(version: &str) -> &str {
    version
        .strip_prefix('#')
        .and_then(|rest| rest.split_once(' '))
        .filter(|(build, _)| build.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(version, |(_, rest)| rest)
        .trim()
}

/// The arguments on a command line from `/proc/<pid>/cmdline`, the program's
/// own name left out: each NUL-terminated.
pub fn arguments(cmdline: &[u8]) -> impl Iterator<Item = &[u8]> {
    cmdline
        .strip_suffix(&[0])
        .unwrap_or(cmdline)
        .split(|&byte| byte == 0)
        .skip(1)
}

/// What the command line asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// The logo and the lines.
    Show(Options),
    /// The usage.
    Help,
    /// The version.
    Version,
}

/// How to show them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The mark beside the lines.
    pub logo: bool,
    /// Colour, by the terminal's escapes.
    pub color: bool,
}

/// The [`Command`] `arguments` ask for, or the first argument that is not
/// one `ferrofetch` takes.
///
/// # Errors
///
/// The argument not understood.
pub fn command<'a>(arguments: impl IntoIterator<Item = &'a [u8]>) -> Result<Command, &'a [u8]> {
    let mut options = Options {
        logo: true,
        color: true,
    };
    for argument in arguments {
        match argument {
            b"-h" | b"--help" => return Ok(Command::Help),
            b"-V" | b"--version" => return Ok(Command::Version),
            b"--no-logo" => options.logo = false,
            b"--no-color" => options.color = false,
            other => return Err(other),
        }
    }
    Ok(Command::Show(options))
}
