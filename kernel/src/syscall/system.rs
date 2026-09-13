//! What the system calls itself, what it has, and how it stops.
//!
//! # Why `sysname` says `Linux`
//!
//! Because a program that asks is deciding what to do, not printing a label.
//! `uname -s` is what configure scripts branch on, what a libc checks before
//! using a system call it thinks is new enough, and what build systems use to
//! choose a code path. Answering `Ferrix` would send every one of them down
//! the path nobody has tested — and Ferrix's whole claim is that the Linux
//! system call interface *is* its interface, not an emulation of somebody
//! else's. `libs/linux-abi` says the same thing at [`Utsname::sysname`].
//!
//! The identity goes where it does no harm and is still visible: `nodename`,
//! `release` and `version`. `uname -a` reads
//!
//! ```text
//! Linux ferrix 6.1.0-ferrix #1 Ferrix 0.1.0 x86_64 GNU/Linux
//! ```
//!
//! which tells a person exactly what they are running while telling a script
//! what it needs to hear. `nodename` and `domainname` are the two a program
//! may change, with `sethostname` and `setdomainname`, and `uname` reports
//! whatever they were last set to. They are system-wide, as they are on Linux
//! outside a UTS namespace.
//!
//! [`Utsname::sysname`]: ferrix_linux_abi::types::Utsname::sysname

use alloc::vec::Vec;

use ferrix_bootinfo::{Arch, PAGE_SIZE, is_user_address};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::nr::Syscall;
use ferrix_sync::IrqControl;

use crate::sync::SpinLock;

use crate::arch;
use crate::console::println;
use crate::mm;
use crate::smp;
use crate::syscall::attributes::int;
use crate::syscall::process::Process;
use crate::syscall::registry;
use crate::syscall::time;
use crate::syscall::uaccess::{self, WORD};

/// Bytes in each of `utsname`'s six fields.
const FIELD: usize = 65;

/// The longest name `sethostname` accepts: `__NEW_UTS_LEN`, a field less its
/// terminator.
pub(crate) const NAME_MAX: usize = FIELD - 1;

/// The system's name, `uname -s`; see the module documentation for why.
pub(crate) const SYSNAME: &str = "Linux";

/// The kernel release.
///
/// Not arbitrary: a configure script compares this against a minimum, and
/// glibc refuses to start under a kernel it reads as older than the one it was
/// built for. So it is a plausible modern Linux version with Ferrix named in
/// the suffix, which is exactly what a distribution kernel does.
pub(crate) const RELEASE: &str = "6.1.0-ferrix";

/// The version string, which by convention starts with a build number.
pub(crate) const VERSION: &str = "#1 Ferrix 0.1.0";

/// A name set by `sethostname` or `setdomainname`: its bytes, NUL-padded, and
/// how many of them were given.
type SetName = ([u8; NAME_MAX], usize);

/// The host name, once something has set one. `None` reads as `ferrix`.
static NODENAME: SpinLock<Option<SetName>> = SpinLock::new(None);

/// The NIS domain name, once something has set one. `None` reads as
/// `(none)`, which is what Linux reports before anything sets it.
static DOMAINNAME: SpinLock<Option<SetName>> = SpinLock::new(None);

/// The host name before anything sets one.
const HOSTNAME_DEFAULT: &str = "ferrix";

/// The domain name before anything sets one.
const DOMAINNAME_DEFAULT: &str = "(none)";

/// The name `slot` holds, or `default` if nothing has set it: its bytes,
/// NUL-padded, and how many of them there are.
fn name_in(slot: &SpinLock<Option<SetName>>, default: &str) -> SetName {
    let set = *slot.lock();
    set.unwrap_or_else(|| {
        let mut bytes = [0_u8; NAME_MAX];
        for (slot, byte) in bytes.iter_mut().zip(default.bytes()) {
            *slot = byte;
        }
        (bytes, default.len())
    })
}

