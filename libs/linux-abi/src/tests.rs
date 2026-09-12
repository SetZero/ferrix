//! Layout and mapping assertions.
//!
//! These tests are the reason the ABI lives in a host-buildable crate. A wrong
//! structure size or field offset is not a compile error anywhere in the
//! kernel; it is a program that reads a file size out of padding, and the
//! symptom appears arbitrarily far from the cause. Every number asserted below
//! is the one Linux uses on that architecture.

extern crate std;

use core::mem::{align_of, offset_of, size_of};
use std::collections::HashSet;
use std::vec::Vec;

use crate::errno::{Errno, encode};
use crate::nr::{Syscall, aarch64, arm, from_aarch64, from_arm, from_x86_64, x86_64};
use crate::types;

/// Calls whose number this crate knows on x86-64 but which the generic table
/// never had, because musl reaches the same effect through an `*at` form or a
/// register write.
///
/// Kept as data so `x86_64_only_calls_are_the_expected_ones` can assert the
/// difference between the two tables exactly, rather than spot-checking it.
const X86_64_ONLY: &[Syscall] = &[
    Syscall::Open,
    Syscall::Stat,
    Syscall::Lstat,
    Syscall::Poll,
    Syscall::Pipe,
    Syscall::Dup2,
    Syscall::Fork,
    Syscall::Vfork,
    Syscall::Access,
    Syscall::Rename,
    Syscall::Mkdir,
    Syscall::Rmdir,
    Syscall::Unlink,
    Syscall::Symlink,
    Syscall::Link,
    Syscall::Readlink,
    Syscall::Chmod,
    Syscall::Chown,
    Syscall::ArchPrctl,
    Syscall::EpollWait,
];

/// Calls both tables have, with the number each architecture gives them.
///
/// This is a second, independent transcription of the numbers in `nr.rs`: a
/// typo in either copy shows up here as a failure rather than as a call
/// dispatched to the wrong handler.
const SHARED: &[(usize, usize, Syscall)] = &[
    (x86_64::READ, aarch64::READ, Syscall::Read),
    (x86_64::WRITE, aarch64::WRITE, Syscall::Write),
    (x86_64::CLOSE, aarch64::CLOSE, Syscall::Close),
    (x86_64::FSTAT, aarch64::FSTAT, Syscall::Fstat),
    (x86_64::NEWFSTATAT, aarch64::NEWFSTATAT, Syscall::Newfstatat),
    (x86_64::STATX, aarch64::STATX, Syscall::Statx),
    (x86_64::OPENAT, aarch64::OPENAT, Syscall::Openat),
    (x86_64::OPENAT2, aarch64::OPENAT2, Syscall::Openat2),
    (x86_64::LSEEK, aarch64::LSEEK, Syscall::Lseek),
    (x86_64::MMAP, aarch64::MMAP, Syscall::Mmap),
    (x86_64::MPROTECT, aarch64::MPROTECT, Syscall::Mprotect),
    (x86_64::MUNMAP, aarch64::MUNMAP, Syscall::Munmap),
    (x86_64::BRK, aarch64::BRK, Syscall::Brk),
    (
        x86_64::RT_SIGACTION,
        aarch64::RT_SIGACTION,
        Syscall::RtSigaction,
    ),
    (
        x86_64::RT_SIGRETURN,
        aarch64::RT_SIGRETURN,
        Syscall::RtSigreturn,
    ),
    (
        x86_64::SIGALTSTACK,
        aarch64::SIGALTSTACK,
        Syscall::Sigaltstack,
    ),
    (x86_64::IOCTL, aarch64::IOCTL, Syscall::Ioctl),
    (x86_64::WRITEV, aarch64::WRITEV, Syscall::Writev),
    (x86_64::PPOLL, aarch64::PPOLL, Syscall::Ppoll),
    (x86_64::CLONE, aarch64::CLONE, Syscall::Clone),
    (x86_64::CLONE3, aarch64::CLONE3, Syscall::Clone3),
    (x86_64::EXECVE, aarch64::EXECVE, Syscall::Execve),
    (x86_64::EXIT, aarch64::EXIT, Syscall::Exit),
    (x86_64::EXIT_GROUP, aarch64::EXIT_GROUP, Syscall::ExitGroup),
    (x86_64::WAIT4, aarch64::WAIT4, Syscall::Wait4),
    (x86_64::FUTEX, aarch64::FUTEX, Syscall::Futex),
    (
        x86_64::SET_TID_ADDRESS,
        aarch64::SET_TID_ADDRESS,
        Syscall::SetTidAddress,
    ),
    (
        x86_64::SET_ROBUST_LIST,
        aarch64::SET_ROBUST_LIST,
        Syscall::SetRobustList,
    ),
    (
        x86_64::CLOCK_GETTIME,
        aarch64::CLOCK_GETTIME,
        Syscall::ClockGettime,
    ),
    (x86_64::GETDENTS64, aarch64::GETDENTS64, Syscall::Getdents64),
    (x86_64::GETRANDOM, aarch64::GETRANDOM, Syscall::Getrandom),
    (x86_64::PRLIMIT64, aarch64::PRLIMIT64, Syscall::Prlimit64),
    (x86_64::UNAME, aarch64::UNAME, Syscall::Uname),
    (x86_64::FACCESSAT2, aarch64::FACCESSAT2, Syscall::Faccessat2),
    (
        x86_64::EPOLL_PWAIT,
        aarch64::EPOLL_PWAIT,
        Syscall::EpollPwait,
    ),
    (x86_64::RSEQ, aarch64::RSEQ, Syscall::Rseq),
    (x86_64::MEMBARRIER, aarch64::MEMBARRIER, Syscall::Membarrier),
];

/// Every call either table maps, found by sweeping the number space.
fn mapped(translate: fn(usize) -> Option<Syscall>) -> HashSet<Syscall> {
    (0..=600).filter_map(translate).collect()
}

/// The numbers an ARMv7-A program can arrive with.
///
/// Not a single range: the EABI table runs from zero, and the six ARM-private
/// calls sit at `0x0f0000`. A sweep that stopped at the shared table's top
/// would silently exclude `set_tls`, which is the one call a threaded program
/// cannot start without.
fn arm_number_space() -> impl Iterator<Item = usize> {
    (0..=600).chain(arm::ARM_PRIVATE_BASE..=arm::ARM_PRIVATE_BASE + 16)
}

