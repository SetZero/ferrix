//! System call numbers, and the translation from a number to a call.
//!
//! There is no single Linux system call table. x86-64 kept the numbering it
//! grew (`arch/x86/entry/syscalls/syscall_64.tbl`), while every architecture
//! added after 2011, AArch64 included, uses the generic table in
//! `include/uapi/asm-generic/unistd.h`. The same call has a different number on
//! each, some calls exist on only one, and a binary compiled for one
//! architecture never sees the other's numbers.
//!
//! The kernel should not care. [`from_x86_64`] and [`from_aarch64`] map each
//! architecture's raw number onto one [`Syscall`], and the dispatcher matches
//! on that. An unknown number maps to [`None`], which the caller answers with
//! [`crate::errno::Errno::ENOSYS`] — the same thing Linux does.
//!
//! # What is here
//!
//! Enough of the table for a static-musl program to start and run: process and
//! thread creation, memory, files, signals, futexes, time and the handful of
//! socket calls musl uses. Calls whose numbers were not verified against the
//! kernel source are absent rather than guessed, because a wrong number is a
//! call silently dispatched to the wrong handler, while a missing one is an
//! `ENOSYS` that names itself.

/// System call numbers for x86-64.
///
/// From `arch/x86/entry/syscalls/syscall_64.tbl`, the 64-bit ABI rather than
/// the x32 one.
pub mod x86_64 {
    /// Read bytes from a file descriptor.
    pub const READ: usize = 0;
    /// Write bytes to a file descriptor.
    pub const WRITE: usize = 1;
    /// Open a file by path. AArch64 has no equivalent; musl uses `openat`.
    pub const OPEN: usize = 2;
    /// Close a file descriptor.
    pub const CLOSE: usize = 3;
    /// Stat a file by path, following symlinks.
    pub const STAT: usize = 4;
    /// Stat an open file descriptor.
    pub const FSTAT: usize = 5;
    /// Stat a file by path without following a final symlink.
    pub const LSTAT: usize = 6;
    /// Wait for events on a set of file descriptors.
    pub const POLL: usize = 7;
    /// Reposition a file descriptor's offset.
    pub const LSEEK: usize = 8;
    /// Map files or anonymous memory into the address space.
    pub const MMAP: usize = 9;
    /// Change the protection of a mapping.
    pub const MPROTECT: usize = 10;
    /// Remove a mapping.
    pub const MUNMAP: usize = 11;
    /// Move the program break, the classic heap boundary.
    pub const BRK: usize = 12;
    /// Install a signal handler.
    pub const RT_SIGACTION: usize = 13;
    /// Change the blocked signal mask.
    pub const RT_SIGPROCMASK: usize = 14;
    /// Return from a signal handler, restoring the interrupted context.
    pub const RT_SIGRETURN: usize = 15;
    /// Device-specific control operation on a file descriptor.
    pub const IOCTL: usize = 16;
    /// Read at an explicit offset, leaving the file position alone.
    pub const PREAD64: usize = 17;
    /// Write at an explicit offset, leaving the file position alone.
    pub const PWRITE64: usize = 18;
    /// Read into several buffers in one call.
    pub const READV: usize = 19;
    /// Write from several buffers in one call.
    pub const WRITEV: usize = 20;
    /// Check a path's accessibility for the real user.
    pub const ACCESS: usize = 21;
    /// Create a pipe, returning two file descriptors.
    pub const PIPE: usize = 22;
    /// Yield the processor to another runnable thread.
    pub const SCHED_YIELD: usize = 24;
    /// Resize, and possibly move, an existing mapping.
    pub const MREMAP: usize = 25;
    /// Flush a file-backed mapping to its file.
    pub const MSYNC: usize = 26;
    /// Advise the kernel about future use of a memory range.
    pub const MADVISE: usize = 28;
    /// Duplicate a file descriptor onto the lowest free number.
    pub const DUP: usize = 32;
    /// Duplicate a file descriptor onto a chosen number.
    pub const DUP2: usize = 33;
    /// Sleep for a duration, resumable after a signal.
    pub const NANOSLEEP: usize = 35;
    /// Return the calling process's identifier.
    pub const GETPID: usize = 39;
    /// Copy data between two file descriptors inside the kernel.
    pub const SENDFILE: usize = 40;
    /// Create a socket.
    pub const SOCKET: usize = 41;
    /// Connect a socket to an address.
    pub const CONNECT: usize = 42;
    /// Create a process or thread; the primitive behind `fork` and `pthread`.
    pub const CLONE: usize = 56;
    /// Create a child process sharing nothing.
    pub const FORK: usize = 57;
    /// Create a child sharing the address space, suspending the parent.
    pub const VFORK: usize = 58;
    /// Replace the current process image with a program.
    pub const EXECVE: usize = 59;
    /// Terminate the calling thread.
    pub const EXIT: usize = 60;
    /// Wait for a child to change state, reporting resource usage.
    pub const WAIT4: usize = 61;
    /// Send a signal to a process or process group.
    pub const KILL: usize = 62;
    /// Report kernel name and version.
    pub const UNAME: usize = 63;
    /// Manipulate a file descriptor's flags and locks.
    pub const FCNTL: usize = 72;
    /// Flush a file's data and metadata to storage.
    pub const FSYNC: usize = 74;
    /// Flush a file's data, and only the metadata needed to read it back.
    pub const FDATASYNC: usize = 75;
    /// Set a file's length by path.
    pub const TRUNCATE: usize = 76;
    /// Set an open file's length.
    pub const FTRUNCATE: usize = 77;
    /// Read the current working directory into a buffer.
    pub const GETCWD: usize = 79;
    /// Change the working directory by path.
    pub const CHDIR: usize = 80;
    /// Change the working directory to an open directory.
    pub const FCHDIR: usize = 81;
    /// Rename a file.
    pub const RENAME: usize = 82;
    /// Create a directory.
    pub const MKDIR: usize = 83;
    /// Remove an empty directory.
    pub const RMDIR: usize = 84;
    /// Create a hard link.
    pub const LINK: usize = 86;
    /// Remove a directory entry.
    pub const UNLINK: usize = 87;
    /// Create a symbolic link.
    pub const SYMLINK: usize = 88;
    /// Read a symbolic link's target.
    pub const READLINK: usize = 89;
    /// Change a file's mode by path.
    pub const CHMOD: usize = 90;
    /// Change an open file's mode.
    pub const FCHMOD: usize = 91;
    /// Change a file's owner by path.
    pub const CHOWN: usize = 92;
    /// Change an open file's owner.
    pub const FCHOWN: usize = 93;
    /// Set the file mode creation mask.
    pub const UMASK: usize = 95;
    /// Read the wall clock, the obsolete predecessor of `clock_gettime`.
    pub const GETTIMEOFDAY: usize = 96;
    /// Read a resource limit; superseded by `prlimit64`.
    pub const GETRLIMIT: usize = 97;
    /// Report accumulated resource usage.
    pub const GETRUSAGE: usize = 98;
    /// Report system-wide memory and load statistics.
    pub const SYSINFO: usize = 99;
    /// Return the real user identifier.
    pub const GETUID: usize = 102;
    /// Return the real group identifier.
    pub const GETGID: usize = 104;
    /// Set the user identifier.
    pub const SETUID: usize = 105;
    /// Set the group identifier.
    pub const SETGID: usize = 106;
    /// Return the effective user identifier.
    pub const GETEUID: usize = 107;
    /// Return the effective group identifier.
    pub const GETEGID: usize = 108;
    /// Set a process's process-group identifier.
    pub const SETPGID: usize = 109;
    /// Return the parent process's identifier.
    pub const GETPPID: usize = 110;
    /// Read the supplementary group list.
    pub const GETGROUPS: usize = 115;
    /// Read the real, effective and saved user identifiers.
    pub const GETRESUID: usize = 118;
    /// Read the real, effective and saved group identifiers.
    pub const GETRESGID: usize = 120;
    /// Return a process's process-group identifier.
    pub const GETPGID: usize = 121;
    /// Replace the signal mask and wait for a signal.
    pub const RT_SIGSUSPEND: usize = 130;
    /// Install or query the alternate signal stack.
    pub const SIGALTSTACK: usize = 131;
    /// Report file system statistics by path.
    pub const STATFS: usize = 137;
    /// Report file system statistics for an open file.
    pub const FSTATFS: usize = 138;
    /// Read a thread's scheduling parameters.
    pub const SCHED_GETPARAM: usize = 143;
    /// Set a thread's scheduling policy and parameters.
    pub const SCHED_SETSCHEDULER: usize = 144;
    /// Read a thread's scheduling policy.
    pub const SCHED_GETSCHEDULER: usize = 145;
    /// Operate on per-process control settings, such as the thread name.
    pub const PRCTL: usize = 157;
    /// Read or set an architecture register, notably `FS_BASE` for TLS.
    ///
    /// x86-64 only: AArch64 writes its thread pointer to `TPIDR_EL0` directly,
    /// so the generic table never needed the call.
    pub const ARCH_PRCTL: usize = 158;
    /// Set a resource limit; superseded by `prlimit64`.
    pub const SETRLIMIT: usize = 160;
    /// Flush all file systems.
    pub const SYNC: usize = 162;
    /// Attach a file system.
    pub const MOUNT: usize = 165;
    /// Detach a file system.
    pub const UMOUNT2: usize = 166;
    /// Return the calling thread's identifier.
    pub const GETTID: usize = 186;
    /// Send a signal to a thread by thread identifier.
    pub const TKILL: usize = 200;
    /// Wait on, or wake, a futex; the primitive under every musl lock.
    pub const FUTEX: usize = 202;
    /// Set a thread's processor affinity mask.
    pub const SCHED_SETAFFINITY: usize = 203;
    /// Read a thread's processor affinity mask.
    pub const SCHED_GETAFFINITY: usize = 204;
    /// Read directory entries in the 64-bit layout.
    pub const GETDENTS64: usize = 217;
    /// Register the address cleared and woken on thread exit.
    pub const SET_TID_ADDRESS: usize = 218;
    /// Read a clock.
    pub const CLOCK_GETTIME: usize = 228;
    /// Sleep against a chosen clock, optionally until an absolute time.
    pub const CLOCK_NANOSLEEP: usize = 230;
    /// Terminate every thread in the process.
    pub const EXIT_GROUP: usize = 231;
    /// Wait for events on an epoll set.
    pub const EPOLL_WAIT: usize = 232;
    /// Add, modify or remove a file descriptor in an epoll set.
    pub const EPOLL_CTL: usize = 233;
    /// Send a signal to a thread, checked against its thread group.
    pub const TGKILL: usize = 234;
    /// Wait for a child to change state, without necessarily reaping it.
    pub const WAITID: usize = 247;
    /// Open a file relative to a directory file descriptor.
    pub const OPENAT: usize = 257;
    /// Create a directory relative to a directory file descriptor.
    pub const MKDIRAT: usize = 258;
    /// Change a file's owner relative to a directory file descriptor.
    pub const FCHOWNAT: usize = 260;
    /// Stat a file relative to a directory file descriptor.
    pub const NEWFSTATAT: usize = 262;
    /// Remove a directory entry relative to a directory file descriptor.
    pub const UNLINKAT: usize = 263;
    /// Rename relative to directory file descriptors.
    pub const RENAMEAT: usize = 264;
    /// Create a hard link relative to directory file descriptors.
    pub const LINKAT: usize = 265;
    /// Create a symbolic link relative to a directory file descriptor.
    pub const SYMLINKAT: usize = 266;
    /// Read a symbolic link relative to a directory file descriptor.
    pub const READLINKAT: usize = 267;
    /// Change a file's mode relative to a directory file descriptor.
    pub const FCHMODAT: usize = 268;
    /// Check accessibility relative to a directory file descriptor.
    pub const FACCESSAT: usize = 269;
    /// Poll with a signal mask and a `timespec` timeout.
    pub const PPOLL: usize = 271;
    /// Detach parts of the calling process's shared execution context.
    pub const UNSHARE: usize = 272;
    /// Register the robust futex list, walked when a thread dies holding a lock.
    pub const SET_ROBUST_LIST: usize = 273;
    /// Read a thread's robust futex list.
    pub const GET_ROBUST_LIST: usize = 274;
    /// Wait on an epoll set with a signal mask.
    pub const EPOLL_PWAIT: usize = 281;
    /// Create an eventfd with flags.
    pub const EVENTFD2: usize = 290;
    /// Create an epoll set with flags.
    pub const EPOLL_CREATE1: usize = 291;
    /// Duplicate a file descriptor onto a chosen number with flags.
    pub const DUP3: usize = 292;
    /// Create a pipe with flags.
    pub const PIPE2: usize = 293;
    /// Read and set a resource limit of any process in one call.
    pub const PRLIMIT64: usize = 302;
    /// Rename with flags, such as `RENAME_NOREPLACE`.
    pub const RENAMEAT2: usize = 316;
    /// Fill a buffer with random bytes.
    pub const GETRANDOM: usize = 318;
    /// Create an anonymous file living in memory.
    pub const MEMFD_CREATE: usize = 319;
    /// Execute a program named by a file descriptor.
    pub const EXECVEAT: usize = 322;
    /// Issue a process-wide memory barrier.
    pub const MEMBARRIER: usize = 324;
    /// Stat a file with an explicit field mask and 64-bit timestamps.
    pub const STATX: usize = 332;
    /// Register a restartable sequence area.
    pub const RSEQ: usize = 334;
    /// Create a process or thread from a versioned argument structure.
    pub const CLONE3: usize = 435;
    /// Open a file from a versioned argument structure.
    pub const OPENAT2: usize = 437;
    /// Check accessibility with flags, the form musl now prefers.
    pub const FACCESSAT2: usize = 439;
}

