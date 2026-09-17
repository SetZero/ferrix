//! What the patterns a window rule uses must do.
//!
//! The rule everywhere is `RE2::FullMatch`: the whole string matches or
//! nothing does. Every example here is a pattern somebody's `hyprland.conf`
//! has in it, or the shape of one.

use crate::Regex;

fn matches(pattern: &str, text: &str) -> bool {
    Regex::new(pattern)
        .unwrap_or_else(|why| panic!("{pattern}: {why}"))
        .matches(text)
}

#[test]
fn a_literal_matches_the_whole_string_and_no_part_of_it() {
    assert!(matches("foot", "foot"));
    assert!(!matches("foot", "footclient"));
    assert!(!matches("foot", "xfoot"));
    assert!(!matches("foot", "FOOT"), "matching is case-sensitive");
    assert!(matches("", ""));
    assert!(!matches("", "a"));
}

/// The anchors every rule in the wild carries, which a full match makes
/// redundant: `^(foot)$` and `foot` are the same pattern.
#[test]
fn the_anchors_change_nothing() {
    assert!(matches("^(foot)$", "foot"));
    assert!(!matches("^(foot)$", "footclient"));
    assert!(matches("^foot$", "foot"));
    assert!(matches("(foot)", "foot"));
}

#[test]
fn a_dot_is_any_character_and_a_star_is_any_number_of_them() {
    assert!(matches("f.ot", "foot"));
    assert!(matches("f.ot", "f0ot"));
    assert!(!matches("f.ot", "fot"), "a dot is one character, not none");
    assert!(matches(".*", "anything at all"));
    assert!(matches(".*", ""));
    assert!(matches("foot.*", "footclient"));
    assert!(matches("^(kitty|foot).*$", "kitty-terminal"));
    assert!(!matches("^(kitty|foot).*$", "alacritty"));
}

#[test]
fn the_quantifiers_take_what_they_should() {
    assert!(matches("a+", "aaa"));
    assert!(!matches("a+", ""), "`+` wants one at least");
    assert!(matches("a*", ""));
    assert!(matches("colou?r", "color"));
    assert!(matches("colou?r", "colour"));
    assert!(!matches("colou?r", "colouur"));
    // Greedy, with backtracking: the `.*` gives characters back so that the
    // `g` at the end has one to match.
    assert!(matches(".*g", "a long string ending in g"));
    assert!(!matches(".*g", "a long string ending in h"));
}

#[test]
fn a_class_matches_what_is_in_it() {
    assert!(matches("[abc]", "b"));
    assert!(!matches("[abc]", "d"));
    assert!(matches("[a-z]+", "lowercase"));
    assert!(!matches("[a-z]+", "Uppercase"));
    assert!(matches("[^0-9]+", "letters"));
    assert!(!matches("[^0-9]+", "digits9"));
    // A `-` at the end is a literal one, and a `]` first is too.
    assert!(matches("[a-]+", "a-a"));
    assert!(matches("[]a]+", "]a"));
    // The escapes inside a class.
    assert!(matches(r"[\]]", "]"));
    assert!(matches(r"[\-]", "-"));
}

#[test]
fn alternatives_and_groups_nest() {
    assert!(matches("^(firefox|chromium)$", "firefox"));
    assert!(matches("^(firefox|chromium)$", "chromium"));
    assert!(!matches("^(firefox|chromium)$", "safari"));
    assert!(matches("^(a(b|c)d)$", "abd"));
    assert!(matches("^(a(b|c)d)$", "acd"));
    assert!(!matches("^(a(b|c)d)$", "ad"));
    // A quantified group.
    assert!(matches("^(ab)+$", "ababab"));
    assert!(!matches("^(ab)+$", "ababa"));
    // A bare alternation splits the whole pattern.
    assert!(matches("cat|dog", "dog"));
    assert!(!matches("cat|dog", "cats"));
}

#[test]
fn an_escape_takes_the_next_character_literally() {
    assert!(matches(r"foo\.bar", "foo.bar"));
    assert!(!matches(r"foo\.bar", "fooxbar"));
    assert!(matches(r"\(paren\)", "(paren)"));
    assert!(matches(r"a\*b", "a*b"));
    assert!(matches(r"tab\there", "tab\there"));
}

/// Hyprland's own negation, which its match engine does with a prefix rather
/// than with a pattern.
#[test]
fn negative_inverts_the_answer() {
    assert!(matches("negative:^(foot)$", "kitty"));
    assert!(!matches("negative:^(foot)$", "foot"));
    assert_eq!(
        Regex::new("negative:^(foot)$").expect("a pattern").source(),
        "negative:^(foot)$"
    );
}

/// What is not done says so, rather than matching wrongly: a rule that
/// silently matched everything would float every window a person owns.
#[test]
fn what_is_not_done_is_refused() {
    for pattern in [
        r"a{2,3}",
        r"\d+",
        r"\w",
        "(unclosed",
        "unopened)",
        "[unterminated",
        "*nothing-before",
        "a**",
        r"ends-with\",
        "[z-a]",
    ] {
        assert!(
            Regex::new(pattern).is_err(),
            "`{pattern}` should have been refused"
        );
    }
}

/// A pattern that would take for ever gives up instead. A backtracking
/// matcher on a quantified alternation is exponential, and a pattern comes
/// from a configuration file.
#[test]
fn a_pattern_that_cannot_finish_gives_up() {
    let pattern = Regex::new("^(a*)*b$").expect("a pattern");
    let text = "a".repeat(40);
    // Whichever way it goes, it comes back: what is being tested is that it
    // returns at all.
    let _ = pattern.matches(&text);
}