/// Every call the ARMv7-A table maps, private range included.
fn mapped_arm() -> HashSet<Syscall> {
    arm_number_space().filter_map(from_arm).collect()
}

// ---------------------------------------------------------------------------
// Time, vectors and directories
// ---------------------------------------------------------------------------

#[test]
fn timespec_and_timeval_are_two_64_bit_words() {
    assert_eq!(
        size_of::<types::Timespec>(),
        16,
        "timespec is two 64-bit fields"
    );
    assert_eq!(
        size_of::<types::Timeval>(),
        16,
        "timeval is two 64-bit fields"
    );
    assert_eq!(
        offset_of!(types::Timespec, tv_nsec),
        8,
        "tv_nsec follows tv_sec"
    );
    assert_eq!(
        offset_of!(types::Timeval, tv_usec),
        8,
        "tv_usec follows tv_sec"
    );
}

#[test]
fn iovec_is_a_pointer_and_a_length() {
    assert_eq!(
        size_of::<types::Iovec>(),
        16,
        "iovec is a base and a length"
    );
    assert_eq!(
        offset_of!(types::Iovec, iov_len),
        8,
        "iov_len follows iov_base"
    );
}

#[test]
fn dirent64_header_matches_the_kernel() {
    assert_eq!(
        offset_of!(types::LinuxDirent64, d_ino),
        0,
        "d_ino comes first"
    );
    assert_eq!(
        offset_of!(types::LinuxDirent64, d_off),
        8,
        "d_off follows d_ino"
    );
    assert_eq!(
        offset_of!(types::LinuxDirent64, d_reclen),
        16,
        "d_reclen follows d_off"
    );
    assert_eq!(
        offset_of!(types::LinuxDirent64, d_type),
        18,
        "d_type follows d_reclen"
    );
    assert_eq!(
        offset_of!(types::LinuxDirent64, d_name),
        types::DIRENT64_NAME_OFFSET,
        "the name starts immediately after d_type, before any tail padding"
    );
}

#[test]
fn dirent64_size_is_not_the_name_offset() {
    assert_eq!(
        size_of::<types::LinuxDirent64>(),
        24,
        "the head rounds up to 8-byte alignment"
    );
    assert!(
        types::DIRENT64_NAME_OFFSET < size_of::<types::LinuxDirent64>(),
        "using size_of instead of the name offset would skip the first bytes of every name"
    );
}

// ---------------------------------------------------------------------------
// stat, per architecture
// ---------------------------------------------------------------------------

#[test]
fn x86_64_stat_is_144_bytes() {
    assert_eq!(
        size_of::<types::x86_64::Stat>(),
        144,
        "x86-64 struct stat is 144 bytes"
    );
    assert_eq!(
        align_of::<types::x86_64::Stat>(),
        8,
        "every field is 8-byte aligned or smaller"
    );
}

#[test]
fn x86_64_stat_field_offsets() {
    type S = types::x86_64::Stat;
    assert_eq!(offset_of!(S, st_dev), 0, "st_dev comes first");
    assert_eq!(offset_of!(S, st_ino), 8, "st_ino follows st_dev");
    assert_eq!(
        offset_of!(S, st_nlink),
        16,
        "x86-64 puts st_nlink before st_mode"
    );
    assert_eq!(
        offset_of!(S, st_mode),
        24,
        "st_mode is a 32-bit field at 24"
    );
    assert_eq!(offset_of!(S, st_uid), 28, "st_uid follows st_mode");
    assert_eq!(offset_of!(S, st_gid), 32, "st_gid follows st_uid");
    assert_eq!(
        offset_of!(S, st_rdev),
        40,
        "st_rdev follows the explicit pad"
    );
    assert_eq!(
        offset_of!(S, st_size),
        48,
        "a wrong st_size offset reads garbage file sizes"
    );
    assert_eq!(
        offset_of!(S, st_blksize),
        56,
        "st_blksize is 64-bit on x86-64"
    );
    assert_eq!(offset_of!(S, st_blocks), 64, "st_blocks follows st_blksize");
    assert_eq!(offset_of!(S, st_atime), 72, "the timestamps start at 72");
    assert_eq!(
        offset_of!(S, st_mtime),
        88,
        "st_mtime follows st_atime and its nanoseconds"
    );
    assert_eq!(
        offset_of!(S, st_ctime),
        104,
        "st_ctime follows st_mtime and its nanoseconds"
    );
    assert_eq!(
        offset_of!(S, __unused),
        120,
        "three reserved words close the structure"
    );
}

#[test]
fn aarch64_stat_is_128_bytes() {
    assert_eq!(
        size_of::<types::aarch64::Stat>(),
        128,
        "the generic struct stat is 128 bytes"
    );
    assert_eq!(
        align_of::<types::aarch64::Stat>(),
        8,
        "every field is 8-byte aligned or smaller"
    );
}

#[test]
fn aarch64_stat_field_offsets() {
    type S = types::aarch64::Stat;
    assert_eq!(offset_of!(S, st_dev), 0, "st_dev comes first");
    assert_eq!(offset_of!(S, st_ino), 8, "st_ino follows st_dev");
    assert_eq!(
        offset_of!(S, st_mode),
        16,
        "the generic layout puts st_mode before st_nlink"
    );
    assert_eq!(
        offset_of!(S, st_nlink),
        20,
        "st_nlink is 32-bit here, 64-bit on x86-64"
    );
    assert_eq!(offset_of!(S, st_uid), 24, "st_uid follows st_nlink");
    assert_eq!(offset_of!(S, st_gid), 28, "st_gid follows st_uid");
    assert_eq!(offset_of!(S, st_rdev), 32, "st_rdev follows st_gid");
    assert_eq!(
        offset_of!(S, st_size),
        48,
        "a wrong st_size offset reads garbage file sizes"
    );
    assert_eq!(
        offset_of!(S, st_blksize),
        56,
        "st_blksize is only 32 bits wide here"
    );
    assert_eq!(
        offset_of!(S, st_blocks),
        64,
        "st_blocks follows the pad after st_blksize"
    );
    assert_eq!(
        offset_of!(S, st_atime),
        72,
        "the timestamps start at 72, as on x86-64"
    );
    assert_eq!(
        offset_of!(S, st_ctime),
        104,
        "st_ctime follows st_mtime and its nanoseconds"
    );
    assert_eq!(
        offset_of!(S, __unused),
        120,
        "two reserved words close the structure"
    );
}