/// System call numbers for AArch64.
///
/// From `include/uapi/asm-generic/unistd.h`, which AArch64 uses unmodified.
/// The table deliberately omits the path-based calls that have an `*at`
/// equivalent, so there is no `open`, `stat`, `lstat`, `poll`, `pipe`, `dup2`,
/// `fork`, `vfork`, `access`, `rename`, `mkdir`, `rmdir`, `unlink`, `symlink`,
/// `readlink`, `chmod` or `chown` here — musl calls the `*at` forms. There is
/// no `arch_prctl` either, since the thread pointer is a writable register.
pub mod aarch64 {
    /// Read the current working directory into a buffer.
    pub const GETCWD: usize = 17;
    /// Create an eventfd with flags.
    pub const EVENTFD2: usize = 19;
    /// Create an epoll set with flags.
    pub const EPOLL_CREATE1: usize = 20;
    /// Add, modify or remove a file descriptor in an epoll set.
    pub const EPOLL_CTL: usize = 21;
    /// Wait on an epoll set with a signal mask.
    ///
    /// The generic table has no plain `epoll_wait`: musl passes a null mask.
    pub const EPOLL_PWAIT: usize = 22;
    /// Duplicate a file descriptor onto the lowest free number.
    pub const DUP: usize = 23;
    /// Duplicate a file descriptor onto a chosen number with flags.
    pub const DUP3: usize = 24;
    /// Manipulate a file descriptor's flags and locks.
    pub const FCNTL: usize = 25;
    /// Device-specific control operation on a file descriptor.
    pub const IOCTL: usize = 29;
    /// Create a directory relative to a directory file descriptor.
    pub const MKDIRAT: usize = 34;
    /// Remove a directory entry relative to a directory file descriptor.
    pub const UNLINKAT: usize = 35;
    /// Create a symbolic link relative to a directory file descriptor.
    pub const SYMLINKAT: usize = 36;
    /// Create a hard link relative to directory file descriptors.
    pub const LINKAT: usize = 37;
    /// Rename relative to directory file descriptors.
    pub const RENAMEAT: usize = 38;
    /// Rename a file, with flags such as `RENAME_NOREPLACE`.
    pub const RENAMEAT2: usize = 276;
    /// Detach a file system.
    pub const UMOUNT2: usize = 39;
    /// Attach a file system.
    pub const MOUNT: usize = 40;
    /// Report file system statistics by path.
    pub const STATFS: usize = 43;
    /// Report file system statistics for an open file.
    pub const FSTATFS: usize = 44;
    /// Set a file's length by path.
    pub const TRUNCATE: usize = 45;
    /// Set an open file's length.
    pub const FTRUNCATE: usize = 46;
    /// Check accessibility relative to a directory file descriptor.
    pub const FACCESSAT: usize = 48;
    /// Change the working directory by path.
    pub const CHDIR: usize = 49;
    /// Change the working directory to an open directory.
    pub const FCHDIR: usize = 50;
    /// Change an open file's mode.
    pub const FCHMOD: usize = 52;
    /// Change a file's mode relative to a directory file descriptor.
    pub const FCHMODAT: usize = 53;
    /// Change a file's owner relative to a directory file descriptor.
    pub const FCHOWNAT: usize = 54;
    /// Change an open file's owner.
    pub const FCHOWN: usize = 55;
    /// Open a file relative to a directory file descriptor.
    pub const OPENAT: usize = 56;
    /// Close a file descriptor.
    pub const CLOSE: usize = 57;
    /// Create a pipe with flags.
    pub const PIPE2: usize = 59;
    /// Read directory entries in the 64-bit layout.
    pub const GETDENTS64: usize = 61;
    /// Reposition a file descriptor's offset.
    pub const LSEEK: usize = 62;
    /// Read bytes from a file descriptor.
    pub const READ: usize = 63;
    /// Write bytes to a file descriptor.
    pub const WRITE: usize = 64;
    /// Read into several buffers in one call.
    pub const READV: usize = 65;
    /// Write from several buffers in one call.
    pub const WRITEV: usize = 66;
    /// Read at an explicit offset, leaving the file position alone.
    pub const PREAD64: usize = 67;
    /// Write at an explicit offset, leaving the file position alone.
    pub const PWRITE64: usize = 68;
    /// Copy data between two file descriptors inside the kernel.
    pub const SENDFILE: usize = 71;
    /// Poll with a signal mask and a `timespec` timeout.
    pub const PPOLL: usize = 73;
    /// Read a symbolic link relative to a directory file descriptor.
    pub const READLINKAT: usize = 78;
    /// Stat a file relative to a directory file descriptor.
    pub const NEWFSTATAT: usize = 79;
    /// Stat an open file descriptor.
    pub const FSTAT: usize = 80;
    /// Flush all file systems.
    pub const SYNC: usize = 81;
    /// Flush a file's data and metadata to storage.
    pub const FSYNC: usize = 82;
    /// Flush a file's data, and only the metadata needed to read it back.
    pub const FDATASYNC: usize = 83;
    /// Terminate the calling thread.
    pub const EXIT: usize = 93;
    /// Terminate every thread in the process.
    pub const EXIT_GROUP: usize = 94;
    /// Wait for a child to change state, without necessarily reaping it.
    pub const WAITID: usize = 95;
    /// Register the address cleared and woken on thread exit.
    pub const SET_TID_ADDRESS: usize = 96;
    /// Detach parts of the calling process's shared execution context.
    pub const UNSHARE: usize = 97;
    /// Wait on, or wake, a futex; the primitive under every musl lock.
    pub const FUTEX: usize = 98;
    /// Register the robust futex list, walked when a thread dies holding a lock.
    pub const SET_ROBUST_LIST: usize = 99;
    /// Read a thread's robust futex list.
    pub const GET_ROBUST_LIST: usize = 100;
    /// Sleep for a duration, resumable after a signal.
    pub const NANOSLEEP: usize = 101;
    /// Read a clock.
    pub const CLOCK_GETTIME: usize = 113;
    /// Sleep against a chosen clock, optionally until an absolute time.
    pub const CLOCK_NANOSLEEP: usize = 115;
    /// Set a thread's scheduling policy and parameters.
    pub const SCHED_SETSCHEDULER: usize = 119;
    /// Read a thread's scheduling policy.
    pub const SCHED_GETSCHEDULER: usize = 120;
    /// Read a thread's scheduling parameters.
    pub const SCHED_GETPARAM: usize = 121;
    /// Set a thread's processor affinity mask.
    pub const SCHED_SETAFFINITY: usize = 122;
    /// Read a thread's processor affinity mask.
    pub const SCHED_GETAFFINITY: usize = 123;
    /// Yield the processor to another runnable thread.
    pub const SCHED_YIELD: usize = 124;
    /// Send a signal to a process or process group.
    pub const KILL: usize = 129;
    /// Send a signal to a thread by thread identifier.
    pub const TKILL: usize = 130;
    /// Send a signal to a thread, checked against its thread group.
    pub const TGKILL: usize = 131;
    /// Install or query the alternate signal stack.
    pub const SIGALTSTACK: usize = 132;
    /// Replace the signal mask and wait for a signal.
    pub const RT_SIGSUSPEND: usize = 133;
    /// Install a signal handler.
    pub const RT_SIGACTION: usize = 134;
    /// Change the blocked signal mask.
    pub const RT_SIGPROCMASK: usize = 135;
    /// Return from a signal handler, restoring the interrupted context.
    pub const RT_SIGRETURN: usize = 139;
    /// Set the group identifier.
    pub const SETGID: usize = 144;
    /// Set the user identifier.
    pub const SETUID: usize = 146;
    /// Read the real, effective and saved user identifiers.
    pub const GETRESUID: usize = 148;
    /// Read the real, effective and saved group identifiers.
    pub const GETRESGID: usize = 150;
    /// Set a process's process-group identifier.
    pub const SETPGID: usize = 154;
    /// Return a process's process-group identifier.
    pub const GETPGID: usize = 155;
    /// Read the supplementary group list.
    pub const GETGROUPS: usize = 158;
    /// Report kernel name and version.
    pub const UNAME: usize = 160;
    /// Read a resource limit; superseded by `prlimit64`.
    pub const GETRLIMIT: usize = 163;
    /// Set a resource limit; superseded by `prlimit64`.
    pub const SETRLIMIT: usize = 164;
    /// Report accumulated resource usage.
    pub const GETRUSAGE: usize = 165;
    /// Set the file mode creation mask.
    pub const UMASK: usize = 166;
    /// Operate on per-process control settings, such as the thread name.
    pub const PRCTL: usize = 167;
    /// Read the wall clock, the obsolete predecessor of `clock_gettime`.
    pub const GETTIMEOFDAY: usize = 169;
    /// Return the calling process's identifier.
    pub const GETPID: usize = 172;
    /// Return the parent process's identifier.
    pub const GETPPID: usize = 173;
    /// Return the real user identifier.
    pub const GETUID: usize = 174;
    /// Return the effective user identifier.
    pub const GETEUID: usize = 175;
    /// Return the real group identifier.
    pub const GETGID: usize = 176;
    /// Return the effective group identifier.
    pub const GETEGID: usize = 177;
    /// Return the calling thread's identifier.
    pub const GETTID: usize = 178;
    /// Report system-wide memory and load statistics.
    pub const SYSINFO: usize = 179;
    /// Create a socket.
    pub const SOCKET: usize = 198;
    /// Connect a socket to an address.
    pub const CONNECT: usize = 203;
    /// Move the program break, the classic heap boundary.
    pub const BRK: usize = 214;
    /// Remove a mapping.
    pub const MUNMAP: usize = 215;
    /// Resize, and possibly move, an existing mapping.
    pub const MREMAP: usize = 216;
    /// Create a process or thread; the primitive behind `fork` and `pthread`.
    pub const CLONE: usize = 220;
    /// Replace the current process image with a program.
    pub const EXECVE: usize = 221;
    /// Map files or anonymous memory into the address space.
    pub const MMAP: usize = 222;
    /// Change the protection of a mapping.
    pub const MPROTECT: usize = 226;
    /// Flush a file-backed mapping to its file.
    pub const MSYNC: usize = 227;
    /// Advise the kernel about future use of a memory range.
    pub const MADVISE: usize = 233;
    /// Wait for a child to change state, reporting resource usage.
    pub const WAIT4: usize = 260;
    /// Read and set a resource limit of any process in one call.
    pub const PRLIMIT64: usize = 261;
    /// Fill a buffer with random bytes.
    pub const GETRANDOM: usize = 278;
    /// Create an anonymous file living in memory.
    pub const MEMFD_CREATE: usize = 279;
    /// Execute a program named by a file descriptor.
    pub const EXECVEAT: usize = 281;
    /// Issue a process-wide memory barrier.
    pub const MEMBARRIER: usize = 283;
    /// Stat a file with an explicit field mask and 64-bit timestamps.
    pub const STATX: usize = 291;
    /// Register a restartable sequence area.
    pub const RSEQ: usize = 293;
    /// Create a process or thread from a versioned argument structure.
    ///
    /// Numbers assigned after the generic table was frozen are the same on both
    /// architectures, which is why this matches the x86-64 value.
    pub const CLONE3: usize = 435;
    /// Open a file from a versioned argument structure.
    pub const OPENAT2: usize = 437;
    /// Check accessibility with flags, the form musl now prefers.
    pub const FACCESSAT2: usize = 439;
}

