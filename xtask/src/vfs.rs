//! Stage 8's exit criterion, as programs with expected output.
//!
//! "`busybox ls -R /proc`, `cat /proc/self/maps` and a shell script that
//! manipulates files under tmpfs, all under the boot test."
//!
//! # Three programs, as the criterion names them
//!
//! The kernel starts each one itself (`kernel/src/init.rs`), in order, over
//! one tmpfs. The third is a shell script, and this busybox's shell starts
//! every applet that touches a file -- `mkdir`, `mv`, `ln`, `cat`, `rm`,
//! `rmdir` -- with `fork`, `execve` and `wait4`, none of its builtins making,
//! renaming or removing one (measured, in `docs/STAGE8-WHAT-THE-EXIT-NEEDS.md`).
//! So the script passes only on a kernel that forks. Until one did, this test
//! ran eleven programs in its place, the kernel starting each applet the
//! script would have forked.
//!
//! # Why output as well as statuses
//!
//! Because the statuses lie. Refused `getdents64`, `ls -R` prints its headings
//! and no names and exits 0; refused `poll`, `while read` reads nothing and
//! the script goes on to its chosen status. So each command's output is
//! checked, and the scripts exit with statuses that are not zero, so that a
//! shell that died and reported success cannot pass.
//!
//! # Applets, reported apart
//!
//! After the criterion's three, the same boot runs [`APPLETS`]: busybox
//! applets that a sweep of the static Alpine build found failing, each for a
//! filesystem piece that has since been added -- `/proc/<pid>/cwd` and
//! `root`, `/proc/sys`, `/proc/partitions`, `/proc/stat`, and `mknod` of a
//! character device. They guard those pieces against coming undone. They are
//! not the criterion, so they are judged and reported as a group of their own:
//! the criterion's line says what it always said, and a failing applet fails
//! the test on a line of its own.
//!
//! # The log
//!
//! `kernel/src/init.rs` writes `  init     command N: ARGV` before command `N`
//! and `  init     command N exited with S` after it, and reports each call
//! answered `ENOSYS` in between as `  syscall  ...`. Everything else between the
//! two is the program's. Kernel lines start with two spaces and no program
//! line here does, which is how the two are told apart; change the formats
//! together.

use std::collections::BTreeMap;
use std::path::Path;

use crate::{Error, Result};

/// The start of the kernel's line before and after each command.
pub(crate) const COMMAND: &str = "init     command ";
/// What follows the number on the line after a command that ran.
const EXITED: &str = " exited with ";
/// What follows the number on the line after a command that did not start.
const NOT_STARTED: &str = " could not be started: ";
/// The kernel's line when the program itself could not be read.
const UNREADABLE: &str = "could not be read: errno";
/// The start of the kernel's line for a call answered `ENOSYS`.
const UNANSWERED: &str = "syscall  ";
/// The kernel's line when every command has run: what the boot waits for.
pub(crate) const DONE: &str = "init     every command has run";

/// A check's verdict: nothing, or why it failed.
type Check = std::result::Result<(), String>;

/// What a command's output must show, beyond its status.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Expect {
    /// These lines, in this order, among others.
    Lines(&'static [&'static str]),
    /// An `ls -R /proc` listing that reaches the program's own directory.
    ProcListing,
    /// Lines that each parse as a line of `/proc/<pid>/maps`, in order.
    Maps,
    /// Lines of these shapes, in this order, among others. In a shape, `#`
    /// stands for a decimal number and `*` for any text, none included; every
    /// other character stands for itself. For output that differs from boot
    /// to boot or from one architecture to another: a pid, a CPU count, a
    /// column's width.
    Shaped(&'static [&'static str]),
    /// No output, or blank lines only.
    Nothing,
}

/// One program for the kernel to run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Command {
    /// Its arguments, `argv[0]` naming the applet.
    pub(crate) argv: &'static [&'static str],
    /// The status it must exit with.
    pub(crate) status: i32,
    /// What its output must show.
    pub(crate) expect: Expect,
}

