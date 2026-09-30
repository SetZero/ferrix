//! `ferrofetch`: Ferrix's fastfetch.
//!
//! The mark beside a few lines about the machine, the shell and the session:
//!
//! ```text
//!             o              ferrix@ferrix
//!         _.-' '-._          -------------
//!     _.-'         '-._      OS: Ferrix 0.1.0 x86_64
//!   o'-._           _.-'o    Kernel: Ferrix 6.1.0-ferrix
//!   |    '-._   _.-'    |    Uptime: 3 hours, 25 mins
//!   |        (@)        |    ...
//! ```
//!
//! A native program, started from a shell by `execve` like any other
//! command: it is given no bootstrap channel and needs none. What it knows it
//! reads as fastfetch does on Linux -- `uname`, and the text of `/proc` and
//! `/sys` -- through the Linux calls the kernel offers every process, made
//! with the runtime's functions or, where it has none, by number (`sys`);
//! and it writes to the descriptors the shell left it. Every parser and the
//! layout are the lib target's, where they are tested; this is the reading
//! and the writing.
//!
//! The shell is the parent process, and the terminal the first ancestor past
//! it that is neither a shell nor a login step, as fastfetch finds them: the
//! environment is not on Ferrix's `/proc`, and `$SHELL` names the login
//! shell, not the one running.
//!
//! Exit status 0, 2 for an option it does not take, and 1 when the output
//! could not be written.

#![no_std]
#![no_main]

use core::fmt::{self, Write};

mod sys;

use ferrix_rt::{Bootstrap, linux};
use ferrofetch::Text;
use ferrofetch::parse::{self, Command, Dirents};
use ferrofetch::render::{self, Display, Facts, MAX_DISPLAYS};

ferrix_rt::entry!(main);

/// Standard output.
const STDOUT: usize = 1;
/// Standard error.
const STDERR: usize = 2;

/// What `--help` prints.
const USAGE: &str = "\
Usage: ferrofetch [--no-logo] [--no-color]

Show the Ferrix mark beside a few lines about this machine, the shell and
the session.

  --no-logo       the lines alone
  --no-color      no colour escapes
  -h, --help      this
  -V, --version   the version
";

/// How many steps up the process tree the terminal is looked for.
const ANCESTORS: usize = 8;

/// Processes between a terminal and a shell that are neither: skipped on
/// the way up.
const BETWEEN: [&str; 6] = ["login", "su", "sudo", "doas", "sh", "busybox"];

fn main(_: Bootstrap) -> i32 {
    let mut cmdline = [0_u8; 512];
    let cmdline = read(b"/proc/self/cmdline\0", &mut cmdline).unwrap_or_default();
    match parse::command(parse::arguments(cmdline)) {
        Ok(Command::Show(options)) => {
            let facts = gather();
            let mut out = Out::new(STDOUT);
            match render::render(&mut out, &facts, options).and_then(|()| out.flush()) {
                Ok(()) => 0,
                Err(fmt::Error) => 1,
            }
        }
        Ok(Command::Help) => say(STDOUT, USAGE, 0),
        Ok(Command::Version) => {
            let mut out = Out::new(STDOUT);
            let written = writeln!(out, "ferrofetch {}", env!("CARGO_PKG_VERSION"))
                .and_then(|()| out.flush());
            i32::from(written.is_err())
        }
        Err(argument) => {
            let mut out = Out::new(STDERR);
            let argument = core::str::from_utf8(argument).unwrap_or("?");
            let _told = write!(out, "ferrofetch: no option {argument}\n\n{USAGE}")
                .and_then(|()| out.flush());
            2
        }
    }
}

/// Write `text` to `fd` and end with `status`, or 1 if it could not be
/// written.
fn say(fd: usize, text: &str, status: i32) -> i32 {
    let mut out = Out::new(fd);
    match out.write_str(text).and_then(|()| out.flush()) {
        Ok(()) => status,
        Err(fmt::Error) => 1,
    }
}

