//! The lines, and the mark beside them.
//!
//! The layout is fastfetch's: the mark on the left, then three spaces, then
//! `user@host`, a rule under it, one `Label: value` line for each fact the
//! program found, and the terminal's sixteen colours. A fact it did not find
//! has no line. The colours are the brand's (`docs/brand/BRAND.md`): rust for
//! what matters -- the labels, the name, the core -- and grey for the rest.

use core::fmt::{self, Write};

use crate::Text;
use crate::logo::{self, Part};
use crate::parse::{Cpu, Memory, Options};

/// The most displays the lines name.
pub const MAX_DISPLAYS: usize = 4;

/// Rust, `#ff7a2b`: the labels, the name and the core.
const RUST: &str = "\x1b[38;2;255;122;43m";
/// Muted grey, `#9aa4b2`: the lattice.
const MUTED: &str = "\x1b[38;2;154;164;178m";
/// The atoms' lighter grey, `#c9d1db`.
const ATOM: &str = "\x1b[38;2;201;209;219m";
/// Bold.
const BOLD: &str = "\x1b[1m";
/// Back to the terminal's own colours.
const RESET: &str = "\x1b[0m";

/// Between the mark and the lines.
const GAP: &str = "   ";

/// What the program found. Every field may be empty or `None`: the line for
/// it is then left out.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// Who is running it.
    pub user: Text<32>,
    /// `uname`'s node name.
    pub host: Text<65>,
    /// The system and its version: `Ferrix 0.1.0`.
    pub os: Text<80>,
    /// `uname`'s machine: `x86_64`.
    pub machine: Text<16>,
    /// The kernel's name and release.
    pub kernel: Text<140>,
    /// Whole seconds since boot.
    pub uptime: Option<u64>,
    /// How many processes there are.
    pub processes: Option<u64>,
    /// The shell it was started from.
    pub shell: Text<32>,
    /// The terminal the shell is in.
    pub terminal: Text<32>,
    /// The processors.
    pub cpu: Option<Cpu>,
    /// Each connected display.
    pub displays: [Option<Display>; MAX_DISPLAYS],
    /// Memory.
    pub memory: Option<Memory>,
    /// The load averages, as `/proc/loadavg` writes them.
    pub load: Text<32>,
}

/// A connected display.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Display {
    /// Its connector: `Virtual-1`, `HDMI-A-1`.
    pub connector: Text<32>,
    /// Its first mode's width, in pixels.
    pub width: u32,
    /// And height.
    pub height: u32,
}

/// One line of the column beside the mark.
#[derive(Debug, Clone, Copy)]
enum Line<'a> {
    /// `user@host`.
    Title,
    /// The rule under it, as long as it.
    Rule,
    /// `Label: value`.
    Field(&'static str, Option<&'a str>, Value<'a>),
    /// Nothing.
    Blank,
    /// The eight colours, or with `true` their eight bright ones.
    Palette(bool),
}

/// The value of a [`Line::Field`].
#[derive(Debug, Clone, Copy)]
enum Value<'a> {
    /// As it is.
    Text(&'a str),
    /// Two words, a space between.
    Pair(&'a str, &'a str),
    /// A number.
    Count(u64),
    /// Seconds, as days, hours and minutes.
    Uptime(u64),
    /// The processors, and the machine they are.
    Cpu(Cpu, &'a str),
    /// A display's size.
    Size(u32, u32),
    /// Memory used, of all of it.
    Memory(Memory),
}

/// The most lines the column can have: the title and its rule, nine facts
/// and [`MAX_DISPLAYS`] displays, a blank line and two of colours.
const MAX_LINES: usize = 2 + 9 + MAX_DISPLAYS + 1 + 2;

/// The column's lines, in order.
struct Column<'a> {
    lines: [Option<Line<'a>>; MAX_LINES],
    len: usize,
}

impl<'a> Column<'a> {
    fn push(&mut self, line: Line<'a>) {
        if let Some(slot) = self.lines.get_mut(self.len) {
            *slot = Some(line);
            self.len += 1;
        }
    }

    fn field(&mut self, label: &'static str, value: Value<'a>) {
        self.push(Line::Field(label, None, value));
    }

    fn text(&mut self, label: &'static str, text: &'a str) {
        if !text.is_empty() {
            self.field(label, Value::Text(text));
        }
    }

    fn get(&self, row: usize) -> Option<Line<'a>> {
        self.lines.get(row).copied().flatten()
    }
}

