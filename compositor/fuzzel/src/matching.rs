//! What is typed, against the entries: fuzzel's `match.c`.
//!
//! The three modes, the fields each is tried against, the order a field is
//! consulted in to decide how an entry matched, and the order the matches
//! are listed in are fuzzel's. So are two of its oddities, because they
//! decide what a person sees ranked first:
//!
//! * In `fzf` mode the matched positions of the title are shared between the
//!   words typed, and a word that fails to match part-way leaves the
//!   positions of the words before it in place.
//! * With several words typed, the keywords and categories of an entry are
//!   tried only up to the first one that an earlier word failed to match
//!   (fuzzel's loop stops advancing its index there).
//!
//! What is not fuzzel's is the sort's treatment of ties: fuzzel sorts with
//! `qsort`, which promises no order for equal elements, and its comparison
//! of title lengths is not antisymmetric. Here the sort is stable and a
//! shorter title comes first, which is what glibc's merge sort does with
//! fuzzel's comparison.

use std::cmp::Ordering;

use crate::config::{Fields, Fuzzy, MatchMode};
use crate::desktop::{Application, Text, lower_char};

/// How an entry matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matched {
    /// It did not; or nothing was typed.
    None,
    /// Every word was found as it was typed.
    Exact,
    /// A word was found within the Levenshtein limits.
    Fuzzy,
}

/// A run of the title that matched, in code points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Substring {
    /// Where it starts.
    pub start: usize,
    /// How long it is.
    pub len: usize,
}

/// One entry that matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// How.
    pub matched: Matched,
    /// Which entry: an index into the application list.
    pub app: usize,
    /// The runs of the title to draw in the match colour.
    pub pos: Vec<Substring>,
    /// The longest run in the title.
    pub score: usize,
    /// Whether the first run starts a word.
    pub word_boundary: bool,
}

/// The words typed, lowercased: split at spaces, runs of spaces collapsed,
/// a trailing space not a word -- fuzzel's tokeniser.
#[must_use]
pub fn tokens(prompt: &[char]) -> Vec<Text> {
    let mut out: Vec<Text> = vec![Vec::new()];
    for &c in prompt {
        if c != ' ' {
            if let Some(last) = out.last_mut() {
                last.push(lower_char(c));
            }
            continue;
        }
        if out.last().is_some_and(Vec::is_empty) {
            continue;
        }
        out.push(Vec::new());
    }
    if out.last().is_some_and(Vec::is_empty) {
        let _ = out.pop();
    }
    out
}

/// `memmem`: where `needle` first occurs in `haystack`.
#[must_use]
pub fn find(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// fuzzel's `match_fzf`: the needle consumed left to right, each step the
/// longest run of it found after the last. Runs are appended to `pos`; a
/// haystack used up before the needle clears it.
pub fn fzf(haystack: &[char], needle: &[char], pos: Option<&mut Vec<Substring>>) -> Matched {
    let mut sink = Vec::new();
    let (pos, record) = match pos {
        Some(pos) => (pos, true),
        None => (&mut sink, false),
    };
    let mut result = Matched::None;
    let mut n = 0;
    let mut search_start = 0;
    while n < needle.len() {
        if search_start >= haystack.len() {
            if record {
                pos.clear();
            }
            return Matched::None;
        }
        let rest = needle.get(n..).unwrap_or_default();
        let mut longest = 0;
        let mut longest_at = 0;
        for start in search_start..haystack.len() {
            let run = haystack
                .get(start..)
                .unwrap_or_default()
                .iter()
                .zip(rest)
                .take_while(|(h, n)| h == n)
                .count();
            if run > longest {
                longest = run;
                longest_at = start;
            }
            if run == rest.len() {
                break;
            }
        }
        if longest == 0 {
            return Matched::None;
        }
        if record {
            pos.push(Substring {
                start: longest_at,
                len: longest,
            });
        }
        result = Matched::Exact;
        n += longest;
        search_start = longest_at + longest;
    }
    result
}

/// Which way a Levenshtein cell was reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Unset,
    First,
    Second,
    Third,
}