/// An architecture-neutral system call.
///
/// The kernel dispatches on this, never on a raw number, so that the two
/// numbering schemes meet in exactly one place: [`from_x86_64`] and
/// [`from_aarch64`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Syscall {
    /// Read bytes from a file descriptor.
    Read,
    /// Write bytes to a file descriptor.
    Write,
    /// Open a file by path. x86-64 only.
    Open,
    /// Open a file relative to a directory file descriptor.
    Openat,
    /// Open a file from a versioned argument structure.
    Openat2,
    /// Close a file descriptor.
    Close,
    /// Stat a file by path, following symlinks. x86-64 only.
    Stat,
    /// Stat an open file descriptor.
    Fstat,
    /// Stat a file by path without following a final symlink. x86-64 only.
    Lstat,
    /// Stat a file relative to a directory file descriptor.
    Newfstatat,
    /// Stat a file with an explicit field mask and 64-bit timestamps.
    Statx,
    /// Wait for events on a set of file descriptors. x86-64 only.
    Poll,
    /// Poll with a signal mask and a `timespec` timeout.
    Ppoll,
    /// Reposition a file descriptor's offset.
    Lseek,
    /// Map files or anonymous memory into the address space.
    Mmap,
    /// Change the protection of a mapping.
    Mprotect,
    /// Remove a mapping.
    Munmap,
    /// Resize, and possibly move, an existing mapping.
    Mremap,
    /// Flush a file-backed mapping to its file.
    Msync,
    /// Advise the kernel about future use of a memory range.
    Madvise,
    /// Move the program break, the classic heap boundary.
    Brk,
    /// Install a signal handler.
    RtSigaction,
    /// Change the blocked signal mask.
    RtSigprocmask,
    /// Return from a signal handler, restoring the interrupted context.
    RtSigreturn,
    /// Replace the signal mask and wait for a signal.
    RtSigsuspend,
    /// Install or query the alternate signal stack.
    Sigaltstack,
    /// Device-specific control operation on a file descriptor.
    Ioctl,
    /// Read at an explicit offset, leaving the file position alone.
    Pread64,
    /// Write at an explicit offset, leaving the file position alone.
    Pwrite64,
    /// Read into several buffers in one call.
    Readv,
    /// Write from several buffers in one call.
    Writev,
    /// Check a path's accessibility for the real user. x86-64 only.
    Access,
    /// Check accessibility relative to a directory file descriptor.
    Faccessat,
    /// Check accessibility with flags, the form musl now prefers.
    Faccessat2,
    /// Create a pipe, returning two file descriptors. x86-64 only.
    Pipe,
    /// Create a pipe with flags.
    Pipe2,
    /// Yield the processor to another runnable thread.
    SchedYield,
    /// Duplicate a file descriptor onto the lowest free number.
    Dup,
    /// Duplicate a file descriptor onto a chosen number. x86-64 only.
    Dup2,
    /// Duplicate a file descriptor onto a chosen number with flags.
    Dup3,
    /// Sleep for a duration, resumable after a signal.
    Nanosleep,
    /// Read a clock.
    ClockGettime,
    /// Sleep against a chosen clock, optionally until an absolute time.
    ClockNanosleep,
    /// Read the wall clock, the obsolete predecessor of `clock_gettime`.
    Gettimeofday,
    /// Return the calling process's identifier.
    Getpid,
    /// Return the parent process's identifier.
    Getppid,
    /// Return the calling thread's identifier.
    Gettid,
    /// Copy data between two file descriptors inside the kernel.
    Sendfile,
    /// Create a socket.
    Socket,
    /// Connect a socket to an address.
    Connect,
    /// Create a process or thread; the primitive behind `fork` and `pthread`.
    Clone,
    /// Create a process or thread from a versioned argument structure.
    Clone3,
    /// Create a child process sharing nothing. x86-64 only.
    Fork,
    /// Create a child sharing the address space, suspending the parent.
    /// x86-64 only.
    Vfork,
    /// Replace the current process image with a program.
    Execve,
    /// Execute a program named by a file descriptor.
    Execveat,
    /// Terminate the calling thread.
    Exit,
    /// Terminate every thread in the process.
    ExitGroup,
    /// Wait for a child to change state, reporting resource usage.
    Wait4,
    /// Wait for a child to change state, without necessarily reaping it.
    Waitid,
    /// Send a signal to a process or process group.
    Kill,
    /// Send a signal to a thread by thread identifier.
    Tkill,
    /// Send a signal to a thread, checked against its thread group.
    Tgkill,
    /// Report kernel name and version.
    Uname,
    /// Manipulate a file descriptor's flags and locks.
    Fcntl,
    /// Flush a file's data and metadata to storage.
    Fsync,
    /// Flush a file's data, and only the metadata needed to read it back.
    Fdatasync,
    /// Flush all file systems.
    Sync,
    /// Set a file's length by path.
    Truncate,
    /// Set an open file's length.
    Ftruncate,
    /// Read the current working directory into a buffer.
    Getcwd,
    /// Change the working directory by path.
    Chdir,
    /// Change the working directory to an open directory.
    Fchdir,
    /// Rename a file. x86-64 only.
    Rename,
    /// Rename relative to directory file descriptors.
    Renameat,
    /// Rename with flags, such as `RENAME_NOREPLACE`.
    Renameat2,
    /// Create a directory. x86-64 only.
    Mkdir,
    /// Create a directory relative to a directory file descriptor.
    Mkdirat,
    /// Remove an empty directory. x86-64 only.
    Rmdir,
    /// Remove a directory entry. x86-64 only.
    Unlink,
    /// Remove a directory entry relative to a directory file descriptor.
    Unlinkat,
    /// Create a symbolic link. x86-64 only.
    Symlink,
    /// Create a symbolic link relative to a directory file descriptor.
    Symlinkat,
    /// Create a hard link. x86-64 only.
    Link,
    /// Create a hard link relative to directory file descriptors.
    Linkat,
    /// Read a symbolic link's target. x86-64 only.
    Readlink,
    /// Read a symbolic link relative to a directory file descriptor.
    Readlinkat,
    /// Change a file's mode by path. x86-64 only.
    Chmod,
    /// Change an open file's mode.
    Fchmod,
    /// Change a file's mode relative to a directory file descriptor.
    Fchmodat,
    /// Change a file's owner by path. x86-64 only.
    Chown,
    /// Change an open file's owner.
    Fchown,
    /// Change a file's owner relative to a directory file descriptor.
    Fchownat,
    /// Set the file mode creation mask.
    Umask,
    /// Read a resource limit; superseded by `prlimit64`.
    Getrlimit,
    /// Set a resource limit; superseded by `prlimit64`.
    Setrlimit,
    /// Read and set a resource limit of any process in one call.
    Prlimit64,
    /// Report accumulated resource usage.
    Getrusage,
    /// Report system-wide memory and load statistics.
    Sysinfo,
    /// Return the real user identifier.
    Getuid,
    /// Return the effective user identifier.
    Geteuid,
    /// Return the real group identifier.
    Getgid,
    /// Return the effective group identifier.
    Getegid,
    /// Set the user identifier.
    Setuid,
    /// Set the group identifier.
    Setgid,
    /// Read the real, effective and saved user identifiers.
    Getresuid,
    /// Read the real, effective and saved group identifiers.
    Getresgid,
    /// Set a process's process-group identifier.
    Setpgid,
    /// Return a process's process-group identifier.
    Getpgid,
    /// Read the supplementary group list.
    Getgroups,
    /// Read directory entries in the 64-bit layout.
    Getdents64,
    /// Wait on, or wake, a futex; the primitive under every musl lock.
    Futex,
    /// Register the address cleared and woken on thread exit.
    SetTidAddress,
    /// Register the robust futex list, walked when a thread dies holding a lock.
    SetRobustList,
    /// Read a thread's robust futex list.
    GetRobustList,
    /// Read a thread's processor affinity mask.
    SchedGetaffinity,
    /// Set a thread's processor affinity mask.
    SchedSetaffinity,
    /// Read a thread's scheduling parameters.
    SchedGetparam,
    /// Set a thread's scheduling policy and parameters.
    SchedSetscheduler,
    /// Read a thread's scheduling policy.
    SchedGetscheduler,
    /// Read or set an architecture register, notably `FS_BASE`. x86-64 only.
    ArchPrctl,
    /// Operate on per-process control settings, such as the thread name.
    Prctl,
    /// Fill a buffer with random bytes.
    Getrandom,
    /// Create an anonymous file living in memory.
    MemfdCreate,
    /// Create an epoll set with flags.
    EpollCreate1,
    /// Add, modify or remove a file descriptor in an epoll set.
    EpollCtl,
    /// Wait for events on an epoll set. x86-64 only; AArch64 has only
    /// [`Syscall::EpollPwait`].
    EpollWait,
    /// Wait on an epoll set with a signal mask.
    EpollPwait,
    /// Create an eventfd with flags.
    Eventfd2,
    /// Issue a process-wide memory barrier.
    Membarrier,
    /// Register a restartable sequence area.
    Rseq,
    /// Detach parts of the calling process's shared execution context.
    Unshare,
    /// Attach a file system.
    Mount,
    /// Detach a file system.
    Umount2,
    /// Report file system statistics by path.
    Statfs,
    /// Report file system statistics for an open file.
    Fstatfs,
}

