//! Host tests: every format against lines a real Linux printed.
//!
//! The fixtures were copied from `/proc` on a Linux 7.0 x86-64 machine by a
//! script and are literals here, so these tests read nothing from the host
//! they run on. A fixture that says `derived` is not a copy: no 32-bit Linux
//! was at hand, and the expected bytes follow the rule `fs/proc/task_mmu.c`
//! states, which the 64-bit copies confirm.

use alloc::vec::Vec;

use crate::kstat::{self, CpuTimes, Kstat};
use crate::maps::{self, Mapping, Width};
use crate::meminfo::{self, Meminfo};
use crate::mounts::{self, Mount};
use crate::stat::{self, Stat};
use crate::status::{self, State, Status};

fn rendered(render: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();
    render(&mut out);
    out
}

fn show(bytes: &[u8]) -> alloc::string::String {
    alloc::string::String::from_utf8_lossy(bytes).into_owned()
}

/// (line as printed, the region it describes)
const HOST_MAPS: &[(&[u8], Mapping<'static>)] = &[
    // text
    (
        b"00422000-007a8000 r-xp 00022000 fc:00 2884149                            /usr/bin/python3.14",
        Mapping {
            start: 0x00422000,
            end: 0x007a8000,
            read: true,
            write: false,
            execute: true,
            shared: false,
            offset: 0x00022000,
            major: 0xfc,
            minor: 0x00,
            inode: 2884149,
            name: Some(b"/usr/bin/python3.14"),
        },
    ),
    // shared
    (
        b"7c87fc8da000-7c87fc8e1000 r--s 00000000 fc:00 2900624                    /usr/lib/x86_64-linux-gnu/gconv/gconv-modules.cache",
        Mapping {
            start: 0x7c87fc8da000,
            end: 0x7c87fc8e1000,
            read: true,
            write: false,
            execute: false,
            shared: true,
            offset: 0x00000000,
            major: 0xfc,
            minor: 0x00,
            inode: 2900624,
            name: Some(b"/usr/lib/x86_64-linux-gnu/gconv/gconv-modules.cache"),
        },
    ),
    // anonymous
    (
        b"00b22000-00b94000 rw-p 00000000 00:00 0 ",
        Mapping {
            start: 0x00b22000,
            end: 0x00b94000,
            read: true,
            write: true,
            execute: false,
            shared: false,
            offset: 0x00000000,
            major: 0x00,
            minor: 0x00,
            inode: 0,
            name: None,
        },
    ),
    // heap
    (
        b"03874000-03a78000 rw-p 00000000 00:00 0                                  [heap]",
        Mapping {
            start: 0x03874000,
            end: 0x03a78000,
            read: true,
            write: true,
            execute: false,
            shared: false,
            offset: 0x00000000,
            major: 0x00,
            minor: 0x00,
            inode: 0,
            name: Some(b"[heap]"),
        },
    ),
    // stack
    (
        b"7fff81e31000-7fff81e53000 rw-p 00000000 00:00 0                          [stack]",
        Mapping {
            start: 0x7fff81e31000,
            end: 0x7fff81e53000,
            read: true,
            write: true,
            execute: false,
            shared: false,
            offset: 0x00000000,
            major: 0x00,
            minor: 0x00,
            inode: 0,
            name: Some(b"[stack]"),
        },
    ),
];

#[test]
fn a_maps_line_is_byte_for_byte_what_linux_printed() {
    for (line, mapping) in HOST_MAPS {
        let out = rendered(|out| maps::render(out, mapping, Width::Bits64));
        let mut expected = line.to_vec();
        expected.push(b'\n');
        assert_eq!(show(&out), show(&expected), "rendering {mapping:?}");
    }
}

#[test]
fn a_maps_line_reads_back_as_the_region_it_describes() {
    for (line, mapping) in HOST_MAPS {
        assert_eq!(
            maps::parse(line).as_ref(),
            Some(mapping),
            "parsing {}",
            show(line)
        );
    }
}

#[test]
fn a_short_address_on_a_64_bit_kernel_pads_to_the_same_column() {
    let mapping = Mapping {
        start: 0x40_0000,
        end: 0x42_2000,
        read: true,
        write: false,
        execute: false,
        shared: false,
        offset: 0,
        major: 0xfc,
        minor: 0,
        inode: 2_884_149,
        name: Some(b"/usr/bin/python3.14"),
    };
    let out = rendered(|out| maps::render(out, &mapping, Width::Bits64));
    assert_eq!(
        show(&out),
        "00400000-00422000 r--p 00000000 fc:00 2884149                            /usr/bin/python3.14\n",
        "copied from the host, where this binary is not position-independent"
    );
}

#[test]
fn a_32_bit_kernel_puts_the_name_at_byte_49() {
    // Derived: 25 + 4 * 6 - 1 = 48 bytes of padding, then a space.
    let mapping = Mapping {
        start: 0x0001_0000,
        end: 0x000a_1000,
        read: true,
        write: false,
        execute: true,
        shared: false,
        offset: 0,
        major: 0,
        minor: 0,
        inode: 0,
        name: Some(b"[stack]"),
    };
    let out = rendered(|out| maps::render(out, &mapping, Width::Bits32));
    assert_eq!(
        show(&out),
        "00010000-000a1000 r-xp 00000000 00:00 0          [stack]\n",
        "the 32-bit column"
    );
    assert_eq!(
        out.iter().position(|&b| b == b'['),
        Some(49),
        "the name column"
    );
}

#[test]
fn a_newline_in_a_mapped_file_name_is_escaped_and_nothing_else_is() {
    let mapping = Mapping {
        name: Some(b"/tmp/a b\nc"),
        ..HOST_MAPS[0].1
    };
    let out = rendered(|out| maps::render(out, &mapping, Width::Bits64));
    assert!(out.ends_with(b" /tmp/a b\\012c\n"), "{}", show(&out));
    assert_eq!(out.iter().filter(|&&b| b == b'\n').count(), 1, "one line");
}

#[test]
fn the_maps_parser_refuses_what_the_renderer_cannot_produce() {
    let refused: [&[u8]; 7] = [
        b"",
        b"00400000-00422000 r--p 00000000 fc:00 2884149",
        b"00400000-00422000 r--q 00000000 fc:00 2884149 ",
        b"0040000-00422000 r--p 00000000 fc:00 2884149 ",
        b"00400000-00422000 r--p 00000000 fc:0 2884149 ",
        b"00400000-00422000 r--p 00000000 FC:00 2884149 ",
        b"00400000-00422000 r--p 00000000 fc:00 +2884149 ",
    ];
    for line in refused {
        assert_eq!(maps::parse(line), None, "{}", show(line));
    }
}

#[test]
fn meminfo_is_byte_for_byte_what_linux_printed() {
    let info = Meminfo {
        total: 62103444,
        free: 768736,
        available: 34850072,
        buffers: 2613488,
        cached: 37462144,
        swap_cached: 538760,
        swap_total: 8388604,
        swap_free: 4308344,
        slab: 3343592,
    };
    let out = rendered(|out| meminfo::render(out, &info));
    assert_eq!(show(&out), show(b"MemTotal:       62103444 kB\nMemFree:          768736 kB\nMemAvailable:   34850072 kB\nBuffers:         2613488 kB\nCached:         37462144 kB\nSwapCached:       538760 kB\nSwapTotal:       8388604 kB\nSwapFree:        4308344 kB\nSlab:            3343592 kB\n"), "meminfo");
}

#[test]
fn a_meminfo_value_wider_than_eight_pushes_the_unit_along() {
    let out = rendered(|out| meminfo::line(out, "VmallocTotal", 34359738367));
    assert_eq!(
        show(&out),
        show(b"VmallocTotal:   34359738367 kB\n"),
        "a wide value"
    );
}

#[test]
fn status_is_byte_for_byte_what_linux_printed() {
    let status = Status {
        name: b"python3",
        umask: 0o0002,
        state: State::Running,
        pid: 457743,
        ppid: 457739,
        uid: 1000,
        gid: 1000,
        fd_size: 64,
        vm_size: 20428,
        vm_locked: 0,
        vm_data: 6724,
        vm_stack: 136,
        threads: 1,
        cpus: 24,
    };
    let out = rendered(|out| status::render(out, &status));
    // Groups is copied from pid 1, which runs as root with no supplementary
    // groups — and still ends the line with the space `array.c` apologises
    // for.
    assert_eq!(show(&out), show(b"Name:\tpython3\nUmask:\t0002\nState:\tR (running)\nTgid:\t457743\nNgid:\t0\nPid:\t457743\nPPid:\t457739\nTracerPid:\t0\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\nFDSize:\t64\nGroups:\t \nNStgid:\t457743\nNSpid:\t457743\nVmSize:\t   20428 kB\nVmLck:\t       0 kB\nVmData:\t    6724 kB\nVmStk:\t     136 kB\nThreads:\t1\nCpus_allowed:\tffffff\nCpus_allowed_list:\t0-23\n"), "status");
}

#[test]
fn a_status_name_escapes_newline_and_backslash_only() {
    let status = Status {
        name: b"a\\b\nc\td",
        umask: 0o22,
        state: State::Sleeping,
        pid: 1,
        ppid: 0,
        uid: 0,
        gid: 0,
        fd_size: 64,
        vm_size: 0,
        vm_locked: 0,
        vm_data: 0,
        vm_stack: 0,
        threads: 1,
        cpus: 1,
    };
    let out = rendered(|out| status::render(out, &status));
    assert!(
        out.starts_with(b"Name:\ta\\\\b\\nc\td\nUmask:\t0022\nState:\tS (sleeping)\n"),
        "{}",
        show(&out)
    );
}

#[test]
fn cpu_masks_group_in_32_bit_chunks_and_lists_are_ranges() {
    let cases: [(u32, &str, &str); 6] = [
        (1, "1", "0"),
        (2, "3", "0-1"),
        (4, "f", "0-3"),
        (24, "ffffff", "0-23"),
        (32, "ffffffff", "0-31"),
        (36, "f,ffffffff", "0-35"),
    ];
    for (cpus, mask, list) in cases {
        assert_eq!(
            show(&rendered(|out| status::cpu_mask(out, cpus))),
            mask,
            "mask of {cpus}"
        );
        assert_eq!(
            show(&rendered(|out| status::cpu_list(out, cpus))),
            list,
            "list of {cpus}"
        );
    }
}

fn a_stat() -> Stat<'static> {
    Stat {
        pid: 7,
        comm: b"cat",
        state: State::Running,
        ppid: 1,
        pgrp: 7,
        session: 7,
        tty_nr: 0,
        tpgid: -1,
        flags: 0,
        utime: 3,
        stime: 4,
        priority: 20,
        nice: 0,
        threads: 1,
        start_time: 460_536,
        vsize: 16_637_952,
        rss: 1824,
        rss_limit: u64::MAX,
        start_code: 0x40_0000,
        end_code: 0x42_2000,
        start_stack: 0x7fff_f000,
        pending: 0,
        blocked: 1,
        ignored: 2,
        caught: 4,
        exit_signal: 17,
        processor: 3,
        start_brk: 0x50_0000,
        arg_start: 11,
        arg_end: 12,
        env_start: 13,
        env_end: 14,
    }
}

