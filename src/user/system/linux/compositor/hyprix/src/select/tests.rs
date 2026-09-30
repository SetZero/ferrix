//! What each of Hyprland's window expressions picks out.

use super::{Seen, Selector};
use compositor_layout::{WindowId, WorkspaceId};

fn tags(of: &[&str]) -> Vec<String> {
    of.iter().map(|tag| (*tag).to_owned()).collect()
}

fn seen<'a>(
    window: u64,
    class: &'a str,
    title: &'a str,
    tags: &'a [String],
    floating: bool,
) -> Seen<'a> {
    Seen {
        window: WindowId(window),
        class,
        title,
        initial_class: class,
        initial_title: "opened as",
        tags,
        pid: 1000 + i32::try_from(window).unwrap_or(0),
        floating,
        workspace: Some(WorkspaceId(1)),
    }
}

/// Every prefix picks the window it names, and a bare expression is a class.
#[test]
fn each_prefix_picks_its_own_field() {
    let none = tags(&[]);
    let marked = tags(&["urgent", "work"]);
    let windows = [
        seen(1, "foot", "a shell", &none, false),
        seen(2, "firefox", "the web", &marked, true),
    ];
    let pick = |text: &str| {
        Selector::parse(text)
            .and_then(|selector| selector.pick(&windows, Some(WindowId(1))))
            .map(|window| window.0)
    };
    assert_eq!(pick("active"), Some(1));
    assert_eq!(pick("activewindow"), Some(1), "anything starting with it");
    assert_eq!(pick("floating"), Some(2));
    assert_eq!(pick("tiled"), Some(1));
    assert_eq!(pick("class:firefox"), Some(2));
    assert_eq!(pick("^(fire.*)$"), Some(2), "a bare one is a class");
    assert_eq!(pick("title:the web"), Some(2));
    assert_eq!(pick("initialtitle:opened as"), Some(1), "the first of them");
    assert_eq!(pick("initialclass:foot"), Some(1));
    assert_eq!(pick("tag:work"), Some(2));
    assert_eq!(pick("address:0x2"), Some(2));
    assert_eq!(pick("stableid:1"), Some(1), "and without the 0x");
    assert_eq!(pick("pid:1002"), Some(2));
}

/// A pattern has to match the whole field, as Hyprland's `RE2::FullMatch`
/// does, so `class:fire` does not pick `firefox`.
#[test]
fn a_pattern_matches_the_whole_field() {
    let none = tags(&[]);
    let windows = [seen(1, "firefox", "the web", &none, false)];
    let pick = |text: &str| {
        Selector::parse(text).and_then(|selector| selector.pick(&windows, Some(WindowId(1))))
    };
    assert_eq!(pick("class:fire"), None);
    assert_eq!(pick("class:fire.*"), Some(WindowId(1)));
}

/// An expression nobody answers, and one that is not an expression at all,
/// are both no window rather than the wrong one.
#[test]
fn an_expression_nobody_answers_picks_nothing() {
    let none = tags(&[]);
    let windows = [seen(1, "foot", "a shell", &none, false)];
    let pick = |text: &str| {
        Selector::parse(text).and_then(|selector| selector.pick(&windows, Some(WindowId(1))))
    };
    assert_eq!(pick("class:nothing"), None);
    assert_eq!(pick("pid:not-a-number"), None);
    assert_eq!(pick("address:zz"), None);
    assert_eq!(pick("class:("), None, "a pattern that does not compile");
    // With nothing focused there is no workspace to look on.
    assert_eq!(
        Selector::parse("floating").and_then(|selector| selector.pick(&windows, None)),
        None
    );
}