/// Translate an x86-64 system call number.
///
/// Returns [`None`] for a number this crate does not know, which the caller
/// reports as `ENOSYS`. Split by range into helpers because one match over the
/// whole table would be both unreadably long and a `too_many_lines` failure.
#[must_use]
pub fn from_x86_64(nr: usize) -> Option<Syscall> {
    // Asked in turn, for the reason given on `from_aarch64`.
    x86_64_file_and_process(nr)
        .or_else(|| x86_64_metadata_and_ids(nr))
        .or_else(|| x86_64_threads_and_time(nr))
        .or_else(|| x86_64_at_family(nr))
        .or_else(|| x86_64_recent(nr))
}

/// x86-64 numbers 0 to 63: the original UNIX core.
fn x86_64_file_and_process(nr: usize) -> Option<Syscall> {
    let call = match nr {
        x86_64::READ => Syscall::Read,
        x86_64::WRITE => Syscall::Write,
        x86_64::OPEN => Syscall::Open,
        x86_64::CLOSE => Syscall::Close,
        x86_64::STAT => Syscall::Stat,
        x86_64::FSTAT => Syscall::Fstat,
        x86_64::LSTAT => Syscall::Lstat,
        x86_64::POLL => Syscall::Poll,
        x86_64::LSEEK => Syscall::Lseek,
        x86_64::MMAP => Syscall::Mmap,
        x86_64::MPROTECT => Syscall::Mprotect,
        x86_64::MUNMAP => Syscall::Munmap,
        x86_64::BRK => Syscall::Brk,
        x86_64::RT_SIGACTION => Syscall::RtSigaction,
        x86_64::RT_SIGPROCMASK => Syscall::RtSigprocmask,
        x86_64::RT_SIGRETURN => Syscall::RtSigreturn,
        x86_64::IOCTL => Syscall::Ioctl,
        x86_64::PREAD64 => Syscall::Pread64,
        x86_64::PWRITE64 => Syscall::Pwrite64,
        x86_64::READV => Syscall::Readv,
        x86_64::WRITEV => Syscall::Writev,
        x86_64::ACCESS => Syscall::Access,
        x86_64::PIPE => Syscall::Pipe,
        x86_64::SCHED_YIELD => Syscall::SchedYield,
        x86_64::MREMAP => Syscall::Mremap,
        x86_64::MSYNC => Syscall::Msync,
        x86_64::MADVISE => Syscall::Madvise,
        x86_64::DUP => Syscall::Dup,
        x86_64::DUP2 => Syscall::Dup2,
        x86_64::NANOSLEEP => Syscall::Nanosleep,
        x86_64::GETPID => Syscall::Getpid,
        x86_64::SENDFILE => Syscall::Sendfile,
        x86_64::SOCKET => Syscall::Socket,
        x86_64::CONNECT => Syscall::Connect,
        x86_64::CLONE => Syscall::Clone,
        x86_64::FORK => Syscall::Fork,
        x86_64::VFORK => Syscall::Vfork,
        x86_64::EXECVE => Syscall::Execve,
        x86_64::EXIT => Syscall::Exit,
        x86_64::WAIT4 => Syscall::Wait4,
        x86_64::KILL => Syscall::Kill,
        x86_64::UNAME => Syscall::Uname,
        _ => return None,
    };
    Some(call)
}