#[test]
fn the_two_stat_layouts_really_do_differ() {
    assert_ne!(
        size_of::<types::x86_64::Stat>(),
        size_of::<types::aarch64::Stat>(),
        "using one architecture's stat on the other is the bug these types exist to prevent"
    );
    assert_ne!(
        offset_of!(types::x86_64::Stat, st_mode),
        offset_of!(types::aarch64::Stat, st_mode),
        "st_mode moves between the two layouts"
    );
}

// ---------------------------------------------------------------------------
// statx
// ---------------------------------------------------------------------------

#[test]
fn statx_timestamp_is_16_bytes() {
    assert_eq!(
        size_of::<types::StatxTimestamp>(),
        16,
        "a statx timestamp is 16 bytes"
    );
    assert_eq!(
        offset_of!(types::StatxTimestamp, tv_nsec),
        8,
        "tv_nsec follows tv_sec"
    );
}

#[test]
fn statx_is_256_bytes() {
    assert_eq!(
        size_of::<types::Statx>(),
        256,
        "struct statx is 256 bytes, reserved space included"
    );
    assert_eq!(
        align_of::<types::Statx>(),
        8,
        "struct statx is 8-byte aligned"
    );
}

#[test]
fn statx_field_offsets() {
    type S = types::Statx;
    assert_eq!(offset_of!(S, stx_mask), 0, "the mask comes first");
    assert_eq!(
        offset_of!(S, stx_blksize),
        4,
        "stx_blksize follows the mask"
    );
    assert_eq!(
        offset_of!(S, stx_attributes),
        8,
        "stx_attributes is 8-byte aligned"
    );
    assert_eq!(
        offset_of!(S, stx_mode),
        28,
        "stx_mode is a 16-bit field at 28"
    );
    assert_eq!(
        offset_of!(S, stx_ino),
        32,
        "stx_ino follows the spare half-word"
    );
    assert_eq!(offset_of!(S, stx_size), 40, "stx_size follows stx_ino");
    assert_eq!(
        offset_of!(S, stx_atime),
        64,
        "the four timestamps start at 64"
    );
    assert_eq!(offset_of!(S, stx_btime), 80, "stx_btime follows stx_atime");
    assert_eq!(offset_of!(S, stx_ctime), 96, "stx_ctime follows stx_btime");
    assert_eq!(
        offset_of!(S, stx_mtime),
        112,
        "stx_mtime is last of the four"
    );
    assert_eq!(
        offset_of!(S, stx_rdev_major),
        128,
        "the device numbers follow the timestamps"
    );
    assert_eq!(
        offset_of!(S, stx_mnt_id),
        144,
        "stx_mnt_id follows the four 32-bit device numbers at 128..144"
    );
}

// ---------------------------------------------------------------------------
// Identity, limits and usage
// ---------------------------------------------------------------------------

#[test]
fn utsname_is_six_65_byte_strings() {
    assert_eq!(
        size_of::<types::Utsname>(),
        390,
        "six fields of __NEW_UTS_LEN + 1 bytes"
    );
    assert_eq!(
        align_of::<types::Utsname>(),
        1,
        "an array of bytes needs no alignment"
    );
    assert_eq!(
        offset_of!(types::Utsname, machine),
        260,
        "machine is the fifth field"
    );
    assert_eq!(
        offset_of!(types::Utsname, domainname),
        325,
        "domainname is the sixth field"
    );
}

#[test]
fn rlimit_is_two_words() {
    assert_eq!(
        size_of::<types::Rlimit>(),
        16,
        "rlimit is a soft and a hard limit"
    );
    assert_eq!(
        offset_of!(types::Rlimit, rlim_max),
        8,
        "the hard limit follows the soft one"
    );
}

#[test]
fn rusage_is_144_bytes() {
    assert_eq!(
        size_of::<types::Rusage>(),
        144,
        "two timevals and fourteen longs"
    );
    assert_eq!(
        offset_of!(types::Rusage, ru_stime),
        16,
        "system time follows user time"
    );
    assert_eq!(
        offset_of!(types::Rusage, ru_maxrss),
        32,
        "the counters start after the times"
    );
    assert_eq!(
        offset_of!(types::Rusage, ru_minflt),
        64,
        "minor faults are the fifth counter"
    );
    assert_eq!(
        offset_of!(types::Rusage, ru_nivcsw),
        136,
        "involuntary switches close it"
    );
}

#[test]
fn sysinfo_is_112_bytes_on_64_bit() {
    assert_eq!(
        size_of::<types::Sysinfo>(),
        112,
        "the 64-bit form leaves no room for _f"
    );
    assert_eq!(
        offset_of!(types::Sysinfo, loads),
        8,
        "the load averages follow uptime"
    );
    assert_eq!(
        offset_of!(types::Sysinfo, totalram),
        32,
        "memory counts follow the loads"
    );
    assert_eq!(
        offset_of!(types::Sysinfo, procs),
        80,
        "procs is a 16-bit field at 80"
    );
    assert_eq!(
        offset_of!(types::Sysinfo, totalhigh),
        88,
        "alignment pads before totalhigh"
    );
    assert_eq!(
        offset_of!(types::Sysinfo, mem_unit),
        104,
        "mem_unit is the last real field"
    );
}

// ---------------------------------------------------------------------------
// Signals and polling
// ---------------------------------------------------------------------------

#[test]
fn sigset_is_one_word() {
    assert_eq!(
        size_of::<types::Sigset>(),
        8,
        "the kernel's sigset_t is 8 bytes; the C library's is not, which is why \
         rt_sigprocmask takes the size as an argument"
    );
    assert_eq!(
        size_of::<types::Sigset>() * 8,
        types::NSIG as usize,
        "one bit per signal"
    );
}

