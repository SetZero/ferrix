#!/usr/bin/env python3
"""Generate ferrousli's system call numbers and errno values from Linux's headers.

    python3 tools/gen-abi.py          # write src/generated/
    python3 tools/gen-abi.py --check  # fail if src/generated/ is stale or wrong

A system call number written from memory dispatches to the wrong call, and
nothing reports it. So every number here is read from the kernel's UAPI
headers: the copies in tools/linux-uapi/, taken from Ubuntu 24.04's
linux-libc-dev 6.8.0-138.138, so that the numbers are the same on every host
and a host with no /usr/include -- Windows -- can generate and check them.
Their README says how to refresh them. The generated Rust is then parsed a second, independent way, and each
constant is looked up in the header again.

To use a new system call, add its name to SYSCALLS and run this.

There is one table per architecture, `src/generated/nr_<arch>.rs`. A call an
architecture does not have is left out of its table, so code that names it
does not compile there -- AArch64 has no `open`, only `openat`. ARMv7-A is
stricter still. It keeps the 32-bit form of every call that passes an offset,
a size, a user id or a time (`lseek`, `fstat`, `getuid`, `clock_gettime`), and
each has a wider twin (`_llseek`, `fstat64`, `getuid32`, `clock_gettime64`)
that a library with 64-bit `off_t` and `time_t` must call instead. The narrow
forms are left out of ARMv7-A's table on purpose, as ARM_NARROW says, and the
wide ones added from ARM_WIDE, so the wrong one cannot be reached by accident.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "src" / "generated"

UAPI = ROOT / "tools" / "linux-uapi"
# Each architecture's header, and the pattern its numbers are written in: a
# generated flat list on the 64-bit ones, and `__NR_SYSCALL_BASE + n` on ARM,
# where the base is zero for the EABI.
UNISTD = {
    "x86_64": UAPI / "x86_64-linux-gnu" / "asm" / "unistd_64.h",
    "aarch64": UAPI / "aarch64-linux-gnu" / "asm" / "unistd_64.h",
    "arm": UAPI / "arm-linux-gnueabihf" / "asm" / "unistd-eabi.h",
}
ARM_PRIVATE = UAPI / "arm-linux-gnueabihf" / "asm" / "unistd.h"
DEFINE_SYSCALL = {
    "x86_64": re.compile(r"^#define __NR_(\w+) (\d+)$", re.M),
    "aarch64": re.compile(r"^#define __NR_(\w+) (\d+)$", re.M),
    "arm": re.compile(r"^#define __NR_(\w+) \(__NR_SYSCALL_BASE \+ (\d+)\)$", re.M),
}
ERRNO_HEADERS = (
    UAPI / "asm-generic" / "errno-base.h",
    UAPI / "asm-generic" / "errno.h",
)


def shown(path: pathlib.Path) -> str:
    """A header's name as the generated files and the errors print it: relative
    to ferrousli/ and with forward slashes, so the output is byte-identical
    whichever host wrote it."""
    return path.relative_to(ROOT).as_posix()
# The aliases C programs use. The headers define a few more for the kernel's
# own use, which a C library does not export.
ERRNO_ALIASES = ("EWOULDBLOCK", "EDEADLOCK")

SYSCALLS = """
read write open close stat fstat lstat poll lseek mmap mprotect munmap brk
rt_sigaction rt_sigprocmask rt_sigreturn ioctl pread64 pwrite64 readv writev
access pipe sched_yield mremap msync madvise dup dup2 pause nanosleep
getitimer alarm setitimer getpid clone fork vfork execve exit wait4 kill uname
fcntl flock fsync fdatasync truncate ftruncate getcwd chdir fchdir rename
mkdir rmdir creat link unlink symlink readlink chmod fchmod chown fchown
lchown umask gettimeofday getrlimit getrusage sysinfo times getuid getgid
setuid setgid geteuid getegid setpgid getppid getpgrp setsid setreuid
setregid getgroups setgroups getpgid getsid rt_sigpending rt_sigtimedwait
rt_sigqueueinfo rt_sigsuspend sigaltstack statfs fstatfs setrlimit sync
arch_prctl gettid tkill futex sched_getaffinity getdents64 set_tid_address
clock_settime clock_gettime clock_getres clock_nanosleep exit_group tgkill
openat mkdirat fchownat newfstatat unlinkat renameat linkat symlinkat
readlinkat fchmodat faccessat pselect6 ppoll utimensat dup3 pipe2 prlimit64
renameat2 getrandom memfd_create execveat statx clone3 faccessat2
epoll_create1 epoll_ctl epoll_pwait epoll_pwait2 eventfd2
fadvise64 fallocate mlock munlock mlockall munlockall getpriority setpriority
waitid preadv pwritev mknodat setresuid setresgid fchmodat2 copy_file_range
sync_file_range
set_robust_list get_robust_list sched_setparam sched_getparam
sched_setscheduler sched_getscheduler sched_get_priority_max
sched_get_priority_min sched_rr_get_interval sched_setaffinity getcpu prctl
rt_tgsigqueueinfo mount umount2 pivot_root chroot swapon swapoff syncfs readahead
socket connect accept accept4 sendto recvfrom sendmsg recvmsg shutdown bind
listen getsockname getpeername socketpair setsockopt getsockopt sendfile
shmget shmat shmctl shmdt semget semop semctl semtimedop msgget msgsnd msgrcv
msgctl capget capset personality setns unshare sethostname setdomainname
reboot syslog clock_adjtime settimeofday inotify_init1 inotify_add_watch
inotify_rm_watch getresuid getresgid splice vmsplice
setxattr lsetxattr fsetxattr getxattr lgetxattr fgetxattr listxattr llistxattr
flistxattr removexattr lremovexattr fremovexattr
""".split()

# ARMv7-A's calls that carry a 32-bit offset, size or time where the library's
# types are 64 bits, or a 16-bit user id. Each has a replacement in ARM_WIDE.
# `mmap`, `getrlimit`, `newfstatat`, `fadvise64` and `sync_file_range` are not
# in ARM's header at all.
ARM_NARROW = """
stat fstat lstat lseek fcntl truncate ftruncate statfs fstatfs sendfile
getuid getgid geteuid getegid setuid setgid setreuid setregid getgroups
setgroups chown fchown lchown setresuid setresgid getresuid getresgid
clock_gettime clock_settime clock_getres clock_nanosleep clock_adjtime futex
ppoll pselect6 rt_sigtimedwait utimensat semtimedop sched_rr_get_interval
gettimeofday settimeofday nanosleep
""".split()

# What ARMv7-A calls instead, and the calls it has that the others do not.
ARM_WIDE = """
mmap2 _llseek fcntl64 truncate64 ftruncate64 fstat64 fstatat64 statfs64
fstatfs64 sendfile64 ugetrlimit getuid32 getgid32 geteuid32 getegid32
setuid32 setgid32 setreuid32 setregid32 getgroups32 setgroups32 chown32
fchown32 lchown32 setresuid32 setresgid32 getresuid32 getresgid32
clock_gettime64 clock_settime64 clock_getres_time64 clock_nanosleep_time64
clock_adjtime64 futex_time64 ppoll_time64 pselect6_time64
rt_sigtimedwait_time64 utimensat_time64 semtimedop_time64
sched_rr_get_interval_time64 arm_fadvise64_64 arm_sync_file_range sigreturn
""".split()

# The narrow calls whose wide form takes the very arguments ferrousli passes
# the narrow one: its `time_t` is 64 bits, so its `struct timespec` is the
# kernel's `__kernel_timespec` (the padding after a 32-bit `tv_nsec` is
# ignored on reading, and zero on writing), and its ids are 32 bits. ARMv7-A's
# table names the wide call under the narrow name, so the call sites are the
# same on every architecture. Every other narrow call is left out: its wide
# form passes something differently -- a 64-bit offset in a pair of
# registers, an offset counted in pages, a structure of another shape -- and
# the call site must say so.
ARM_SAME_ARGUMENTS = {
    "futex": "futex_time64",
    "clock_gettime": "clock_gettime64",
    "clock_settime": "clock_settime64",
    "clock_getres": "clock_getres_time64",
    "clock_nanosleep": "clock_nanosleep_time64",
    "ppoll": "ppoll_time64",
    "pselect6": "pselect6_time64",
    "rt_sigtimedwait": "rt_sigtimedwait_time64",
    "utimensat": "utimensat_time64",
    "semtimedop": "semtimedop_time64",
    "sched_rr_get_interval": "sched_rr_get_interval_time64",
    "getuid": "getuid32",
    "getgid": "getgid32",
    "geteuid": "geteuid32",
    "getegid": "getegid32",
    "setuid": "setuid32",
    "setgid": "setgid32",
    "setreuid": "setreuid32",
    "setregid": "setregid32",
    "getgroups": "getgroups32",
    "setgroups": "setgroups32",
    "chown": "chown32",
    "fchown": "fchown32",
    "lchown": "lchown32",
    "setresuid": "setresuid32",
    "setresgid": "setresgid32",
    "getresuid": "getresuid32",
    "getresgid": "getresgid32",
    "fcntl": "fcntl64",
    "sendfile": "sendfile64",
}

# ARM's private calls, `__ARM_NR_BASE + n`, which `unistd.h` defines itself.
# They are `ARM_SET_TLS` and `ARM_CACHEFLUSH` in the table.
ARM_PRIVATE_CALLS = ("set_tls", "cacheflush")
ARM_PRIVATE_BASE = re.compile(
    r"^#define __ARM_NR_BASE\s+\(__NR_SYSCALL_BASE\+0x([0-9a-f]+)\)$", re.M
)

DEFINE_ERRNO = re.compile(
    r"^#define\s+(E[A-Z0-9]+)\s+(\d+)\s*(?:/\*\s*(.*?)\s*\*/)?\s*$", re.M
)
DEFINE_ALIAS = re.compile(r"^#define\s+(E[A-Z0-9]+)\s+(E[A-Z0-9]+)\b", re.M)


def const(name: str) -> str:
    """The Rust name for a call: `_llseek` is `LLSEEK`."""
    return name.lstrip("_").upper()


def arm_private() -> dict[str, int]:
    """ARM's private calls, read from `unistd.h`. `__ARM_NR_BASE` is
    `__NR_SYSCALL_BASE + 0x0f0000`, and the base is zero for the EABI."""
    text = ARM_PRIVATE.read_text(encoding="utf-8")
    base = ARM_PRIVATE_BASE.search(text)
    if not base:
        sys.exit(f"gen-abi: no __ARM_NR_BASE in {shown(ARM_PRIVATE)}")
    calls = {}
    for name in ARM_PRIVATE_CALLS:
        found = re.search(rf"^#define __ARM_NR_{name}\s+\(__ARM_NR_BASE\+(\d+)\)$", text, re.M)
        if not found:
            sys.exit(f"gen-abi: no __ARM_NR_{name} in {shown(ARM_PRIVATE)}")
        calls[f"arm_{name}"] = int(base[1], 16) + int(found[1])
    return calls


def syscalls(arch: str) -> tuple[list[tuple[str, int]], list[str]]:
    """An architecture's table, and the calls it was asked for but lacks."""
    for names in (SYSCALLS, ARM_NARROW, ARM_WIDE):
        if len(set(names)) != len(names):
            sys.exit("gen-abi: a system call is listed twice")
    for narrow, wide in ARM_SAME_ARGUMENTS.items():
        if narrow not in ARM_NARROW or wide not in ARM_WIDE:
            sys.exit(f"gen-abi: ARM_SAME_ARGUMENTS pairs {narrow} with {wide}, not narrow with wide")
    header = UNISTD[arch]
    numbers = dict(DEFINE_SYSCALL[arch].findall(header.read_text(encoding="utf-8")))
    wanted = SYSCALLS
    if arch == "arm":
        missing = [name for name in ARM_NARROW + ARM_WIDE if name not in numbers]
        if missing:
            sys.exit(f"gen-abi: not in {shown(header)}: {' '.join(missing)}")
        wanted = [name for name in SYSCALLS if name not in ARM_NARROW] + ARM_WIDE
    table = [(name, int(numbers[name])) for name in wanted if name in numbers]
    absent = [name for name in wanted if name not in numbers]
    if arch == "arm":
        table += list(arm_private().items())
        absent = [name for name in ARM_NARROW if name not in ARM_SAME_ARGUMENTS] + absent
    return table, absent