/// x86-64 numbers 64 to 159: file metadata, credentials and signals.
fn x86_64_metadata_and_ids(nr: usize) -> Option<Syscall> {
    let call = match nr {
        x86_64::FCNTL => Syscall::Fcntl,
        x86_64::FSYNC => Syscall::Fsync,
        x86_64::FDATASYNC => Syscall::Fdatasync,
        x86_64::TRUNCATE => Syscall::Truncate,
        x86_64::FTRUNCATE => Syscall::Ftruncate,
        x86_64::GETCWD => Syscall::Getcwd,
        x86_64::CHDIR => Syscall::Chdir,
        x86_64::FCHDIR => Syscall::Fchdir,
        x86_64::RENAME => Syscall::Rename,
        x86_64::MKDIR => Syscall::Mkdir,
        x86_64::RMDIR => Syscall::Rmdir,
        x86_64::LINK => Syscall::Link,
        x86_64::UNLINK => Syscall::Unlink,
        x86_64::SYMLINK => Syscall::Symlink,
        x86_64::READLINK => Syscall::Readlink,
        x86_64::CHMOD => Syscall::Chmod,
        x86_64::FCHMOD => Syscall::Fchmod,
        x86_64::CHOWN => Syscall::Chown,
        x86_64::FCHOWN => Syscall::Fchown,
        x86_64::UMASK => Syscall::Umask,
        x86_64::GETTIMEOFDAY => Syscall::Gettimeofday,
        x86_64::GETRLIMIT => Syscall::Getrlimit,
        x86_64::GETRUSAGE => Syscall::Getrusage,
        x86_64::SYSINFO => Syscall::Sysinfo,
        x86_64::GETUID => Syscall::Getuid,
        x86_64::GETGID => Syscall::Getgid,
        x86_64::SETUID => Syscall::Setuid,
        x86_64::SETGID => Syscall::Setgid,
        x86_64::GETEUID => Syscall::Geteuid,
        x86_64::GETEGID => Syscall::Getegid,
        x86_64::SETPGID => Syscall::Setpgid,
        x86_64::GETPPID => Syscall::Getppid,
        x86_64::GETGROUPS => Syscall::Getgroups,
        x86_64::GETRESUID => Syscall::Getresuid,
        x86_64::GETRESGID => Syscall::Getresgid,
        x86_64::GETPGID => Syscall::Getpgid,
        x86_64::RT_SIGSUSPEND => Syscall::RtSigsuspend,
        x86_64::SIGALTSTACK => Syscall::Sigaltstack,
        x86_64::STATFS => Syscall::Statfs,
        x86_64::FSTATFS => Syscall::Fstatfs,
        x86_64::SCHED_GETPARAM => Syscall::SchedGetparam,
        x86_64::SCHED_SETSCHEDULER => Syscall::SchedSetscheduler,
        x86_64::SCHED_GETSCHEDULER => Syscall::SchedGetscheduler,
        x86_64::PRCTL => Syscall::Prctl,
        x86_64::ARCH_PRCTL => Syscall::ArchPrctl,
        _ => return None,
    };
    Some(call)
}