/// Manipulates files under tmpfs: the script the criterion names.
///
/// Builtins create, append to, read back, test and truncate files; `mkdir`,
/// `mv`, `ln`, `cat`, `rm` and `rmdir` are applets the shell forks and execs,
/// so this one script also exercises `fork`, `execve` and `wait4` against the
/// VFS. Each applet that fails exits with a status of its own, so the report
/// says which one it was.
///
/// `/tmp` is tested before the last `-e`, because a `stat` the kernel refuses
/// makes `-e` false too: without that line, a kernel answering no file calls
/// at all would pass the last check.
const TMPFS_SCRIPT: &str = r#"mkdir -p /tmp/vfs/deep || exit 1
cd /tmp/vfs || exit 1
echo "tmpfs: in $PWD"
echo "tmpfs: one" > file
echo "tmpfs: two" >> file
while read -r line; do echo "read back: $line"; done < file
[ -f file ] && echo "tmpfs: file is a regular file"
[ -d deep ] && echo "tmpfs: deep is a directory"
: > empty
[ -s empty ] || echo "tmpfs: empty is empty"
mv file deep/moved || exit 2
ln -s deep/moved link || exit 3
[ -e file ] || echo "tmpfs: the old name is gone"
[ -f deep/moved ] && echo "tmpfs: the new name is a file"
[ -L link ] && echo "tmpfs: link is a symbolic link"
cat link || exit 4
while read -r line; do echo "through the link: $line"; done < link
cd / || exit 1
rm /tmp/vfs/link /tmp/vfs/deep/moved /tmp/vfs/empty || exit 5
rmdir /tmp/vfs/deep /tmp/vfs || exit 6
[ -d /tmp ] || exit 7
[ -e /tmp/vfs ] || echo "tmpfs: removed"
exit 8
"#;

/// The programs, in the order the kernel runs them: the criterion's three.
pub(crate) const COMMANDS: &[Command] = &[
    Command {
        argv: &["ls", "-R", "/proc"],
        status: 0,
        expect: Expect::ProcListing,
    },
    Command {
        argv: &["cat", "/proc/self/maps"],
        status: 0,
        expect: Expect::Maps,
    },
    Command {
        argv: &["sh", "-c", TMPFS_SCRIPT],
        status: 8,
        expect: Expect::Lines(&[
            "tmpfs: in /tmp/vfs",
            "read back: tmpfs: one",
            "read back: tmpfs: two",
            "tmpfs: file is a regular file",
            "tmpfs: deep is a directory",
            "tmpfs: empty is empty",
            "tmpfs: the old name is gone",
            "tmpfs: the new name is a file",
            "tmpfs: link is a symbolic link",
            "tmpfs: one",
            "tmpfs: two",
            "through the link: tmpfs: one",
            "through the link: tmpfs: two",
            "tmpfs: removed",
        ]),
    },
];

/// Changes the host name through `/proc/sys`, reads it back through `uname`,
/// and puts the old name back whatever happened, so that no later applet sees
/// this one's name.
const HOSTNAME_SCRIPT: &str = r#"old=$(uname -n) || exit 1
sysctl -w kernel.hostname=applets && uname -n
sysctl -w "kernel.hostname=$old" && [ "$(uname -n)" = "$old" ] && echo "hostname: restored"
exit 4
"#;