#[test]
fn stat_has_every_field_in_its_place() {
    let out = show(&rendered(|out| stat::render(out, &a_stat())));
    assert_eq!(
        out,
        "7 (cat) R 1 7 7 0 -1 0 0 0 0 0 3 4 0 0 20 0 1 0 460536 16637952 1824 \
         18446744073709551615 4194304 4333568 2147479552 0 0 0 1 2 4 0 0 0 17 3 0 0 0 0 0 0 0 \
         5242880 11 12 13 14 0\n",
        "stat"
    );
    // As many fields as this machine's Linux printed for itself.
    let after_comm = out.rsplit_once(") ").map(|(_, rest)| rest).unwrap_or("");
    assert_eq!(after_comm.split_whitespace().count() + 2, 52, "field count");
}

#[test]
fn a_parenthesis_in_the_command_name_is_why_readers_find_the_last_one() {
    let odd = Stat {
        comm: b"a) b",
        ..a_stat()
    };
    let out = show(&rendered(|out| stat::render(out, &odd)));
    assert!(out.starts_with("7 (a) b) R 1 "), "{out}");
}

#[test]
fn a_mounts_line_is_what_linux_printed_and_escapes_what_would_split_it() {
    let host = Mount {
        source: b"proc",
        point: b"/proc",
        fstype: b"proc",
        options: b"rw,nosuid,nodev,noexec,relatime",
    };
    let out = rendered(|out| mounts::render(out, &host));
    assert_eq!(
        show(&out),
        show(b"proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n"),
        "the host's /proc"
    );

    let awkward = Mount {
        source: b"tmpfs",
        point: b"/mnt/a b\\c\td\ne",
        fstype: b"tmpfs",
        options: b"rw",
    };
    let out = rendered(|out| mounts::render(out, &awkward));
    assert_eq!(
        show(&out),
        "tmpfs /mnt/a\\040b\\134c\\011d\\012e tmpfs rw 0 0\n",
        "escapes"
    );
}