/// x86-64 numbers 160 to 255: threads, futexes, clocks and epoll.
fn x86_64_threads_and_time(nr: usize) -> Option<Syscall> {
    let call = match nr {
        x86_64::SETRLIMIT => Syscall::Setrlimit,
        x86_64::SYNC => Syscall::Sync,
        x86_64::MOUNT => Syscall::Mount,
        x86_64::UMOUNT2 => Syscall::Umount2,
        x86_64::GETTID => Syscall::Gettid,
        x86_64::TKILL => Syscall::Tkill,
        x86_64::FUTEX => Syscall::Futex,
        x86_64::SCHED_SETAFFINITY => Syscall::SchedSetaffinity,
        x86_64::SCHED_GETAFFINITY => Syscall::SchedGetaffinity,
        x86_64::GETDENTS64 => Syscall::Getdents64,
        x86_64::SET_TID_ADDRESS => Syscall::SetTidAddress,
        x86_64::CLOCK_GETTIME => Syscall::ClockGettime,
        x86_64::CLOCK_NANOSLEEP => Syscall::ClockNanosleep,
        x86_64::EXIT_GROUP => Syscall::ExitGroup,
        x86_64::EPOLL_WAIT => Syscall::EpollWait,
        x86_64::EPOLL_CTL => Syscall::EpollCtl,
        x86_64::TGKILL => Syscall::Tgkill,
        x86_64::WAITID => Syscall::Waitid,
        _ => return None,
    };
    Some(call)
}