/// Everything there is a line for.
fn gather() -> Facts {
    let mut facts = Facts::default();
    names(&mut facts);
    facts.user = user();
    let mut text = [0_u8; 256];
    facts.uptime = read_str(b"/proc/uptime\0", &mut text).and_then(parse::uptime_seconds);
    if let Some(load) = read_str(b"/proc/loadavg\0", &mut text).and_then(parse::loadavg) {
        facts.processes = Some(load.processes);
        let [one, five, fifteen] = load.averages;
        let _cut = write!(facts.load, "{one} {five} {fifteen}");
    }
    ancestry(&mut facts);
    let mut cpuinfo = [0_u8; 16 * 1024];
    facts.cpu = read_str(b"/proc/cpuinfo\0", &mut cpuinfo).map(parse::cpuinfo);
    facts.displays = displays();
    let mut meminfo = [0_u8; 2048];
    facts.memory = read_str(b"/proc/meminfo\0", &mut meminfo).and_then(parse::meminfo);
    facts
}

/// The host, the system and the kernel, from `uname`.
fn names(facts: &mut Facts) {
    let mut names = [0_u8; parse::UTSNAME];
    if !sys::uname(&mut names) {
        return;
    }
    let field = |field| parse::uts_field(&names, field).unwrap_or_default();
    facts.host = Text::from(field(parse::Uts::Nodename));
    facts.machine = Text::from(field(parse::Uts::Machine));
    let system = field(parse::Uts::Sysname);
    if system.is_empty() {
        return;
    }
    // `#1 Ferrix 0.1.0`: the version names the system, so it is the OS line
    // when it does; the system's name alone when it does not.
    let version = parse::os_version(field(parse::Uts::Version));
    facts.os = Text::from(if version.starts_with(system) {
        version
    } else {
        system
    });
    let _cut = write!(facts.kernel, "{system} {}", field(parse::Uts::Release));
}

/// Who is running this: `/etc/passwd`'s name for the real uid, or the
/// number when it has none.
fn user() -> Text<32> {
    let mut status = [0_u8; 2048];
    let Some(uid) = read_str(b"/proc/self/status\0", &mut status).and_then(parse::status_uid)
    else {
        return Text::new();
    };
    let mut passwd = [0_u8; 4096];
    let mut name = Text::new();
    match read_str(b"/etc/passwd\0", &mut passwd).and_then(|text| parse::passwd_name(text, uid)) {
        Some(found) => name.push(found),
        None if uid == 0 => name.push("root"),
        None => {
            let _cut = write!(name, "{uid}");
        }
    }
    name
}

/// The shell, which is the parent, and the terminal above it.
fn ancestry(facts: &mut Facts) {
    let Some(shell) = parent(None) else {
        return;
    };
    facts.shell = comm(shell);
    let mut at = shell;
    for _ in 0..ANCESTORS {
        let Some(up) = parent(Some(at)).filter(|&pid| pid > 1) else {
            // Started by the kernel, or by init: the console.
            facts.terminal = Text::from("console");
            return;
        };
        let name = comm(up);
        let name = name.as_str();
        if name.starts_with("sshd") {
            facts.terminal = Text::from("ssh");
            return;
        }
        if name.contains("getty") {
            facts.terminal = Text::from("console");
            return;
        }
        if !BETWEEN.contains(&name) && name != facts.shell.as_str() {
            facts.terminal = Text::from(name);
            return;
        }
        at = up;
    }
}

/// The parent of `pid`, or of this process for `None`.
fn parent(pid: Option<u32>) -> Option<u32> {
    let mut path = Text::<32>::new();
    match pid {
        Some(pid) => write!(path, "/proc/{pid}/stat\0").ok()?,
        None => path.push("/proc/self/stat\0"),
    }
    let mut stat = [0_u8; 512];
    read_str(path.as_str().as_bytes(), &mut stat).and_then(parse::stat_parent)
}

