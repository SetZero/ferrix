//! The initramfs: a cpio "newc" archive the loader hands the kernel.
//!
//! Written here rather than by `cpio` or `bsdcpio` for the reason `fat.rs`
//! writes FAT32: one fewer host tool, and an archive that is the same bytes on
//! every machine, because every timestamp, inode number and owner below is a
//! constant rather than something read off the build host's filesystem.
//!
//! # What is in it
//!
//! The directories a Linux userland expects to find — `/bin`, `/dev`, `/etc`,
//! `/proc`, `/tmp` — and a few files the kernel's stage 8 self-check reads back
//! through the VFS: a marker with known contents, a hard link to it and a
//! symbolic link to it. Each is a shape the unpacker has to get right, placed
//! where a check that the unpack happened can find it.
//!
//! Given a program — `test-vfs`, or `build` and `run` with `--init` or
//! `FERRIX_INIT` — the archive also carries it at `/bin/busybox`, and a
//! symbolic link to it in `/bin` for every name in [`APPLETS`]. Without one it
//! is the same bytes it was before programs could be added.

use std::path::Path;

use crate::{Error, Result};

/// Every timestamp in the archive: 2026-01-01 00:00:00 UTC, the same instant
/// `fat.rs` stamps the boot image with.
pub(crate) const FIXED_MTIME: u32 = 1_767_225_600;

/// Where the marker is unpacked, relative to the root.
pub(crate) const MARKER_PATH: &str = "etc/ferrix/initramfs";

/// The marker's contents. The kernel's self-check compares them byte for byte,
/// and `kernel/src/fs/check.rs` carries the same string.
pub(crate) const MARKER: &[u8] =
    b"unpacked by the kernel from a cpio archive the loader handed it\n";

/// The magic of a plain newc header.
const MAGIC: &[u8] = b"070701";

/// `c_mode` type bits.
const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;
const S_IFLNK: u32 = 0o120_000;

/// A newc archive being written.
struct Newc {
    bytes: Vec<u8>,
    next_ino: u32,
}

impl Newc {
    fn new() -> Newc {
        Newc {
            bytes: Vec::new(),
            next_ino: 1,
        }
    }

    fn pad(&mut self) {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
    }

    fn entry(&mut self, name: &str, mode: u32, ino: u32, nlink: u32, data: &[u8]) -> Result<()> {
        let size = u32::try_from(data.len())
            .map_err(|_| Error::new(format!("{name} is too large for a newc entry")))?;
        let name_size = u32::try_from(name.len() + 1)
            .map_err(|_| Error::new(format!("{name} is too long for a newc entry")))?;
        // ino, mode, uid, gid, nlink, mtime, filesize, devmajor, devminor,
        // rdevmajor, rdevminor, namesize, check.
        let fields = [
            ino,
            mode,
            0,
            0,
            nlink,
            FIXED_MTIME,
            size,
            0,
            0,
            0,
            0,
            name_size,
            0,
        ];
        self.bytes.extend_from_slice(MAGIC);
        for field in fields {
            self.bytes
                .extend_from_slice(format!("{field:08X}").as_bytes());
        }
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.push(0);
        self.pad();
        self.bytes.extend_from_slice(data);
        self.pad();
        Ok(())
    }

    fn ino(&mut self) -> u32 {
        let ino = self.next_ino;
        self.next_ino += 1;
        ino
    }

    fn directory(&mut self, name: &str, permissions: u32) -> Result<()> {
        let ino = self.ino();
        self.entry(name, S_IFDIR | permissions, ino, 2, &[])
    }

    fn file(&mut self, name: &str, permissions: u32, data: &[u8]) -> Result<()> {
        let ino = self.ino();
        self.entry(name, S_IFREG | permissions, ino, 1, data)
    }

    fn symlink(&mut self, name: &str, target: &str) -> Result<()> {
        let ino = self.ino();
        self.entry(name, S_IFLNK | 0o777, ino, 1, target.as_bytes())
    }

