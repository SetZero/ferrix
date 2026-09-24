//! Value syntaxes and command lines, pinned against `parse-util.c`,
//! `time-util.c`, `extract-word.c` and `config_parse_exec`.

use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use crate::exec::{self, Privilege};
use crate::value::{self, Signal, Span, ValueError};

fn secs(text: &str) -> Result<Span, ValueError> {
    value::seconds(text)
}

fn micros(n: u64) -> Result<Span, ValueError> {
    Ok(Span::Finite(Duration::from_micros(n)))
}

#[test]
fn booleans() {
    for yes in ["1", "yes", "Y", "true", "t", "ON"] {
        assert_eq!(value::boolean(yes), Ok(true), "{yes}");
    }
    for no in ["0", "no", "n", "FALSE", "f", "off"] {
        assert_eq!(value::boolean(no), Ok(false), "{no}");
    }
    assert_eq!(value::boolean("maybe"), Err(ValueError::Invalid));
    assert_eq!(value::boolean(""), Err(ValueError::Invalid));
}

#[test]
fn time_spans() {
    assert_eq!(secs("5"), micros(5_000_000));
    assert_eq!(secs("5s"), micros(5_000_000));
    assert_eq!(secs("100ms"), micros(100_000));
    assert_eq!(secs("1min"), micros(60_000_000));
    assert_eq!(secs("1min 30s"), micros(90_000_000));
    assert_eq!(secs("1min30s"), micros(90_000_000));
    assert_eq!(secs("2.5s"), micros(2_500_000));
    assert_eq!(secs(".5s"), micros(500_000));
    assert_eq!(secs("1h"), micros(3_600_000_000));
    assert_eq!(secs("2 weeks"), micros(1_209_600_000_000));
    assert_eq!(secs("3us"), micros(3));
    assert_eq!(secs("0"), micros(0));
    assert_eq!(secs(" 7 s "), micros(7_000_000));
    assert_eq!(secs("infinity"), Ok(Span::Infinity));
    assert_eq!(
        value::timespan("5", 1_000),
        micros(5_000),
        "a bare number is the default unit"
    );
    assert_eq!(secs("-5s"), Err(ValueError::Invalid));
    assert_eq!(secs("5 parsecs"), Err(ValueError::Invalid));
    assert_eq!(secs(""), Err(ValueError::Invalid));
    assert_eq!(secs("5 6s"), Err(ValueError::Invalid));
    assert_eq!(secs("99999999999999999999s"), Err(ValueError::Range));
    assert_eq!(secs("600000000y"), Err(ValueError::Range));
}

#[test]
fn sizes_are_base_1024() {
    assert_eq!(value::size("512"), Ok(512));
    assert_eq!(value::size("64M"), Ok(64 << 20));
    assert_eq!(value::size("1G"), Ok(1 << 30));
    assert_eq!(value::size("1.5K"), Ok(1536));
    assert_eq!(value::size("1G 512M"), Ok((1 << 30) + (512 << 20)));
    assert_eq!(value::size("4 K"), Ok(4096));
    assert_eq!(value::size("1B"), Ok(1));
    assert_eq!(value::size("16E"), Err(ValueError::Range));
    assert_eq!(value::size("1Q"), Err(ValueError::Invalid));
    assert_eq!(value::size("-1"), Err(ValueError::Invalid));
    assert_eq!(value::size("M"), Err(ValueError::Invalid));
}

#[test]
fn percentages_are_hundredths() {
    assert_eq!(value::permyriad("50%", true), Ok(5_000));
    assert_eq!(value::permyriad("12.5%", true), Ok(1_250));
    assert_eq!(value::permyriad("0.25%", true), Ok(25));
    assert_eq!(value::permyriad("5‰", true), Ok(50));
    assert_eq!(value::permyriad("7‱", true), Ok(7));
    assert_eq!(value::permyriad("150%", false), Ok(15_000));
    assert_eq!(value::permyriad("150%", true), Err(ValueError::Range));
    assert_eq!(value::permyriad("1.234%", true), Err(ValueError::Invalid));
    assert_eq!(value::permyriad("50", true), Err(ValueError::Invalid));
    assert_eq!(value::permyriad("5.%", true), Err(ValueError::Invalid));
}

