//! `--dmenu`: entries from standard input, the choice on standard output
//! (fuzzel's `dmenu.c` and `column.c`).
//!
//! Each line (or NUL-separated record, with `--dmenu0`) is an entry. A line
//! may carry rofi's icon extension, `text\0icon\x1fname[,fallback…]`. The
//! `nth` options pick columns out of a line by a template: `{2}` is the
//! second column, `{2..4}` the second to fourth joined by spaces, `{2..}`
//! the second onwards, and text around the braces is kept.

use crate::desktop::{Application, lower};

/// fuzzel's `nth_column`: `format` filled in from `line`'s columns.
#[must_use]
pub fn nth_column(line: &str, delim: char, format: &str) -> String {
    let columns: Vec<&str> = line.split(delim).collect();
    let count = columns.len();
    let mut out = String::new();
    let mut copy_start = 0;
    let mut search = 0;
    loop {
        let Some(open) = format
            .get(search..)
            .and_then(|rest| rest.find('{'))
            .map(|at| at + search)
        else {
            out.push_str(format.get(copy_start..).unwrap_or_default());
            break;
        };
        let Some(close) = format
            .get(open..)
            .and_then(|rest| rest.find('}'))
            .map(|at| at + open)
        else {
            // An unclosed brace: fuzzel moves one character on and looks
            // again, which finds the same brace until the loop gives up at
            // the text's end -- the brace is kept as text.
            out.push_str(format.get(copy_start..).unwrap_or_default());
            break;
        };
        let inside = format.get(open + 1..close).unwrap_or_default();
        match range(inside, count) {
            Some((first, last)) => {
                out.push_str(format.get(copy_start..open).unwrap_or_default());
                let picked: Vec<&str> = columns.get(first - 1..last).unwrap_or_default().to_vec();
                out.push_str(&picked.join(" "));
                copy_start = close + 1;
                search = close + 1;
            }
            None => {
                // An invalid index: skip past this brace and keep it as text.
                search = open + 1;
            }
        }
    }
    out
}

/// `N`, `N..M` or `N..` against `count` columns, one-based and inclusive.
fn range(inside: &str, count: usize) -> Option<(usize, usize)> {
    let (first, last) = match inside.split_once("..") {
        None => {
            let n: usize = digits(inside)?;
            (n, n)
        }
        Some((first, "")) => (digits(first)?, count),
        Some((first, last)) => (digits(first)?, digits(last)?),
    };
    (first != 0 && first <= count && last <= count && last >= first).then_some((first, last))
}

fn digits(text: &str) -> Option<usize> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// What `--with-nth=` and its siblings mean: a number is that column, zero
/// is none, anything else is a template.
#[must_use]
pub fn nth_option(value: &str) -> Option<String> {
    let number: String = value.chars().take_while(char::is_ascii_digit).collect();
    if let Ok(n) = number.parse::<u32>() {
        return (n != 0).then(|| format!("{{{n}}}"));
    }
    (!value.is_empty()).then(|| value.to_owned())
}

/// The entries in `input`, as fuzzel's `dmenu_load_entries` makes them.
#[must_use]
pub fn entries(
    input: &[u8],
    delim: u8,
    with_nth: Option<&str>,
    match_nth: Option<&str>,
    nth_delim: char,
) -> Vec<Application> {
    let mut out = Vec::new();
    if input.is_empty() {
        return out;
    }
    let body = input.strip_suffix(&[delim]).unwrap_or(input);
    for (index, record) in body.split(|b| *b == delim).enumerate() {
        let (text, icon) = match record.iter().position(|b| *b == 0) {
            Some(at) => {
                let extra = record.get(at..).unwrap_or_default();
                let icon = (extra.len() > 6 && extra.starts_with(b"\0icon\x1f")).then(|| {
                    String::from_utf8_lossy(extra.get(6..).unwrap_or_default()).into_owned()
                });
                (record.get(..at).unwrap_or_default(), icon)
            }
            None => (record, None),
        };
        // A record that is not UTF-8 is dropped, and still counts for
        // `--index`.
        let Ok(line) = std::str::from_utf8(text) else {
            continue;
        };
        let title = with_nth.map_or_else(
            || line.to_owned(),
            |format| nth_column(line, nth_delim, format),
        );
        out.push(Application {
            index,
            dmenu_input: Some(line.to_owned()),
            dmenu_match_nth: match_nth.map(|format| lower(&nth_column(line, nth_delim, format))),
            title_lower: lower(&title),
            title: title.chars().collect(),
            icon_name: icon,
            visible: true,
            startup_notify: true,
            ..Application::default()
        });
    }
    out
}

/// What is printed for the choice: the entry (or the typed text), through
/// `--accept-nth`, or its index.
#[must_use]
pub fn output(
    app: Option<&Application>,
    typed: &str,
    index_mode: bool,
    accept_nth: Option<&str>,
    nth_delim: char,
) -> String {
    if index_mode {
        // fuzzel prints -1 for typed text with no entry.
        return app.map_or_else(|| "-1".to_owned(), |app| app.index.to_string());
    }
    let text = app
        .and_then(|app| app.dmenu_input.clone())
        .unwrap_or_else(|| typed.to_owned());
    match accept_nth {
        Some(format) => nth_column(&text, nth_delim, format),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::{entries, nth_column, nth_option, output};

    #[test]
    fn columns() {
        let line = "one\ttwo\tthree\tfour";
        assert_eq!(nth_column(line, '\t', "{2}"), "two");
        assert_eq!(nth_column(line, '\t', "{2..3}"), "two three");
        assert_eq!(nth_column(line, '\t', "{3..}"), "three four");
        assert_eq!(nth_column(line, '\t', "[{1}] {4}!"), "[one] four!");
        assert_eq!(nth_column(line, '\t', "{9} {1}"), "{9} one");
        assert_eq!(nth_column(line, '\t', "{x}"), "{x}");
        assert_eq!(nth_column(line, '\t', "a {1"), "a {1");
        assert_eq!(nth_option("2"), Some("{2}".to_owned()));
        assert_eq!(nth_option("0"), None);
        assert_eq!(nth_option("{1} {2}"), Some("{1} {2}".to_owned()));
    }

    #[test]
    fn records() {
        let input = b"alpha\nbeta\0icon\x1ffirefox\n\xff\xfe\ngamma";
        let got = entries(input, b'\n', None, None, '\t');
        let shown: Vec<(usize, String, Option<String>)> = got
            .iter()
            .map(|a| (a.index, a.title_string(), a.icon_name.clone()))
            .collect();
        assert_eq!(
            shown,
            vec![
                (0, "alpha".to_owned(), None),
                (1, "beta".to_owned(), Some("firefox".to_owned())),
                (3, "gamma".to_owned(), None),
            ]
        );
        let nul = entries(b"a\0b\0", 0, None, None, '\t');
        assert_eq!(nul.len(), 2);
        assert!(entries(b"", b'\n', None, None, '\t').is_empty());
        let columns = entries(b"id1\tShown one\n", b'\n', Some("{2}"), Some("{1}"), '\t');
        let first = columns.first().cloned().unwrap_or_default();
        assert_eq!(first.title_string(), "Shown one");
        assert_eq!(
            output(Some(&first), "", false, None, '\t'),
            "id1\tShown one"
        );
        assert_eq!(output(Some(&first), "", false, Some("{1}"), '\t'), "id1");
        assert_eq!(output(Some(&first), "", true, None, '\t'), "0");
        assert_eq!(output(None, "typed", false, None, '\t'), "typed");
    }
}