def render_nr(arch: str, table: list[tuple[str, int]], absent: list[str]) -> str:
    sources = shown(UNISTD[arch])
    if arch == "arm":
        sources += f" and {shown(ARM_PRIVATE)}"
    lines = [f"// Generated by tools/gen-abi.py from {sources}. Do not edit.", ""]
    if absent:
        lines.append(f"// Not in this table, because {arch} lacks them or has wider forms:")
        text = "//"
        for name in absent:
            if len(text) + 1 + len(name) > 80:
                lines.append(text)
                text = "//"
            text += f" {name}"
        lines += [text, ""]
    for name, number in table:
        lines.append(f"/// `{name}`.")
        lines.append(f"pub const {const(name)}: usize = {number};")
    if arch == "arm":
        lines += ["", "// The narrow names of wide calls that take the same arguments here."]
        for narrow, wide in ARM_SAME_ARGUMENTS.items():
            lines.append(f"/// `{narrow}`: [`{const(wide)}`], which takes its arguments here.")
            lines.append(f"pub const {const(narrow)}: usize = {const(wide)};")
    return "\n".join(lines) + "\n"


def errnos() -> tuple[list[tuple[str, int, str]], list[tuple[str, str]]]:
    values: list[tuple[str, int, str]] = []
    aliases: list[tuple[str, str]] = []
    for header in ERRNO_HEADERS:
        text = header.read_text(encoding="utf-8")
        values += [(m[1], int(m[2]), m[3] or "") for m in DEFINE_ERRNO.finditer(text)]
        aliases += [
            (m[1], m[2]) for m in DEFINE_ALIAS.finditer(text) if m[1] in ERRNO_ALIASES
        ]
    return values, aliases