/// x86-64 numbers 256 to 349: the `*at` family and its contemporaries.
fn x86_64_at_family(nr: usize) -> Option<Syscall> {
    let call = match nr {
        x86_64::OPENAT => Syscall::Openat,
        x86_64::MKDIRAT => Syscall::Mkdirat,
        x86_64::FCHOWNAT => Syscall::Fchownat,
        x86_64::NEWFSTATAT => Syscall::Newfstatat,
        x86_64::UNLINKAT => Syscall::Unlinkat,
        x86_64::RENAMEAT => Syscall::Renameat,
        x86_64::LINKAT => Syscall::Linkat,
        x86_64::SYMLINKAT => Syscall::Symlinkat,
        x86_64::READLINKAT => Syscall::Readlinkat,
        x86_64::FCHMODAT => Syscall::Fchmodat,
        x86_64::FACCESSAT => Syscall::Faccessat,
        x86_64::PPOLL => Syscall::Ppoll,
        x86_64::UNSHARE => Syscall::Unshare,
        x86_64::SET_ROBUST_LIST => Syscall::SetRobustList,
        x86_64::GET_ROBUST_LIST => Syscall::GetRobustList,
        x86_64::EPOLL_PWAIT => Syscall::EpollPwait,
        x86_64::EVENTFD2 => Syscall::Eventfd2,
        x86_64::EPOLL_CREATE1 => Syscall::EpollCreate1,
        x86_64::DUP3 => Syscall::Dup3,
        x86_64::PIPE2 => Syscall::Pipe2,
        x86_64::PRLIMIT64 => Syscall::Prlimit64,
        x86_64::RENAMEAT2 => Syscall::Renameat2,
        x86_64::GETRANDOM => Syscall::Getrandom,
        x86_64::MEMFD_CREATE => Syscall::MemfdCreate,
        x86_64::EXECVEAT => Syscall::Execveat,
        x86_64::MEMBARRIER => Syscall::Membarrier,
        x86_64::STATX => Syscall::Statx,
        x86_64::RSEQ => Syscall::Rseq,
        _ => return None,
    };
    Some(call)
}

/// x86-64 numbers from 350 up: calls added after the number spaces converged.
fn x86_64_recent(nr: usize) -> Option<Syscall> {
    let call = match nr {
        x86_64::CLONE3 => Syscall::Clone3,
        x86_64::OPENAT2 => Syscall::Openat2,
        x86_64::FACCESSAT2 => Syscall::Faccessat2,
        _ => return None,
    };
    Some(call)
}

/// Translate an AArch64 system call number.
///
/// Returns [`None`] both for a number this crate does not know and for one the
/// generic table never assigned, which is the same answer: `ENOSYS`.
#[must_use]
pub fn from_aarch64(nr: usize) -> Option<Syscall> {
    // Each helper is asked in turn rather than selected by range. The grouping
    // exists to keep any one function under the line limit, and it should not
    // be able to affect the answer -- when it could, `socket` (198) sat in the
    // 200..=299 helper and was unreachable, which is the kind of hole a
    // dispatch table must not have.
    aarch64_files(nr)
        .or_else(|| aarch64_signals_and_ids(nr))
        .or_else(|| aarch64_memory_and_process(nr))
        .or_else(|| aarch64_recent(nr))
}