/// A name as `uname` reports it, without the padding.
fn name_bytes(slot: &SpinLock<Option<SetName>>, default: &str) -> Vec<u8> {
    let (bytes, len) = name_in(slot, default);
    bytes.get(..len).unwrap_or_default().to_vec()
}

/// Store `name` in `slot`, if it fits: at most [`NAME_MAX`] bytes, or
/// `EINVAL`.
fn set_name(slot: &SpinLock<Option<SetName>>, name: &[u8]) -> Result<(), Errno> {
    let mut bytes = [0_u8; NAME_MAX];
    bytes
        .get_mut(..name.len())
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(name);
    *slot.lock() = Some((bytes, name.len()));
    Ok(())
}

/// Whether something has set the host name, rather than it reading as the
/// default.
pub(crate) fn hostname_is_set() -> bool {
    NODENAME.lock().is_some()
}

/// The host name `uname` reports as `nodename`.
pub(crate) fn hostname() -> Vec<u8> {
    name_bytes(&NODENAME, HOSTNAME_DEFAULT)
}

/// The domain name `uname` reports as `domainname`.
pub(crate) fn domainname() -> Vec<u8> {
    name_bytes(&DOMAINNAME, DOMAINNAME_DEFAULT)
}

/// Set the host name, as `sethostname` does once it has read the name.
pub(crate) fn set_hostname(name: &[u8]) -> Result<(), Errno> {
    set_name(&NODENAME, name)
}

/// Set the domain name, as `setdomainname` does once it has read the name.
pub(crate) fn set_domainname(name: &[u8]) -> Result<(), Errno> {
    set_name(&DOMAINNAME, name)
}

/// Answer `call` if it is one of this module's.
pub(crate) fn dispatch(
    call: Syscall,
    a: &[u64; 6],
    process: &Process,
) -> Option<Result<usize, Errno>> {
    let answer = match call {
        Syscall::Sysinfo => sys_sysinfo(process, a[0]),
        Syscall::Sethostname => sys_sethostname(process, a[0], int(a[1])),
        Syscall::Setdomainname => sys_setdomainname(process, a[0], int(a[1])),
        Syscall::Getcpu => sys_getcpu(process, a[0], a[1]),
        Syscall::Syslog => sys_syslog(process, int(a[0]), a[1], int(a[2])),
        Syscall::Reboot => sys_reboot(process, a[0] as u32, a[1] as u32, a[2] as u32, a[3]),
        _ => return None,
    };
    Some(answer)
}

/// `uname`.
pub(crate) fn sys_uname(process: &Process, at: u64) -> Result<usize, Errno> {
    // The name Linux uses for the machine, which is not always the name this
    // tree uses for the architecture: 32-bit Arm is `armv7l` to `uname` and
    // `armv7a` here.
    let machine = match arch::ARCH {
        Arch::X86_64 => "x86_64",
        Arch::AArch64 => "aarch64",
        Arch::Armv7a => "armv7l",
    };
    let (node, node_len) = name_in(&NODENAME, HOSTNAME_DEFAULT);
    let (domain, domain_len) = name_in(&DOMAINNAME, DOMAINNAME_DEFAULT);
    let fields: [&[u8]; 6] = [
        SYSNAME.as_bytes(),
        node.get(..node_len).unwrap_or_default(),
        RELEASE.as_bytes(),
        VERSION.as_bytes(),
        machine.as_bytes(),
        domain.get(..domain_len).unwrap_or_default(),
    ];

    // Built whole and copied once. Every field is NUL-padded because the
    // structure is fixed-width and a reader stops at the first NUL.
    let mut buffer = [0_u8; FIELD * 6];
    for (slot, text) in buffer.chunks_mut(FIELD).zip(fields) {
        for (byte, source) in slot.iter_mut().take(NAME_MAX).zip(text) {
            *byte = *source;
        }
    }
    uaccess::copy_to_user(process.space(), at, &buffer).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// Read a name for `sethostname` or `setdomainname`.
///
/// `len` bytes exactly, NULs included, as `kernel/sys.c` copies them: the
/// length is the caller's statement of what the name is.
fn read_name(process: &Process, at: u64, len: i32) -> Result<SetName, Errno> {
    let len = usize::try_from(len)
        .ok()
        .filter(|&len| len <= NAME_MAX)
        .ok_or(Errno::EINVAL)?;
    let mut bytes = [0_u8; NAME_MAX];
    let name = bytes.get_mut(..len).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), at, name).map_err(|_| Errno::EFAULT)?;
    Ok((bytes, len))
}