/// The lines `facts` make, in fastfetch's order.
fn column(facts: &Facts, color: bool) -> Column<'_> {
    let mut column = Column {
        lines: [None; MAX_LINES],
        len: 0,
    };
    if !facts.host.is_empty() || !facts.user.is_empty() {
        column.push(Line::Title);
        column.push(Line::Rule);
    }
    if !facts.os.is_empty() {
        column.field("OS", Value::Pair(facts.os.as_str(), facts.machine.as_str()));
    }
    column.text("Kernel", facts.kernel.as_str());
    if let Some(seconds) = facts.uptime {
        column.field("Uptime", Value::Uptime(seconds));
    }
    if let Some(count) = facts.processes {
        column.field("Processes", Value::Count(count));
    }
    column.text("Shell", facts.shell.as_str());
    column.text("Terminal", facts.terminal.as_str());
    for display in facts.displays.iter().flatten() {
        let connector = Some(display.connector.as_str()).filter(|name| !name.is_empty());
        let size = Value::Size(display.width, display.height);
        column.push(Line::Field("Display", connector, size));
    }
    if let Some(cpu) = facts.cpu.filter(|cpu| cpu.count > 0) {
        column.field("CPU", Value::Cpu(cpu, facts.machine.as_str()));
    }
    if let Some(memory) = facts.memory {
        column.field("Memory", Value::Memory(memory));
    }
    column.text("Load", facts.load.as_str());
    if color {
        column.push(Line::Blank);
        column.push(Line::Palette(false));
        column.push(Line::Palette(true));
    }
    column
}

/// Write the mark and the lines `facts` make to `out`, as `options` ask.
///
/// # Errors
///
/// Whatever `out` answers.
pub fn render(out: &mut impl Write, facts: &Facts, options: Options) -> fmt::Result {
    let column = column(facts, options.color);
    let rows = if options.logo {
        column.len.max(logo::ROWS.len())
    } else {
        column.len
    };
    for row in 0..rows {
        let line = column.get(row);
        if options.logo {
            let art = logo::ROWS.get(row).copied().unwrap_or_default();
            let width = if line.is_some() { logo::WIDTH } else { 0 };
            mark_row(out, art, width, options.color)?;
            if line.is_some() {
                out.write_str(GAP)?;
            }
        }
        if let Some(line) = line {
            write_line(out, facts, line, options.color)?;
        }
        out.write_str("\n")?;
    }
    Ok(())
}

/// One row of the mark, padded with spaces to `width`.
fn mark_row(out: &mut impl Write, art: &str, width: usize, color: bool) -> fmt::Result {
    let mut current = Part::Space;
    for character in art.bytes() {
        let part = logo::part(character);
        if color && part != Part::Space && part != current {
            out.write_str(match part {
                Part::Lattice => MUTED,
                Part::Atom => ATOM,
                Part::Core | Part::Space => RUST,
            })?;
            current = part;
        }
        out.write_char(char::from(character))?;
    }
    if color && current != Part::Space {
        out.write_str(RESET)?;
    }
    for _ in art.len()..width {
        out.write_char(' ')?;
    }
    Ok(())
}