/// A host's `/proc/stat` with `cpu2` to `cpu23` left out, and the interrupt
/// counts after the thirty-eighth: every line kept is as printed, and the
/// `intr` line is what Linux prints for a machine with that many interrupts.
const HOST_KSTAT: &str = "cpu  9076855 270647 1436233 94650326 308715 0 31227 0 169401 437\n\
cpu0 425646 12091 77968 3850347 14446 0 12100 0 11352 6\n\
cpu1 657376 15596 82034 3625601 13139 0 3049 0 5940 3\n\
intr 1540466352 127 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 1 3291 1 1 1 1 0 0 0 1\n\
ctxt 3405985925\n\
btime 1789246016\n\
processes 2384717\n\
procs_running 6\n\
procs_blocked 0\n\
softirq 256823522 47856483 13436684 9923 12532756 236740 0 353061 83875241 911 98521723\n";

/// The host's per-interrupt counts, as numbers.
fn host_interrupts() -> Vec<u64> {
    HOST_KSTAT
        .lines()
        .find_map(|line| line.strip_prefix("intr "))
        .unwrap_or("")
        .split(' ')
        .skip(1)
        .map(|word| word.parse().unwrap_or(u64::MAX))
        .collect()
}

fn host_cpus() -> [(u32, CpuTimes); 2] {
    [
        (
            0,
            CpuTimes::from_fields([425646, 12091, 77968, 3850347, 14446, 0, 12100, 0, 11352, 6]),
        ),
        (
            1,
            CpuTimes::from_fields([657376, 15596, 82034, 3625601, 13139, 0, 3049, 0, 5940, 3]),
        ),
    ]
}

