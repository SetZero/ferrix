//! `hyprctl`'s requests and answers.
//!
//! The JSON is checked by parsing it back with a parser written here, which
//! is the only way to say "this is JSON" without taking a dependency the
//! compositor does not want. The field names and their order are Hyprland
//! 0.56.2's, read from `src/debug/HyprCtl.cpp`, because a bar reads them by
//! name and a missing one is a crash in somebody else's program.

use crate::json::Json;
use crate::reply::{Reply, Version, answer};
use crate::request::{Flags, Format, Request};
use crate::state::{Monitor, Snapshot, Window, Workspace};

/// A small parser, so a test can say "this answer is JSON" and look inside
/// it without the compositor taking a JSON crate.
mod parse {
    /// A parsed value.
    #[derive(Clone, Debug, PartialEq)]
    pub(super) enum Value {
        /// `null`.
        Null,
        /// `true` or `false`.
        Bool(bool),
        /// A number, which JSON has only one kind of.
        Number(f64),
        /// A string, with its escapes undone.
        Text(String),
        /// An array.
        List(Vec<Value>),
        /// An object, in the order its fields were written.
        Map(Vec<(String, Value)>),
    }

    impl Value {
        /// The field `name`, if this is a map with one.
        pub(super) fn get(&self, name: &str) -> Option<&Self> {
            match self {
                Self::Map(fields) => fields
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value),
                _ => None,
            }
        }

        /// The items, if this is a list.
        pub(super) fn items(&self) -> &[Self] {
            match self {
                Self::List(items) => items,
                _ => &[],
            }
        }

        /// The field names, in order, if this is a map.
        pub(super) fn keys(&self) -> Vec<&str> {
            match self {
                Self::Map(fields) => fields.iter().map(|(key, _)| key.as_str()).collect(),
                _ => Vec::new(),
            }
        }

        /// The string, if this is one.
        pub(super) fn text(&self) -> Option<&str> {
            match self {
                Self::Text(value) => Some(value),
                _ => None,
            }
        }

        /// The number, if this is one.
        pub(super) fn number(&self) -> Option<f64> {
            match self {
                Self::Number(value) => Some(*value),
                _ => None,
            }
        }