/// One line of the column.
fn write_line(out: &mut impl Write, facts: &Facts, line: Line<'_>, color: bool) -> fmt::Result {
    let (on, off) = if color { (RUST, RESET) } else { ("", "") };
    let bold = if color { BOLD } else { "" };
    match line {
        Line::Title => {
            let (user, host) = (facts.user.as_str(), facts.host.as_str());
            match (user.is_empty(), host.is_empty()) {
                (false, false) => write!(out, "{bold}{on}{user}{off}@{bold}{on}{host}{off}"),
                (true, _) => write!(out, "{bold}{on}{host}{off}"),
                (false, true) => write!(out, "{bold}{on}{user}{off}"),
            }
        }
        Line::Rule => {
            let (user, host) = (facts.user.as_str(), facts.host.as_str());
            let at = usize::from(!user.is_empty() && !host.is_empty());
            let length = user.chars().count() + at + host.chars().count();
            (0..length).try_for_each(|_| out.write_char('-'))
        }
        Line::Field(label, detail, value) => {
            write!(out, "{bold}{on}{label}")?;
            if let Some(detail) = detail {
                write!(out, " ({detail})")?;
            }
            write!(out, "{off}: {value}")
        }
        Line::Blank => Ok(()),
        Line::Palette(bright) => {
            let base = if bright { 100 } else { 40 };
            (0..8).try_for_each(|colour| write!(out, "\x1b[{}m   ", base + colour))?;
            out.write_str(RESET)
        }
    }
}

impl fmt::Display for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Value::Text(text) => f.write_str(text),
            Value::Pair(first, "") => f.write_str(first),
            Value::Pair(first, second) => write!(f, "{first} {second}"),
            Value::Count(count) => write!(f, "{count}"),
            Value::Uptime(seconds) => uptime(f, seconds),
            Value::Cpu(cpu, machine) => {
                if !machine.is_empty() {
                    write!(f, "{machine} ")?;
                }
                write!(f, "({})", cpu.count)?;
                match cpu.khz {
                    Some(khz) => write!(
                        f,
                        " @ {}.{:02} GHz",
                        khz / 1_000_000,
                        (khz % 1_000_000) / 10_000
                    ),
                    None => Ok(()),
                }
            }
            Value::Size(width, height) => write!(f, "{width}x{height}"),
            Value::Memory(memory) => {
                let used = memory.used_kib();
                let percent = used
                    .saturating_mul(100)
                    .checked_div(memory.total_kib)
                    .unwrap_or_default();
                write!(f, "{} / {} ({percent}%)", Kib(used), Kib(memory.total_kib))
            }
        }
    }
}

/// `2 days, 3 hours, 4 mins`, as fastfetch writes an uptime; under a minute,
/// the seconds.
fn uptime(f: &mut fmt::Formatter<'_>, seconds: u64) -> fmt::Result {
    let parts = [
        (seconds / 86_400, "day"),
        ((seconds / 3600) % 24, "hour"),
        ((seconds / 60) % 60, "min"),
    ];
    if seconds < 60 {
        return plural(f, seconds, "sec");
    }
    let mut first = true;
    for (count, unit) in parts {
        if count == 0 {
            continue;
        }
        if !first {
            f.write_str(", ")?;
        }
        plural(f, count, unit)?;
        first = false;
    }
    Ok(())
}

/// `1 day`, `2 days`.
fn plural(f: &mut fmt::Formatter<'_>, count: u64, unit: &str) -> fmt::Result {
    let s = if count == 1 { "" } else { "s" };
    write!(f, "{count} {unit}{s}")
}

/// A size in KiB, written in MiB, or in GiB to two places from one GiB up.
struct Kib(u64);

impl fmt::Display for Kib {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MIB: u64 = 1024;
        const GIB: u64 = 1024 * 1024;
        let Self(kib) = *self;
        if kib >= GIB {
            let hundredths = kib.saturating_mul(100) / GIB;
            write!(f, "{}.{:02} GiB", hundredths / 100, hundredths % 100)
        } else {
            write!(f, "{} MiB", kib / MIB)
        }
    }
}
