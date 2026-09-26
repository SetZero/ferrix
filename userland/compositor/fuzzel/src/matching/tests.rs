//! Matching and ranking against fuzzel's `match.c`.

use super::{Match, Matched, Matcher, Matches, Substring, find, fzf, levenshtein, tokens};
use crate::config::{Fields, Fuzzy, MatchMode};
use crate::desktop::{Application, lower};

const FUZZY: Fuzzy = Fuzzy {
    min_length: 3,
    max_length_discrepancy: 2,
    max_distance: 1,
};

fn chars(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn app(title: &str, file: &str) -> Application {
    Application {
        id: Some(format!("{file}.desktop")),
        title: chars(title),
        title_lower: lower(title),
        basename: lower(file),
        exec: Some(file.to_owned()),
        wexec: lower(file),
        visible: true,
        ..Application::default()
    }
}

fn matcher(mode: MatchMode) -> Matcher {
    Matcher {
        fields: Fields(Fields::FILENAME | Fields::NAME | Fields::GENERIC),
        mode,
        fuzzy: FUZZY,
        sort: true,
    }
}

fn ranked(apps: &[Application], typed: &str, mode: MatchMode) -> Vec<String> {
    let mut matches = Matches::new(matcher(mode), 12);
    matches.update(apps, &chars(typed), false, true);
    matches
        .list
        .iter()
        .filter_map(|m| apps.get(m.app).map(Application::title_string))
        .collect()
}

#[test]
fn words_are_split_at_spaces() {
    assert_eq!(
        tokens(&chars("Fire  Fox ")),
        vec![chars("fire"), chars("fox")]
    );
    assert_eq!(tokens(&chars("  a")), vec![chars("a")]);
    assert!(tokens(&chars("   ")).is_empty());
}

#[test]
fn fzf_takes_the_longest_run_each_step() {
    let mut pos = Vec::new();
    assert_eq!(
        fzf(&chars("firefox"), &chars("fox"), Some(&mut pos)),
        Matched::Exact
    );
    assert_eq!(pos, vec![Substring { start: 4, len: 3 }]);
    let mut pos = Vec::new();
    assert_eq!(
        fzf(&chars("terminal"), &chars("tml"), Some(&mut pos)),
        Matched::Exact
    );
    assert_eq!(
        pos,
        vec![
            Substring { start: 0, len: 1 },
            Substring { start: 3, len: 1 },
            Substring { start: 7, len: 1 }
        ]
    );
    assert_eq!(fzf(&chars("abc"), &chars("abd"), None), Matched::None);
    // Out of haystack: the runs so far are cleared.
    let mut pos = Vec::new();
    assert_eq!(
        fzf(&chars("ab"), &chars("abb"), Some(&mut pos)),
        Matched::None
    );
    assert!(pos.is_empty());
}

#[test]
fn levenshtein_within_the_limits() {
    assert_eq!(
        levenshtein(&chars("firefox"), &chars("firefx"), &FUZZY),
        Some((0, 7))
    );
    assert_eq!(levenshtein(&chars("firefox"), &chars("frfx"), &FUZZY), None);
    // Shorter than fuzzy-min-length is not tried.
    assert_eq!(levenshtein(&chars("firefox"), &chars("fx"), &FUZZY), None);
    assert_eq!(find(&chars("firefox"), &chars("ref")), Some(2));
}

#[test]
fn the_longest_run_ranks_first() {
    // fuzzel's own comment: searching for 'oo' sorts 'foot' before
    // 'firefox browser'.
    let apps = vec![app("Firefox Browser", "firefox"), app("Foot", "foot")];
    assert_eq!(
        ranked(&apps, "oo", MatchMode::Fzf),
        vec!["Foot", "Firefox Browser"]
    );
}

#[test]
fn part_of_a_name_ranks_that_application_first() {
    let apps = vec![
        app("Chromium", "chromium"),
        app("Settings", "settings"),
        app("Terminal", "term"),
        app("Text Editor", "editor"),
    ];
    assert_eq!(
        ranked(&apps, "ter", MatchMode::Fzf)
            .first()
            .map(String::as_str),
        Some("Terminal")
    );
    // A word boundary breaks a tie in score.
    let apps = vec![app("Poster", "a"), app("Termite", "b")];
    assert_eq!(
        ranked(&apps, "ter", MatchMode::Fzf),
        vec!["Termite", "Poster"]
    );
}

#[test]
fn modes_differ() {
    let apps = vec![app("Firefox", "firefox"), app("Files", "nautilus")];
    assert_eq!(ranked(&apps, "ffx", MatchMode::Fzf), vec!["Firefox"]);
    assert!(ranked(&apps, "ffx", MatchMode::Exact).is_empty());
    assert_eq!(ranked(&apps, "firefx", MatchMode::Fuzzy), vec!["Firefox"]);
    assert!(ranked(&apps, "firefx", MatchMode::Exact).is_empty());
    // A wrong letter is beyond fzf, but within one edit.
    assert!(ranked(&apps, "firefix", MatchMode::Fzf).is_empty());
    assert_eq!(ranked(&apps, "firefix", MatchMode::Fuzzy), vec!["Firefox"]);
    // The filename matches too: fields defaults include it.
    assert_eq!(ranked(&apps, "naut", MatchMode::Exact), vec!["Files"]);
}

#[test]
fn every_word_must_match_a_field() {
    let apps = vec![
        app("Firefox Web Browser", "firefox"),
        app("Web Camera", "cheese"),
    ];
    assert_eq!(
        ranked(&apps, "web fire", MatchMode::Fzf),
        vec!["Firefox Web Browser"]
    );
    assert_eq!(
        ranked(&apps, "web", MatchMode::Fzf),
        vec!["Web Camera", "Firefox Web Browser"]
    );
}

#[test]
fn exact_beats_fuzzy_and_launches_break_ties() {
    let mut firefox = app("Firefox", "firefox");
    let mut firewall = app("Firewall", "firewall");
    firewall.count = 3;
    firefox.count = 0;
    let apps = vec![firefox.clone(), firewall.clone()];
    assert_eq!(
        ranked(&apps, "fire", MatchMode::Fzf),
        vec!["Firewall", "Firefox"]
    );
    // With nothing typed, the launch count alone orders the list.
    assert_eq!(
        ranked(&apps, "", MatchMode::Fzf),
        vec!["Firewall", "Firefox"]
    );
    firewall.count = 0;
    let apps = vec![firefox, firewall];
    // A shorter title wins the last tie.
    assert_eq!(
        ranked(&apps, "fire", MatchMode::Fzf),
        vec!["Firefox", "Firewall"]
    );
}

#[test]
fn keywords_and_generic_names() {
    let mut a = app("Nautilus", "org.gnome.nautilus");
    a.keywords = vec![lower("folder"), lower("manager")];
    a.generic_name = Some(lower("File Manager"));
    let apps = vec![a];
    assert_eq!(ranked(&apps, "manager", MatchMode::Exact), vec!["Nautilus"]);
    let with_keywords = Matcher {
        fields: Fields(Fields::KEYWORDS),
        ..matcher(MatchMode::Exact)
    };
    let mut matches = Matches::new(with_keywords, 5);
    matches.update(&apps, &chars("folder"), false, true);
    assert_eq!(matches.list.len(), 1);
    matches.update(&apps, &chars("zzz"), false, true);
    assert!(matches.list.is_empty());
}

#[test]
fn hidden_entries_never_match() {
    let mut hidden = app("Hidden", "hidden");
    hidden.visible = false;
    let apps = vec![hidden, app("Shown", "shown")];
    assert_eq!(ranked(&apps, "", MatchMode::Fzf), vec!["Shown"]);
    assert!(ranked(&apps, "hid", MatchMode::Fzf).is_empty());
}

#[test]
fn incremental_keeps_the_previous_order_of_ties() {
    let apps = vec![app("ab", "x1"), app("ba", "x2")];
    let mut matches = Matches::new(matcher(MatchMode::Fzf), 10);
    matches.update(&apps, &chars("a"), false, true);
    let order: Vec<usize> = matches.list.iter().map(|m| m.app).collect();
    matches.update(&apps, &chars("a"), true, true);
    assert_eq!(
        matches.list.iter().map(|m| m.app).collect::<Vec<_>>(),
        order
    );
}

#[test]
fn pages_and_selection() {
    let apps: Vec<Application> = (0..7)
        .map(|n| app(&format!("app{n}"), &format!("a{n}")))
        .collect();
    let mut m = Matches::new(matcher(MatchMode::Fzf), 3);
    m.update(&apps, &[], false, true);
    assert_eq!((m.list.len(), m.page_count), (7, 3));
    assert_eq!(m.on_page().len(), 3);
    assert!(!m.prev(false));
    assert!(m.prev(true));
    assert_eq!((m.selected, m.page(), m.index_on_page()), (6, 2, 0));
    assert_eq!(m.on_page().len(), 1);
    assert!(m.next(true));
    assert_eq!(m.selected, 0);
    assert!(m.next_page(false));
    assert_eq!(m.selected, 3);
    assert!(m.next_page(false));
    assert_eq!(m.selected, 6);
    assert!(!m.next_page(false));
    assert!(m.prev_page(false));
    assert_eq!(m.selected, 3);
    assert!(m.first() && !m.first());
    assert!(m.last());
    assert_eq!(m.selected, 6);
    // Narrowing the list pulls the selection back inside it.
    m.update(&apps, &chars("app1"), false, true);
    assert_eq!(m.selected, 0);
    assert_eq!(
        m.selected_match().map(|x: &Match| x.pos.clone()),
        Some(vec![Substring { start: 0, len: 4 }])
    );
}
