//! waybar's `style.css`, as GTK3 reads it: a stylesheet, the cascade, and
//! the computed style of each node.
//!
//! waybar loads the file with `gtk_css_provider_load_from_path` at
//! `GTK_STYLE_PROVIDER_PRIORITY_USER`, after rewriting every `#rrggbbaa` to
//! `rgba()` (GTK3 has no eight-digit hex; `transform_8bit_to_hex`). A parse
//! error is a GTK warning, `Theme parsing error: style.css:12:5: …`, and
//! only the declaration or rule at fault is dropped. [`Stylesheet::parse`]
//! does the same and hands the warnings to [`Diagnostics`].
//!
//! GTK's theme (Adwaita, Yaru) is a second provider under the user's, and
//! it is not here: every property a node's user rules do not set takes the
//! CSS initial value or its parent's, as with `GTK_THEME` pointing at an
//! empty theme. The user's file sets everything its bar shows -- colours,
//! fonts, padding, and zeroes the tooltip's theme padding explicitly -- so
//! the difference is limited to what the theme alone would have drawn.

pub mod property;
pub mod selector;
pub mod style;
pub mod token;
pub mod value;

use std::path::{Path, PathBuf};

use crate::diag::Diagnostics;
use property::Declared;
use selector::{Node, Selector};
use token::{Place, Token, tokenize};
use value::Color;

/// One declaration in a rule, with its longhands.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    /// The property name as written.
    pub name: String,
    /// What it sets.
    pub set: Vec<Declared>,
    /// Where it is.
    pub place: Place,
}

/// A rule: its selectors and declarations.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    /// The selector list.
    pub selectors: Vec<Selector>,
    /// The declarations, in order.
    pub declarations: Vec<Declaration>,
}

/// A stylesheet, parsed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stylesheet {
    /// The rules, in source order (imports in place).
    pub rules: Vec<Rule>,
    /// `@define-color`s; a later one of the same name wins.
    pub colors: Vec<(String, Color)>,
    /// The directory `url()`s resolve against: the file's own.
    pub base: PathBuf,
}

/// waybar's `transform_8bit_to_hex`: `#rrggbbaa` becomes
/// `rgba(r,g,b,a.aa)`, wherever it is in the text.
#[must_use]
pub fn rewrite_hex_alpha(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    let mut copied = 0usize;
    while at < bytes.len() {
        if bytes.get(at) == Some(&b'#')
            && let Some(digits) = text.get(at + 1..at + 9)
            && digits.bytes().all(|b| b.is_ascii_hexdigit())
        {
            let channel = |i: usize| {
                digits
                    .get(i * 2..i * 2 + 2)
                    .and_then(|d| u8::from_str_radix(d, 16).ok())
                    .unwrap_or(0)
            };
            out.push_str(text.get(copied..at).unwrap_or_default());
            out.push_str(&format!(
                "rgba({},{},{},{:.2})",
                channel(0),
                channel(1),
                channel(2),
                f32::from(channel(3)) / 255.0
            ));
            at += 9;
            copied = at;
        } else {
            at += 1;
        }
    }
    out.push_str(text.get(copied..).unwrap_or_default());
    out
}

impl Stylesheet {
    /// Parse `text`, the file at `path`, with its `@import`s read through
    /// `read`.
    pub fn parse(
        text: &str,
        path: &Path,
        read: &dyn Fn(&Path) -> Option<String>,
        diag: &mut Diagnostics,
    ) -> Self {
        let mut sheet = Stylesheet {
            base: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            ..Stylesheet::default()
        };
        sheet.add(text, path, read, diag, 0);
        sheet
    }

    fn add(
        &mut self,
        text: &str,
        path: &Path,
        read: &dyn Fn(&Path) -> Option<String>,
        diag: &mut Diagnostics,
        depth: usize,
    ) {
        let rewritten = rewrite_hex_alpha(text);
        let tokens = tokenize(&rewritten);
        let file = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        // Warnings are held and said at the end, in order, so an import's
        // own warnings (said as it is read) come first, as GTK's would.
        let mut pending: Vec<(Place, String)> = Vec::new();
        let mut at = 0usize;
        while let Some((token, place)) = tokens.get(at) {
            match token {
                Token::Space => at += 1,
                Token::AtKeyword(keyword) => {
                    let end = statement_end(&tokens, at);
                    let body: Vec<Token> = tokens
                        .get(at + 1..end)
                        .unwrap_or_default()
                        .iter()
                        .map(|(t, _)| t.clone())
                        .filter(|t| *t != Token::Semicolon)
                        .collect();
                    self.at_rule(
                        keyword,
                        &body,
                        *place,
                        path,
                        read,
                        diag,
                        depth,
                        &mut pending,
                    );
                    at = end + 1;
                }
                _ => {
                    at = self.rule(&tokens, at, &mut |place: Place, message: &str| {
                        pending.push((place, message.to_owned()));
                    });
                }
            }
        }
        for (place, message) in pending {
            diag.warn(format!(
                "Theme parsing error: {file}:{}:{}: {message}",
                place.line, place.column
            ));
        }
    }