/// The applets, run in the same boot after [`COMMANDS`] and reported apart
/// from them. Each guards a filesystem piece a busybox applet was found to
/// need, and checks what the applet printed, not only that it exited.
///
/// A program the shell is not needed for is started by the kernel directly,
/// so that its status is the applet's own. The rest are `sh -c` scripts that
/// end in a status of their own, for the reason [`COMMANDS`]' script does.
pub(crate) const APPLETS: &[Command] = &[
    // `/proc/<pid>/cwd` and `root`: the directory the shell moved to, and
    // the root it has.
    Command {
        argv: &[
            "sh",
            "-c",
            "cd /tmp && pwdx $$ && readlink /proc/$$/root; exit 3",
        ],
        status: 3,
        expect: Expect::Shaped(&["#: /tmp", "/"]),
    },
    // `/proc/sys`: a write the file refuses fails, and leaves the value
    // alone, which the read after it shows.
    Command {
        argv: &["sysctl", "-w", "kernel.ostype=x"],
        status: 1,
        expect: Expect::Shaped(&["sysctl: *kernel.ostype*"]),
    },
    Command {
        argv: &["sysctl", "kernel.ostype"],
        status: 0,
        expect: Expect::Lines(&["kernel.ostype = Ferrix"]),
    },
    Command {
        argv: &["sysctl", "kernel.pid_max"],
        status: 0,
        expect: Expect::Shaped(&["kernel.pid_max = #"]),
    },
    Command {
        argv: &["sh", "-c", HOSTNAME_SCRIPT],
        status: 4,
        expect: Expect::Shaped(&[
            "kernel.hostname = applets",
            "applets",
            "kernel.hostname = *",
            "hostname: restored",
        ]),
    },
    // The btrfs fixture the boot check mounted at `/mnt`, read by a program:
    // `big.txt` is 140000 bytes in the manifest.
    Command {
        argv: &["sh", "-c", "wc -c < /mnt/big.txt; exit 9"],
        status: 9,
        expect: Expect::Lines(&["140000"]),
    },
    // `/proc/partitions`: the disks the boot check's drivers serve, in
    // Linux's format, 64 MiB of 1 KiB blocks at the virtio-blk major; and
    // `fdisk -l`, which reads it, finding nothing it can open rather than
    // dying of a signal.
    Command {
        argv: &["cat", "/proc/partitions"],
        status: 0,
        expect: Expect::Lines(&[
            "major minor  #blocks  name",
            " 254        0      65536 vda",
            " 254       16     131072 vdb",
        ]),
    },
    Command {
        argv: &["fdisk", "-l"],
        status: 0,
        expect: Expect::Nothing,
    },
    // `/proc/stat`: each reader's summary of the CPUs.
    Command {
        argv: &["top", "-b", "-n1"],
        status: 0,
        expect: Expect::Shaped(&["Mem: *", "CPU: *% usr *% idle*", "Load average:*"]),
    },
    Command {
        argv: &["mpstat"],
        status: 0,
        expect: Expect::Shaped(&["*CPU *%usr*%idle", "* all *"]),
    },
    Command {
        argv: &["iostat", "-c"],
        status: 0,
        expect: Expect::Shaped(&["avg-cpu: *%user*%idle"]),
    },
    // `mknod` of character devices, each opening as its devfs device: null
    // swallows a write and reads empty, zero reads zeros, and a number no
    // driver has fails to open.
    Command {
        argv: &[
            "sh",
            "-c",
            "mknod /tmp/n c 1 3 && echo hi > /tmp/n && head -c 4 /tmp/n | wc -c; exit 5",
        ],
        status: 5,
        expect: Expect::Lines(&["0"]),
    },
    Command {
        argv: &[
            "sh",
            "-c",
            "mknod /tmp/z c 1 5 && head -c 8 /tmp/z | od -An -tx1; exit 6",
        ],
        status: 6,
        expect: Expect::Lines(&[" 00 00 00 00 00 00 00 00"]),
    },
    Command {
        argv: &["sh", "-c", "mknod /tmp/x c 240 0 && cat /tmp/x"],
        status: 1,
        expect: Expect::Shaped(&["cat: *: No such device or address"]),
    },
    // Permissions: a user who is not root is refused what the modes
    // withhold, owns what it makes, and cannot delete another's file from
    // the sticky `/tmp`; root is refused none of it.
    Command {
        argv: &["sh", "-c", PERMISSIONS_SCRIPT],
        status: 9,
        expect: Expect::Shaped(&[
            "uid=1000(ferrix) gid=1000(ferrix)*",
            "cat: can't open '/tmp/dac-private': Permission denied",
            "owned by 1000 1000",
            "rm: can't remove '/tmp/dac-private': Operation not permitted",
            "chmod: /tmp/dac-private: Operation not permitted",
            "ls: can't open '/tmp/dac-closed': Permission denied",
            "*/tmp/dac-noexec: Permission denied",
            "root still reads: secret",
        ]),
    },
    // `mount -t proc` and `mount -t devtmpfs` go here once mount takes them.
];