/// AArch64 numbers 0 to 99: descriptors, paths and process exit.
fn aarch64_files(nr: usize) -> Option<Syscall> {
    let call = match nr {
        aarch64::GETCWD => Syscall::Getcwd,
        aarch64::EVENTFD2 => Syscall::Eventfd2,
        aarch64::EPOLL_CREATE1 => Syscall::EpollCreate1,
        aarch64::EPOLL_CTL => Syscall::EpollCtl,
        aarch64::EPOLL_PWAIT => Syscall::EpollPwait,
        aarch64::DUP => Syscall::Dup,
        aarch64::DUP3 => Syscall::Dup3,
        aarch64::FCNTL => Syscall::Fcntl,
        aarch64::IOCTL => Syscall::Ioctl,
        aarch64::MKDIRAT => Syscall::Mkdirat,
        aarch64::UNLINKAT => Syscall::Unlinkat,
        aarch64::SYMLINKAT => Syscall::Symlinkat,
        aarch64::LINKAT => Syscall::Linkat,
        aarch64::RENAMEAT => Syscall::Renameat,
        aarch64::RENAMEAT2 => Syscall::Renameat2,
        aarch64::UMOUNT2 => Syscall::Umount2,
        aarch64::MOUNT => Syscall::Mount,
        aarch64::STATFS => Syscall::Statfs,
        aarch64::FSTATFS => Syscall::Fstatfs,
        aarch64::TRUNCATE => Syscall::Truncate,
        aarch64::FTRUNCATE => Syscall::Ftruncate,
        aarch64::FACCESSAT => Syscall::Faccessat,
        aarch64::CHDIR => Syscall::Chdir,
        aarch64::FCHDIR => Syscall::Fchdir,
        aarch64::FCHMOD => Syscall::Fchmod,
        aarch64::FCHMODAT => Syscall::Fchmodat,
        aarch64::FCHOWNAT => Syscall::Fchownat,
        aarch64::FCHOWN => Syscall::Fchown,
        aarch64::OPENAT => Syscall::Openat,
        aarch64::CLOSE => Syscall::Close,
        aarch64::PIPE2 => Syscall::Pipe2,
        aarch64::GETDENTS64 => Syscall::Getdents64,
        aarch64::LSEEK => Syscall::Lseek,
        aarch64::READ => Syscall::Read,
        aarch64::WRITE => Syscall::Write,
        aarch64::READV => Syscall::Readv,
        aarch64::WRITEV => Syscall::Writev,
        aarch64::PREAD64 => Syscall::Pread64,
        aarch64::PWRITE64 => Syscall::Pwrite64,
        aarch64::SENDFILE => Syscall::Sendfile,
        aarch64::PPOLL => Syscall::Ppoll,
        aarch64::READLINKAT => Syscall::Readlinkat,
        aarch64::NEWFSTATAT => Syscall::Newfstatat,
        aarch64::FSTAT => Syscall::Fstat,
        aarch64::SYNC => Syscall::Sync,
        aarch64::FSYNC => Syscall::Fsync,
        aarch64::FDATASYNC => Syscall::Fdatasync,
        aarch64::EXIT => Syscall::Exit,
        aarch64::EXIT_GROUP => Syscall::ExitGroup,
        aarch64::WAITID => Syscall::Waitid,
        aarch64::SET_TID_ADDRESS => Syscall::SetTidAddress,
        aarch64::UNSHARE => Syscall::Unshare,
        aarch64::FUTEX => Syscall::Futex,
        aarch64::SET_ROBUST_LIST => Syscall::SetRobustList,
        _ => return None,
    };
    Some(call)
}

/// AArch64 numbers 100 to 199: clocks, scheduling, signals and credentials.
fn aarch64_signals_and_ids(nr: usize) -> Option<Syscall> {
    let call = match nr {
        aarch64::GET_ROBUST_LIST => Syscall::GetRobustList,
        aarch64::NANOSLEEP => Syscall::Nanosleep,
        aarch64::CLOCK_GETTIME => Syscall::ClockGettime,
        aarch64::CLOCK_NANOSLEEP => Syscall::ClockNanosleep,
        aarch64::SCHED_SETSCHEDULER => Syscall::SchedSetscheduler,
        aarch64::SCHED_GETSCHEDULER => Syscall::SchedGetscheduler,
        aarch64::SCHED_GETPARAM => Syscall::SchedGetparam,
        aarch64::SCHED_SETAFFINITY => Syscall::SchedSetaffinity,
        aarch64::SCHED_GETAFFINITY => Syscall::SchedGetaffinity,
        aarch64::SCHED_YIELD => Syscall::SchedYield,
        aarch64::KILL => Syscall::Kill,
        aarch64::TKILL => Syscall::Tkill,
        aarch64::TGKILL => Syscall::Tgkill,
        aarch64::SIGALTSTACK => Syscall::Sigaltstack,
        aarch64::RT_SIGSUSPEND => Syscall::RtSigsuspend,
        aarch64::RT_SIGACTION => Syscall::RtSigaction,
        aarch64::RT_SIGPROCMASK => Syscall::RtSigprocmask,
        aarch64::RT_SIGRETURN => Syscall::RtSigreturn,
        aarch64::SETGID => Syscall::Setgid,
        aarch64::SETUID => Syscall::Setuid,
        aarch64::GETRESUID => Syscall::Getresuid,
        aarch64::GETRESGID => Syscall::Getresgid,
        aarch64::SETPGID => Syscall::Setpgid,
        aarch64::GETPGID => Syscall::Getpgid,
        aarch64::GETGROUPS => Syscall::Getgroups,
        aarch64::UNAME => Syscall::Uname,
        aarch64::GETRLIMIT => Syscall::Getrlimit,
        aarch64::SETRLIMIT => Syscall::Setrlimit,
        aarch64::GETRUSAGE => Syscall::Getrusage,
        aarch64::UMASK => Syscall::Umask,
        aarch64::PRCTL => Syscall::Prctl,
        aarch64::GETTIMEOFDAY => Syscall::Gettimeofday,
        aarch64::GETPID => Syscall::Getpid,
        aarch64::GETPPID => Syscall::Getppid,
        aarch64::GETUID => Syscall::Getuid,
        aarch64::GETEUID => Syscall::Geteuid,
        aarch64::GETGID => Syscall::Getgid,
        aarch64::GETEGID => Syscall::Getegid,
        aarch64::GETTID => Syscall::Gettid,
        aarch64::SYSINFO => Syscall::Sysinfo,
        _ => return None,
    };
    Some(call)
}

/// AArch64 numbers 200 to 299: sockets, memory and process creation.
fn aarch64_memory_and_process(nr: usize) -> Option<Syscall> {
    let call = match nr {
        aarch64::SOCKET => Syscall::Socket,
        aarch64::CONNECT => Syscall::Connect,
        aarch64::BRK => Syscall::Brk,
        aarch64::MUNMAP => Syscall::Munmap,
        aarch64::MREMAP => Syscall::Mremap,
        aarch64::CLONE => Syscall::Clone,
        aarch64::EXECVE => Syscall::Execve,
        aarch64::MMAP => Syscall::Mmap,
        aarch64::MPROTECT => Syscall::Mprotect,
        aarch64::MSYNC => Syscall::Msync,
        aarch64::MADVISE => Syscall::Madvise,
        aarch64::WAIT4 => Syscall::Wait4,
        aarch64::PRLIMIT64 => Syscall::Prlimit64,
        aarch64::GETRANDOM => Syscall::Getrandom,
        aarch64::MEMFD_CREATE => Syscall::MemfdCreate,
        aarch64::EXECVEAT => Syscall::Execveat,
        aarch64::MEMBARRIER => Syscall::Membarrier,
        aarch64::STATX => Syscall::Statx,
        aarch64::RSEQ => Syscall::Rseq,
        _ => return None,
    };
    Some(call)
}

/// AArch64 numbers from 300 up: calls added after the number spaces converged.
fn aarch64_recent(nr: usize) -> Option<Syscall> {
    let call = match nr {
        aarch64::CLONE3 => Syscall::Clone3,
        aarch64::OPENAT2 => Syscall::Openat2,
        aarch64::FACCESSAT2 => Syscall::Faccessat2,
        _ => return None,
    };
    Some(call)
}