    /// `@define-color` or `@import`; any other at-rule is an error.
    #[expect(clippy::too_many_arguments, reason = "the parser's state, passed down")]
    fn at_rule(
        &mut self,
        keyword: &str,
        body: &[Token],
        place: Place,
        path: &Path,
        read: &dyn Fn(&Path) -> Option<String>,
        diag: &mut Diagnostics,
        depth: usize,
        pending: &mut Vec<(Place, String)>,
    ) {
        let mut cursor = value::Cursor::new(body);
        match keyword.to_ascii_lowercase().as_str() {
            "define-color" => {
                let Some(Token::Ident(name)) = cursor.next_token() else {
                    pending.push((place, "Expected a valid color name".into()));
                    return;
                };
                match value::color(&mut cursor) {
                    Ok(color) if cursor.done() => self.colors.push((name.clone(), color)),
                    Ok(_) => pending.push((place, "Junk at end of value".into())),
                    Err(message) => pending.push((place, message)),
                }
            }
            "import" => {
                let Some(Token::Url(name) | Token::String(name)) = cursor.next_token() else {
                    pending.push((place, "Expected a URL for @import".into()));
                    return;
                };
                if depth >= 32 {
                    pending.push((place, "Loop detected while importing".into()));
                    return;
                }
                let target = path.parent().unwrap_or(Path::new("")).join(name);
                match read(&target) {
                    Some(text) => self.add(&text, &target, read, diag, depth + 1),
                    None => {
                        pending.push((place, format!("Failed to import: {}", target.display())))
                    }
                }
            }
            other => pending.push((place, format!("unknown @ rule '@{other}'"))),
        }
    }

    /// Parse one rule starting at `at`; return where the next begins.
    fn rule(
        &mut self,
        tokens: &[(Token, Place)],
        at: usize,
        warn: &mut dyn FnMut(Place, &str),
    ) -> usize {
        let start_place = tokens.get(at).map(|(_, p)| *p).unwrap_or_default();
        let Some(open) = tokens
            .get(at..)
            .unwrap_or_default()
            .iter()
            .position(|(t, _)| *t == Token::OpenBrace)
            .map(|offset| at + offset)
        else {
            warn(start_place, "Expected '{' after selectors");
            return tokens.len();
        };
        let close = block_end(tokens, open);
        let prelude: Vec<Token> = tokens
            .get(at..open)
            .unwrap_or_default()
            .iter()
            .map(|(t, _)| t.clone())
            .collect();
        let selectors = match selector::parse_list(&prelude) {
            Ok(selectors) => selectors,
            Err(message) => {
                warn(start_place, &message);
                return close + 1;
            }
        };
        let body = tokens.get(open + 1..close).unwrap_or_default();
        let mut declarations = Vec::new();
        for declaration in split_declarations(body) {
            let Some(first) = declaration.iter().position(|(t, _)| *t != Token::Space) else {
                continue;
            };
            let declaration = declaration.get(first..).unwrap_or_default();
            let Some((Token::Ident(name), place)) = declaration.first() else {
                let place = declaration.first().map(|(_, p)| *p).unwrap_or_default();
                warn(place, "Expected a valid property name");
                continue;
            };
            let after: Vec<&(Token, Place)> =
                declaration.get(1..).unwrap_or_default().iter().collect();
            let colon = after.iter().position(|(t, _)| *t != Token::Space);
            let Some(colon) =
                colon.filter(|&c| after.get(c).is_some_and(|(t, _)| *t == Token::Colon))
            else {
                warn(*place, &format!("Expected ':' after '{name}'"));
                continue;
            };
            let value: Vec<Token> = after
                .get(colon + 1..)
                .unwrap_or_default()
                .iter()
                .map(|(t, _)| t.clone())
                .collect();
            match property::parse(name, &value) {
                Ok(set) => declarations.push(Declaration {
                    name: name.clone(),
                    set,
                    place: *place,
                }),
                Err(message) => warn(*place, &message),
            }
        }
        self.rules.push(Rule {
            selectors,
            declarations,
        });
        close + 1
    }