#[test]
fn sigaction_has_the_kernel_field_order() {
    type S = types::Sigaction;
    assert_eq!(
        size_of::<S>(),
        32,
        "handler, flags, restorer and an 8-byte mask"
    );
    assert_eq!(offset_of!(S, sa_handler), 0, "the handler comes first");
    assert_eq!(
        offset_of!(S, sa_flags),
        8,
        "flags follow the handler, unlike in POSIX"
    );
    assert_eq!(
        offset_of!(S, sa_restorer),
        16,
        "the restorer sits before the mask"
    );
    assert_eq!(
        offset_of!(S, sa_mask),
        24,
        "the mask is last so the structure can grow"
    );
}

#[test]
fn stack_t_is_24_bytes() {
    assert_eq!(
        size_of::<types::Stack>(),
        24,
        "a pointer, an int and a size with padding"
    );
    assert_eq!(
        offset_of!(types::Stack, ss_flags),
        8,
        "ss_flags follows ss_sp"
    );
    assert_eq!(
        offset_of!(types::Stack, ss_size),
        16,
        "ss_size is realigned to 16"
    );
}

#[test]
fn pollfd_is_8_bytes() {
    assert_eq!(size_of::<types::Pollfd>(), 8, "an int and two shorts");
    assert_eq!(offset_of!(types::Pollfd, events), 4, "events follows fd");
    assert_eq!(
        offset_of!(types::Pollfd, revents),
        6,
        "revents follows events"
    );
}