        /// The boolean, if this is one.
        pub(super) fn boolean(&self) -> Option<bool> {
            match self {
                Self::Bool(value) => Some(*value),
                _ => None,
            }
        }
    }

    /// Parse `text` whole, or say where it stopped making sense.
    pub(super) fn parse(text: &str) -> Result<Value, String> {
        let bytes: Vec<char> = text.chars().collect();
        let mut at = 0;
        let value = value(&bytes, &mut at)?;
        skip(&bytes, &mut at);
        if at != bytes.len() {
            return Err(format!("{} characters left over at {at}", bytes.len() - at));
        }
        Ok(value)
    }

    fn skip(bytes: &[char], at: &mut usize) {
        while bytes.get(*at).is_some_and(|c| c.is_whitespace()) {
            *at += 1;
        }
    }

    fn value(bytes: &[char], at: &mut usize) -> Result<Value, String> {
        skip(bytes, at);
        match bytes.get(*at) {
            Some('{') => map(bytes, at),
            Some('[') => list(bytes, at),
            Some('"') => text(bytes, at).map(Value::Text),
            Some('t') => word(bytes, at, "true").map(|()| Value::Bool(true)),
            Some('f') => word(bytes, at, "false").map(|()| Value::Bool(false)),
            Some('n') => word(bytes, at, "null").map(|()| Value::Null),
            Some(_) => number(bytes, at),
            None => Err("nothing at all".to_owned()),
        }
    }

    fn word(bytes: &[char], at: &mut usize, want: &str) -> Result<(), String> {
        for expected in want.chars() {
            if bytes.get(*at) != Some(&expected) {
                return Err(format!("expected {want} at {at}"));
            }
            *at += 1;
        }
        Ok(())
    }

    fn number(bytes: &[char], at: &mut usize) -> Result<Value, String> {
        let start = *at;
        while bytes
            .get(*at)
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
        {
            *at += 1;
        }
        let text: String = bytes.get(start..*at).unwrap_or(&[]).iter().collect();
        text.parse()
            .map(Value::Number)
            .map_err(|_| format!("{text:?} at {start} is not a number"))
    }

    fn text(bytes: &[char], at: &mut usize) -> Result<String, String> {
        if bytes.get(*at) != Some(&'"') {
            return Err(format!("expected a string at {at}"));
        }
        *at += 1;
        let mut out = String::new();
        loop {
            match bytes.get(*at) {
                None => return Err("a string that never ends".to_owned()),
                Some('"') => {
                    *at += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    *at += 1;
                    let escape = *bytes.get(*at).ok_or("an escape that never ends")?;
                    *at += 1;
                    out.push(match escape {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'b' => '\u{8}',
                        'f' => '\u{c}',
                        'u' => {
                            let digits: String = bytes
                                .get(*at..*at + 4)
                                .ok_or("a short unicode escape")?
                                .iter()
                                .collect();
                            *at += 4;
                            let code = u32::from_str_radix(&digits, 16)
                                .map_err(|_| format!("{digits:?} is not hex"))?;
                            char::from_u32(code).ok_or("not a character")?
                        }
                        other => other,
                    });
                }
                Some(other) => {
                    // A raw control character inside a string is exactly what
                    // an unescaped title would put here, and what a bar's own
                    // parser would refuse.
                    if (*other as u32) < 0x20 {
                        return Err(format!("a raw control character {:#04x}", *other as u32));
                    }
                    out.push(*other);
                    *at += 1;
                }
            }
        }
    }

    fn list(bytes: &[char], at: &mut usize) -> Result<Value, String> {
        *at += 1;
        let mut items = Vec::new();
        loop {
            skip(bytes, at);
            if bytes.get(*at) == Some(&']') {
                *at += 1;
                return Ok(Value::List(items));
            }
            if !items.is_empty() {
                if bytes.get(*at) != Some(&',') {
                    return Err(format!("expected a comma at {at}"));
                }
                *at += 1;
            }
            skip(bytes, at);
            if bytes.get(*at) == Some(&']') {
                return Err(format!("a trailing comma at {at}"));
            }
            items.push(value(bytes, at)?);
        }
    }

    fn map(bytes: &[char], at: &mut usize) -> Result<Value, String> {
        *at += 1;
        let mut fields = Vec::new();
        loop {
            skip(bytes, at);
            if bytes.get(*at) == Some(&'}') {
                *at += 1;
                return Ok(Value::Map(fields));
            }
            if !fields.is_empty() {
                if bytes.get(*at) != Some(&',') {
                    return Err(format!("expected a comma at {at}"));
                }
                *at += 1;
            }
            skip(bytes, at);
            if bytes.get(*at) == Some(&'}') {
                return Err(format!("a trailing comma at {at}"));
            }
            let name = text(bytes, at)?;
            skip(bytes, at);
            if bytes.get(*at) != Some(&':') {
                return Err(format!("expected a colon at {at}"));
            }
            *at += 1;
            fields.push((name, value(bytes, at)?));
        }
    }
}

