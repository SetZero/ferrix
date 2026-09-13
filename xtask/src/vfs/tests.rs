use super::*;

fn owned(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

/// The log a kernel that passes every command would write.
fn passing_log() -> Vec<String> {
    let mut log = owned(&["FERRIX-BOOT-OK stages 1-9"]);
    for (index, command) in COMMANDS.iter().enumerate() {
        log.push(format!("  init     command {index}: {}", command.argv[0]));
        match command.expect {
            Expect::Lines(lines) => log.extend(owned(lines)),
            Expect::ProcListing => {
                log.extend(owned(&[
                    "/proc:", "1", "self", "", "/proc/1:", "maps", "exe",
                ]));
            }
            Expect::Maps => log.extend(owned(&[
                "00400000-00401000 r--p 00000000 00:00 0",
                "7fffffdde000-7fffffdff000 rw-p 00000000 00:00 0          [stack]",
            ])),
        }
        log.push(format!(
            "  init     command {index} exited with {}",
            command.status
        ));
    }
    log.push("  init     every command has run".to_owned());
    log
}

#[test]
fn the_list_encodes_as_nul_terminated_arguments_and_empty_ones_between_commands() {
    let commands = [
        Command {
            argv: &["ls", "-R"],
            status: 0,
            expect: Expect::Lines(&[]),
        },
        Command {
            argv: &["sh", "-c", "echo a\necho b"],
            status: 0,
            expect: Expect::Lines(&[]),
        },
    ];
    assert_eq!(
        encode(&commands).unwrap(),
        b"ls\0-R\0\0sh\0-c\0echo a\necho b\0\0"
    );
    assert!(encode(COMMANDS).unwrap().ends_with(b"\0\0"));
}

#[test]
fn the_list_refuses_what_its_encoding_cannot_carry() {
    for argv in [&[][..], &["sh", ""][..], &["sh", "a\0b"][..]] {
        let command = Command {
            argv,
            status: 0,
            expect: Expect::Lines(&[]),
        };
        assert!(encode(&[command]).is_err(), "{argv:?} must be refused");
    }
}

#[test]
fn every_program_is_an_applet_the_initramfs_links() {
    for command in COMMANDS {
        assert!(
            crate::initramfs::APPLETS.contains(&command.argv[0]),
            "{} is not linked in /bin",
            command.argv[0]
        );
    }
}

#[test]
fn no_script_exits_zero_and_no_expectation_is_empty() {
    for command in COMMANDS {
        if command.argv[0] == "sh" {
            assert_ne!(command.status, 0, "a dead shell reports 0");
        }
        if let Expect::Lines(lines) = command.expect {
            assert!(!lines.is_empty(), "{:?} expects no lines", command.argv);
        }
    }
}

#[test]
fn kernel_lines_about_commands_are_read() {
    assert_eq!(
        marker("  init     command 3: sh -c <200 bytes>"),
        Some((3, Marker::Started))
    );
    assert_eq!(
        marker("  init     command 12 exited with -1"),
        Some((12, Marker::Ended(Ending::Exited(-1))))
    );
    assert_eq!(
        marker("  init     command 0 could not be started: Load(NotElf)"),
        Some((
            0,
            Marker::Ended(Ending::NotStarted("Load(NotElf)".to_owned()))
        ))
    );
    assert_eq!(marker("  init     every command has run"), None);
    assert_eq!(marker("  init     command x: ls"), None);
    assert_eq!(marker("tmpfs: one"), None);
}

#[test]
fn a_log_splits_into_each_commands_output_and_unanswered_calls() {
    let log = owned(&[
        "FERRIX-BOOT-OK stages 1-9",
        "  init     /bin/busybox is 857 KiB, running 2 commands",
        "  init     command 0: ls -R /proc",
        "  syscall  Getdents64 (number 217) answered ENOSYS",
        "/proc:",
        "",
        "  init     command 0 exited with 0\r",
        "  init     command 1: cat /proc/self/maps",
        "  init     command 1 could not be started: Start(\"no stack\")",
        "stray output after every command",
    ]);
    let ran = split(&log);
    assert_eq!(ran.len(), 2);
    assert_eq!(ran[&0].output, owned(&["/proc:", ""]));
    assert_eq!(
        ran[&0].unanswered,
        owned(&["syscall  Getdents64 (number 217) answered ENOSYS"])
    );
    assert_eq!(ran[&0].ending, Some(Ending::Exited(0)));
    assert!(ran[&1].output.is_empty());
    assert!(matches!(ran[&1].ending, Some(Ending::NotStarted(_))));
}

#[test]
fn a_listing_reads_the_same_in_columns_and_in_colour() {
    let plain = owned(&["/proc:", "1", "self", "", "/proc/1:", "maps"]);
    let columns = owned(&[
        "/proc:",
        "\u{1b}[1;34m1\u{1b}[0m     \u{1b}[1;36mself\u{1b}[0m",
        "",
        "/proc/1:",
        "maps",
    ]);
    assert_eq!(listing(&plain), listing(&columns));
    assert_eq!(listing(&plain)["/proc"], owned(&["1", "self"]));
}

#[test]
fn a_proc_listing_must_reach_self_and_the_programs_maps() {
    assert_eq!(
        proc_listing(&owned(&[
            "/proc:", "7", "self", "", "/proc/7:", "exe", "maps"
        ])),
        Ok(())
    );
    assert_eq!(
        proc_listing(&owned(&["/proc:", "self", "", "/proc/self:", "maps"])),
        Ok(())
    );
    // What refusing `getdents64` looks like: headings, and nothing under them.
    assert!(proc_listing(&owned(&["/proc:"])).is_err());
    assert!(proc_listing(&owned(&["/proc:", "1", "", "/proc/1:", "maps"])).is_err());
    assert!(proc_listing(&owned(&["/proc:", "self", "cpuinfo"])).is_err());
    assert!(
        proc_listing(&owned(&["/proc:", "self", "", "/proc/sys:", "maps"])).is_err(),
        "a directory that is not a process's does not count"
    );
}

#[test]
fn maps_lines_parse_as_linux_writes_them() {
    let line = maps_line(
        "00400000-00401000 r-xp 00001000 fc:00 9044019                            \
         /bin/busy box",
    )
    .unwrap();
    assert_eq!(
        line,
        MapsLine {
            start: 0x40_0000,
            end: 0x40_1000,
            perms: "r-xp".to_owned(),
            offset: 0x1000,
            dev: (0xfc, 0),
            inode: 9_044_019,
            path: Some("/bin/busy box".to_owned()),
        }
    );
    assert_eq!(
        maps_line("7ffd22ede000-7ffd22eff000 rw-p 00000000 00:00 0 [stack]")
            .unwrap()
            .path
            .as_deref(),
        Some("[stack]")
    );
    let anonymous = maps_line("004fd000-004ff000 rw-p 00000000 00:00 0 ").unwrap();
    assert_eq!(anonymous.path, None);
    assert_eq!(
        maps_line("ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0 [vsyscall]")
            .unwrap()
            .perms,
        "--xp"
    );
}

#[test]
fn malformed_maps_lines_say_what_is_wrong() {
    for line in [
        "",
        "00400000 r--p 00000000 00:00 0",
        "00401000-00400000 r--p 00000000 00:00 0",
        "00400000-00401000 rw-- 00000000 00:00 0",
        "00400000-00401000 r--p 0000zz00 00:00 0",
        "00400000-00401000 r--p 00000000 0000 0",
        "00400000-00401000 r--p 00000000 00:00",
        "00400000-00401000 r--p 00000000 00:00 -1",
        "-00401000 r--p 00000000 00:00 0",
    ] {
        assert!(maps_line(line).is_err(), "`{line}` must be refused");
    }
}

#[test]
fn maps_must_ascend_without_overlap_and_not_be_empty() {
    let ordered = owned(&[
        "00400000-00401000 r--p 00000000 00:00 0",
        "00401000-00402000 r-xp 00000000 00:00 0",
    ]);
    assert_eq!(maps(&ordered), Ok(()));
    let overlapping = owned(&[
        "00400000-00402000 r--p 00000000 00:00 0",
        "00401000-00403000 r-xp 00000000 00:00 0",
    ]);
    assert!(maps(&overlapping).is_err());
    assert!(maps(&[]).is_err());
    assert!(maps(&owned(&["FERRIX says hello"])).is_err());
}

#[test]
fn a_passing_log_passes() {
    let passed = judge(COMMANDS, &passing_log()).unwrap();
    assert_eq!(passed.len(), COMMANDS.len());
}

#[test]
fn every_failure_is_reported_with_the_calls_that_went_unanswered() {
    let mut log = passing_log();
    // Command 1's maps line, garbled; command 2's status, wrong, with a
    // refused call reported while it ran.
    let maps = log.iter().position(|l| l.starts_with("00400000-")).unwrap();
    log[maps] = "not a maps line".to_owned();
    let exit = log
        .iter()
        .position(|l| l == "  init     command 2 exited with 8")
        .unwrap();
    log[exit] = "  init     command 2 exited with 1".to_owned();
    log.insert(
        exit,
        "  syscall  Dup2 (number 33) answered ENOSYS".to_owned(),
    );

    let failed = judge(COMMANDS, &log).unwrap_err();
    assert_eq!(failed.len(), 2, "{failed:#?}");
    assert!(failed[0].starts_with("command 1 (cat /proc/self/maps)"));
    assert!(failed[1].starts_with("command 2 (sh -c)"));
    assert!(failed[1].contains("exited with 1, not 8"));
    assert!(failed[1].contains("Dup2 (number 33)"));
}

#[test]
fn a_command_that_never_ends_or_never_starts_fails() {
    let mut log = passing_log();
    let last = COMMANDS.len() - 1;
    log.retain(|line| {
        !line.contains(&format!("command {last} exited")) && !line.contains("command 0")
    });
    let failed = judge(COMMANDS, &log).unwrap_err();
    assert!(failed[0].contains("never started"), "{failed:#?}");
    assert!(
        failed.last().unwrap().contains("never exited"),
        "{failed:#?}"
    );
}

#[test]
fn an_unreadable_program_is_the_whole_report() {
    let log = owned(&["  init     /bin/busybox could not be read: errno 2"]);
    assert_eq!(
        judge(COMMANDS, &log).unwrap_err(),
        owned(&["init     /bin/busybox could not be read: errno 2"])
    );
}
