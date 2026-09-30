//! The parsers against the kernel's own text, and the layout byte for byte.

extern crate std;

use std::vec::Vec;
use std::{format, vec};

use crate::Text;
use crate::logo;
use crate::parse::{self, Command, Cpu, Dirents, Loadavg, Memory, Options};
use crate::render::{self, Display, Facts};

/// `render` into text.
fn rendered(facts: &Facts, options: Options) -> Text<8192> {
    let mut out = Text::new();
    render::render(&mut out, facts, options).expect("the output fits");
    out
}

/// The facts of a machine like a QEMU guest's.
fn guest() -> Facts {
    let mut facts = Facts {
        user: Text::from("ferrix"),
        host: Text::from("ferrix"),
        os: Text::from("Ferrix 0.1.0"),
        machine: Text::from("x86_64"),
        kernel: Text::from("Ferrix 6.1.0-ferrix"),
        uptime: Some(3 * 3600 + 25 * 60 + 9),
        processes: Some(42),
        shell: Text::from("zinc"),
        terminal: Text::from("hyprix-term"),
        cpu: Some(Cpu {
            count: 4,
            khz: Some(2_995_198),
        }),
        memory: Some(Memory {
            total_kib: 2 * 1024 * 1024,
            available_kib: 1024 * 1024 + 512 * 1024,
        }),
        load: Text::from("0.08 0.03 0.01"),
        ..Facts::default()
    };
    facts.displays[0] = Some(Display {
        connector: Text::from("Virtual-1"),
        width: 1280,
        height: 800,
    });
    facts
}

const PLAIN: Options = Options {
    logo: false,
    color: false,
};

#[test]
fn the_lines_are_fastfetchs_in_its_order() {
    let out = rendered(&guest(), PLAIN);
    assert_eq!(
        out.as_str(),
        "ferrix@ferrix\n\
         -------------\n\
         OS: Ferrix 0.1.0 x86_64\n\
         Kernel: Ferrix 6.1.0-ferrix\n\
         Uptime: 3 hours, 25 mins\n\
         Processes: 42\n\
         Shell: zinc\n\
         Terminal: hyprix-term\n\
         Display (Virtual-1): 1280x800\n\
         CPU: x86_64 (4) @ 2.99 GHz\n\
         Memory: 512 MiB / 2.00 GiB (25%)\n\
         Load: 0.08 0.03 0.01\n"
    );
}

#[test]
fn a_fact_not_found_has_no_line() {
    let facts = Facts {
        host: Text::from("ferrix"),
        os: Text::from("Ferrix 0.1.0"),
        cpu: Some(Cpu {
            count: 2,
            khz: None,
        }),
        ..Facts::default()
    };
    assert_eq!(
        rendered(&facts, PLAIN).as_str(),
        "ferrix\n------\nOS: Ferrix 0.1.0\nCPU: (2)\n"
    );
}

#[test]
fn the_mark_stands_beside_the_lines_and_runs_on_below_them() {
    let facts = Facts {
        host: Text::from("h"),
        ..Facts::default()
    };
    let out = rendered(
        &facts,
        Options {
            logo: true,
            color: false,
        },
    );
    let lines: Vec<&str> = out.as_str().lines().collect();
    assert_eq!(lines.len(), logo::ROWS.len());
    assert_eq!(lines[0], format!("{:<23}   h", logo::ROWS[0]));
    assert_eq!(lines[1], format!("{:<23}   -", logo::ROWS[1]));
    // Past the column, the mark's own rows, with no padding after them.
    assert_eq!(&lines[2..], &logo::ROWS[2..]);
}

#[test]
fn the_marks_width_is_its_widest_row() {
    let widest = logo::ROWS.iter().map(|row| row.len()).max();
    assert_eq!(widest, Some(logo::WIDTH));
    assert!(logo::ROWS.iter().all(|row| row.is_ascii()));
}

#[test]
fn colour_is_the_brands_and_always_reset() {
    let out = rendered(
        &guest(),
        Options {
            logo: true,
            color: true,
        },
    );
    let text = out.as_str();
    // The label in rust, bold; the core in rust; a reset on every row that
    // coloured anything.
    assert!(text.contains("\x1b[1m\x1b[38;2;255;122;43mOS\x1b[0m: Ferrix 0.1.0 x86_64"));
    assert!(text.contains("\x1b[38;2;255;122;43m(@)"));
    for line in text.lines().filter(|line| line.contains('\x1b')) {
        assert!(line.contains("\x1b[0m"), "{line:?} is left coloured");
    }
    // The sixteen colours, eight a row.
    assert!(text.contains("\x1b[40m   \x1b[41m"));
    assert!(text.contains("\x1b[107m   \x1b[0m"));
}