/// `sethostname`: at most 64 bytes, or `EINVAL`.
pub(crate) fn sys_sethostname(process: &Process, at: u64, len: i32) -> Result<usize, Errno> {
    let (bytes, len) = read_name(process, at, len)?;
    set_hostname(bytes.get(..len).ok_or(Errno::EINVAL)?)?;
    Ok(0)
}

/// `setdomainname`: as `sethostname`, for the other field.
pub(crate) fn sys_setdomainname(process: &Process, at: u64, len: i32) -> Result<usize, Errno> {
    let (bytes, len) = read_name(process, at, len)?;
    set_domainname(bytes.get(..len).ok_or(Errno::EINVAL)?)?;
    Ok(0)
}

/// Put the host name back to what it was before the boot checks changed it.
pub(crate) fn forget_hostname() {
    *NODENAME.lock() = None;
}

/// Bytes in `struct sysinfo` (`linux/sysinfo.h`): a `long` of uptime, three
/// load averages and six memory counts as `unsigned long`s, two `__u16`s,
/// two more counts, a `__u32` unit and `20 - 2 * word - 4` bytes of padding,
/// rounded to a word. 112 on the 64-bit pair and 64 on ARMv7-A; checked with
/// `sizeof` and `offsetof` against the header compiled for x86-64 and for
/// `arm-linux-gnueabihf` (AArch64 is LP64 with no override of it).
pub(crate) const SYSINFO_SIZE: usize = (WORD * 11 + 20).next_multiple_of(WORD);
const _: () = assert!(
    SYSINFO_SIZE == if WORD == 8 { 112 } else { 64 },
    "struct sysinfo is 112 bytes on LP64 and 64 on ILP32"
);

/// Where each field of `struct sysinfo` is, in native words, from the same
/// `offsetof` check: `procs` at 80 or 40, `mem_unit` at 104 or 52.
pub(crate) mod sysinfo_at {
    use super::WORD;
    /// `uptime`.
    pub(crate) const UPTIME: usize = 0;
    /// `totalram`.
    pub(crate) const TOTALRAM: usize = WORD * 4;
    /// `freeram`.
    pub(crate) const FREERAM: usize = WORD * 5;
    /// `procs`, a `__u16`.
    pub(crate) const PROCS: usize = WORD * 10;
    /// `mem_unit`, a `__u32`.
    pub(crate) const MEM_UNIT: usize = WORD * 13;
}

