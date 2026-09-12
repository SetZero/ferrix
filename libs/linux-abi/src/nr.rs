//! System call numbers, and the translation from a number to a call.
//!
//! There is no single Linux system call table. Ferrix's three architectures
//! use three of them: x86-64 kept the numbering it grew
//! (`arch/x86/entry/syscalls/syscall_64.tbl`), every architecture added after
//! 2011 -- AArch64 included -- uses the generic table in
//! `include/uapi/asm-generic/unistd.h`, and ARMv7-A carries the EABI table in
//! `arch/arm/include/uapi/asm/unistd-common.h`, which predates both. The same
//! call has a different number on each, some calls exist on only one, and a
//! binary compiled for one architecture never sees the others' numbers.
//!
//! The kernel should not care. [`from_x86_64`], [`from_aarch64`] and
//! [`from_arm`] map each architecture's raw number onto one [`Syscall`], and
//! the dispatcher matches on that. An unknown number maps to [`None`], which
//! the caller answers with [`crate::errno::Errno::ENOSYS`] — the same thing
//! Linux does.
//!
//! # The 32-bit architecture is not just a third column
//!
//! ARMv7-A does not merely renumber the same calls. A 32-bit register cannot
//! carry a file offset, a file size or a post-2038 `time_t`, so the calls that
//! pass one exist twice, and the wider form is a *different call with a
//! different signature*: [`Syscall::Mmap2`] counts its offset in pages rather
//! than bytes, [`Syscall::Llseek`] returns its result through a pointer, and
//! [`Syscall::Fstat64`] writes a structure `fstat` would not recognise. Those
//! are separate [`Syscall`] variants for exactly that reason -- folding them
//! onto their 64-bit namesakes would hand a handler arguments it would
//! silently misread.
//!
//! # What is here
//!
//! Enough of the table for a static-musl program to start and run: process and
//! thread creation, memory, files, signals, futexes, time and the handful of
//! socket calls musl uses. Calls whose numbers were not verified against the
//! kernel source are absent rather than guessed, because a wrong number is a
//! call silently dispatched to the wrong handler, while a missing one is an
//! `ENOSYS` that names itself.
//!
//! The pre-2.4 16-bit credential calls (`getuid`, `setuid` and their kin at
//! ARMv7-A numbers 23 to 50) are deliberately absent for the same reason: musl
//! issues only the `32` forms, so a call arriving on one of the old numbers is
//! more likely a mistake than a request, and `ENOSYS` says so.