/// Root makes a private file, a closed directory and a script nobody may
/// execute; `su` becomes the image's user `ferrix`, uid 1000, which is
/// refused each of them with the error Linux gives -- `EACCES` for a mode,
/// `EPERM` for the sticky bit and for `chmod` of another's file -- and owns
/// the file it makes. Root reads the private file afterwards, so a kernel
/// that refused everything to everyone cannot pass.
const PERMISSIONS_SCRIPT: &str = r#"echo secret > /tmp/dac-private && chmod 600 /tmp/dac-private || exit 1
mkdir -m 700 /tmp/dac-closed || exit 2
echo 'echo ran' > /tmp/dac-noexec && chmod 644 /tmp/dac-noexec || exit 3
su ferrix -c 'id
cat /tmp/dac-private
echo mine > /tmp/dac-mine && stat -c "owned by %u %g" /tmp/dac-mine
rm -f /tmp/dac-private
chmod 777 /tmp/dac-private
ls /tmp/dac-closed
/tmp/dac-noexec'
echo "root still reads: $(cat /tmp/dac-private)"
exit 9
"#;

/// The list as `kernel/build.rs` takes it: each argument ends in a NUL, and
/// each command in an empty argument.
///
/// # Errors
///
/// A command with no arguments, or an argument that is empty or holds a NUL,
/// none of which the encoding can carry.
pub(crate) fn encode(commands: &[Command]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for command in commands {
        if command.argv.is_empty() {
            return Err(Error::new("a command with no arguments names no program"));
        }
        for arg in command.argv {
            if arg.is_empty() || arg.contains('\0') {
                return Err(Error::new(format!(
                    "the argument {arg:?} is empty or holds a NUL, which the list cannot carry"
                )));
            }
            bytes.extend_from_slice(arg.as_bytes());
            bytes.push(0);
        }
        bytes.push(0);
    }
    Ok(bytes)
}

/// Write `bytes` to `path` unless it already holds them.
///
/// The kernel's build script reruns when the file's timestamp changes, so
/// rewriting the same list would rebuild the kernel on every run.
pub(crate) fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::read(path).is_ok_and(|held| held == bytes) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
        .map_err(|error| Error::new(format!("writing {}: {error}", path.display())))
}

/// How a command ended, as the log tells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ending {
    /// It ran and exited with this status.
    Exited(i32),
    /// It never started, for this reason.
    NotStarted(String),
}

/// What one command did, as the log tells it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Ran {
    /// The program's lines.
    pub(crate) output: Vec<String>,
    /// The kernel's reports of calls answered `ENOSYS` while it ran.
    pub(crate) unanswered: Vec<String>,
    /// How it ended, if the log got that far.
    pub(crate) ending: Option<Ending>,
}

/// A kernel line about command `N`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Marker {
    /// It is starting.
    Started,
    /// It has ended.
    Ended(Ending),
}

/// Read a kernel line about a command, if `line` is one.
fn marker(line: &str) -> Option<(usize, Marker)> {
    let (_, rest) = line.split_once(COMMAND)?;
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (number, tail) = rest.split_at_checked(digits)?;
    let index = number.parse().ok()?;
    if tail.starts_with(": ") {
        return Some((index, Marker::Started));
    }
    if let Some(status) = tail.strip_prefix(EXITED) {
        let status = status.trim().parse().ok()?;
        return Some((index, Marker::Ended(Ending::Exited(status))));
    }
    let why = tail.strip_prefix(NOT_STARTED)?.trim().to_owned();
    Some((index, Marker::Ended(Ending::NotStarted(why))))
}