/// fuzzel's `match_levenshtein`: the substring of `src` closest to `pat`,
/// if it is within `fuzzy`'s limits. Returns its start and length.
#[must_use]
pub fn levenshtein(src: &[char], pat: &[char], fuzzy: &Fuzzy) -> Option<(usize, usize)> {
    if pat.len() < fuzzy.min_length || src.len() < pat.len() {
        return None;
    }
    let width = src.len() + 1;
    let mut distance = vec![0usize; (pat.len() + 1) * width];
    let mut choice = vec![Choice::Unset; (pat.len() + 1) * width];
    let cell = |i: usize, j: usize| i * width + j;
    for i in 1..=pat.len() {
        if let (Some(d), Some(c)) = (distance.get_mut(cell(i, 0)), choice.get_mut(cell(i, 0))) {
            *d = i;
            *c = Choice::First;
        }
    }
    for i in 1..=pat.len() {
        for j in 1..=src.len() {
            let cost = usize::from(src.get(j - 1) != pat.get(i - 1));
            let at = |i, j| distance.get(cell(i, j)).copied().unwrap_or(usize::MAX / 2);
            let first = at(i - 1, j) + 1;
            let second = at(i, j - 1) + 1;
            let third = at(i - 1, j - 1) + cost;
            let shortest = first.min(second).min(third);
            if let Some(d) = distance.get_mut(cell(i, j)) {
                *d = shortest;
            }
            if let Some(c) = choice.get_mut(cell(i, j)) {
                *c = if shortest == first {
                    Choice::First
                } else if shortest == second {
                    Choice::Second
                } else {
                    Choice::Third
                };
            }
        }
    }
    let last_row = |j: usize| {
        distance
            .get(cell(pat.len(), j))
            .copied()
            .unwrap_or(usize::MAX)
    };
    let mut c = 0;
    let mut best = last_row(0);
    for j in (0..=src.len()).rev() {
        if last_row(j) < best {
            best = last_row(j);
            c = j;
        }
    }
    let end = c;
    let mut r = pat.len();
    while r > 0 {
        match choice.get(cell(r, c)).copied().unwrap_or(Choice::Unset) {
            Choice::Unset => return None,
            Choice::First => r -= 1,
            Choice::Second => c = c.saturating_sub(1),
            Choice::Third => {
                r -= 1;
                c = c.saturating_sub(1);
            }
        }
    }
    let len = end - c;
    let discrepancy = len.abs_diff(pat.len());
    (discrepancy <= fuzzy.max_length_discrepancy && best <= fuzzy.max_distance).then_some((c, len))
}

/// How one field matched one word, and where, in the three modes.
fn field(
    mode: MatchMode,
    fuzzy: &Fuzzy,
    haystack: &[char],
    tok: &[char],
) -> (Matched, Option<(usize, usize)>) {
    match mode {
        MatchMode::Exact => match find(haystack, tok) {
            Some(at) => (Matched::Exact, Some((at, tok.len()))),
            None => (Matched::None, None),
        },
        MatchMode::Fzf => (fzf(haystack, tok, None), None),
        MatchMode::Fuzzy => {
            if let Some(at) = find(haystack, tok) {
                (Matched::Exact, Some((at, tok.len())))
            } else if let Some(found) = levenshtein(haystack, tok, fuzzy) {
                (Matched::Fuzzy, Some(found))
            } else {
                (Matched::None, None)
            }
        }
    }
}

/// A field's running result after word `t`: the first word sets it, a
/// failing word clears it, and a fuzzy word turns an exact one fuzzy.
fn combine(t: usize, running: Matched, this: Matched) -> Matched {
    if t == 0 || this == Matched::None || running == Matched::Exact {
        this
    } else {
        running
    }
}