/// System call numbers for x86-64.
///
/// From `arch/x86/entry/syscalls/syscall_64.tbl`, the 64-bit ABI rather than
/// the x32 one.
pub mod x86_64 {
    /// Read bytes from a file descriptor.
    pub const READ: usize = 0;
    /// Write bytes to a file descriptor.
    pub const WRITE: usize = 1;
    /// Open a file by path. AArch64 has no equivalent; musl uses `openat`
    /// there. ARMv7-A kept it, at a number of its own.
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
    /// x86-64 only. AArch64 writes its thread pointer to `TPIDR_EL0` directly,
    /// so the generic table never needed the call; ARMv7-A cannot write its
    /// own either, and asks through [`super::arm::ARM_SET_TLS`] instead.
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

/// System call numbers for 32-bit ARMv7-A, the EABI table.
///
/// From `arch/arm/include/uapi/asm/unistd-common.h` with `__NR_SYSCALL_BASE`
/// at zero, which is what EABI sets it to; the OABI table this replaced based
/// every number at `0x900000` and is not a target here.
///
/// # Why so many names end in `64`
///
/// This is the only 32-bit architecture Ferrix targets, and the difference is
/// not cosmetic. A register cannot carry a file offset, a file size or a
/// `time_t`, so the calls that pass one exist twice: an original that has
/// outgrown its arguments, and a replacement that splits them across two
/// registers or points at a wider structure. musl calls the replacement
/// every time, so those are the numbers a real binary arrives with.
pub mod arm {
    /// Terminate the calling thread.
    pub const EXIT: usize = 1;
    /// Create a child process sharing nothing.
    pub const FORK: usize = 2;
    /// Read bytes from a file descriptor.
    pub const READ: usize = 3;
    /// Write bytes to a file descriptor.
    pub const WRITE: usize = 4;
    /// Open a file by path.
    pub const OPEN: usize = 5;
    /// Close a file descriptor.
    pub const CLOSE: usize = 6;
    /// Create a hard link.
    pub const LINK: usize = 9;
    /// Remove a directory entry.
    pub const UNLINK: usize = 10;
    /// Replace the current process image with a program.
    pub const EXECVE: usize = 11;
    /// Change the working directory by path.
    pub const CHDIR: usize = 12;
    /// Change a file's mode by path.
    pub const CHMOD: usize = 15;
    /// Reposition a file descriptor's offset.
    pub const LSEEK: usize = 19;
    /// Return the calling process's identifier.
    pub const GETPID: usize = 20;
    /// Attach a file system.
    pub const MOUNT: usize = 21;
    /// Check a path's accessibility for the real user.
    pub const ACCESS: usize = 33;
    /// Flush all file systems.
    pub const SYNC: usize = 36;
    /// Send a signal to a process or process group.
    pub const KILL: usize = 37;
    /// Rename a file.
    pub const RENAME: usize = 38;
    /// Create a directory.
    pub const MKDIR: usize = 39;
    /// Remove an empty directory.
    pub const RMDIR: usize = 40;
    /// Duplicate a file descriptor onto the lowest free number.
    pub const DUP: usize = 41;
    /// Create a pipe, returning two file descriptors.
    pub const PIPE: usize = 42;
    /// Move the program break, the classic heap boundary.
    pub const BRK: usize = 45;
    /// Detach a file system.
    pub const UMOUNT2: usize = 52;
    /// Device-specific control operation on a file descriptor.
    pub const IOCTL: usize = 54;
    /// Manipulate a file descriptor's flags and locks.
    pub const FCNTL: usize = 55;
    /// Set a process's process-group identifier.
    pub const SETPGID: usize = 57;
    /// Set the file mode creation mask.
    pub const UMASK: usize = 60;
    /// Duplicate a file descriptor onto a chosen number.
    pub const DUP2: usize = 63;
    /// Return the parent process's identifier.
    pub const GETPPID: usize = 64;
    /// Set a resource limit; superseded by `prlimit64`.
    pub const SETRLIMIT: usize = 75;
    /// Report accumulated resource usage.
    pub const GETRUSAGE: usize = 77;
    /// Read the wall clock, the obsolete predecessor of `clock_gettime`.
    pub const GETTIMEOFDAY: usize = 78;
    /// Create a symbolic link.
    pub const SYMLINK: usize = 83;
    /// Read a symbolic link's target.
    pub const READLINK: usize = 85;
    /// Remove a mapping.
    pub const MUNMAP: usize = 91;
    /// Set a file's length by path.
    pub const TRUNCATE: usize = 92;
    /// Set an open file's length.
    pub const FTRUNCATE: usize = 93;
    /// Change an open file's mode.
    pub const FCHMOD: usize = 94;
    /// Wait for a child to change state, reporting resource usage.
    pub const WAIT4: usize = 114;
    /// Report system-wide memory and load statistics.
    pub const SYSINFO: usize = 116;
    /// Flush a file's data and metadata to storage.
    pub const FSYNC: usize = 118;
    /// Create a process or thread; the primitive behind `fork` and `pthread`.
    pub const CLONE: usize = 120;
    /// Report kernel name and version.
    pub const UNAME: usize = 122;
    /// Change the protection of a mapping.
    pub const MPROTECT: usize = 125;
    /// Return a process's process-group identifier.
    pub const GETPGID: usize = 132;
    /// Change the working directory to an open directory.
    pub const FCHDIR: usize = 133;
    /// Reposition a file descriptor's offset, with the offset split across
    /// two registers and the result written through a pointer. ARMv7-A only,
    /// because a 32-bit register cannot carry a 64-bit offset and the return
    /// register cannot carry one back.
    pub const LLSEEK: usize = 140;
    /// Flush a file-backed mapping to its file.
    pub const MSYNC: usize = 144;
    /// Read into several buffers in one call.
    pub const READV: usize = 145;
    /// Write from several buffers in one call.
    pub const WRITEV: usize = 146;
    /// Flush a file's data, and only the metadata needed to read it back.
    pub const FDATASYNC: usize = 148;
    /// Read a thread's scheduling parameters.
    pub const SCHED_GETPARAM: usize = 155;
    /// Set a thread's scheduling policy and parameters.
    pub const SCHED_SETSCHEDULER: usize = 156;
    /// Read a thread's scheduling policy.
    pub const SCHED_GETSCHEDULER: usize = 157;
    /// Yield the processor to another runnable thread.
    pub const SCHED_YIELD: usize = 158;
    /// Sleep for a duration, resumable after a signal.
    pub const NANOSLEEP: usize = 162;
    /// Resize, and possibly move, an existing mapping.
    pub const MREMAP: usize = 163;
    /// Wait for events on a set of file descriptors.
    pub const POLL: usize = 168;
    /// Operate on per-process control settings, such as the thread name.
    pub const PRCTL: usize = 172;
    /// Return from a signal handler, restoring the interrupted context.
    pub const RT_SIGRETURN: usize = 173;
    /// Install a signal handler.
    pub const RT_SIGACTION: usize = 174;
    /// Change the blocked signal mask.
    pub const RT_SIGPROCMASK: usize = 175;
    /// Replace the signal mask and wait for a signal.
    pub const RT_SIGSUSPEND: usize = 179;
    /// Read at an explicit offset, leaving the file position alone.
    pub const PREAD64: usize = 180;
    /// Write at an explicit offset, leaving the file position alone.
    pub const PWRITE64: usize = 181;
    /// Read the current working directory into a buffer.
    pub const GETCWD: usize = 183;
    /// Install or query the alternate signal stack.
    pub const SIGALTSTACK: usize = 186;
    /// Create a child sharing the address space, suspending the parent.
    pub const VFORK: usize = 190;
    /// Read a resource limit; superseded by `prlimit64`.
    pub const UGETRLIMIT: usize = 191;
    /// Map files or anonymous memory, with the file offset counted in
    /// 4096-byte units. ARMv7-A only, and *not* interchangeable with
    /// [`super::Syscall::Mmap`]: the sixth argument must be multiplied by 4096
    /// before use, and a handler that forgets maps the wrong part of the
    /// file.
    pub const MMAP2: usize = 192;
    /// Set a file's length by path, with the length split across two
    /// registers. ARMv7-A only.
    pub const TRUNCATE64: usize = 193;
    /// Set an open file's length, with the length split across two registers.
    /// ARMv7-A only.
    pub const FTRUNCATE64: usize = 194;
    /// Stat a file by path, following symlinks, into `struct stat64`. ARMv7-A
    /// only: the 32-bit `stat` it replaced cannot express a file larger than
    /// 2 GiB.
    pub const STAT64: usize = 195;
    /// Stat a file by path without following a final symlink, into `struct
    /// stat64`. ARMv7-A only.
    pub const LSTAT64: usize = 196;
    /// Stat an open file descriptor into `struct stat64`. ARMv7-A only.
    pub const FSTAT64: usize = 197;
    /// Return the real user identifier.
    pub const GETUID32: usize = 199;
    /// Return the real group identifier.
    pub const GETGID32: usize = 200;
    /// Return the effective user identifier.
    pub const GETEUID32: usize = 201;
    /// Return the effective group identifier.
    pub const GETEGID32: usize = 202;
    /// Read the supplementary group list.
    pub const GETGROUPS32: usize = 205;
    /// Change an open file's owner.
    pub const FCHOWN32: usize = 207;
    /// Read the real, effective and saved user identifiers.
    pub const GETRESUID32: usize = 209;
    /// Read the real, effective and saved group identifiers.
    pub const GETRESGID32: usize = 211;
    /// Change a file's owner by path.
    pub const CHOWN32: usize = 212;
    /// Set the user identifier.
    pub const SETUID32: usize = 213;
    /// Set the group identifier.
    pub const SETGID32: usize = 214;
    /// Read directory entries in the 64-bit layout.
    pub const GETDENTS64: usize = 217;
    /// Advise the kernel about future use of a memory range.
    pub const MADVISE: usize = 220;
    /// Manipulate a file descriptor's flags and locks, with 64-bit `flock64`
    /// structures. ARMv7-A only; musl calls it for every `fcntl`, not only
    /// the locking commands.
    pub const FCNTL64: usize = 221;
    /// Return the calling thread's identifier.
    pub const GETTID: usize = 224;
    /// Send a signal to a thread by thread identifier.
    pub const TKILL: usize = 238;
    /// Copy data between two file descriptors, with a 64-bit offset argument.
    /// ARMv7-A's form of [`super::Syscall::Sendfile`].
    pub const SENDFILE64: usize = 239;
    /// Wait on, or wake, a futex; the primitive under every musl lock.
    pub const FUTEX: usize = 240;
    /// Set a thread's processor affinity mask.
    pub const SCHED_SETAFFINITY: usize = 241;
    /// Read a thread's processor affinity mask.
    pub const SCHED_GETAFFINITY: usize = 242;
    /// Terminate every thread in the process.
    pub const EXIT_GROUP: usize = 248;
    /// Add, modify or remove a file descriptor in an epoll set.
    pub const EPOLL_CTL: usize = 251;
    /// Wait for events on an epoll set.
    pub const EPOLL_WAIT: usize = 252;
    /// Register the address cleared and woken on thread exit.
    pub const SET_TID_ADDRESS: usize = 256;
    /// Read a clock.
    pub const CLOCK_GETTIME: usize = 263;
    /// Sleep against a chosen clock, optionally until an absolute time.
    pub const CLOCK_NANOSLEEP: usize = 265;
    /// Report file system statistics by path, into `struct statfs64`. ARMv7-A
    /// only, and it takes the structure's size as an explicit second
    /// argument, which [`super::Syscall::Statfs`] does not.
    pub const STATFS64: usize = 266;
    /// Report file system statistics for an open file, into `struct
    /// statfs64`. ARMv7-A only, with the same explicit size argument as
    /// [`super::Syscall::Statfs64`].
    pub const FSTATFS64: usize = 267;
    /// Send a signal to a thread, checked against its thread group.
    pub const TGKILL: usize = 268;
    /// Wait for a child to change state, without necessarily reaping it.
    pub const WAITID: usize = 280;
    /// Create a socket.
    pub const SOCKET: usize = 281;
    /// Connect a socket to an address.
    pub const CONNECT: usize = 283;
    /// Open a file relative to a directory file descriptor.
    pub const OPENAT: usize = 322;
    /// Create a directory relative to a directory file descriptor.
    pub const MKDIRAT: usize = 323;
    /// Change a file's owner relative to a directory file descriptor.
    pub const FCHOWNAT: usize = 325;
    /// Stat a file relative to a directory file descriptor, into `struct
    /// stat64`. ARMv7-A's form of [`super::Syscall::Newfstatat`], which it cannot
    /// share because the structure written back has a different layout.
    pub const FSTATAT64: usize = 327;
    /// Remove a directory entry relative to a directory file descriptor.
    pub const UNLINKAT: usize = 328;
    /// Rename relative to directory file descriptors.
    pub const RENAMEAT: usize = 329;
    /// Create a hard link relative to directory file descriptors.
    pub const LINKAT: usize = 330;
    /// Create a symbolic link relative to a directory file descriptor.
    pub const SYMLINKAT: usize = 331;
    /// Read a symbolic link relative to a directory file descriptor.
    pub const READLINKAT: usize = 332;
    /// Change a file's mode relative to a directory file descriptor.
    pub const FCHMODAT: usize = 333;
    /// Check accessibility relative to a directory file descriptor.
    pub const FACCESSAT: usize = 334;
    /// Poll with a signal mask and a `timespec` timeout.
    pub const PPOLL: usize = 336;
    /// Detach parts of the calling process's shared execution context.
    pub const UNSHARE: usize = 337;
    /// Register the robust futex list, walked when a thread dies holding a
    /// lock.
    pub const SET_ROBUST_LIST: usize = 338;
    /// Read a thread's robust futex list.
    pub const GET_ROBUST_LIST: usize = 339;
    /// Wait on an epoll set with a signal mask.
    pub const EPOLL_PWAIT: usize = 346;
    /// Create an eventfd with flags.
    pub const EVENTFD2: usize = 356;
    /// Create an epoll set with flags.
    pub const EPOLL_CREATE1: usize = 357;
    /// Duplicate a file descriptor onto a chosen number with flags.
    pub const DUP3: usize = 358;
    /// Create a pipe with flags.
    pub const PIPE2: usize = 359;
    /// Read and set a resource limit of any process in one call.
    pub const PRLIMIT64: usize = 369;
    /// Rename with flags, such as `RENAME_NOREPLACE`.
    pub const RENAMEAT2: usize = 382;
    /// Fill a buffer with random bytes.
    pub const GETRANDOM: usize = 384;
    /// Create an anonymous file living in memory.
    pub const MEMFD_CREATE: usize = 385;
    /// Execute a program named by a file descriptor.
    pub const EXECVEAT: usize = 387;
    /// Issue a process-wide memory barrier.
    pub const MEMBARRIER: usize = 389;
    /// Stat a file with an explicit field mask and 64-bit timestamps.
    pub const STATX: usize = 397;
    /// Register a restartable sequence area.
    pub const RSEQ: usize = 398;
    /// Read a clock into a 64-bit `timespec`. ARMv7-A only. musl has been
    /// time64 since 1.2, so this, not [`super::Syscall::ClockGettime`], is what a
    /// current 32-bit binary calls.
    pub const CLOCK_GETTIME64: usize = 403;
    /// Sleep against a chosen clock, with 64-bit `timespec` arguments.
    /// ARMv7-A only.
    pub const CLOCK_NANOSLEEP_TIME64: usize = 407;
    /// Poll with a signal mask and a 64-bit `timespec`. The timeout is a pair
    /// of 64-bit fields rather than the 32-bit pair [`PPOLL`] takes.
    pub const PPOLL_TIME64: usize = 414;
    /// Wait on, or wake, a futex, with a 64-bit `timespec` timeout. ARMv7-A
    /// only, and the one a time64 musl's locks actually reach.
    pub const FUTEX_TIME64: usize = 422;
    /// Create a process or thread from a versioned argument structure.
    pub const CLONE3: usize = 435;
    /// Open a file from a versioned argument structure.
    pub const OPENAT2: usize = 437;
    /// Check accessibility with flags, the form musl now prefers.
    pub const FACCESSAT2: usize = 439;