/// Split a log into what each command did, by number.
pub(crate) fn split(lines: &[String]) -> BTreeMap<usize, Ran> {
    let mut ran: BTreeMap<usize, Ran> = BTreeMap::new();
    let mut current = None;
    for line in lines {
        let line = line.trim_end();
        if let Some((index, marker)) = marker(line) {
            let entry = ran.entry(index).or_default();
            match marker {
                Marker::Started => current = Some(index),
                Marker::Ended(ending) => {
                    entry.ending = Some(ending);
                    current = None;
                }
            }
            continue;
        }
        let Some(entry) = current.and_then(|index| ran.get_mut(&index)) else {
            continue;
        };
        if line.trim_start().starts_with(UNANSWERED) && line.starts_with("  ") {
            entry.unanswered.push(line.trim().to_owned());
        } else if !line.starts_with("  ") {
            entry.output.push(line.to_owned());
        }
    }
    ran
}

/// A line with terminal escape sequences removed: `ESC [`, parameters, and
/// the letter that ends them. busybox colours `ls` when it thinks it is
/// talking to a terminal, and whether it does depends on what the console
/// answers to `ioctl`, which is not what this test is about.
fn strip_escapes(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            let _ = chars.by_ref().find(char::is_ascii_alphabetic);
        } else {
            plain.push(c);
        }
    }
    plain
}

/// An `ls -R` listing: each directory's heading, and the names under it.
///
/// Names are split on whitespace, so a listing in columns reads the same as
/// one name per line.
pub(crate) fn listing(output: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut sections: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in output {
        let line = strip_escapes(line);
        let line = line.trim_end();
        if let Some(heading) = line.strip_suffix(':').filter(|h| h.starts_with('/')) {
            let _ = sections.entry(heading.to_owned()).or_default();
            current = Some(heading.to_owned());
        } else if let Some(directory) = &current {
            sections
                .entry(directory.clone())
                .or_default()
                .extend(line.split_whitespace().map(str::to_owned));
        }
    }
    sections
}

/// Whether `directory` is a process's own directory in `/proc`.
fn is_own_directory(directory: &str) -> bool {
    directory == "/proc/self"
        || directory
            .strip_prefix("/proc/")
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// `ls -R /proc` reached `self` and the program's own `maps`.
///
/// `ls -R` does not follow `/proc/self`, which is a symbolic link, so the
/// program's files appear under `/proc/<pid>:` -- or under `/proc/self:`, if
/// procfs makes `self` a directory instead.
fn proc_listing(output: &[String]) -> Check {
    let sections = listing(output);
    let top = sections
        .get("/proc")
        .ok_or("the listing has no `/proc:` heading")?;
    if !top.iter().any(|name| name == "self") {
        return Err(format!("`/proc:` does not list `self`; it lists {top:?}"));
    }
    let reached = sections
        .iter()
        .any(|(dir, names)| is_own_directory(dir) && names.iter().any(|name| name == "maps"));
    if !reached {
        let headings: Vec<&String> = sections.keys().collect();
        return Err(format!(
            "no `/proc/self:` or `/proc/<pid>:` section lists `maps`; the headings were \
             {headings:?}"
        ));
    }
    Ok(())
}

/// One line of `/proc/<pid>/maps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MapsLine {
    /// The first address.
    pub(crate) start: u64,
    /// The first address past the end.
    pub(crate) end: u64,
    /// `r`, `w`, `x` or `-` each, then `p` or `s`.
    pub(crate) perms: String,
    /// The offset into the file.
    pub(crate) offset: u64,
    /// The file's device, major and minor.
    pub(crate) dev: (u32, u32),
    /// The file's inode, zero for anonymous memory.
    pub(crate) inode: u64,
    /// The file's path or a name like `[stack]`, if there is one.
    pub(crate) path: Option<String>,
}