    /// The `@define-color` named `name`: the last one.
    #[must_use]
    pub fn color(&self, name: &str) -> Option<&Color> {
        self.colors
            .iter()
            .rev()
            .find(|(defined, _)| defined == name)
            .map(|(_, color)| color)
    }

    /// The declarations that apply to `node`, in cascade order: lower
    /// specificity first, then source order.
    pub fn matching<N: Node>(&self, node: &N) -> Vec<&Declaration> {
        let mut matched: Vec<((u32, u32, u32), usize, &Rule)> = Vec::new();
        for (order, rule) in self.rules.iter().enumerate() {
            // A list is its selectors written as separate rules: the most
            // specific one that matches counts.
            let best = rule
                .selectors
                .iter()
                .filter(|selector| selector.matches(node))
                .map(Selector::specificity)
                .max();
            if let Some(specificity) = best {
                matched.push((specificity, order, rule));
            }
        }
        matched.sort_by_key(|entry| (entry.0, entry.1));
        matched
            .into_iter()
            .flat_map(|(_, _, rule)| rule.declarations.iter())
            .collect()
    }
}

/// The index of the `;` ending an at-rule statement (or of the `}`
/// closing its block).
fn statement_end(tokens: &[(Token, Place)], at: usize) -> usize {
    let mut index = at;
    while let Some((token, _)) = tokens.get(index) {
        match token {
            Token::Semicolon => return index,
            Token::OpenBrace => return block_end(tokens, index),
            _ => index += 1,
        }
    }
    tokens.len()
}

/// The index of the `}` matching the `{` at `open`.
fn block_end(tokens: &[(Token, Place)], open: usize) -> usize {
    let mut depth = 0usize;
    let mut index = open;
    while let Some((token, _)) = tokens.get(index) {
        match token {
            Token::OpenBrace => depth += 1,
            Token::CloseBrace => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index;
                }
            }
            _ => {}
        }
        index += 1;
    }
    tokens.len()
}

/// A block's declarations, split at top-level `;`.
fn split_declarations(body: &[(Token, Place)]) -> Vec<&[(Token, Place)]> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (at, (token, _)) in body.iter().enumerate() {
        match token {
            Token::Function(_) | Token::OpenParen | Token::OpenBrace => depth += 1,
            Token::CloseParen | Token::CloseBrace => depth = depth.saturating_sub(1),
            Token::Semicolon if depth == 0 => {
                out.push(body.get(start..at).unwrap_or_default());
                start = at + 1;
            }
            _ => {}
        }
    }
    out.push(body.get(start..).unwrap_or_default());
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Stylesheet, rewrite_hex_alpha};
    use crate::diag::Diagnostics;

    fn sheet(text: &str) -> (Stylesheet, Diagnostics) {
        let mut diag = Diagnostics::default();
        let sheet = Stylesheet::parse(text, Path::new("/s/style.css"), &|_| None, &mut diag);
        (sheet, diag)
    }

    #[test]
    fn eight_digit_hex_becomes_rgba_as_waybar_rewrites_it() {
        assert_eq!(
            rewrite_hex_alpha("a { color: #ff000080; }"),
            "a { color: rgba(255,0,0,0.50); }"
        );
        assert_eq!(rewrite_hex_alpha("#fff #123456"), "#fff #123456");
    }

    #[test]
    fn rules_colors_and_errors() {
        let (sheet, diag) = sheet(
            "@define-color ink #dbe8f7;\n* { font-size: 15px; bogus: 1; }\nwindow#waybar { color: @ink; }\nlabel:nope { color: red; }\n#a { color: 12px; margin: 1px }",
        );
        assert_eq!(sheet.rules.len(), 3, "the bad selector drops its rule");
        assert_eq!(sheet.colors.len(), 1);
        assert_eq!(sheet.rules.first().map(|r| r.declarations.len()), Some(1));
        assert_eq!(sheet.rules.get(2).map(|r| r.declarations.len()), Some(1));
        assert!(
            diag.has("Theme parsing error: style.css:2:22: 'bogus' is not a valid property name"),
            "{:?}",
            diag.lines
        );
        assert!(diag.has("style.css:4:1: Unknown name of pseudo-class: 'nope'"));
    }

    #[test]
    fn imports_are_read_in_place() {
        let mut diag = Diagnostics::default();
        let read = |path: &Path| -> Option<String> {
            (path == Path::new("/s/colors.css")).then(|| "@define-color x #000;".to_owned())
        };
        let sheet = Stylesheet::parse(
            "@import url(\"colors.css\");\na { color: @x; }",
            Path::new("/s/style.css"),
            &read,
            &mut diag,
        );
        assert_eq!(sheet.colors.len(), 1);
        assert_eq!(sheet.rules.len(), 1);
    }
}