#[test]
fn uptime_is_written_as_fastfetch_writes_it() {
    let line = |seconds| {
        let facts = Facts {
            uptime: Some(seconds),
            ..Facts::default()
        };
        rendered(&facts, PLAIN)
    };
    assert_eq!(line(0).as_str(), "Uptime: 0 secs\n");
    assert_eq!(line(1).as_str(), "Uptime: 1 sec\n");
    assert_eq!(line(60).as_str(), "Uptime: 1 min\n");
    assert_eq!(line(86_400 + 60).as_str(), "Uptime: 1 day, 1 min\n");
    assert_eq!(
        line(2 * 86_400 + 7200).as_str(),
        "Uptime: 2 days, 2 hours\n"
    );
}

#[test]
fn stat_parent_counts_after_the_last_parenthesis() {
    assert_eq!(parse::stat_parent("7 (zinc) S 1 7 7 0 -1"), Some(1));
    // A name with a parenthesis and a space in it.
    assert_eq!(parse::stat_parent("9 (a) b) R 7 9 9"), Some(7));
    assert_eq!(parse::stat_parent("9 zinc S 1"), None);
}

#[test]
fn status_gives_the_real_uid() {
    let status =
        "Name:\tzinc\nState:\tS (sleeping)\nUid:\t1000\t0\t0\t0\nGid:\t1000\t1000\t1000\t1000\n";
    assert_eq!(parse::status_uid(status), Some(1000));
    assert_eq!(parse::status_uid("Name:\tzinc\n"), None);
}

#[test]
fn passwd_names_the_uid() {
    let passwd = "root:x:0:0:root:/:/bin/sh\nferrix:x:1000:1000:ferrix:/home/ferrix:/bin/zsh\n";
    assert_eq!(parse::passwd_name(passwd, 0), Some("root"));
    assert_eq!(parse::passwd_name(passwd, 1000), Some("ferrix"));
    assert_eq!(parse::passwd_name(passwd, 7), None);
}

#[test]
fn uptime_is_the_first_fields_whole_seconds() {
    assert_eq!(parse::uptime_seconds("12.34 40.01\n"), Some(12));
    assert_eq!(parse::uptime_seconds("12 40\n"), Some(12));
    assert_eq!(parse::uptime_seconds(""), None);
}

#[test]
fn meminfo_as_the_kernel_writes_it() {
    let text =
        "MemTotal:       62103444 kB\nMemFree:          768736 kB\nMemAvailable:   34850072 kB\n";
    let memory = parse::meminfo(text).expect("both fields");
    assert_eq!(memory.total_kib, 62_103_444);
    assert_eq!(memory.available_kib, 34_850_072);
    assert_eq!(memory.used_kib(), 62_103_444 - 34_850_072);
    // `MemTotalX` is not `MemTotal`.
    assert_eq!(
        parse::meminfo("MemTotalX: 1 kB\nMemAvailable: 1 kB\n"),
        None
    );
}

#[test]
fn loadavg_gives_the_averages_and_the_processes() {
    assert_eq!(
        parse::loadavg("0.08 0.03 0.01 1/42 1234\n"),
        Some(Loadavg {
            averages: ["0.08", "0.03", "0.01"],
            processes: 42,
        })
    );
    assert_eq!(parse::loadavg("0.08 0.03\n"), None);
}

#[test]
fn cpuinfo_counts_processors_and_reads_the_first_rate() {
    let x86 = "processor\t: 0\ncpu MHz\t\t: 2995.198\napicid\t\t: 0\n\n\
               processor\t: 1\ncpu MHz\t\t: 1000.000\napicid\t\t: 1\n\n";
    assert_eq!(
        parse::cpuinfo(x86),
        Cpu {
            count: 2,
            khz: Some(2_995_198),
        }
    );
    let arm = "processor\t: 0\nCPU architecture: 8\n\nprocessor\t: 1\nCPU architecture: 8\n\n";
    assert_eq!(
        parse::cpuinfo(arm),
        Cpu {
            count: 2,
            khz: None,
        }
    );
    assert_eq!(
        parse::cpuinfo("processor\t: 0\ncpu MHz\t\t: 3000\n").khz,
        Some(3_000_000)
    );
}