/// A snapshot with two windows on one monitor, as the compositor's own test
/// has.
fn snapshot() -> Snapshot {
    Snapshot {
        monitors: vec![Monitor {
            id: 0,
            name: "HEADLESS-1".to_owned(),
            width: 1024,
            height: 768,
            refresh: 60.0,
            at: (0, 0),
            active_workspace: 1,
            active_workspace_name: "1".to_owned(),
            scale: 1.0,
            focused: true,
        }],
        workspaces: vec![Workspace {
            id: 1,
            name: "1".to_owned(),
            monitor: "HEADLESS-1".to_owned(),
            windows: 2,
            has_fullscreen: false,
        }],
        windows: vec![
            Window {
                address: 1,
                mapped: true,
                visible: true,
                at: (10, 10),
                size: (485, 726),
                workspace: 1,
                workspace_name: "1".to_owned(),
                floating: false,
                fullscreen: false,
                monitor: 0,
                class: "rocks.magical.pattern".to_owned(),
                title: "one".to_owned(),
                pid: 1234,
                focus_history: 0,
            },
            Window {
                address: 2,
                mapped: false,
                visible: true,
                at: (515, 10),
                size: (485, 726),
                workspace: 1,
                workspace_name: "1".to_owned(),
                floating: true,
                fullscreen: false,
                monitor: 0,
                class: "rocks.magical.pattern".to_owned(),
                // Everything an unescaped answer would break on, because a
                // title is whatever a program chose to call itself.
                title: "a \"quoted\" \\ title\nwith a newline".to_owned(),
                pid: 5678,
                focus_history: 1,
            },
        ],
        active_window: Some(1),
        active_workspace: 1,
    }
}

/// Answer one request written as a client would send it.
fn ask(line: &str) -> Reply {
    let request = Request::parse(line);
    answer(&request, &snapshot(), Version::default())
}

/// The text of an answer, requiring it to be one.
fn text(line: &str) -> String {
    match ask(line) {
        Reply::Text(text) => text,
        other => panic!("{line:?} asked for {other:?}"),
    }
}

/// The parsed JSON of an answer.
fn json(line: &str) -> parse::Value {
    let answer = text(line);
    parse::parse(&answer).unwrap_or_else(|error| panic!("{line:?}: {error}\n{answer}"))
}

#[test]
fn the_flags_in_front_of_a_request_are_read() {
    assert_eq!(Request::parse("clients").flags, Flags::default());
    assert!(Request::parse("j/clients").flags.json);
    assert_eq!(Request::parse("j/clients").command, "clients");
    let both = Request::parse("jr/clients").flags;
    assert!(both.json && both.raw);
    assert!(Request::parse("a/clients").flags.all);
    assert!(Request::parse("-j/clients").flags.no_newline);
    // An unknown letter is ignored rather than refused, so a client built
    // against a newer Hyprland is still answered.
    assert_eq!(Request::parse("jz/clients").command, "clients");
    assert!(Request::parse("jz/clients").flags.json);
}

#[test]
fn a_command_keeps_its_argument_and_a_slash_in_one_is_left_alone() {
    let request = Request::parse("dispatch movefocus l");
    assert_eq!(request.command, "dispatch");
    assert_eq!(request.argument, "movefocus l");

    // A path in a `keyword` has slashes, and they are past the first space,
    // so they are not flags.
    let request = Request::parse("keyword misc:font_family /usr/share/fonts/x.ttf");
    assert_eq!(request.command, "keyword");
    assert_eq!(request.argument, "misc:font_family /usr/share/fonts/x.ttf");

    // The name is lower-cased, as Hyprland lower-cases it.
    assert_eq!(Request::parse("CLIENTS").command, "clients");
    // And a trailing newline, which a socket carries, is not part of it.
    assert_eq!(Request::parse("clients\n").command, "clients");
}

#[test]
fn a_batch_runs_several_and_they_share_the_flags() {
    let batch = Request::parse_batch("j/[[BATCH]]dispatch workspace 2;dispatch killactive");
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].command, "dispatch");
    assert_eq!(batch[0].argument, "workspace 2");
    assert_eq!(batch[1].argument, "killactive");
    assert!(batch.iter().all(|request| request.flags.json));

    // One request is a batch of one.
    let single = Request::parse_batch("clients");
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].command, "clients");
    // And an empty part is dropped, which a trailing `;` makes.
    assert_eq!(Request::parse_batch("[[BATCH]]clients;").len(), 1);
}