/// `sysinfo`.
///
/// The memory counts are the frame allocator's, which is what `free` prints.
/// Load averages, shared and buffer memory, swap and high memory are zero
/// because none of them exists here.
///
/// `mem_unit` is chosen as `kernel/sys.c`'s `do_sysinfo` chooses it: bytes
/// (unit 1) when the total fits in an `unsigned long`, pages otherwise. On a
/// 64-bit build that is always bytes. On ARMv7-A a machine with 4 GiB or more
/// reports pages, because a count of bytes would wrap in 32 bits and `free`
/// would print a small machine.
///
/// Uptime rounds up, as Linux's does, so a machine that has been up for any
/// time at all has been up for at least a second.
pub(crate) fn sys_sysinfo(process: &Process, at: u64) -> Result<usize, Errno> {
    let nanos = time::now_nanos();
    let uptime = nanos / 1_000_000_000 + u64::from(!nanos.is_multiple_of(1_000_000_000));
    let total_pages = mm::managed_frames();
    let free_pages = mm::free_frames();
    let word_max = if WORD == 8 {
        u64::MAX
    } else {
        u64::from(u32::MAX)
    };
    let (unit, total, free) = match total_pages.checked_mul(PAGE_SIZE) {
        Some(bytes) if bytes <= word_max => (1_u32, bytes, free_pages * PAGE_SIZE),
        _ => (PAGE_SIZE as u32, total_pages, free_pages),
    };
    let procs = u16::try_from(registry::live().len()).unwrap_or(u16::MAX);

    let mut bytes = [0_u8; 112];
    let buffer = bytes.get_mut(..SYSINFO_SIZE).ok_or(Errno::EINVAL)?;
    let mut put = |at: usize, value: &[u8]| {
        if let Some(slot) = buffer.get_mut(at..at + value.len()) {
            slot.copy_from_slice(value);
        }
    };
    let word = |value: u64| value.to_le_bytes();
    put(
        sysinfo_at::UPTIME,
        word(uptime).get(..WORD).unwrap_or_default(),
    );
    put(
        sysinfo_at::TOTALRAM,
        word(total).get(..WORD).unwrap_or_default(),
    );
    put(
        sysinfo_at::FREERAM,
        word(free).get(..WORD).unwrap_or_default(),
    );
    put(sysinfo_at::PROCS, &procs.to_le_bytes());
    put(sysinfo_at::MEM_UNIT, &unit.to_le_bytes());
    uaccess::copy_to_user(process.space(), at, buffer).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// `getcpu`: the logical number of the processor this is running on, and
/// NUMA node 0, which is the only node.
///
/// The per-CPU record is read with interrupts masked, so that the register it
/// is found through and the record it names belong to the same processor --
/// a preemption between the two could otherwise migrate the task and report
/// a number that was never true. The answer can be stale by the time the
/// program reads it, as it can on Linux; that is the nature of the call. The
/// third argument, a cache, has been ignored since Linux 2.6.24.
pub(crate) fn sys_getcpu(process: &Process, cpu_at: u64, node_at: u64) -> Result<usize, Errno> {
    let saved = <arch::Irq as IrqControl>::disable();
    let cpu = smp::this_cpu().map_or(0, |record| record.logical);
    <arch::Irq as IrqControl>::restore(saved);
    if cpu_at != 0 {
        uaccess::put_u32(process.space(), cpu_at, u32::try_from(cpu).unwrap_or(0))?;
    }
    if node_at != 0 {
        uaccess::put_u32(process.space(), node_at, 0)?;
    }
    Ok(0)
}

/// The size `SYSLOG_ACTION_SIZE_BUFFER` reports: Linux's default
/// `CONFIG_LOG_BUF_SHIFT` of 17.
const LOG_BUFFER: usize = 1 << 17;

/// `syslog`, the kernel log's system call -- **with no kernel log behind it**.
///
/// The kernel prints to its console and keeps nothing, so there is nothing
/// to read. Every action answers as Linux answers with an empty buffer:
/// reading all of it or reading and clearing it copies zero bytes, the unread
/// count is zero, opening, closing, clearing and switching console output off
/// or on succeed, and the buffer size is the default `dmesg` sizes its read
/// by. `SYSLOG_ACTION_READ` waits for a message, as on Linux, and since none
/// will ever come it waits until the process is ended. The argument checks are
/// `kernel/printk/printk.c`'s.
pub(crate) fn sys_syslog(
    process: &Process,
    action: i32,
    buf: u64,
    len: i32,
) -> Result<usize, Errno> {
    match action {
        // Close, open, clear, console off, console on, unread size.
        0 | 1 | 5 | 6 | 7 | 9 => Ok(0),
        // Read, read all, read and clear.
        2..=4 => {
            if buf == 0 || len < 0 {
                return Err(Errno::EINVAL);
            }
            if len == 0 {
                return Ok(0);
            }
            let last = buf.checked_add(u64::from(len.unsigned_abs()) - 1);
            if !is_user_address(buf) || !last.is_some_and(is_user_address) {
                return Err(Errno::EFAULT);
            }
            if action == 2 {
                let _ = process.wait_for_exit(u64::MAX);
                return Err(Errno::EINTR);
            }
            Ok(0)
        }
        // Console level.
        8 if (1..=8).contains(&len) => Ok(0),
        10 => Ok(LOG_BUFFER),
        _ => Err(Errno::EINVAL),
    }
}

/// `LINUX_REBOOT_MAGIC1` and the four `MAGIC2`s (`linux/reboot.h`): Linus's
/// and his daughters' birthdays, and the guard against a stray call.
const REBOOT_MAGIC1: u32 = 0xFEE1_DEAD;
/// See [`REBOOT_MAGIC1`].
const REBOOT_MAGIC2: [u32; 4] = [0x2812_1969, 0x0512_1996, 0x1604_1998, 0x2011_2000];

/// The `reboot` commands this kernel answers, from `linux/reboot.h`.
mod command {
    /// Restart the machine.
    pub(super) const RESTART: u32 = 0x0123_4567;
    /// Stop the machine without powering it off.
    pub(super) const HALT: u32 = 0xCDEF_0123;
    /// Let Ctrl-Alt-Del restart the machine.
    pub(super) const CAD_ON: u32 = 0x89AB_CDEF;
    /// Send Ctrl-Alt-Del to init instead.
    pub(super) const CAD_OFF: u32 = 0;
    /// Power the machine off.
    pub(super) const POWER_OFF: u32 = 0x4321_FEDC;
    /// Restart with a command string for the firmware.
    pub(super) const RESTART2: u32 = 0xA1B2_C3D4;
}

/// `reboot`: power off, halt or restart the machine, after checking the magic
/// numbers that keep a stray call from doing it.
///
/// Power-off and halt both call the architecture's `shutdown`, which is the
/// kernel's own way of stopping and powers the machine off where it can --
/// halting without powering off would leave a QEMU run waiting forever for a
/// machine that has nothing more to say. **Restart also shuts down**, because
/// the architecture facade has no reset: that is the one answer here that is
/// not what the call asked for, and it is the one that fails safe, since a
/// machine that was asked to come back and did not is noticed, and a kernel
/// that pretended to reset and carried on running is not. The Ctrl-Alt-Del
/// switches are accepted; there is no keyboard to send the combination.
///
/// The line printed first is the one Linux prints, so a log reads the same.
/// Commands for kexec and suspend are `EINVAL`, as on a kernel built without
/// them.
pub(crate) fn sys_reboot(
    process: &Process,
    magic1: u32,
    magic2: u32,
    cmd: u32,
    arg: u64,
) -> Result<usize, Errno> {
    if magic1 != REBOOT_MAGIC1 || !REBOOT_MAGIC2.contains(&magic2) {
        return Err(Errno::EINVAL);
    }
    match cmd {
        command::CAD_ON | command::CAD_OFF => Ok(0),
        command::POWER_OFF => {
            println!("reboot: Power down");
            arch::shutdown()
        }
        command::HALT => {
            println!("reboot: System halted");
            arch::shutdown()
        }
        command::RESTART => {
            println!("reboot: Restarting system (no reset on this machine; powering off)");
            arch::shutdown()
        }
        command::RESTART2 => {
            // The command string is read, so a bad pointer is still EFAULT,
            // and then has nothing to be given to.
            let mut byte = [0_u8; 1];
            uaccess::copy_from_user(process.space(), arg, &mut byte).map_err(|_| Errno::EFAULT)?;
            println!("reboot: Restarting system with command (no reset; powering off)");
            arch::shutdown()
        }
        _ => Err(Errno::EINVAL),
    }
}