#[test]
fn signals_by_name_and_number() {
    assert_eq!(value::signal("SIGTERM"), Ok(Signal::TERM));
    assert_eq!(value::signal("TERM"), Ok(Signal::TERM));
    assert_eq!(value::signal("9"), Ok(Signal::KILL));
    assert_eq!(value::signal("SIGWINCH"), Ok(Signal(28)));
    assert_eq!(value::signal("64"), Ok(Signal(64)));
    assert_eq!(value::signal("0"), Err(ValueError::Range));
    assert_eq!(value::signal("65"), Err(ValueError::Range));
    assert_eq!(value::signal("SIGNOPE"), Err(ValueError::Invalid));
    assert_eq!(Signal::HUP.name(), Some("HUP"));
}

fn words(text: &str) -> Vec<String> {
    value::words(text)
        .unwrap()
        .into_iter()
        .map(|w| w.text)
        .collect()
}

#[test]
fn words_unquote_and_unescape() {
    assert_eq!(words("a  b\tc"), ["a", "b", "c"]);
    assert_eq!(words("'a b' \"c d\""), ["a b", "c d"]);
    assert_eq!(words("a\"b c\"d"), ["ab cd"]);
    assert_eq!(words("\"\""), [""]);
    assert_eq!(words("a\\ b"), ["a b"]);
    assert_eq!(words("\"tab\\there\""), ["tab\there"]);
    assert_eq!(words("\\x41\\102\\u00e9"), ["ABé"]);
    assert_eq!(
        words("'\\n'"),
        ["\n"],
        "escapes apply inside single quotes too"
    );
    assert_eq!(value::words("\"open"), Err(ValueError::Unterminated));
    assert_eq!(value::words("end\\"), Err(ValueError::Unterminated));
    assert_eq!(value::words("\\x0"), Err(ValueError::Invalid));
    assert_eq!(value::words("\\000"), Err(ValueError::Invalid), "no NUL");
    assert!(words("").is_empty());
}

#[test]
fn command_lines() {
    let commands = exec::commands("/sbin/getty --noclear 'tty 1'").unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].path, "/sbin/getty");
    assert_eq!(commands[0].argv, ["/sbin/getty", "--noclear", "tty 1"]);
    assert!(!commands[0].ignore_failure && commands[0].expand_environment);

    let prefixed = exec::commands("-:@/bin/busybox sh -c 'echo $X'").unwrap();
    assert!(prefixed[0].ignore_failure);
    assert!(!prefixed[0].expand_environment);
    assert_eq!(prefixed[0].path, "/bin/busybox");
    assert_eq!(prefixed[0].argv, ["sh", "-c", "echo $X"]);

    assert_eq!(
        exec::commands("+/bin/x").unwrap()[0].privilege,
        Privilege::Full
    );
    assert_eq!(
        exec::commands("!!/bin/x").unwrap()[0].privilege,
        Privilege::KeepCapabilitiesIfNoAmbient
    );
    assert_eq!(
        exec::commands("true").unwrap()[0].path,
        "true",
        "a bare name is searched"
    );
}

#[test]
fn a_bare_semicolon_separates_commands() {
    let commands = exec::commands("/bin/a 1 ; /bin/b ';' \\; 2").unwrap();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].argv, ["/bin/a", "1"]);
    assert_eq!(commands[1].argv, ["/bin/b", ";", ";", "2"]);
}

#[test]
fn bad_command_lines() {
    assert!(exec::commands("--/bin/x").is_err(), "a repeated prefix");
    assert!(exec::commands("+!/bin/x").is_err(), "+ and ! conflict");
    assert!(exec::commands("-").is_err(), "no path");
    assert!(exec::commands("bin/x").is_err(), "relative with a slash");
    assert!(exec::commands("@/bin/x").is_err(), "@ without argv[0]");
    assert!(exec::commands("/bin/x 'open").is_err());
    assert!(exec::commands("").unwrap().is_empty());
}

#[test]
fn environment_assignments() {
    let (good, bad) = exec::environment("A=1 \"B=two words\" C= not-one 9X=1").unwrap();
    let good: Vec<(&str, &str)> = good.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(good, [("A", "1"), ("B", "two words"), ("C", "")]);
    assert_eq!(bad, ["not-one", "9X=1"]);
}