#[test]
fn clients_names_every_field_hyprland_names_in_the_order_it_names_them() {
    // A bar reads these by name, and Hyprland's own order is what a person
    // comparing two outputs expects. From `CHyprCtl::getWindowData`.
    let value = json("j/clients");
    let windows = value.items();
    assert_eq!(windows.len(), 1, "only mapped windows without `a`");
    assert_eq!(
        windows[0].keys(),
        [
            "address",
            "mapped",
            "hidden",
            "visible",
            "acceptsInput",
            "at",
            "size",
            "workspace",
            "floating",
            "monitor",
            "class",
            "title",
            "initialClass",
            "initialTitle",
            "pid",
            "xwayland",
            "pinned",
            "fullscreen",
            "fullscreenClient",
            "grouped",
            "tags",
            "swallowing",
            "focusHistoryID",
            "inhibitingIdle",
        ]
    );
    assert_eq!(
        windows[0].get("address").and_then(parse::Value::text),
        Some("0x1")
    );
    assert_eq!(
        windows[0].get("title").and_then(parse::Value::text),
        Some("one")
    );
    assert_eq!(
        windows[0].get("class").and_then(parse::Value::text),
        Some("rocks.magical.pattern")
    );
    assert_eq!(windows[0].get("at").expect("at").items().len(), 2);
    assert_eq!(
        windows[0]
            .get("size")
            .and_then(|size| size.items().first())
            .and_then(parse::Value::number),
        Some(485.0)
    );
    assert_eq!(
        windows[0]
            .get("workspace")
            .and_then(|workspace| workspace.get("name"))
            .and_then(parse::Value::text),
        Some("1")
    );
}

#[test]
fn a_window_that_is_not_mapped_is_shown_only_with_the_all_flag() {
    assert_eq!(json("j/clients").items().len(), 1);
    assert_eq!(json("ja/clients").items().len(), 2);
    assert!(text("clients").contains("one"));
    assert!(!text("clients").contains("quoted"));
    assert!(text("a/clients").contains("quoted"));
}

#[test]
fn a_title_a_program_chose_cannot_break_the_json_a_bar_parses() {
    // The second window's title holds a quote, a backslash and a newline,
    // which are exactly what an unescaped answer would break on. The parser
    // above refuses a raw control character in a string, so this fails rather
    // than quietly producing something a bar cannot read.
    let value = json("ja/clients");
    let windows = value.items();
    assert_eq!(windows.len(), 2);
    assert_eq!(
        windows[1].get("title").and_then(parse::Value::text),
        Some("a \"quoted\" \\ title\nwith a newline"),
        "the title did not come back as it went in"
    );
}

#[test]
fn activewindow_is_the_focused_one_and_an_empty_object_when_there_is_none() {
    let value = json("j/activewindow");
    assert_eq!(
        value.get("address").and_then(parse::Value::text),
        Some("0x1")
    );

    let mut empty = snapshot();
    empty.active_window = None;
    let request = Request::parse("j/activewindow");
    let Reply::Text(written) = answer(&request, &empty, Version::default()) else {
        panic!("not an answer");
    };
    // Hyprland answers `{}`, and every bar is written for that.
    assert_eq!(written.trim(), "{}");
    let request = Request::parse("activewindow");
    let Reply::Text(written) = answer(&request, &empty, Version::default()) else {
        panic!("not an answer");
    };
    assert_eq!(written.trim(), "Invalid");
}

#[test]
fn monitors_and_workspaces_carry_what_a_bar_reads() {
    let value = json("j/monitors");
    let monitors = value.items();
    assert_eq!(monitors.len(), 1);
    assert_eq!(
        monitors[0].get("name").and_then(parse::Value::text),
        Some("HEADLESS-1")
    );
    assert_eq!(
        monitors[0].get("width").and_then(parse::Value::number),
        Some(1024.0)
    );
    assert_eq!(
        monitors[0]
            .get("refreshRate")
            .and_then(parse::Value::number),
        Some(60.0)
    );
    assert_eq!(
        monitors[0].get("focused").and_then(parse::Value::boolean),
        Some(true)
    );
    // `activeWorkspace` is an object and there is only one field with that
    // name: two fields of one key is a document where which a reader takes
    // is its own business.
    assert_eq!(
        monitors[0]
            .keys()
            .iter()
            .filter(|key| **key == "activeWorkspace")
            .count(),
        1
    );
    assert_eq!(
        monitors[0]
            .get("activeWorkspace")
            .and_then(|workspace| workspace.get("id"))
            .and_then(parse::Value::number),
        Some(1.0)
    );

    let value = json("j/workspaces");
    assert_eq!(value.items().len(), 1);
    assert_eq!(
        value.items()[0]
            .get("windows")
            .and_then(parse::Value::number),
        Some(2.0)
    );

    let value = json("j/activeworkspace");
    assert_eq!(value.get("id").and_then(parse::Value::number), Some(1.0));
}

