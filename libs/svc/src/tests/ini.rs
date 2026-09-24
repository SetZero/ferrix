//! The file syntax, pinned against `conf-parser.c`.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::Warnings;
use crate::ini::{self, Document};

fn parse(text: &str) -> (Document, Warnings) {
    let file: Arc<str> = Arc::from("/lib/ferrix/units/t.service");
    let mut warnings = Warnings::new();
    let document = ini::parse(&file, text.as_bytes(), &mut warnings).unwrap();
    (document, warnings)
}

fn values(document: &Document, section: &str, key: &str) -> Vec<String> {
    document
        .section(section)
        .unwrap()
        .values(key)
        .map(|a| a.value.clone())
        .collect()
}

#[test]
fn sections_keys_and_comments() {
    let (document, warnings) = parse(
        "# a comment\n; another\n[Unit]\nDescription = Login prompt \n  # indented comment\n\n[Service]\nExecStart=/sbin/getty %i\n",
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(values(&document, "Unit", "Description"), ["Login prompt"]);
    assert_eq!(
        values(&document, "Service", "ExecStart"),
        ["/sbin/getty %i"]
    );
    let line = document.section("Service").unwrap().assignments[0].line;
    assert_eq!(line, 8, "lines count from 1");
}

#[test]
fn repeated_keys_and_sections_keep_every_assignment_in_order() {
    let (document, _) =
        parse("[Unit]\nAfter=a.target\n[Service]\nX=1\n[Unit]\nAfter=b.target\nAfter=\n");
    assert_eq!(
        values(&document, "Unit", "After"),
        ["a.target", "b.target", ""]
    );
}

#[test]
fn a_trailing_backslash_continues_the_line_with_a_space() {
    let (document, _) = parse(
        "[Service]\nExecStart=/bin/echo \\\n  one \\\n# a comment inside is dropped\n  two\nNext=x\n",
    );
    assert_eq!(
        values(&document, "Service", "ExecStart"),
        ["/bin/echo    one    two"]
    );
    let assignment = &document.section("Service").unwrap().assignments[0];
    assert_eq!(
        assignment.line, 2,
        "a continued line is reported where it starts"
    );
    assert_eq!(values(&document, "Service", "Next"), ["x"]);
}

#[test]
fn an_escaped_backslash_does_not_continue() {
    let (document, _) = parse("[Service]\nA=x\\\\\nB=y\n");
    assert_eq!(values(&document, "Service", "A"), ["x\\\\"]);
    assert_eq!(values(&document, "Service", "B"), ["y"]);
}

#[test]
fn a_continuation_at_the_end_of_the_file_is_kept() {
    let (document, _) = parse("[Service]\nA=x \\");
    assert_eq!(values(&document, "Service", "A"), ["x"]);
}

#[test]
fn crlf_and_a_byte_order_mark_are_read() {
    let (document, warnings) = parse("\u{feff}[Unit]\r\nDescription=x\r\n");
    assert!(warnings.is_empty());
    assert_eq!(values(&document, "Unit", "Description"), ["x"]);
}

#[test]
fn bad_lines_are_warnings_and_are_skipped() {
    let (document, warnings) = parse("Early=1\n[Unit]\nno equals sign\n=value\nGood=1\n");
    let messages: Vec<&str> = warnings.list().iter().map(|w| w.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "Assignment outside of section. Ignoring.",
            "Missing '=', ignoring line.",
            "Missing key name before '=', ignoring line.",
        ]
    );
    assert_eq!(warnings.list()[1].line, 3);
    assert_eq!(document.section("Unit").unwrap().assignments.len(), 1);
}

#[test]
fn a_value_keeps_its_inner_equals_signs_and_quotes() {
    let (document, _) = parse("[Service]\nEnvironment=\"A=b c\" D=e\n");
    assert_eq!(
        values(&document, "Service", "Environment"),
        ["\"A=b c\" D=e"]
    );
}

#[test]
fn an_unclosed_section_header_refuses_the_file() {
    let file: Arc<str> = Arc::from("f");
    let mut warnings = Warnings::new();
    let error = ini::parse(&file, b"[Unit]\n[Service\nA=1\n", &mut warnings).unwrap_err();
    assert_eq!(error.line, 2);
    assert_eq!(error.message, "Invalid section header '[Service'");
}

#[test]
fn a_line_that_is_not_utf8_is_skipped() {
    let file: Arc<str> = Arc::from("f");
    let mut warnings = Warnings::new();
    let document = ini::parse(&file, b"[Unit]\nA=\xff\nB=1\n", &mut warnings).unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(values(&document, "Unit", "B"), ["1"]);
    assert!(values(&document, "Unit", "A").is_empty());
}

#[test]
fn a_drop_in_merges_after_its_unit() {
    let (mut document, _) = parse("[Service]\nExecStart=/a\nRestart=no\n");
    let (drop_in, _) = parse("[Service]\nExecStart=\nExecStart=/b\n[Unit]\nAfter=x.target\n");
    document.merge(drop_in);
    assert_eq!(values(&document, "Service", "ExecStart"), ["/a", "", "/b"]);
    assert_eq!(values(&document, "Unit", "After"), ["x.target"]);
}