fn a_host_kstat<'a>(cpus: &'a [(u32, CpuTimes)], interrupts: &'a [u64]) -> Kstat<'a> {
    Kstat {
        total: CpuTimes::from_fields([
            9076855, 270647, 1436233, 94650326, 308715, 0, 31227, 0, 169401, 437,
        ]),
        cpus,
        interrupts: 1540466352,
        per_interrupt: interrupts,
        context_switches: 3405985925,
        boot_time: 1789246016,
        processes: 2384717,
        running: 6,
        blocked: 0,
        softirqs: 256823522,
        per_softirq: [
            47856483, 13436684, 9923, 12532756, 236740, 0, 353061, 83875241, 911, 98521723,
        ],
    }
}

#[test]
fn kstat_is_byte_for_byte_what_linux_printed() {
    let cpus = host_cpus();
    let interrupts = host_interrupts();
    assert_eq!(interrupts.len(), 38, "the fixture's interrupt counts");
    let out = rendered(|out| kstat::render(out, &a_host_kstat(&cpus, &interrupts)));
    assert_eq!(show(&out), HOST_KSTAT);
}

#[test]
fn kstat_reads_back_as_what_it_was_given() {
    let cpus = host_cpus();
    let parsed = kstat::parse(HOST_KSTAT.as_bytes());
    let parsed = parsed.as_ref();
    assert_eq!(parsed.map(|p| p.cpus.as_slice()), Some(cpus.as_slice()));
    assert_eq!(parsed.map(|p| p.total.idle), Some(94650326));
    assert_eq!(parsed.map(|p| p.interrupts), Some(1540466352));
    assert_eq!(parsed.map(|p| p.context_switches), Some(3405985925));
    assert_eq!(parsed.map(|p| p.boot_time), Some(1789246016));
    assert_eq!(parsed.map(|p| p.processes), Some(2384717));
    assert_eq!(parsed.map(|p| p.running), Some(6));
    assert_eq!(parsed.map(|p| p.blocked), Some(0));

    // With no per-interrupt counts, which is what a kernel that keeps none
    // prints: the total alone, and still a file that reads back.
    let bare = Kstat {
        per_interrupt: &[],
        per_softirq: [0; kstat::SOFTIRQS],
        softirqs: 0,
        ..a_host_kstat(&cpus, &[])
    };
    let out = show(&rendered(|out| kstat::render(out, &bare)));
    assert!(
        out.contains("\nintr 1540466352\nctxt 3405985925\n"),
        "{out}"
    );
    assert!(out.ends_with("\nsoftirq 0 0 0 0 0 0 0 0 0 0 0\n"), "{out}");
    assert_eq!(
        kstat::parse(out.as_bytes()).map(|p| p.interrupts),
        Some(1540466352)
    );
}

#[test]
fn the_kstat_parser_refuses_what_the_renderer_cannot_produce() {
    let refused = [
        (
            "one space after cpu",
            HOST_KSTAT.replacen("cpu  ", "cpu ", 1),
        ),
        (
            "nine values",
            HOST_KSTAT.replacen(" 11352 6\n", " 11352\n", 1),
        ),
        (
            "eleven values",
            HOST_KSTAT.replacen(" 11352 6\n", " 11352 6 7\n", 1),
        ),
        ("a repeated label", HOST_KSTAT.replacen("ctxt", "btime", 1)),
        ("an unknown label", HOST_KSTAT.replacen("ctxt", "cxtt", 1)),
        (
            "no softirq line",
            HOST_KSTAT.replacen("softirq", "procs_blocked", 1),
        ),
        (
            "a word for a number",
            HOST_KSTAT.replacen("processes 2384717", "processes many", 1),
        ),
        (
            "a counter with two",
            HOST_KSTAT.replacen("ctxt 3405985925", "ctxt 3405 985925", 1),
        ),
        ("no total", HOST_KSTAT.replacen("cpu  ", "cpu9 ", 1)),
    ];
    for (why, text) in refused {
        assert_ne!(text, HOST_KSTAT, "{why}: the fixture did not change");
        assert_eq!(kstat::parse(text.as_bytes()), None, "{why}");
    }
}