/// The matcher's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Matcher {
    /// `fields`.
    pub fields: Fields,
    /// `match-mode`.
    pub mode: MatchMode,
    /// The fuzzy limits.
    pub fuzzy: Fuzzy,
    /// `sort-result`.
    pub sort: bool,
}

impl Matcher {
    /// One word against the title, the runs it matched added to `pos`: in
    /// `fzf` mode by [`fzf`] itself, otherwise joined to the last run when
    /// they touch, as fuzzel does.
    fn title_word(&self, app: &Application, tok: &[char], pos: &mut Vec<Substring>) -> Matched {
        if self.mode == MatchMode::Fzf {
            return fzf(&app.title_lower, tok, Some(pos));
        }
        let (this, found) = field(self.mode, &self.fuzzy, &app.title_lower, tok);
        let Some((start, len)) = found.filter(|(_, len)| *len > 0) else {
            return this;
        };
        match pos.last_mut() {
            Some(last) if last.start + last.len == start => last.len += len,
            _ => pos.push(Substring { start, len }),
        }
        this
    }

    /// fuzzel's `match_app`: how `app` (at `index`) matches `tokens`, or
    /// `None`.
    #[must_use]
    pub fn match_app(&self, index: usize, app: &Application, tokens: &[Text]) -> Option<Match> {
        let fields = self.fields;
        let mode = self.mode;
        let fuzzy = &self.fuzzy;
        let mut pos: Vec<Substring> = Vec::new();
        let mut name = Matched::None;
        let mut filename = Matched::None;
        let mut generic = Matched::None;
        let mut exec = Matched::None;
        let mut comment = Matched::None;
        let mut nth = Matched::None;
        let mut keywords = vec![Matched::None; app.keywords.len()];
        let mut categories = vec![Matched::None; app.categories.len()];

        for (t, tok) in tokens.iter().enumerate() {
            let live = |running: Matched| t == 0 || running != Matched::None;

            if fields.has(Fields::NAME) && live(name) {
                name = combine(t, name, self.title_word(app, tok, &mut pos));
            }

            let simple = |running: &mut Matched, haystack: Option<&[char]>| {
                if let Some(haystack) = haystack
                    && live(*running)
                {
                    *running = combine(t, *running, field(mode, fuzzy, haystack, tok).0);
                }
            };
            if fields.has(Fields::NTH) {
                simple(&mut nth, app.dmenu_match_nth.as_deref());
            }
            if fields.has(Fields::FILENAME) && app.id.is_some() {
                simple(&mut filename, Some(&app.basename));
            }
            if fields.has(Fields::GENERIC) {
                simple(&mut generic, app.generic_name.as_deref());
            }
            if fields.has(Fields::EXEC) && app.exec.is_some() {
                simple(&mut exec, Some(&app.wexec));
            }
            if fields.has(Fields::COMMENT) {
                simple(&mut comment, app.comment.as_deref());
            }
            let listed = |slots: &mut [Matched], items: &[Text]| {
                // fuzzel's loop `continue`s at the first item an earlier
                // word failed without moving its index on, so no later item
                // is tried either.
                for (slot, item) in slots
                    .iter_mut()
                    .zip(items)
                    .take_while(|(slot, _)| live(**slot))
                {
                    *slot = combine(t, *slot, field(mode, fuzzy, item, tok).0);
                }
            };
            if fields.has(Fields::KEYWORDS) {
                listed(&mut keywords, &app.keywords);
            }
            if fields.has(Fields::CATEGORIES) {
                listed(&mut categories, &app.categories);
            }
        }

        // A list field is exact if any item is, and fuzzy if any is.
        let any = |slots: &[Matched]| {
            slots
                .iter()
                .fold(Matched::None, |all, one| match (all, one) {
                    (Matched::Exact, _) => Matched::Exact,
                    (_, Matched::None) => all,
                    (_, one) => *one,
                })
        };
        let keywords = any(&keywords);
        let categories = any(&categories);

        let score = if fields.has(Fields::NAME) {
            pos.iter().map(|p| p.len).max().unwrap_or(0)
        } else {
            0
        };
        let matched = [
            name, filename, generic, exec, comment, keywords, categories, nth,
        ]
        .into_iter()
        .find(|m| *m != Matched::None)?;
        let word_boundary = fields.has(Fields::NAME)
            && pos.first().is_some_and(|first| {
                first.start == 0
                    || app
                        .title_lower
                        .get(first.start - 1)
                        .is_some_and(|c| c.is_whitespace())
            });
        Some(Match {
            matched,
            app: index,
            pos,
            score,
            word_boundary,
        })
    }
}