/// The next whitespace-separated field of `rest`, and what follows it.
fn field(rest: &str) -> Option<(&str, &str)> {
    let rest = rest.trim_start();
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    rest.split_at_checked(end)
        .filter(|(field, _)| !field.is_empty())
}

/// A hexadecimal number, refusing an empty or signed one.
fn hex(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(text, 16).ok()
}

/// Parse a line of `/proc/<pid>/maps`:
/// `start-end perms offset major:minor inode [path]`.
///
/// # Errors
///
/// Which field is missing or malformed.
pub(crate) fn maps_line(line: &str) -> std::result::Result<MapsLine, String> {
    let (range, rest) = field(line).ok_or("the line is empty")?;
    let (start, end) = range
        .split_once('-')
        .and_then(|(start, end)| Some((hex(start)?, hex(end)?)))
        .ok_or_else(|| format!("`{range}` is not a hexadecimal `start-end` range"))?;
    if start >= end {
        return Err(format!("`{range}` does not end after it starts"));
    }
    let (perms, rest) = field(rest).ok_or("there are no permissions")?;
    let bytes = perms.as_bytes();
    let valid = matches!(bytes, [b'r' | b'-', b'w' | b'-', b'x' | b'-', b'p' | b's']);
    if !valid {
        return Err(format!("`{perms}` is not `rwxp`-shaped permissions"));
    }
    let (offset, rest) = field(rest).ok_or("there is no offset")?;
    let offset = hex(offset).ok_or_else(|| format!("`{offset}` is not a hexadecimal offset"))?;
    let (dev, rest) = field(rest).ok_or("there is no device")?;
    let parsed_dev = dev.split_once(':').and_then(|(major, minor)| {
        Some((
            u32::try_from(hex(major)?).ok()?,
            u32::try_from(hex(minor)?).ok()?,
        ))
    });
    let dev = parsed_dev.ok_or_else(|| format!("`{dev}` is not a `major:minor` device"))?;
    let (inode, rest) = field(rest).ok_or("there is no inode")?;
    let inode = inode
        .parse()
        .map_err(|_| format!("`{inode}` is not a decimal inode"))?;
    let path = Some(rest.trim()).filter(|path| !path.is_empty());
    Ok(MapsLine {
        start,
        end,
        perms: perms.to_owned(),
        offset,
        dev,
        inode,
        path: path.map(str::to_owned),
    })
}

/// Every line parses as a maps line, and the ranges ascend without overlap.
fn maps(output: &[String]) -> Check {
    let mut previous_end = 0;
    let mut count = 0_usize;
    for line in output.iter().filter(|line| !line.trim().is_empty()) {
        let parsed =
            maps_line(line).map_err(|why| format!("`{line}` is not a maps line: {why}"))?;
        if parsed.start < previous_end {
            return Err(format!("`{line}` overlaps or precedes the line before it"));
        }
        previous_end = parsed.end;
        count += 1;
    }
    if count == 0 {
        return Err("it printed no maps lines".to_owned());
    }
    Ok(())
}

/// Whether `line` has the shape `shape`, as [`Expect::Shaped`] describes:
/// `#` a run of digits, `*` any text, anything else itself.
pub(crate) fn shaped(shape: &str, line: &str) -> bool {
    fn from(shape: &[u8], line: &[u8]) -> bool {
        match shape.split_first() {
            None => line.is_empty(),
            Some((b'*', rest)) => {
                (0..=line.len()).any(|at| line.get(at..).is_some_and(|tail| from(rest, tail)))
            }
            Some((b'#', rest)) => {
                let digits = line.iter().take_while(|b| b.is_ascii_digit()).count();
                (1..=digits).any(|at| line.get(at..).is_some_and(|tail| from(rest, tail)))
            }
            Some((byte, rest)) => line
                .split_first()
                .is_some_and(|(first, tail)| first == byte && from(rest, tail)),
        }
    }
    from(shape.as_bytes(), line.as_bytes())
}

