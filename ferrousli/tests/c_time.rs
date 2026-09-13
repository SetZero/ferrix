//! The C programs that exercise `time.h` beyond the clocks: calendar time
//! and time zones, and `sys/times.h`.
//!
//! The zone tests read the host's `/usr/share/zoneinfo`. Their expected values
//! are written into the programs, not taken from the host at run time.
//!
//! The harness is in `tests/common`.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a test reports failure by panicking"
)]

mod common;

use std::path::Path;
use std::sync::OnceLock;

use common::{Case, check};

/// Where the damaged zone files are written, read by `corrupt.c` through
/// `TZDIR`.
const ZONES: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/c_time_zones");

/// The big-endian `u32` at `at`.
fn be32(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes")) as usize
}

/// Writes copies of the host's `Europe/Berlin`, intact and damaged in each of
/// the ways `corrupt.c` names, into [`ZONES`].
fn write_zones() {
    static DONE: OnceLock<()> = OnceLock::new();
    let () = *DONE.get_or_init(|| {
        let good = std::fs::read("/usr/share/zoneinfo/Europe/Berlin")
            .expect("the host's Europe/Berlin zone file");
        assert!(good.starts_with(b"TZif2") || good.starts_with(b"TZif3"));
        let dir = Path::new(ZONES);
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir.join("Dir")).expect("create the zone directory");

        // The layout of the version 2 data, from the two headers.
        let v1_len = be32(&good, 32) * 5
            + be32(&good, 36) * 6
            + be32(&good, 40)
            + be32(&good, 28) * 8
            + be32(&good, 24)
            + be32(&good, 20);
        let second = 44 + v1_len;
        let times = be32(&good, second + 32);
        let types = be32(&good, second + 36);
        assert!(times >= 2 && types >= 1);
        let times_at = second + 44;
        let kinds_at = times_at + times * 8;
        let types_at = kinds_at + times;
        let footer_at = good[..good.len() - 1]
            .iter()
            .rposition(|&byte| byte == b'\n')
            .expect("a footer");

        let edit = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = good.clone();
            f(&mut bytes);
            bytes
        };
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("Valid", good.clone()),
            ("Empty", Vec::new()),
            ("Short", good[..20].to_vec()),
            ("HeaderOnly", good[..44].to_vec()),
            ("Truncated", good[..good.len() / 2].to_vec()),
            ("NoNewline", good[..good.len() - 1].to_vec()),
            ("BadMagic", edit(&|b| b[3] = b'g')),
            ("BadVersion", edit(&|b| b[4] = b'1')),
            (
                "HugeCount",
                edit(&|b| b[second + 32..second + 36].copy_from_slice(&[0xff; 4])),
            ),
            ("BadType", edit(&|b| b[kinds_at + times / 2] = 0xff)),
            (
                "Unsorted",
                edit(&|b| {
                    let (first, rest) = b[times_at..].split_at_mut(8);
                    first.swap_with_slice(&mut rest[..8]);
                }),
            ),
            ("BadAbbrev", edit(&|b| b[types_at + 5] = 0xff)),
            (
                "BadOffset",
                edit(&|b| b[types_at..types_at + 4].copy_from_slice(&[0x7f, 0xff, 0xff, 0xff])),
            ),
            (
                "BadFooter",
                edit(&|b| {
                    b.truncate(footer_at);
                    b.extend_from_slice(b"\nnot a zone!\n");
                }),
            ),
            ("TrailingData", edit(&|b| b.extend_from_slice(b"junk"))),
        ];
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).expect("write a zone file");
        }
    });
}

#[test]
fn gmtime_timegm_asctime_and_difftime_in_utc() {
    check(&Case {
        env: &[("TZ", "UTC0")],
        ..Case::named("time/gmtime")
    });
}

#[test]
fn local_time_in_posix_strings_and_zone_files() {
    check(&Case::named("time/zones"));
}

#[test]
fn a_damaged_zone_file_gives_utc() {
    write_zones();
    check(&Case {
        env: &[("TZDIR", ZONES)],
        ..Case::named("time/corrupt")
    });
}

#[test]
fn clock_and_times_report_cpu_time() {
    check(&Case::named("time/cpu"));
}