#[test]
fn epoll_event_is_packed_on_x86_64_only() {
    assert_eq!(
        size_of::<types::x86_64::EpollEvent>(),
        12,
        "x86-64 defines EPOLL_PACKED so that 32-bit processes see the same layout"
    );
    assert_eq!(
        offset_of!(types::x86_64::EpollEvent, data),
        4,
        "packed: data follows events"
    );
    assert_eq!(
        size_of::<types::aarch64::EpollEvent>(),
        16,
        "AArch64 uses the natural layout"
    );
    assert_eq!(
        offset_of!(types::aarch64::EpollEvent, data),
        8,
        "unpacked: data is aligned to 8"
    );
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

#[test]
fn open_flags_match_the_generic_header() {
    assert_eq!(types::O_CREAT, 64, "O_CREAT is octal 100");
    assert_eq!(types::O_TRUNC, 512, "O_TRUNC is octal 1000");
    assert_eq!(types::O_APPEND, 1024, "O_APPEND is octal 2000");
    assert_eq!(types::O_NONBLOCK, 2048, "O_NONBLOCK is octal 4000");
    assert_eq!(types::O_DIRECTORY, 65536, "O_DIRECTORY is octal 200000");
    assert_eq!(types::O_CLOEXEC, 524_288, "O_CLOEXEC is octal 2000000");
    assert_eq!(
        types::EFD_CLOEXEC,
        types::O_CLOEXEC,
        "eventfd reuses the open flag"
    );
}

#[test]
fn file_type_bits_are_disjoint_under_the_mask() {
    let types_seen = [
        types::S_IFIFO,
        types::S_IFCHR,
        types::S_IFDIR,
        types::S_IFBLK,
        types::S_IFREG,
        types::S_IFLNK,
        types::S_IFSOCK,
    ];
    let unique: HashSet<u32> = types_seen.iter().copied().collect();
    assert_eq!(
        unique.len(),
        types_seen.len(),
        "every file type has a distinct encoding"
    );
    assert!(
        types_seen.iter().all(|&t| t & types::S_IFMT == t),
        "every file type fits inside S_IFMT"
    );
}

#[test]
fn at_fdcwd_is_negative_and_not_minus_one() {
    assert_eq!(types::AT_FDCWD, -100, "AT_FDCWD is -100");
    assert_ne!(
        types::AT_FDCWD,
        -1,
        "a stray -1 must not be mistaken for the working directory"
    );
}

#[test]
fn auxv_keys_musl_startup_reads() {
    assert_eq!(types::AT_PHDR, 3, "the program headers key");
    assert_eq!(types::AT_PAGESZ, 6, "the page size key");
    assert_eq!(types::AT_ENTRY, 9, "the entry point key");
    assert_eq!(
        types::AT_SECURE,
        23,
        "the key musl checks before trusting the environment"
    );
    assert_eq!(
        types::AT_RANDOM,
        25,
        "the key musl seeds its stack guard from"
    );
    assert_eq!(types::AT_SYSINFO_EHDR, 33, "the vDSO key");
    assert_eq!(types::AT_MINSIGSTKSZ, 51, "the minimum signal stack key");
}

#[test]
fn signal_numbers_are_the_generic_ones() {
    assert_eq!(types::SIGKILL, 9, "SIGKILL is 9");
    assert_eq!(types::SIGSEGV, 11, "SIGSEGV is 11");
    assert_eq!(
        types::SIGCHLD,
        17,
        "SIGCHLD is 17 on Linux, unlike on other UNIX systems"
    );
    assert_eq!(types::SIGSTOP, 19, "SIGSTOP is 19");
    assert_eq!(types::SIGSYS, 31, "SIGSYS closes the classic range");
    assert_eq!(
        types::SIGRTMIN,
        32,
        "the kernel's first real-time signal is 32"
    );
}

#[test]
fn clock_identifiers_are_the_generic_ones() {
    assert_eq!(types::CLOCK_REALTIME, 0, "the wall clock is 0");
    assert_eq!(types::CLOCK_MONOTONIC, 1, "the monotonic clock is 1");
    assert_eq!(
        types::CLOCK_MONOTONIC_RAW,
        4,
        "the unadjusted monotonic clock is 4"
    );
    assert_eq!(types::CLOCK_BOOTTIME, 7, "the suspend-aware clock is 7");
}

// ---------------------------------------------------------------------------
// errno
// ---------------------------------------------------------------------------

#[test]
fn errno_values_are_the_generic_ones() {
    assert_eq!(Errno::EPERM.0, 1, "EPERM is 1");
    assert_eq!(Errno::ENOENT.0, 2, "ENOENT is 2");
    assert_eq!(
        Errno::EAGAIN.0,
        11,
        "EAGAIN, which is also EWOULDBLOCK, is 11"
    );
    assert_eq!(Errno::EINVAL.0, 22, "EINVAL is 22");
    assert_eq!(Errno::ERANGE.0, 34, "ERANGE closes the errno-base range");
    assert_eq!(
        Errno::ENOSYS.0,
        38,
        "ENOSYS is what an unknown call number returns"
    );
    assert_eq!(Errno::EOVERFLOW.0, 75, "EOVERFLOW is 75");
    assert_eq!(Errno::ECONNREFUSED.0, 111, "ECONNREFUSED is 111");
}

#[test]
fn errno_encodes_as_a_negative_return_value() {
    assert_eq!(Errno::EPERM.as_return_value(), -1, "EPERM returns as -1");
    assert_eq!(
        Errno::ENOSYS.as_return_value(),
        -38,
        "ENOSYS returns as -38"
    );
    assert!(
        Errno::EINPROGRESS.as_return_value() > -(crate::errno::MAX_ERRNO as isize),
        "every error stays inside the range reserved for them"
    );
}

#[test]
fn encode_maps_results_onto_the_return_register() {
    assert_eq!(encode(Ok(0)), 0, "a successful zero is returned unchanged");
    assert_eq!(
        encode(Ok(4096)),
        4096,
        "a successful count is returned unchanged"
    );
    assert_eq!(encode(Err(Errno::ENOENT)), -2, "ENOENT is returned as -2");
    assert_eq!(encode(Err(Errno::EPERM)), -1, "EPERM is returned as -1");
}

// ---------------------------------------------------------------------------
// Syscall number translation
// ---------------------------------------------------------------------------

#[test]
fn x86_64_round_trips() {
    assert_eq!(
        from_x86_64(x86_64::WRITE),
        Some(Syscall::Write),
        "write is 1 on x86-64"
    );
    assert_eq!(
        from_x86_64(x86_64::READ),
        Some(Syscall::Read),
        "read is 0 on x86-64"
    );
    assert_eq!(
        from_x86_64(x86_64::OPEN),
        Some(Syscall::Open),
        "open is 2 on x86-64"
    );
    assert_eq!(
        from_x86_64(x86_64::MMAP),
        Some(Syscall::Mmap),
        "mmap is 9 on x86-64"
    );
    assert_eq!(
        from_x86_64(x86_64::CLONE3),
        Some(Syscall::Clone3),
        "clone3 is 435"
    );
}

#[test]
fn aarch64_round_trips() {
    assert_eq!(
        from_aarch64(aarch64::WRITE),
        Some(Syscall::Write),
        "write is 64 on AArch64"
    );
    assert_eq!(
        from_aarch64(aarch64::READ),
        Some(Syscall::Read),
        "read is 63 on AArch64"
    );
    assert_eq!(
        from_aarch64(aarch64::OPENAT),
        Some(Syscall::Openat),
        "openat is 56 on AArch64"
    );
    assert_eq!(
        from_aarch64(aarch64::MMAP),
        Some(Syscall::Mmap),
        "mmap is 222 on AArch64"
    );
    assert_eq!(
        from_aarch64(aarch64::CLONE3),
        Some(Syscall::Clone3),
        "clone3 is 435 on both"
    );
}

#[test]
fn shared_calls_agree_across_architectures() {
    for &(x86, arm, call) in SHARED {
        assert_eq!(
            from_x86_64(x86),
            Some(call),
            "x86-64 number {x86} should be {call:?}"
        );
        assert_eq!(
            from_aarch64(arm),
            Some(call),
            "AArch64 number {arm} should be {call:?}"
        );
    }
}

#[test]
fn shared_table_mostly_uses_different_numbers() {
    let differing = SHARED.iter().filter(|&&(x86, arm, _)| x86 != arm).count();
    assert!(
        differing > SHARED.len() / 2,
        "the two tables genuinely disagree, so a single dispatch table would be wrong"
    );
}

#[test]
fn aarch64_has_no_path_only_calls() {
    for &(nr, name) in &[
        (x86_64::OPEN, "open"),
        (x86_64::FORK, "fork"),
        (x86_64::DUP2, "dup2"),
        (x86_64::STAT, "stat"),
        (x86_64::LSTAT, "lstat"),
        (x86_64::POLL, "poll"),
        (x86_64::PIPE, "pipe"),
        (x86_64::ACCESS, "access"),
        (x86_64::ARCH_PRCTL, "arch_prctl"),
    ] {
        let mapped_to = from_aarch64(nr);
        assert!(
            mapped_to != Some(Syscall::Open) && mapped_to != Some(Syscall::Fork),
            "AArch64 must not resolve {name}'s x86-64 number to the same call"
        );
    }
    assert_eq!(
        from_aarch64(2),
        None,
        "generic syscall 2 is io_destroy, which this crate does not carry -- and it \n         is certainly not `open`, which the generic table has no number for at all"
    );
}

#[test]
fn x86_64_only_calls_are_the_expected_ones() {
    let only_on_x86: HashSet<Syscall> = mapped(from_x86_64)
        .difference(&mapped(from_aarch64))
        .copied()
        .collect();
    let expected: HashSet<Syscall> = X86_64_ONLY.iter().copied().collect();
    let mut missing: Vec<&Syscall> = expected.difference(&only_on_x86).collect();
    missing.sort_unstable();
    let mut extra: Vec<&Syscall> = only_on_x86.difference(&expected).collect();
    extra.sort_unstable();
    assert!(
        missing.is_empty(),
        "expected to be x86-64 only but AArch64 maps them: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "x86-64 only, but not listed as such: {extra:?}"
    );
}

#[test]
fn aarch64_maps_no_call_x86_64_lacks() {
    let only_on_arm: HashSet<Syscall> = mapped(from_aarch64)
        .difference(&mapped(from_x86_64))
        .copied()
        .collect();
    assert!(
        only_on_arm.is_empty(),
        "every AArch64 call also has an x86-64 number: {only_on_arm:?}"
    );
}

#[test]
fn no_two_numbers_map_to_the_same_call() {
    let mut seen = HashSet::new();
    for nr in 0..=600 {
        if let Some(call) = from_x86_64(nr) {
            assert!(seen.insert(call), "two x86-64 numbers both map to {call:?}");
        }
    }
    let mut seen = HashSet::new();
    for nr in 0..=600 {
        if let Some(call) = from_aarch64(nr) {
            assert!(
                seen.insert(call),
                "two AArch64 numbers both map to {call:?}"
            );
        }
    }
}

#[test]
fn unknown_numbers_map_to_none_without_panicking() {
    for nr in [
        23,
        31,
        36,
        44,
        51,
        55,
        65,
        71,
        73,
        78,
        85,
        94,
        101,
        111,
        117,
        119,
        122,
        300,
        400,
        434,
        436,
        438,
        440,
        500,
        1000,
        4096,
        usize::MAX / 2,
        usize::MAX - 1,
        usize::MAX,
    ] {
        assert_eq!(
            from_x86_64(nr),
            None,
            "x86-64 number {nr} is not one this crate knows"
        );
    }
    for nr in [
        1,
        5,
        10,
        16,
        18,
        26,
        30,
        42,
        47,
        58,
        60,
        70,
        90,
        110,
        140,
        190,
        250,
        300,
        400,
        434,
        436,
        438,
        440,
        1000,
        usize::MAX / 2,
        usize::MAX - 1,
        usize::MAX,
    ] {
        assert_eq!(
            from_aarch64(nr),
            None,
            "AArch64 number {nr} is not one this crate knows"
        );
    }
}

#[test]
fn the_whole_number_space_is_total() {
    for nr in 0..=1200 {
        let _x86 = from_x86_64(nr);
        let _aarch64 = from_aarch64(nr);
        let _arm = from_arm(nr);
    }
    assert_eq!(
        from_x86_64(usize::MAX),
        None,
        "the largest number is answered, not trapped"
    );
    assert_eq!(
        from_aarch64(usize::MAX),
        None,
        "the largest number is answered, not trapped"
    );
    assert_eq!(
        from_arm(usize::MAX),
        None,
        "the largest number is answered, not trapped"
    );
}

#[test]
fn both_tables_cover_the_calls_musl_startup_makes() {
    let x86 = mapped(from_x86_64);
    let arm = mapped(from_aarch64);
    for call in [
        Syscall::Brk,
        Syscall::Mmap,
        Syscall::Mprotect,
        Syscall::SetTidAddress,
        Syscall::SetRobustList,
        Syscall::RtSigprocmask,
        Syscall::Readlinkat,
        Syscall::Writev,
        Syscall::ExitGroup,
        Syscall::Futex,
        Syscall::ClockGettime,
        Syscall::Getrandom,
    ] {
        assert!(x86.contains(&call), "x86-64 must dispatch {call:?}");
        assert!(arm.contains(&call), "AArch64 must dispatch {call:?}");
    }
}

#[test]
fn table_sizes_are_stable() {
    // A canary, not a specification. The numbers are whatever the tables
    // currently hold; the point is that adding or losing a call is a visible
    // change to this line rather than something nobody notices. It caught
    // `socket` being unreachable on AArch64.
    assert_eq!(
        mapped(from_x86_64).len(),
        136,
        "the x86-64 table maps 136 calls"
    );
    assert_eq!(
        mapped(from_aarch64).len(),
        116,
        "the AArch64 table maps 116 calls"
    );
}
/// Calls only ARMv7-A has, because it is the only 32-bit target.
///
/// Every one of them exists because a 32-bit register cannot carry what the
/// call has to pass, or because ARM userspace cannot reach a register the
/// other two write for themselves. None is a synonym: each takes arguments
/// its 64-bit namesake does not.
const ARM_ONLY: &[Syscall] = &[
    Syscall::Stat64,
    Syscall::Lstat64,
    Syscall::Fstat64,
    Syscall::Fstatat64,
    Syscall::Statfs64,
    Syscall::Fstatfs64,
    Syscall::Truncate64,
    Syscall::Ftruncate64,
    Syscall::Sendfile64,
    Syscall::Fcntl64,
    Syscall::Llseek,
    Syscall::Mmap2,
    Syscall::ClockGettime64,
    Syscall::ClockNanosleepTime64,
    Syscall::PpollTime64,
    Syscall::FutexTime64,
    Syscall::ArmSetTls,
    Syscall::ArmCacheflush,
];

/// Every ARMv7-A number this crate knows, paired with the call it means.
///
/// The same role `SHARED` plays for the other two tables, but stronger: these
/// are literal numbers rather than references to `nr::arm`, so a mistyped
/// constant fails this test rather than silently dispatching a program's
/// `write` into, say, `unlink`.
const ARM_NUMBERS: &[(usize, Syscall)] = &[
    // Generated against `arch/arm/include/uapi/asm/unistd-common.h`; the
    // number is written out rather than taken from `nr::arm` so that a
    // wrong constant fails here instead of dispatching a call to the
    // wrong handler.
    (1, Syscall::Exit),                   // exit
    (2, Syscall::Fork),                   // fork
    (3, Syscall::Read),                   // read
    (4, Syscall::Write),                  // write
    (5, Syscall::Open),                   // open
    (6, Syscall::Close),                  // close
    (9, Syscall::Link),                   // link
    (10, Syscall::Unlink),                // unlink
    (11, Syscall::Execve),                // execve
    (12, Syscall::Chdir),                 // chdir
    (15, Syscall::Chmod),                 // chmod
    (19, Syscall::Lseek),                 // lseek
    (20, Syscall::Getpid),                // getpid
    (21, Syscall::Mount),                 // mount
    (33, Syscall::Access),                // access
    (36, Syscall::Sync),                  // sync
    (37, Syscall::Kill),                  // kill
    (38, Syscall::Rename),                // rename
    (39, Syscall::Mkdir),                 // mkdir
    (40, Syscall::Rmdir),                 // rmdir
    (41, Syscall::Dup),                   // dup
    (42, Syscall::Pipe),                  // pipe
    (45, Syscall::Brk),                   // brk
    (52, Syscall::Umount2),               // umount2
    (54, Syscall::Ioctl),                 // ioctl
    (55, Syscall::Fcntl),                 // fcntl
    (57, Syscall::Setpgid),               // setpgid
    (60, Syscall::Umask),                 // umask
    (63, Syscall::Dup2),                  // dup2
    (64, Syscall::Getppid),               // getppid
    (75, Syscall::Setrlimit),             // setrlimit
    (77, Syscall::Getrusage),             // getrusage
    (78, Syscall::Gettimeofday),          // gettimeofday
    (83, Syscall::Symlink),               // symlink
    (85, Syscall::Readlink),              // readlink
    (91, Syscall::Munmap),                // munmap
    (92, Syscall::Truncate),              // truncate
    (93, Syscall::Ftruncate),             // ftruncate
    (94, Syscall::Fchmod),                // fchmod
    (114, Syscall::Wait4),                // wait4
    (116, Syscall::Sysinfo),              // sysinfo
    (118, Syscall::Fsync),                // fsync
    (120, Syscall::Clone),                // clone
    (122, Syscall::Uname),                // uname
    (125, Syscall::Mprotect),             // mprotect
    (132, Syscall::Getpgid),              // getpgid
    (133, Syscall::Fchdir),               // fchdir
    (140, Syscall::Llseek),               // _llseek
    (144, Syscall::Msync),                // msync
    (145, Syscall::Readv),                // readv
    (146, Syscall::Writev),               // writev
    (148, Syscall::Fdatasync),            // fdatasync
    (155, Syscall::SchedGetparam),        // sched_getparam
    (156, Syscall::SchedSetscheduler),    // sched_setscheduler
    (157, Syscall::SchedGetscheduler),    // sched_getscheduler
    (158, Syscall::SchedYield),           // sched_yield
    (162, Syscall::Nanosleep),            // nanosleep
    (163, Syscall::Mremap),               // mremap
    (168, Syscall::Poll),                 // poll
    (172, Syscall::Prctl),                // prctl
    (173, Syscall::RtSigreturn),          // rt_sigreturn
    (174, Syscall::RtSigaction),          // rt_sigaction
    (175, Syscall::RtSigprocmask),        // rt_sigprocmask
    (179, Syscall::RtSigsuspend),         // rt_sigsuspend
    (180, Syscall::Pread64),              // pread64
    (181, Syscall::Pwrite64),             // pwrite64
    (183, Syscall::Getcwd),               // getcwd
    (186, Syscall::Sigaltstack),          // sigaltstack
    (190, Syscall::Vfork),                // vfork
    (191, Syscall::Getrlimit),            // ugetrlimit
    (192, Syscall::Mmap2),                // mmap2
    (193, Syscall::Truncate64),           // truncate64
    (194, Syscall::Ftruncate64),          // ftruncate64
    (195, Syscall::Stat64),               // stat64
    (196, Syscall::Lstat64),              // lstat64
    (197, Syscall::Fstat64),              // fstat64
    (199, Syscall::Getuid),               // getuid32
    (200, Syscall::Getgid),               // getgid32
    (201, Syscall::Geteuid),              // geteuid32
    (202, Syscall::Getegid),              // getegid32
    (205, Syscall::Getgroups),            // getgroups32
    (207, Syscall::Fchown),               // fchown32
    (209, Syscall::Getresuid),            // getresuid32
    (211, Syscall::Getresgid),            // getresgid32
    (212, Syscall::Chown),                // chown32
    (213, Syscall::Setuid),               // setuid32
    (214, Syscall::Setgid),               // setgid32
    (217, Syscall::Getdents64),           // getdents64
    (220, Syscall::Madvise),              // madvise
    (221, Syscall::Fcntl64),              // fcntl64
    (224, Syscall::Gettid),               // gettid
    (238, Syscall::Tkill),                // tkill
    (239, Syscall::Sendfile64),           // sendfile64
    (240, Syscall::Futex),                // futex
    (241, Syscall::SchedSetaffinity),     // sched_setaffinity
    (242, Syscall::SchedGetaffinity),     // sched_getaffinity
    (248, Syscall::ExitGroup),            // exit_group
    (251, Syscall::EpollCtl),             // epoll_ctl
    (252, Syscall::EpollWait),            // epoll_wait
    (256, Syscall::SetTidAddress),        // set_tid_address
    (263, Syscall::ClockGettime),         // clock_gettime
    (265, Syscall::ClockNanosleep),       // clock_nanosleep
    (266, Syscall::Statfs64),             // statfs64
    (267, Syscall::Fstatfs64),            // fstatfs64
    (268, Syscall::Tgkill),               // tgkill
    (280, Syscall::Waitid),               // waitid
    (281, Syscall::Socket),               // socket
    (283, Syscall::Connect),              // connect
    (322, Syscall::Openat),               // openat
    (323, Syscall::Mkdirat),              // mkdirat
    (325, Syscall::Fchownat),             // fchownat
    (327, Syscall::Fstatat64),            // fstatat64
    (328, Syscall::Unlinkat),             // unlinkat
    (329, Syscall::Renameat),             // renameat
    (330, Syscall::Linkat),               // linkat
    (331, Syscall::Symlinkat),            // symlinkat
    (332, Syscall::Readlinkat),           // readlinkat
    (333, Syscall::Fchmodat),             // fchmodat
    (334, Syscall::Faccessat),            // faccessat
    (336, Syscall::Ppoll),                // ppoll
    (337, Syscall::Unshare),              // unshare
    (338, Syscall::SetRobustList),        // set_robust_list
    (339, Syscall::GetRobustList),        // get_robust_list
    (346, Syscall::EpollPwait),           // epoll_pwait
    (356, Syscall::Eventfd2),             // eventfd2
    (357, Syscall::EpollCreate1),         // epoll_create1
    (358, Syscall::Dup3),                 // dup3
    (359, Syscall::Pipe2),                // pipe2
    (369, Syscall::Prlimit64),            // prlimit64
    (382, Syscall::Renameat2),            // renameat2
    (384, Syscall::Getrandom),            // getrandom
    (385, Syscall::MemfdCreate),          // memfd_create
    (387, Syscall::Execveat),             // execveat
    (389, Syscall::Membarrier),           // membarrier
    (397, Syscall::Statx),                // statx
    (398, Syscall::Rseq),                 // rseq
    (403, Syscall::ClockGettime64),       // clock_gettime64
    (407, Syscall::ClockNanosleepTime64), // clock_nanosleep_time64
    (414, Syscall::PpollTime64),          // ppoll_time64
    (422, Syscall::FutexTime64),          // futex_time64
    (435, Syscall::Clone3),               // clone3
    (437, Syscall::Openat2),              // openat2
    (439, Syscall::Faccessat2),           // faccessat2
];

// ---------------------------------------------------------------------------
// The ARMv7-A (EABI) table
// ---------------------------------------------------------------------------

#[test]
fn arm_numbers_all_resolve_to_the_call_they_name() {
    for &(nr, call) in ARM_NUMBERS {
        assert_eq!(
            from_arm(nr),
            Some(call),
            "ARMv7-A number {nr} must dispatch {call:?}"
        );
    }
}

#[test]
fn arm_round_trips() {
    assert_eq!(
        from_arm(arm::READ),
        Some(Syscall::Read),
        "read is 3 on EABI"
    );
    assert_eq!(
        from_arm(arm::WRITE),
        Some(Syscall::Write),
        "write is 4 on EABI"
    );
    assert_eq!(
        from_arm(arm::EXIT_GROUP),
        Some(Syscall::ExitGroup),
        "exit_group is 248 on EABI"
    );
    assert_eq!(
        from_arm(arm::CLONE3),
        Some(Syscall::Clone3),
        "numbers assigned after the generic table froze match everywhere"
    );
}

#[test]
fn arm_numbers_are_the_eabi_ones_not_the_oabi_ones() {
    // OABI based every number at 0x900000. If a constant had been taken from
    // that table the whole file would be shifted, and `read` is the cheapest
    // place to notice.
    assert_eq!(arm::READ, 3, "EABI's __NR_SYSCALL_BASE is zero");
    assert_eq!(
        from_arm(0x0090_0003),
        None,
        "an OABI-based number is not a number this kernel answers"
    );
}

#[test]
fn arm_uses_the_wide_forms_a_32_bit_musl_actually_calls() {
    // Each of these is the pair that a naive "ARM is x86-64 with different
    // numbers" table would get wrong, and each would be wrong silently.
    assert_eq!(
        from_arm(arm::MMAP2),
        Some(Syscall::Mmap2),
        "mmap2 is a different call from mmap: its offset counts pages"
    );
    assert_eq!(
        from_arm(arm::FSTAT64),
        Some(Syscall::Fstat64),
        "fstat64 writes struct stat64, which is not struct stat"
    );
    assert_eq!(
        from_arm(arm::LLSEEK),
        Some(Syscall::Llseek),
        "_llseek returns its offset through a pointer"
    );
    assert_eq!(
        from_arm(arm::CLOCK_GETTIME64),
        Some(Syscall::ClockGettime64),
        "musl has been time64 since 1.2"
    );
    assert_eq!(
        from_arm(arm::FUTEX_TIME64),
        Some(Syscall::FutexTime64),
        "a time64 musl's locks wait on futex_time64"
    );
}

#[test]
fn arm_16_bit_credential_calls_are_absent() {
    // Deliberate: musl issues only the `32` forms, so an arrival on one of the
    // pre-2.4 numbers is more likely a mistake than a request.
    for (nr, name) in [
        (24, "getuid"),
        (47, "getgid"),
        (49, "geteuid"),
        (23, "setuid"),
    ] {
        assert_eq!(
            from_arm(nr),
            None,
            "the 16-bit {name} is not carried, and answers ENOSYS"
        );
    }
    assert_eq!(
        from_arm(arm::GETUID32),
        Some(Syscall::Getuid),
        "the 32-bit form is the one that dispatches"
    );
}

#[test]
fn arm_private_calls_sit_far_above_the_shared_table() {
    assert_eq!(
        arm::ARM_PRIVATE_BASE,
        0x000f_0000,
        "__ARM_NR_BASE, with EABI's zero syscall base"
    );
    assert_eq!(from_arm(arm::ARM_SET_TLS), Some(Syscall::ArmSetTls));
    assert_eq!(from_arm(arm::ARM_CACHEFLUSH), Some(Syscall::ArmCacheflush));
    // The gap is the whole point: no number the shared table will ever grow
    // into can collide with one of these.
    let highest_shared = ARM_NUMBERS
        .iter()
        .map(|&(nr, _)| nr)
        .max()
        .expect("the table is not empty");
    assert!(
        highest_shared < arm::ARM_PRIVATE_BASE,
        "the private range must stay above every shared number"
    );
}

#[test]
fn arm_only_calls_are_the_expected_ones() {
    let sixty_four_bit: HashSet<Syscall> = mapped(from_x86_64)
        .union(&mapped(from_aarch64))
        .copied()
        .collect();
    let only_on_arm: HashSet<Syscall> = mapped_arm().difference(&sixty_four_bit).copied().collect();
    let expected: HashSet<Syscall> = ARM_ONLY.iter().copied().collect();
    let mut missing: Vec<&Syscall> = expected.difference(&only_on_arm).collect();
    missing.sort_unstable();
    let mut extra: Vec<&Syscall> = only_on_arm.difference(&expected).collect();
    extra.sort_unstable();
    assert!(
        missing.is_empty(),
        "expected to be ARMv7-A only, but a 64-bit table maps them: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "ARMv7-A only, but not listed as such: {extra:?}"
    );
}

#[test]
fn arm_no_two_numbers_map_to_the_same_call() {
    let mut seen = HashSet::new();
    for nr in arm_number_space() {
        if let Some(call) = from_arm(nr) {
            assert!(
                seen.insert(call),
                "two ARMv7-A numbers both map to {call:?}"
            );
        }
    }
}

#[test]
fn arm_covers_the_calls_musl_startup_makes() {
    // The 32-bit spellings of `both_tables_cover_the_calls_musl_startup_makes`.
    // Three entries differ from the 64-bit list, and those three are exactly
    // the reason this test is written out separately rather than folded in.
    let arm_calls = mapped_arm();
    for call in [
        Syscall::Brk,
        Syscall::Mmap2,
        Syscall::Mprotect,
        Syscall::SetTidAddress,
        Syscall::SetRobustList,
        Syscall::RtSigprocmask,
        Syscall::Readlinkat,
        Syscall::Writev,
        Syscall::ExitGroup,
        Syscall::FutexTime64,
        Syscall::ClockGettime64,
        Syscall::Getrandom,
        Syscall::ArmSetTls,
    ] {
        assert!(arm_calls.contains(&call), "ARMv7-A must dispatch {call:?}");
    }
}

#[test]
fn arm_table_size_is_stable() {
    // A canary, as for the other two tables.
    assert_eq!(mapped_arm().len(), 145, "the ARMv7-A table maps 145 calls");
}