/// Nothing but blank lines.
fn nothing(output: &[String]) -> Check {
    match output.iter().find(|line| !line.trim().is_empty()) {
        Some(line) => Err(format!("it printed `{line}`, where nothing was expected")),
        None => Ok(()),
    }
}

/// `want` appears among `output`, in order, each line as `matches` says.
fn in_order(output: &[String], want: &[&str], matches: fn(&str, &str) -> bool) -> Check {
    let mut remaining = output.iter();
    for line in want {
        if !remaining.any(|got| matches(line, got.trim_end())) {
            return Err(format!(
                "its output is missing `{line}`, or it came out of order"
            ));
        }
    }
    Ok(())
}

/// The most lines of a failing command's output an error repeats.
const SHOWN_LINES: usize = 8;

/// Judge one command by what the log says it did.
fn verdict(command: &Command, ran: Option<&Ran>) -> Check {
    let ran = ran.ok_or("it never started")?;
    let unanswered = if ran.unanswered.is_empty() {
        String::new()
    } else {
        format!("\n        unanswered: {}", ran.unanswered.join("; "))
    };
    let shown: Vec<&String> = ran.output.iter().take(SHOWN_LINES).collect();
    match &ran.ending {
        None => return Err(format!("it never exited{unanswered}")),
        Some(Ending::NotStarted(why)) => return Err(format!("it could not be started: {why}")),
        Some(Ending::Exited(status)) if *status != command.status => {
            return Err(format!(
                "it exited with {status}, not {}; its output began {shown:?}{unanswered}",
                command.status
            ));
        }
        Some(Ending::Exited(_)) => {}
    }
    let checked = match command.expect {
        Expect::Lines(want) => in_order(&ran.output, want, |want, got| want == got),
        Expect::ProcListing => proc_listing(&ran.output),
        Expect::Maps => maps(&ran.output),
        Expect::Shaped(want) => in_order(&ran.output, want, shaped),
        Expect::Nothing => nothing(&ran.output),
    };
    checked.map_err(|why| format!("{why}; its output began {shown:?}{unanswered}"))
}

/// The longest one-line script a command's name quotes.
const NAMED_SCRIPT: usize = 80;

/// A command as a report names it: its first two arguments, and a script's
/// text when it is one short line, which is what tells one applet's `sh -c`
/// from the next.
fn name(command: &Command) -> String {
    let mut name = command
        .argv
        .iter()
        .take(2)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if let ["sh", "-c", script] = command.argv
        && !script.contains('\n')
    {
        let quoted: String = script.chars().take(NAMED_SCRIPT).collect();
        let more = if quoted.len() < script.len() {
            "..."
        } else {
            ""
        };
        name = format!("{name} '{quoted}{more}'");
    }
    name
}

/// Judge a log, from the boot marker on, against `commands`, the first of
/// which the log numbers `first`.
///
/// # Errors
///
/// A line for each command that failed, saying why, with the calls it found
/// unanswered. Every command is judged, not just the first to fail, because
/// which of them a missing call breaks is the report.
pub(crate) fn judge(
    commands: &[Command],
    first: usize,
    lines: &[String],
) -> std::result::Result<Vec<String>, Vec<String>> {
    if let Some(line) = lines.iter().find(|line| line.contains(UNREADABLE)) {
        return Err(vec![line.trim().to_owned()]);
    }
    let ran = split(lines);
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for (index, command) in (first..).zip(commands) {
        let name = name(command);
        match verdict(command, ran.get(&index)) {
            Ok(()) => passed.push(format!("command {index} ({name}) passed")),
            Err(why) => failed.push(format!("command {index} ({name}): {why}")),
        }
    }
    if failed.is_empty() {
        Ok(passed)
    } else {
        Err(failed)
    }
}

#[cfg(test)]
mod tests;