    /// One file with several names. newc repeats the inode number on each and
    /// carries the data on the last, which is what GNU cpio writes and what an
    /// unpacker has to cope with.
    fn hard_linked(&mut self, names: &[&str], permissions: u32, data: &[u8]) -> Result<()> {
        let ino = self.ino();
        let nlink = u32::try_from(names.len()).map_err(|_| Error::new("too many hard links"))?;
        for (at, name) in names.iter().enumerate() {
            let body = if at + 1 == names.len() { data } else { &[] };
            self.entry(name, S_IFREG | permissions, ino, nlink, body)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<u8>> {
        self.entry("TRAILER!!!", 0, 0, 1, &[])?;
        Ok(self.bytes)
    }
}

/// Where a program given with `--init` goes, relative to the root.
pub(crate) const PROGRAM_PATH: &str = "bin/busybox";

/// Every applet a busybox 1.37 may provide, each linked in `/bin` beside the
/// program: the union of `busybox --list` from Ubuntu's build and from Alpine's
/// `busybox-static`, less `busybox` itself, whose name the program already has.
///
/// Written out rather than asked of the program, because the program is built
/// for the target and this runs on the host. A link to an applet a given
/// binary lacks costs one directory entry, and running it prints `applet not
/// found`. The kernel starts a program by `argv[0]` and needs none of them;
/// they are there so that the shell's `PATH=/bin` finds a command where a
/// person types it, and so that `cargo xtask test-vfs` can name its programs.
/// Sorted, so that a name is added in one obvious place.
#[rustfmt::skip]
pub(crate) const APPLETS: &[&str] = &[
    "[", "[[", "acpid", "add-shell", "addgroup", "adduser", "adjtimex", "ar", "arch",
    "arp", "arping", "ascii", "ash", "awk", "base64", "basename", "bbconfig", "bc",
    "beep", "blkdiscard", "blkid", "blockdev", "brctl", "bunzip2", "bzcat", "bzip2",
    "cal", "cat", "chattr", "chgrp", "chmod", "chown", "chpasswd", "chroot", "chvt",
    "cksum", "clear", "cmp", "comm", "cp", "cpio", "crc32", "crond", "crontab",
    "cryptpw", "cttyhack", "cut", "date", "dc", "dd", "deallocvt", "delgroup",
    "deluser", "depmod", "devmem", "df", "diff", "dirname", "dmesg", "dnsdomainname",
    "dos2unix", "dpkg", "dpkg-deb", "du", "dumpkmap", "dumpleases", "echo", "ed",
    "egrep", "eject", "env", "ether-wake", "expand", "expr", "factor", "fallocate",
    "false", "fatattr", "fbset", "fbsplash", "fdflush", "fdisk", "fgrep", "find",
    "findfs", "flock", "fold", "free", "freeramdisk", "fsck", "fsfreeze", "fstrim",
    "fsync", "ftpget", "ftpput", "fuser", "getfattr", "getopt", "getty", "grep",
    "groups", "gunzip", "gzip", "halt", "hd", "head", "hexdump", "hostid", "hostname",
    "httpd", "hwclock", "i2cdetect", "i2cdump", "i2cget", "i2cset", "i2ctransfer", "id",
    "ifconfig", "ifdown", "ifenslave", "ifup", "init", "inotifyd", "insmod", "install",
    "ionice", "iostat", "ip", "ipaddr", "ipcalc", "ipcrm", "ipcs", "iplink", "ipneigh",
    "iproute", "iprule", "iptunnel", "kbd_mode", "kill", "killall", "killall5", "klogd",
    "last", "less", "link", "linux32", "linux64", "linuxrc", "ln", "loadfont",
    "loadkmap", "logger", "login", "logname", "logread", "losetup", "ls", "lsattr",
    "lsmod", "lsof", "lsscsi", "lsusb", "lzcat", "lzma", "lzop", "lzopcat", "makemime",
    "md5sum", "mdev", "mesg", "microcom", "mim", "mkdir", "mkdosfs", "mke2fs", "mkfifo",
    "mkfs.vfat", "mknod", "mkpasswd", "mkswap", "mktemp", "modinfo", "modprobe", "more",
    "mount", "mountpoint", "mpstat", "mt", "mv", "nameif", "nanddump", "nandwrite",
    "nbd-client", "nc", "netstat", "nice", "nl", "nmeter", "nohup", "nologin", "nproc",
    "nsenter", "nslookup", "ntpd", "nuke", "od", "openvt", "partprobe", "passwd",
    "paste", "patch", "pgrep", "pidof", "ping", "ping6", "pipe_progress", "pivot_root",
    "pkill", "pmap", "poweroff", "printenv", "printf", "ps", "pscan", "pstree", "pwd",
    "pwdx", "raidautorun", "rdate", "rdev", "readahead", "readlink", "realpath",
    "reboot", "reformime", "remove-shell", "renice", "reset", "resize", "resume", "rev",
    "rfkill", "rm", "rmdir", "rmmod", "route", "rpm", "rpm2cpio", "run-init",
    "run-parts", "sed", "sendmail", "seq", "setconsole", "setfont", "setkeycodes",
    "setlogcons", "setpriv", "setserial", "setsid", "sh", "sha1sum", "sha256sum",
    "sha3sum", "sha512sum", "showkey", "shred", "shuf", "slattach", "sleep", "sort",
    "split", "ssl_client", "start-stop-daemon", "stat", "static-sh", "strings", "stty",
    "su", "sulogin", "sum", "svc", "svok", "swapoff", "swapon", "switch_root", "sync",
    "sysctl", "syslogd", "tac", "tail", "tar", "taskset", "tc", "tee", "telnet",
    "telnetd", "test", "tftp", "time", "timeout", "top", "touch", "tr", "traceroute",
    "traceroute6", "tree", "true", "truncate", "ts", "tty", "ttysize", "tunctl",
    "ubirename", "udhcpc", "udhcpc6", "udhcpd", "uevent", "umount", "uname",
    "uncompress", "unexpand", "uniq", "unix2dos", "unlink", "unlzma", "unlzop",
    "unshare", "unxz", "unzip", "uptime", "usleep", "uudecode", "uuencode", "vconfig",
    "vi", "vlock", "volname", "w", "watch", "watchdog", "wc", "wget", "which", "who",
    "whoami", "whois", "xargs", "xxd", "xz", "xzcat", "yes", "zcat", "zcip",
];

/// The archive every image carries, with `program` at `/bin/busybox` when one
/// is given.
pub(crate) fn build(program: Option<&Path>) -> Result<Vec<u8>> {
    let program = program
        .map(|path| {
            std::fs::read(path)
                .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
        })
        .transpose()?;
    build_with(program.as_deref())
}

/// [`build`], with the program's bytes rather than its path.
fn build_with(program: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut archive = Newc::new();
    archive.directory(".", 0o755)?;
    for (name, permissions) in [
        ("bin", 0o755),
        ("dev", 0o755),
        ("etc", 0o755),
        ("etc/ferrix", 0o755),
        ("proc", 0o555),
        ("tmp", 0o1777),
    ] {
        archive.directory(name, permissions)?;
    }
    archive.file("etc/hostname", 0o644, b"ferrix\n")?;
    let link = format!("{MARKER_PATH}.link");
    archive.hard_linked(&[MARKER_PATH, &link], 0o644, MARKER)?;
    archive.symlink(&format!("{MARKER_PATH}.symlink"), "initramfs")?;
    if let Some(program) = program {
        archive.file(PROGRAM_PATH, 0o755, program)?;
        for applet in APPLETS {
            archive.symlink(&format!("bin/{applet}"), "busybox")?;
        }
    }
    archive.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_is_the_same_bytes_every_time() {
        assert_eq!(build(None).unwrap(), build(None).unwrap());
        let program: &[u8] = b"\x7fELF not really";
        assert_eq!(
            build_with(Some(program)).unwrap(),
            build_with(Some(program)).unwrap()
        );
    }

    #[test]
    fn a_program_goes_in_bin_with_its_applets_beside_it() {
        let program: &[u8] = b"\x7fELF not really";
        let bytes = build_with(Some(program)).unwrap();
        let archive = ferrix_cpio::Archive::new(&bytes);
        assert_eq!(archive.find(PROGRAM_PATH).unwrap().unwrap().data, program);
        for applet in APPLETS {
            let link = archive.find(&format!("bin/{applet}")).unwrap().unwrap();
            assert_eq!(link.symlink_target(), Some("busybox"), "bin/{applet}");
        }
        let in_bin = archive
            .entries()
            .map(|entry| entry.unwrap().name)
            .filter(|name| name.starts_with("bin/"))
            .count();
        assert_eq!(
            in_bin,
            APPLETS.len() + 1,
            "the program and one link per applet"
        );
        assert!(
            archive
                .entries()
                .all(|entry| ferrix_cpio::is_safe_path(entry.unwrap().name)),
            "every applet name joins to /bin without escaping it"
        );
    }

    #[test]
    fn without_a_program_bin_is_empty() {
        let bytes = build(None).unwrap();
        let archive = ferrix_cpio::Archive::new(&bytes);
        assert!(archive.find(PROGRAM_PATH).unwrap().is_none());
        assert!(
            archive
                .entries()
                .all(|entry| !entry.unwrap().name.starts_with("bin/")),
            "without --init there is nothing in /bin"
        );
    }

    #[test]
    fn the_applets_are_the_ones_a_shell_needs_and_collide_with_nothing() {
        for needed in [
            "sh", "ash", "ls", "cat", "mkdir", "uname", "echo", "[", "[[",
        ] {
            assert!(APPLETS.contains(&needed), "{needed} is missing");
        }
        assert!(
            APPLETS.windows(2).all(|pair| pair[0] < pair[1]),
            "APPLETS is sorted and has no name twice"
        );
        for applet in APPLETS {
            assert!(
                !applet.is_empty() && !applet.contains('/') && *applet != "." && *applet != "..",
                "{applet:?} is not a name in /bin"
            );
            assert_ne!(
                format!("bin/{applet}"),
                PROGRAM_PATH,
                "a link must not replace the program"
            );
        }
    }

    #[test]
    fn the_archive_reads_back_with_the_kernels_own_reader() {
        let bytes = build(None).unwrap();
        let archive = ferrix_cpio::Archive::new(&bytes);
        let names: Vec<&str> = archive.entries().map(|entry| entry.unwrap().name).collect();
        assert_eq!(names.first(), Some(&"."));
        assert!(names.contains(&"tmp"));

        let marker = archive.find(MARKER_PATH).unwrap().unwrap();
        assert!(marker.data.is_empty(), "the data belongs on the last link");
        assert_eq!(marker.nlink, 2);
        let link = archive
            .find(&format!("{MARKER_PATH}.link"))
            .unwrap()
            .unwrap();
        assert_eq!(link.data, MARKER);
        assert_eq!(link.ino, marker.ino);

        let symlink = archive
            .find(&format!("{MARKER_PATH}.symlink"))
            .unwrap()
            .unwrap();
        assert_eq!(symlink.symlink_target(), Some("initramfs"));
        assert!(names.iter().all(|name| ferrix_cpio::is_safe_path(name)));
    }
}