#[test]
fn a_connectors_first_mode_is_its_preferred() {
    assert_eq!(parse::first_mode("1280x800\n1024x768\n"), Some((1280, 800)));
    assert_eq!(parse::first_mode(""), None);
    assert_eq!(parse::connector("card0-Virtual-1"), Some("Virtual-1"));
    assert_eq!(parse::connector("card1-HDMI-A-1"), Some("HDMI-A-1"));
    assert_eq!(parse::connector("card0"), None);
    assert_eq!(parse::connector("renderD128"), None);
    assert_eq!(parse::connector("cardX-Virtual-1"), None);
}

/// One `linux_dirent64` record for `name`, padded to eight bytes as Linux
/// pads them.
fn record(name: &str) -> Vec<u8> {
    let length = (19 + name.len() + 1).next_multiple_of(8);
    let mut bytes = vec![0_u8; length];
    bytes[16..18].copy_from_slice(&u16::try_from(length).expect("short").to_ne_bytes());
    bytes[19..19 + name.len()].copy_from_slice(name.as_bytes());
    bytes
}

#[test]
fn dirents_walks_getdents64s_records() {
    let mut bytes = Vec::new();
    for name in [".", "..", "card0", "card0-Virtual-1"] {
        bytes.extend(record(name));
    }
    let names: Vec<&[u8]> = Dirents::new(&bytes).collect();
    assert_eq!(names, [&b"."[..], b"..", b"card0", b"card0-Virtual-1"]);
    // A record cut short ends the walk rather than reading past it.
    let cut = &bytes[..bytes.len() - 4];
    assert_eq!(Dirents::new(cut).count(), 3);
    assert_eq!(Dirents::new(&[]).count(), 0);
}

#[test]
fn uname_fields_end_at_their_nul() {
    let mut field = [0_u8; 65];
    field[..6].copy_from_slice(b"Ferrix");
    assert_eq!(parse::c_str(&field), Some("Ferrix"));
    assert_eq!(parse::c_str(&[0; 65]), None);
    let mut names = [0_u8; parse::UTSNAME];
    for (at, text) in [
        (0, "Ferrix"),
        (65, "ferrix"),
        (130, "6.1.0-ferrix"),
        (195, "#1 Ferrix 0.1.0"),
        (260, "x86_64"),
    ] {
        names[at..at + text.len()].copy_from_slice(text.as_bytes());
    }
    assert_eq!(
        parse::uts_field(&names, parse::Uts::Sysname),
        Some("Ferrix")
    );
    assert_eq!(
        parse::uts_field(&names, parse::Uts::Nodename),
        Some("ferrix")
    );
    assert_eq!(
        parse::uts_field(&names, parse::Uts::Release),
        Some("6.1.0-ferrix")
    );
    assert_eq!(
        parse::uts_field(&names, parse::Uts::Version),
        Some("#1 Ferrix 0.1.0")
    );
    assert_eq!(
        parse::uts_field(&names, parse::Uts::Machine),
        Some("x86_64")
    );
    assert_eq!(parse::uts_field(&names[..100], parse::Uts::Machine), None);
    assert_eq!(parse::os_version("#1 Ferrix 0.1.0"), "Ferrix 0.1.0");
    assert_eq!(parse::os_version("Ferrix 0.1.0"), "Ferrix 0.1.0");
    assert_eq!(parse::os_version("#x Ferrix"), "#x Ferrix");
}

#[test]
fn the_command_line_is_read_after_the_programs_name() {
    let arguments = |cmdline: &'static [u8]| parse::command(parse::arguments(cmdline));
    let both = Options {
        logo: true,
        color: true,
    };
    assert_eq!(arguments(b"ferrofetch\0"), Ok(Command::Show(both)));
    assert_eq!(arguments(b"ferrofetch"), Ok(Command::Show(both)));
    assert_eq!(
        arguments(b"/bin/ferrofetch\0--no-logo\0--no-color\0"),
        Ok(Command::Show(Options {
            logo: false,
            color: false,
        }))
    );
    assert_eq!(
        arguments(b"ferrofetch\0--help\0--bogus\0"),
        Ok(Command::Help)
    );
    assert_eq!(arguments(b"ferrofetch\0-V\0"), Ok(Command::Version));
    assert_eq!(arguments(b"ferrofetch\0--bogus\0"), Err(&b"--bogus"[..]));
}

#[test]
fn text_keeps_whole_characters_and_says_when_it_cut() {
    use core::fmt::Write as _;
    let mut text: Text<4> = Text::from("aé");
    assert_eq!(text.as_str(), "aé");
    text.push("éé");
    assert_eq!(text.as_str(), "aé", "the next é does not fit whole");
    let mut path: Text<8> = Text::new();
    assert!(write!(path, "/proc/{}", 1).is_ok());
    assert!(write!(path, "/comm").is_err());
}