    /// The base of the ARM-private call range, `__ARM_NR_BASE`.
    /// Four registers and two cache operations that no other architecture
    /// needs, parked far above the shared table so that a number added to it
    /// can never collide with one of these.
    pub const ARM_PRIVATE_BASE: usize = 0x000f_0000;

    /// Make a range coherent between the data and instruction caches.
    pub const ARM_CACHEFLUSH: usize = ARM_PRIVATE_BASE + 2;
    /// Set the thread pointer read through `TPIDRURO`.
    pub const ARM_SET_TLS: usize = ARM_PRIVATE_BASE + 5;
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
    /// Open a file by path. x86-64 and ARMv7-A only.
    Open,
    /// Open a file relative to a directory file descriptor.
    Openat,
    /// Open a file from a versioned argument structure.
    Openat2,
    /// Close a file descriptor.
    Close,
    /// Stat a file by path, following symlinks. x86-64 only; ARMv7-A's
    /// equivalent is [`Syscall::Stat64`].
    Stat,
    /// Stat a file by path, following symlinks, into `struct stat64`. ARMv7-A
    /// only: the 32-bit `stat` it replaced cannot express a file larger than 2
    /// GiB.
    Stat64,
    /// Stat an open file descriptor. ARMv7-A's equivalent is
    /// [`Syscall::Fstat64`].
    Fstat,
    /// Stat an open file descriptor into `struct stat64`. ARMv7-A only.
    Fstat64,
    /// Stat a file by path without following a final symlink. x86-64 only;
    /// ARMv7-A's equivalent is [`Syscall::Lstat64`].
    Lstat,
    /// Stat a file by path without following a final symlink, into `struct
    /// stat64`. ARMv7-A only.
    Lstat64,
    /// Stat a file relative to a directory file descriptor.
    Newfstatat,
    /// Stat a file relative to a directory file descriptor, into `struct stat64`.
    /// ARMv7-A's form of [`Syscall::Newfstatat`], which it cannot share because
    /// the structure written back has a different layout.
    Fstatat64,
    /// Stat a file with an explicit field mask and 64-bit timestamps.
    Statx,
    /// Wait for events on a set of file descriptors. x86-64 and ARMv7-A only.
    Poll,
    /// Poll with a signal mask and a `timespec` timeout.
    Ppoll,
    /// Poll with a signal mask and a 64-bit `timespec`. ARMv7-A only: the timeout
    /// is a pair of 64-bit fields rather than the 32-bit pair [`Syscall::Ppoll`]
    /// takes on that architecture.
    PpollTime64,
    /// Reposition a file descriptor's offset.
    Lseek,
    /// Reposition a file descriptor's offset, with the offset split across two
    /// registers and the result written through a pointer. ARMv7-A only, because
    /// a 32-bit register cannot carry a 64-bit offset and the return register
    /// cannot carry one back.
    Llseek,
    /// Map files or anonymous memory into the address space.
    Mmap,
    /// Map files or anonymous memory, with the file offset counted in 4096-byte
    /// units. ARMv7-A only, and *not* interchangeable with [`Syscall::Mmap`]: the
    /// sixth argument must be multiplied by 4096 before use, and a handler that
    /// forgets maps the wrong part of the file.
    Mmap2,
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
    /// Check a path's accessibility for the real user. x86-64 and ARMv7-A only.
    Access,
    /// Check accessibility relative to a directory file descriptor.
    Faccessat,
    /// Check accessibility with flags, the form musl now prefers.
    Faccessat2,
    /// Create a pipe, returning two file descriptors. x86-64 and ARMv7-A only.
    Pipe,
    /// Create a pipe with flags.
    Pipe2,
    /// Yield the processor to another runnable thread.
    SchedYield,
    /// Duplicate a file descriptor onto the lowest free number.
    Dup,
    /// Duplicate a file descriptor onto a chosen number. x86-64 and ARMv7-A only.
    Dup2,
    /// Duplicate a file descriptor onto a chosen number with flags.
    Dup3,
    /// Sleep for a duration, resumable after a signal.
    Nanosleep,
    /// Read a clock.
    ClockGettime,
    /// Read a clock into a 64-bit `timespec`. ARMv7-A only. musl has been time64
    /// since 1.2, so this, not [`Syscall::ClockGettime`], is what a current
    /// 32-bit binary calls.
    ClockGettime64,
    /// Sleep against a chosen clock, optionally until an absolute time.
    ClockNanosleep,
    /// Sleep against a chosen clock, with 64-bit `timespec` arguments. ARMv7-A
    /// only.
    ClockNanosleepTime64,
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
    /// Copy data between two file descriptors, with a 64-bit offset argument.
    /// ARMv7-A's form of [`Syscall::Sendfile`].
    Sendfile64,
    /// Create a socket.
    Socket,
    /// Connect a socket to an address.
    Connect,
    /// Create a process or thread; the primitive behind `fork` and `pthread`.
    Clone,
    /// Create a process or thread from a versioned argument structure.
    Clone3,
    /// Create a child process sharing nothing. x86-64 and ARMv7-A only.
    Fork,
    /// Create a child sharing the address space, suspending the parent.
    /// x86-64 and ARMv7-A only.
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
    /// Manipulate a file descriptor's flags and locks, with 64-bit `flock64`
    /// structures. ARMv7-A only; musl calls it for every `fcntl`, not only the
    /// locking commands.
    Fcntl64,
    /// Flush a file's data and metadata to storage.
    Fsync,
    /// Flush a file's data, and only the metadata needed to read it back.
    Fdatasync,
    /// Flush all file systems.
    Sync,
    /// Set a file's length by path.
    Truncate,
    /// Set a file's length by path, with the length split across two registers.
    /// ARMv7-A only.
    Truncate64,
    /// Set an open file's length.
    Ftruncate,
    /// Set an open file's length, with the length split across two registers.
    /// ARMv7-A only.
    Ftruncate64,
    /// Read the current working directory into a buffer.
    Getcwd,
    /// Change the working directory by path.
    Chdir,
    /// Change the working directory to an open directory.
    Fchdir,
    /// Rename a file. x86-64 and ARMv7-A only.
    Rename,
    /// Rename relative to directory file descriptors.
    Renameat,
    /// Rename with flags, such as `RENAME_NOREPLACE`.
    Renameat2,
    /// Create a directory. x86-64 and ARMv7-A only.
    Mkdir,
    /// Create a directory relative to a directory file descriptor.
    Mkdirat,
    /// Remove an empty directory. x86-64 and ARMv7-A only.
    Rmdir,
    /// Remove a directory entry. x86-64 and ARMv7-A only.
    Unlink,
    /// Remove a directory entry relative to a directory file descriptor.
    Unlinkat,
    /// Create a symbolic link. x86-64 and ARMv7-A only.
    Symlink,
    /// Create a symbolic link relative to a directory file descriptor.
    Symlinkat,
    /// Create a hard link. x86-64 and ARMv7-A only.
    Link,
    /// Create a hard link relative to directory file descriptors.
    Linkat,
    /// Read a symbolic link's target. x86-64 and ARMv7-A only.
    Readlink,
    /// Read a symbolic link relative to a directory file descriptor.
    Readlinkat,
    /// Change a file's mode by path. x86-64 and ARMv7-A only.
    Chmod,
    /// Change an open file's mode.
    Fchmod,
    /// Change a file's mode relative to a directory file descriptor.
    Fchmodat,
    /// Change a file's owner by path. x86-64 and ARMv7-A only.
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
    /// Wait on, or wake, a futex, with a 64-bit `timespec` timeout. ARMv7-A only,
    /// and the one a time64 musl's locks actually reach.
    FutexTime64,
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
    /// Read or set an architecture register, notably `FS_BASE`. x86-64 only;
    /// ARMv7-A reaches the same effect through [`Syscall::ArmSetTls`].
    ArchPrctl,
    /// Set the thread pointer read through `TPIDRURO`. ARMv7-A only, and the
    /// reason ARM needs no [`Syscall::ArchPrctl`]: a 32-bit ARM userspace cannot
    /// write the register itself, so setting up thread-local storage is a call
    /// into the kernel.
    ArmSetTls,
    /// Make a range of memory coherent between the data and instruction caches.
    /// ARMv7-A only. Unlike x86-64 and AArch64, a 32-bit ARM userspace cannot
    /// reach the cache maintenance operations, so anything that writes code — a
    /// JIT, a trampoline — has to ask the kernel.
    ArmCacheflush,
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
    /// Wait for events on an epoll set. x86-64 and ARMv7-A only; AArch64 has
    /// only [`Syscall::EpollPwait`].
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
    /// Report file system statistics by path, into `struct statfs64`. ARMv7-A
    /// only, and it takes the structure's size as an explicit second argument,
    /// which [`Syscall::Statfs`] does not.
    Statfs64,
    /// Report file system statistics for an open file.
    Fstatfs,
    /// Report file system statistics for an open file, into `struct statfs64`.
    /// ARMv7-A only, with the same explicit size argument as
    /// [`Syscall::Statfs64`].
    Fstatfs64,
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

/// Translate a 32-bit ARMv7-A (EABI) system call number.
///
/// Returns [`None`] for a number this crate does not know, which the caller
/// reports as `ENOSYS`.
#[must_use]
pub fn from_arm(nr: usize) -> Option<Syscall> {
    // Asked in turn, for the reason given on `from_aarch64`.
    arm_early(nr)
        .or_else(|| arm_signals_and_mm(nr))
        .or_else(|| arm_ids_and_at_family(nr))
        .or_else(|| arm_recent(nr))
}

/// ARMv7-A numbers 0 to 99: the calls inherited from the very first Linux/ARM
/// table -- process lifetime, descriptors and paths.
fn arm_early(nr: usize) -> Option<Syscall> {
    let call = match nr {
        arm::EXIT => Syscall::Exit,
        arm::FORK => Syscall::Fork,
        arm::READ => Syscall::Read,
        arm::WRITE => Syscall::Write,
        arm::OPEN => Syscall::Open,
        arm::CLOSE => Syscall::Close,
        arm::LINK => Syscall::Link,
        arm::UNLINK => Syscall::Unlink,
        arm::EXECVE => Syscall::Execve,
        arm::CHDIR => Syscall::Chdir,
        arm::CHMOD => Syscall::Chmod,
        arm::LSEEK => Syscall::Lseek,
        arm::GETPID => Syscall::Getpid,
        arm::MOUNT => Syscall::Mount,
        arm::ACCESS => Syscall::Access,
        arm::SYNC => Syscall::Sync,
        arm::KILL => Syscall::Kill,
        arm::RENAME => Syscall::Rename,
        arm::MKDIR => Syscall::Mkdir,
        arm::RMDIR => Syscall::Rmdir,
        arm::DUP => Syscall::Dup,
        arm::PIPE => Syscall::Pipe,
        arm::BRK => Syscall::Brk,
        arm::UMOUNT2 => Syscall::Umount2,
        arm::IOCTL => Syscall::Ioctl,
        arm::FCNTL => Syscall::Fcntl,
        arm::SETPGID => Syscall::Setpgid,
        arm::UMASK => Syscall::Umask,
        arm::DUP2 => Syscall::Dup2,
        arm::GETPPID => Syscall::Getppid,
        arm::SETRLIMIT => Syscall::Setrlimit,
        arm::GETRUSAGE => Syscall::Getrusage,
        arm::GETTIMEOFDAY => Syscall::Gettimeofday,
        arm::SYMLINK => Syscall::Symlink,
        arm::READLINK => Syscall::Readlink,
        arm::MUNMAP => Syscall::Munmap,
        arm::TRUNCATE => Syscall::Truncate,
        arm::FTRUNCATE => Syscall::Ftruncate,
        arm::FCHMOD => Syscall::Fchmod,
        _ => return None,
    };
    Some(call)
}

/// ARMv7-A numbers 100 to 199: signals, memory, scheduling, and the `64`
/// forms that replaced calls a 32-bit `off_t` had outgrown.
fn arm_signals_and_mm(nr: usize) -> Option<Syscall> {
    let call = match nr {
        arm::WAIT4 => Syscall::Wait4,
        arm::SYSINFO => Syscall::Sysinfo,
        arm::FSYNC => Syscall::Fsync,
        arm::CLONE => Syscall::Clone,
        arm::UNAME => Syscall::Uname,
        arm::MPROTECT => Syscall::Mprotect,
        arm::GETPGID => Syscall::Getpgid,
        arm::FCHDIR => Syscall::Fchdir,
        arm::LLSEEK => Syscall::Llseek,
        arm::MSYNC => Syscall::Msync,
        arm::READV => Syscall::Readv,
        arm::WRITEV => Syscall::Writev,
        arm::FDATASYNC => Syscall::Fdatasync,
        arm::SCHED_GETPARAM => Syscall::SchedGetparam,
        arm::SCHED_SETSCHEDULER => Syscall::SchedSetscheduler,
        arm::SCHED_GETSCHEDULER => Syscall::SchedGetscheduler,
        arm::SCHED_YIELD => Syscall::SchedYield,
        arm::NANOSLEEP => Syscall::Nanosleep,
        arm::MREMAP => Syscall::Mremap,
        arm::POLL => Syscall::Poll,
        arm::PRCTL => Syscall::Prctl,
        arm::RT_SIGRETURN => Syscall::RtSigreturn,
        arm::RT_SIGACTION => Syscall::RtSigaction,
        arm::RT_SIGPROCMASK => Syscall::RtSigprocmask,
        arm::RT_SIGSUSPEND => Syscall::RtSigsuspend,
        arm::PREAD64 => Syscall::Pread64,
        arm::PWRITE64 => Syscall::Pwrite64,
        arm::GETCWD => Syscall::Getcwd,
        arm::SIGALTSTACK => Syscall::Sigaltstack,
        arm::VFORK => Syscall::Vfork,
        arm::UGETRLIMIT => Syscall::Getrlimit,
        arm::MMAP2 => Syscall::Mmap2,
        arm::TRUNCATE64 => Syscall::Truncate64,
        arm::FTRUNCATE64 => Syscall::Ftruncate64,
        arm::STAT64 => Syscall::Stat64,
        arm::LSTAT64 => Syscall::Lstat64,
        arm::FSTAT64 => Syscall::Fstat64,
        arm::GETUID32 => Syscall::Getuid,
        _ => return None,
    };
    Some(call)
}

/// ARMv7-A numbers 200 to 329: the 32-bit credential calls, futexes, and the
/// `*at` family.
fn arm_ids_and_at_family(nr: usize) -> Option<Syscall> {
    let call = match nr {
        arm::GETGID32 => Syscall::Getgid,
        arm::GETEUID32 => Syscall::Geteuid,
        arm::GETEGID32 => Syscall::Getegid,
        arm::GETGROUPS32 => Syscall::Getgroups,
        arm::FCHOWN32 => Syscall::Fchown,
        arm::GETRESUID32 => Syscall::Getresuid,
        arm::GETRESGID32 => Syscall::Getresgid,
        arm::CHOWN32 => Syscall::Chown,
        arm::SETUID32 => Syscall::Setuid,
        arm::SETGID32 => Syscall::Setgid,
        arm::GETDENTS64 => Syscall::Getdents64,
        arm::MADVISE => Syscall::Madvise,
        arm::FCNTL64 => Syscall::Fcntl64,
        arm::GETTID => Syscall::Gettid,
        arm::TKILL => Syscall::Tkill,
        arm::SENDFILE64 => Syscall::Sendfile64,
        arm::FUTEX => Syscall::Futex,
        arm::SCHED_SETAFFINITY => Syscall::SchedSetaffinity,
        arm::SCHED_GETAFFINITY => Syscall::SchedGetaffinity,
        arm::EXIT_GROUP => Syscall::ExitGroup,
        arm::EPOLL_CTL => Syscall::EpollCtl,
        arm::EPOLL_WAIT => Syscall::EpollWait,
        arm::SET_TID_ADDRESS => Syscall::SetTidAddress,
        arm::CLOCK_GETTIME => Syscall::ClockGettime,
        arm::CLOCK_NANOSLEEP => Syscall::ClockNanosleep,
        arm::STATFS64 => Syscall::Statfs64,
        arm::FSTATFS64 => Syscall::Fstatfs64,
        arm::TGKILL => Syscall::Tgkill,
        arm::WAITID => Syscall::Waitid,
        arm::SOCKET => Syscall::Socket,
        arm::CONNECT => Syscall::Connect,
        arm::OPENAT => Syscall::Openat,
        arm::MKDIRAT => Syscall::Mkdirat,
        arm::FCHOWNAT => Syscall::Fchownat,
        arm::FSTATAT64 => Syscall::Fstatat64,
        arm::UNLINKAT => Syscall::Unlinkat,
        arm::RENAMEAT => Syscall::Renameat,
        _ => return None,
    };
    Some(call)
}

/// ARMv7-A numbers 330 and up, and the ARM-private range: everything added
/// after the `*at` family, including the time64 calls a current musl actually
/// issues.
fn arm_recent(nr: usize) -> Option<Syscall> {
    let call = match nr {
        arm::LINKAT => Syscall::Linkat,
        arm::SYMLINKAT => Syscall::Symlinkat,
        arm::READLINKAT => Syscall::Readlinkat,
        arm::FCHMODAT => Syscall::Fchmodat,
        arm::FACCESSAT => Syscall::Faccessat,
        arm::PPOLL => Syscall::Ppoll,
        arm::UNSHARE => Syscall::Unshare,
        arm::SET_ROBUST_LIST => Syscall::SetRobustList,
        arm::GET_ROBUST_LIST => Syscall::GetRobustList,
        arm::EPOLL_PWAIT => Syscall::EpollPwait,
        arm::EVENTFD2 => Syscall::Eventfd2,
        arm::EPOLL_CREATE1 => Syscall::EpollCreate1,
        arm::DUP3 => Syscall::Dup3,
        arm::PIPE2 => Syscall::Pipe2,
        arm::PRLIMIT64 => Syscall::Prlimit64,
        arm::RENAMEAT2 => Syscall::Renameat2,
        arm::GETRANDOM => Syscall::Getrandom,
        arm::MEMFD_CREATE => Syscall::MemfdCreate,
        arm::EXECVEAT => Syscall::Execveat,
        arm::MEMBARRIER => Syscall::Membarrier,
        arm::STATX => Syscall::Statx,
        arm::RSEQ => Syscall::Rseq,
        arm::CLOCK_GETTIME64 => Syscall::ClockGettime64,
        arm::CLOCK_NANOSLEEP_TIME64 => Syscall::ClockNanosleepTime64,
        arm::PPOLL_TIME64 => Syscall::PpollTime64,
        arm::FUTEX_TIME64 => Syscall::FutexTime64,
        arm::CLONE3 => Syscall::Clone3,
        arm::OPENAT2 => Syscall::Openat2,
        arm::FACCESSAT2 => Syscall::Faccessat2,
        arm::ARM_CACHEFLUSH => Syscall::ArmCacheflush,
        arm::ARM_SET_TLS => Syscall::ArmSetTls,
        _ => return None,
    };
    Some(call)
}