#[test]
fn the_compact_form_has_no_newlines_in_it() {
    let pretty = text("j/clients");
    let compact = text("jr/clients");
    assert!(pretty.contains("\n    "), "`j` alone is pretty-printed");
    assert_eq!(
        compact.trim().lines().count(),
        1,
        "`r` asks for it on one line: {compact}"
    );
    // Both are the same document.
    assert_eq!(
        parse::parse(&pretty).expect("pretty is json"),
        parse::parse(&compact).expect("compact is json")
    );
    // And `-` leaves the trailing newline off.
    assert!(text("j/clients").ends_with('\n'));
    assert!(!text("-j/clients").ends_with('\n'));
}

#[test]
fn what_changes_the_compositor_comes_back_for_it_to_do() {
    assert_eq!(
        ask("dispatch movefocus l"),
        Reply::Dispatch {
            name: "movefocus".to_owned(),
            argument: "l".to_owned()
        }
    );
    assert_eq!(
        ask("dispatch killactive"),
        Reply::Dispatch {
            name: "killactive".to_owned(),
            argument: String::new()
        }
    );
    assert_eq!(
        ask("keyword general:gaps_in 10"),
        Reply::Keyword {
            name: "general:gaps_in".to_owned(),
            value: "10".to_owned()
        }
    );
    assert_eq!(ask("reload"), Reply::Reload);
}

#[test]
fn an_unknown_request_is_answered_rather_than_refused() {
    // Hyprland answers with a line and keeps the connection, so a program
    // asking for something newer keeps working for everything else it asks.
    let written = text("nonsense");
    assert!(written.contains("unknown request nonsense"), "{written}");
    assert!(!text("").is_empty(), "even an empty request is answered");
}

#[test]
fn version_says_what_this_compositor_is() {
    let readable = text("version");
    assert!(readable.contains("hyprix"), "{readable}");
    let value = json("j/version");
    // A bar reads `tag`; Hyprland's other fields are there so nothing looking
    // for them crashes.
    assert!(value.get("tag").and_then(parse::Value::text).is_some());
    assert!(value.get("commit").is_some());
    assert!(value.get("flags").is_some());
}

#[test]
fn the_json_writer_escapes_what_json_requires() {
    let mut out = Json::new(false);
    out.object();
    out.string("text", "a\"b\\c\nd\te\u{1}f");
    out.end('}');
    let written = out.finish();
    assert_eq!(
        written, "{\"text\":\"a\\\"b\\\\c\\nd\\te\\u0001f\"}",
        "an escape is missing"
    );
    let value = parse::parse(&written).expect("still json");
    assert_eq!(
        value.get("text").and_then(parse::Value::text),
        Some("a\"b\\c\nd\te\u{1}f")
    );
}

#[test]
fn every_answer_this_crate_writes_is_json_when_json_was_asked_for() {
    for command in [
        "version",
        "monitors",
        "workspaces",
        "clients",
        "activewindow",
        "activeworkspace",
    ] {
        for flags in ["j", "jr", "ja"] {
            let line = format!("{flags}/{command}");
            let written = text(&line);
            let _ =
                parse::parse(&written).unwrap_or_else(|error| panic!("{line}: {error}\n{written}"));
        }
        // And the readable form is not empty, which is what `hyprctl` without
        // `-j` prints.
        assert!(!text(command).is_empty(), "{command} said nothing");
    }
    assert_eq!(Flags::default().format(), Format::Readable);
}