/// fuzzel's `match_compar`, with the ties made stable and the title length
/// made a real order.
#[must_use]
pub fn compare(a: &Match, b: &Match, apps: &[Application]) -> Ordering {
    let count = |m: &Match| apps.get(m.app).map_or(0, |app| app.count);
    let title_len = |m: &Match| apps.get(m.app).map_or(0, |app| app.title.len());
    if a.matched != b.matched {
        return if a.matched == Matched::Exact {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    b.score
        .cmp(&a.score)
        .then_with(|| b.word_boundary.cmp(&a.word_boundary))
        .then_with(|| a.pos.len().cmp(&b.pos.len()))
        .then_with(|| count(b).cmp(&count(a)))
        .then_with(|| match (a.pos.first(), b.pos.first()) {
            (Some(pa), Some(pb)) => pa
                .start
                .cmp(&pb.start)
                .then_with(|| title_len(a).cmp(&title_len(b))),
            _ => Ordering::Equal,
        })
}

/// The list of matches, the page it is shown in and the selection: fuzzel's
/// `struct matches` without its threads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matches {
    /// The settings.
    pub matcher: Matcher,
    /// The matches, in order.
    pub list: Vec<Match>,
    /// The selected one, as an index into `list`.
    pub selected: usize,
    /// Rows a page: `lines`.
    pub per_page: usize,
    /// How many pages.
    pub page_count: usize,
}

impl Matches {
    /// An empty list with these settings.
    #[must_use]
    pub fn new(matcher: Matcher, per_page: usize) -> Self {
        Self {
            matcher,
            list: Vec::new(),
            selected: 0,
            per_page,
            page_count: 0,
        }
    }

    /// fuzzel's `matches_update_internal`: match `prompt` against every
    /// visible entry, or with `incremental`, against the current matches
    /// only -- which is what typing a character does outside fuzzy mode, and
    /// which keeps the order ties were in.
    pub fn update(
        &mut self,
        apps: &[Application],
        prompt: &[char],
        incremental: bool,
        all_loaded: bool,
    ) {
        if prompt.is_empty() {
            self.list = apps
                .iter()
                .enumerate()
                .filter(|(_, app)| app.visible)
                .map(|(app, _)| Match {
                    matched: Matched::None,
                    app,
                    pos: Vec::new(),
                    score: 0,
                    word_boundary: false,
                })
                .collect();
            if self.matcher.sort && all_loaded {
                self.list.sort_by(|a, b| compare(a, b, apps));
            }
            self.finish(true);
            return;
        }
        let tokens = tokens(prompt);
        let candidates: Vec<usize> = if incremental {
            self.list.iter().map(|m| m.app).collect()
        } else {
            (0..apps.len())
                .filter(|&at| apps.get(at).is_some_and(|app| app.visible))
                .collect()
        };
        self.list = candidates
            .into_iter()
            .filter_map(|at| {
                apps.get(at)
                    .and_then(|app| self.matcher.match_app(at, app, &tokens))
            })
            .collect();
        if self.matcher.sort {
            self.list.sort_by(|a, b| compare(a, b, apps));
        }
        self.finish(false);
    }

    /// Work out the pages and keep the selection inside the list.
    fn finish(&mut self, _empty_prompt: bool) {
        self.page_count = if self.per_page > 0 {
            self.list.len().div_ceil(self.per_page)
        } else {
            1
        };
        if self.selected >= self.list.len() && self.selected > 0 {
            self.selected = self.list.len().saturating_sub(1);
        }
    }

    /// The page the selection is on.
    #[must_use]
    pub fn page(&self) -> usize {
        self.selected
            .checked_div(self.per_page)
            .unwrap_or(self.selected)
    }

    /// The matches on the current page.
    #[must_use]
    pub fn on_page(&self) -> &[Match] {
        if self.per_page == 0 || self.list.is_empty() {
            return &[];
        }
        let start = self.page() * self.per_page;
        let end = (start + self.per_page).min(self.list.len());
        self.list.get(start..end).unwrap_or_default()
    }

    /// Where the selection is on its page.
    #[must_use]
    pub fn index_on_page(&self) -> usize {
        if self.per_page == 0 {
            0
        } else {
            self.selected % self.per_page
        }
    }

    /// The selected match.
    #[must_use]
    pub fn selected_match(&self) -> Option<&Match> {
        self.list.get(self.selected)
    }

    /// Select row `row` of the current page.
    pub fn select_on_page(&mut self, row: usize) -> bool {
        let at = self.page() * self.per_page + row;
        if at < self.list.len() {
            self.selected = at;
            return true;
        }
        false
    }

    /// Select `at`, if it is in the list.
    pub fn select(&mut self, at: usize) -> bool {
        if at < self.list.len() {
            self.selected = at;
            return true;
        }
        false
    }

    /// `--select`: the first match whose title contains `string`.
    pub fn select_containing(&mut self, apps: &[Application], string: &str) -> bool {
        let needle: Vec<char> = string.chars().collect();
        let found = self.list.iter().position(|m| {
            apps.get(m.app)
                .is_some_and(|app| find(&app.title, &needle).is_some())
        });
        found.is_some_and(|at| self.select(at))
    }

    /// `first`.
    pub fn first(&mut self) -> bool {
        if self.list.is_empty() || self.selected == 0 {
            return false;
        }
        self.selected = 0;
        true
    }

    /// `last`.
    pub fn last(&mut self) -> bool {
        if self.list.is_empty() || self.selected + 1 >= self.list.len() {
            return false;
        }
        self.selected = self.list.len() - 1;
        true
    }

    /// `prev`, or `prev-with-wrap`.
    pub fn prev(&mut self, wrap: bool) -> bool {
        if self.selected > 0 {
            self.selected -= 1;
            true
        } else if wrap && self.list.len() > 1 {
            self.selected = self.list.len() - 1;
            true
        } else {
            false
        }
    }

    /// `next`, or `next-with-wrap`.
    pub fn next(&mut self, wrap: bool) -> bool {
        if self.selected + 1 < self.list.len() {
            self.selected += 1;
            true
        } else if wrap && self.list.len() > 1 {
            self.selected = 0;
            true
        } else {
            false
        }
    }

    /// `prev-page`; `scrolling` is the mouse wheel, which does not go to
    /// the first entry of the first page.
    pub fn prev_page(&mut self, scrolling: bool) -> bool {
        if self.page() > 0 {
            self.selected -= self.per_page;
            true
        } else if !scrolling && self.selected > 0 {
            self.selected = 0;
            true
        } else {
            false
        }
    }

    /// `next-page`.
    pub fn next_page(&mut self, scrolling: bool) -> bool {
        if self.page() + 1 < self.page_count {
            self.selected = (self.selected + self.per_page).min(self.list.len().saturating_sub(1));
            true
        } else if !scrolling && self.selected + 1 < self.list.len() {
            self.selected = self.list.len() - 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