/// The command name of `pid`, or nothing.
fn comm(pid: u32) -> Text<32> {
    let mut path = Text::<32>::new();
    let mut name = [0_u8; 32];
    if write!(path, "/proc/{pid}/comm\0").is_err() {
        return Text::new();
    }
    read_str(path.as_str().as_bytes(), &mut name)
        .map(str::trim)
        .map(Text::from)
        .unwrap_or_default()
}

/// Every connected display in `/sys/class/drm`, by its first mode.
fn displays() -> [Option<Display>; MAX_DISPLAYS] {
    let mut found = [None; MAX_DISPLAYS];
    let Some(fd) = sys::open(b"/sys/class/drm\0") else {
        return found;
    };
    let mut slots = found.iter_mut();
    let mut records = [0_u8; 2048];
    'directory: while let Some(filled @ 1..) = sys::getdents64(fd, &mut records) {
        for name in Dirents::new(records.get(..filled).unwrap_or_default()) {
            let Some(display) = display(name) else {
                continue;
            };
            let Some(slot) = slots.next() else {
                break 'directory;
            };
            *slot = Some(display);
        }
    }
    let _closed = linux::close(fd);
    found
}

/// The display `/sys/class/drm/<entry>` is, if it is a connected connector
/// with a mode.
fn display(entry: &[u8]) -> Option<Display> {
    let entry = core::str::from_utf8(entry).ok()?;
    let connector = parse::connector(entry)?;
    let mut path = Text::<96>::new();
    write!(path, "/sys/class/drm/{entry}/status\0").ok()?;
    let mut status = [0_u8; 32];
    if read_str(path.as_str().as_bytes(), &mut status)?.trim() != "connected" {
        return None;
    }
    let mut path = Text::<96>::new();
    write!(path, "/sys/class/drm/{entry}/modes\0").ok()?;
    let mut modes = [0_u8; 256];
    let (width, height) = parse::first_mode(read_str(path.as_str().as_bytes(), &mut modes)?)?;
    Some(Display {
        connector: Text::from(connector),
        width,
        height,
    })
}

/// The file at `path`, NUL-terminated, as text, as much of it as fits in
/// `buffer`.
fn read_str<'a>(path: &[u8], buffer: &'a mut [u8]) -> Option<&'a str> {
    core::str::from_utf8(read(path, buffer)?).ok()
}

/// The file at `path`, NUL-terminated, as much of it as fits in `buffer`.
fn read<'a>(path: &[u8], buffer: &'a mut [u8]) -> Option<&'a [u8]> {
    let fd = sys::open(path)?;
    let mut filled = 0;
    while let Some(rest) = buffer.get_mut(filled..).filter(|rest| !rest.is_empty()) {
        match linux::read(fd, rest) {
            Ok(0) | Err(_) => break,
            Ok(count) => filled += count,
        }
    }
    let _closed = linux::close(fd);
    buffer.get(..filled)
}

/// Output to a descriptor, a buffer's worth at a time.
struct Out {
    fd: usize,
    buffer: [u8; 4096],
    len: usize,
}

impl Out {
    const fn new(fd: usize) -> Self {
        Self {
            fd,
            buffer: [0; 4096],
            len: 0,
        }
    }

    /// Write out what is buffered, all of it.
    fn flush(&mut self) -> fmt::Result {
        let mut from = 0;
        while let Some(rest) = self
            .buffer
            .get(from..self.len)
            .filter(|rest| !rest.is_empty())
        {
            match linux::write(self.fd, rest) {
                Ok(0) | Err(_) => return Err(fmt::Error),
                Ok(count) => from += count,
            }
        }
        self.len = 0;
        Ok(())
    }
}

impl Write for Out {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut bytes = text.as_bytes();
        while !bytes.is_empty() {
            if self.len == self.buffer.len() {
                self.flush()?;
            }
            let room = self.buffer.len() - self.len;
            let (now, later) = bytes.split_at(bytes.len().min(room));
            self.buffer
                .get_mut(self.len..self.len + now.len())
                .ok_or(fmt::Error)?
                .copy_from_slice(now);
            self.len += now.len();
            bytes = later;
        }
        Ok(())
    }
}