def render_errno(values: list[tuple[str, int, str]], aliases: list[tuple[str, str]]) -> str:
    sources = " and ".join(shown(header) for header in ERRNO_HEADERS)
    lines = [f"// Generated by tools/gen-abi.py from {sources}. Do not edit.", ""]
    for name, value, comment in values:
        text = comment.rstrip(".")
        lines.append(f"/// `{name}`: {text}." if text else f"/// `{name}`.")
        lines.append(f"pub const {name}: c_int = {value};")
    for name, target in aliases:
        lines.append(f"/// `{name}`: the same as [`{target}`].")
        lines.append(f"pub const {name}: c_int = {target};")
    return "\n".join(lines) + "\n"


def check_back(nr_texts: dict[str, str], errno_text: str) -> list[str]:
    """Parse the generated Rust and look every constant up in the headers."""
    problems = []
    for arch, nr_text in nr_texts.items():
        unistd = UNISTD[arch].read_text(encoding="utf-8")
        private = ARM_PRIVATE.read_text(encoding="utf-8") if arch == "arm" else ""
        consts = re.findall(r"pub const ([A-Z0-9_]+): usize = (\d+);", nr_text)
        docs = re.findall(r"^/// `(\w+)`\.$", nr_text, re.M)
        if len(docs) != len(consts):
            problems.append(f"nr_{arch}.rs does not document every constant")
        for doc, (name, value) in zip(docs, consts):
            where = shown(UNISTD[arch])
            if const(doc) != name:
                problems.append(f"{arch} nr::{name} is documented as `{doc}`")
            elif arch == "arm" and doc in ARM_NARROW:
                problems.append(f"arm nr::{name} is a narrow form, which ARM_NARROW leaves out")
            elif arch == "arm" and doc.startswith("arm_") and doc[4:] in ARM_PRIVATE_CALLS:
                found = re.search(
                    rf"^#define __ARM_NR_{doc[4:]}\s+\(__ARM_NR_BASE\+(\d+)\)$", private, re.M
                )
                # The EABI's base, spelled out rather than read, so this check
                # does not share arm_private()'s parsing.
                if not found or 0x0F0000 + int(found[1]) != int(value):
                    problems.append(f"arm nr::{name} = {value} is not in {shown(ARM_PRIVATE)}")
            elif arch == "arm":
                if not re.search(
                    rf"^#define __NR_{doc} \(__NR_SYSCALL_BASE \+ {value}\)$", unistd, re.M
                ):
                    problems.append(f"arm nr::{name} = {value} is not in {where}")
            elif not re.search(rf"^#define __NR_{doc} {value}$", unistd, re.M):
                problems.append(f"{arch} nr::{name} = {value} is not in {where}")
        pairs = {const(n): const(w) for n, w in ARM_SAME_ARGUMENTS.items()}
        for narrow, wide in re.findall(
            r"^pub const ([A-Z0-9_]+): usize = ([A-Z][A-Z0-9_]*);$", nr_text, re.M
        ):
            if arch != "arm" or pairs.get(narrow) != wide:
                problems.append(f"{arch} nr::{narrow} = {wide} is not an ARM_SAME_ARGUMENTS pair")
    headers = "\n".join(header.read_text(encoding="utf-8") for header in ERRNO_HEADERS)
    for name, value in re.findall(r"pub const (E[A-Z0-9]+): c_int = (\d+);", errno_text):
        if not re.search(rf"^#define\s+{name}\s+{value}\b", headers, re.M):
            problems.append(f"{name} = {value} is not in the errno headers")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--check", action="store_true", help="fail if src/generated/ is stale or wrong"
    )
    args = parser.parse_args()

    nr_texts = {arch: render_nr(arch, *syscalls(arch)) for arch in UNISTD}
    errno_text = render_errno(*errnos())
    files = {OUT / f"nr_{arch}.rs": text for arch, text in nr_texts.items()}
    files[OUT / "errno.rs"] = errno_text

    problems = check_back(nr_texts, errno_text)
    if args.check:
        for path, text in files.items():
            if not path.exists() or path.read_text(encoding="utf-8") != text:
                problems.append(f"{shown(path)} is stale: run tools/gen-abi.py")
    elif not problems:
        OUT.mkdir(exist_ok=True)
        for path, text in files.items():
            # LF on every host: Windows would otherwise write CRLF, which the
            # line-ending gate refuses.
            path.write_text(text, encoding="utf-8", newline="\n")

    for problem in problems:
        print(f"gen-abi: {problem}", file=sys.stderr)
    if not problems:
        counts = ", ".join(f"{text.count('pub const')} on {arch}" for arch, text in nr_texts.items())
        print(f"gen-abi: system calls {counts}; {errno_text.count('pub const')} errno values")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
