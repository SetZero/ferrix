//! CSS tokens, as CSS Syntax Level 3 cuts a stylesheet up -- which is what
//! GTK3's `gtkcsstokenizer.c` implements, down to `url(` without quotes
//! being one token and a comment being white space.

/// Where a token starts: 1-based line and column, for GTK's
/// `file:line:column: message` parse errors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Place {
    /// 1-based line.
    pub line: usize,
    /// 1-based column, in characters.
    pub column: usize,
}

/// One token.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    /// An identifier: `solid`, `window`, `-gtk-icon-source`.
    Ident(String),
    /// A function's name and its `(`: `alpha(`, `linear-gradient(`.
    Function(String),
    /// `@define-color`, as `define-color`.
    AtKeyword(String),
    /// `#name`: an id selector or a hex colour; `true` when it could be an
    /// identifier (an id), `false` for `#1c3049`-style digits first.
    Hash(String, bool),
    /// A quoted string, escapes decoded.
    String(String),
    /// `url(unquoted)`.
    Url(String),
    /// A number with no unit.
    Number(f64),
    /// `50%`.
    Percentage(f64),
    /// `4px`, `1.5em`: the number and the unit.
    Dimension(f64, String),
    /// White space or a comment.
    Space,
    /// `:`.
    Colon,
    /// `;`.
    Semicolon,
    /// `,`.
    Comma,
    /// `{`.
    OpenBrace,
    /// `}`.
    CloseBrace,
    /// `(`.
    OpenParen,
    /// `)`.
    CloseParen,
    /// `[`.
    OpenBracket,
    /// `]`.
    CloseBracket,
    /// Any other character: `>`, `+`, `~`, `*`, `.`, `!`, `/`.
    Delim(char),
}

/// Cut `text` into tokens, each with where it starts.
#[must_use]
pub fn tokenize(text: &str) -> Vec<(Token, Place)> {
    let chars: Vec<char> = text.chars().collect();
    let mut lexer = Lexer {
        chars: &chars,
        at: 0,
        line: 1,
        column: 1,
    };
    let mut out = Vec::new();
    while lexer.at < chars.len() {
        let place = Place {
            line: lexer.line,
            column: lexer.column,
        };
        let token = lexer.token();
        // Runs of space and comments are one token.
        if token == Token::Space && out.last().is_some_and(|(last, _)| *last == Token::Space) {
            continue;
        }
        out.push((token, place));
    }
    out
}

struct Lexer<'a> {
    chars: &'a [char],
    at: usize,
    line: usize,
    column: usize,
}

impl Lexer<'_> {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek(0)?;
        self.at += 1;
        if c == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(c)
    }

    fn starts_ident(&self, ahead: usize) -> bool {
        match self.peek(ahead) {
            Some('-') => match self.peek(ahead + 1) {
                Some(c) if is_name_start(c) || c == '-' => true,
                Some('\\') => self.peek(ahead + 2).is_some_and(|c| c != '\n'),
                _ => false,
            },
            Some('\\') => self.peek(ahead + 1).is_some_and(|c| c != '\n'),
            Some(c) => is_name_start(c),
            None => false,
        }
    }

    fn starts_number(&self) -> bool {
        match self.peek(0) {
            Some('0'..='9') => true,
            Some('.') => self.peek(1).is_some_and(|c| c.is_ascii_digit()),
            Some('+' | '-') => match self.peek(1) {
                Some('0'..='9') => true,
                Some('.') => self.peek(2).is_some_and(|c| c.is_ascii_digit()),
                _ => false,
            },
            _ => false,
        }
    }

    fn token(&mut self) -> Token {
        let Some(c) = self.peek(0) else {
            return Token::Space;
        };
        if c == '/' && self.peek(1) == Some('*') {
            let _ = self.bump();
            let _ = self.bump();
            while let Some(c) = self.bump() {
                if c == '*' && self.peek(0) == Some('/') {
                    let _ = self.bump();
                    break;
                }
            }
            return Token::Space;
        }
        if c.is_whitespace() {
            while self.peek(0).is_some_and(char::is_whitespace) {
                let _ = self.bump();
            }
            return Token::Space;
        }
        if c == '"' || c == '\'' {
            let _ = self.bump();
            return Token::String(self.string(c));
        }
        if self.starts_number() {
            return self.numeric();
        }
        if self.starts_ident(0) {
            let name = self.name();
            if self.peek(0) == Some('(') {
                let _ = self.bump();
                if name.eq_ignore_ascii_case("url") {
                    return self.url();
                }
                return Token::Function(name);
            }
            return Token::Ident(name);
        }
        let _ = self.bump();
        match c {
            '#' => {
                if self.peek(0).is_some_and(is_name) || self.peek(0) == Some('\\') {
                    let identifier = self.starts_ident(0);
                    Token::Hash(self.name(), identifier)
                } else {
                    Token::Delim('#')
                }
            }
            '@' => {
                if self.starts_ident(0) {
                    Token::AtKeyword(self.name())
                } else {
                    Token::Delim('@')
                }
            }
            ':' => Token::Colon,
            ';' => Token::Semicolon,
            ',' => Token::Comma,
            '{' => Token::OpenBrace,
            '}' => Token::CloseBrace,
            '(' => Token::OpenParen,
            ')' => Token::CloseParen,
            '[' => Token::OpenBracket,
            ']' => Token::CloseBracket,
            other => Token::Delim(other),
        }
    }

    fn escape(&mut self) -> char {
        let mut hex = String::new();
        while hex.len() < 6 && self.peek(0).is_some_and(|c| c.is_ascii_hexdigit()) {
            if let Some(c) = self.bump() {
                hex.push(c);
            }
        }
        if hex.is_empty() {
            return self.bump().unwrap_or('\u{FFFD}');
        }
        if self.peek(0).is_some_and(char::is_whitespace) {
            let _ = self.bump();
        }
        u32::from_str_radix(&hex, 16)
            .ok()
            .and_then(char::from_u32)
            .filter(|&c| c != '\0')
            .unwrap_or('\u{FFFD}')
    }

    fn name(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                Some('\\') if self.peek(1).is_some_and(|c| c != '\n') => {
                    let _ = self.bump();
                    out.push(self.escape());
                }
                Some(c) if is_name(c) => {
                    out.push(c);
                    let _ = self.bump();
                }
                _ => return out,
            }
        }
    }

    fn string(&mut self, quote: char) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek(0) {
            match c {
                c if c == quote => {
                    let _ = self.bump();
                    break;
                }
                // An unescaped newline ends a bad string; GTK reports it and
                // so does the parser, which gets what was read so far.
                '\n' => break,
                '\\' => {
                    let _ = self.bump();
                    match self.peek(0) {
                        Some('\n') => {
                            let _ = self.bump();
                        }
                        Some(_) => out.push(self.escape()),
                        None => {}
                    }
                }
                c => {
                    out.push(c);
                    let _ = self.bump();
                }
            }
        }
        out
    }

    fn url(&mut self) -> Token {
        while self.peek(0).is_some_and(char::is_whitespace) {
            let _ = self.bump();
        }
        // `url("x")` is a function whose argument is a string; `url(x)` is
        // one token.
        if let Some(quote @ ('"' | '\'')) = self.peek(0) {
            let _ = self.bump();
            let text = self.string(quote);
            while self.peek(0).is_some_and(char::is_whitespace) {
                let _ = self.bump();
            }
            if self.peek(0) == Some(')') {
                let _ = self.bump();
            }
            return Token::Url(text);
        }
        let mut out = String::new();
        while let Some(c) = self.bump() {
            match c {
                ')' => break,
                '\\' => out.push(self.escape()),
                c if c.is_whitespace() => {}
                c => out.push(c),
            }
        }
        Token::Url(out)
    }

    fn numeric(&mut self) -> Token {
        let mut text = String::new();
        if let Some(sign @ ('+' | '-')) = self.peek(0) {
            text.push(sign);
            let _ = self.bump();
        }
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
            text.extend(self.bump());
        }
        if self.peek(0) == Some('.') && self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            text.extend(self.bump());
            while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                text.extend(self.bump());
            }
        }
        if matches!(self.peek(0), Some('e' | 'E'))
            && (self.peek(1).is_some_and(|c| c.is_ascii_digit())
                || (matches!(self.peek(1), Some('+' | '-'))
                    && self.peek(2).is_some_and(|c| c.is_ascii_digit())))
        {
            text.extend(self.bump());
            text.extend(self.bump());
            while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                text.extend(self.bump());
            }
        }
        let value: f64 = text.parse().unwrap_or(0.0);
        if self.starts_ident(0) {
            return Token::Dimension(value, self.name());
        }
        if self.peek(0) == Some('%') {
            let _ = self.bump();
            return Token::Percentage(value);
        }
        Token::Number(value)
    }
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn is_name(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-'
}

#[cfg(test)]
mod tests {
    use super::{Token, tokenize};

    fn kinds(text: &str) -> Vec<Token> {
        tokenize(text).into_iter().map(|(token, _)| token).collect()
    }

    #[test]
    fn a_rule_cuts_into_its_tokens() {
        assert_eq!(
            kinds("window#waybar .ws:hover{margin:4px -2px}"),
            vec![
                Token::Ident("window".into()),
                Token::Hash("waybar".into(), true),
                Token::Space,
                Token::Delim('.'),
                Token::Ident("ws".into()),
                Token::Colon,
                Token::Ident("hover".into()),
                Token::OpenBrace,
                Token::Ident("margin".into()),
                Token::Colon,
                Token::Dimension(4.0, "px".into()),
                Token::Space,
                Token::Dimension(-2.0, "px".into()),
                Token::CloseBrace,
            ]
        );
    }

    #[test]
    fn colours_urls_and_comments() {
        assert_eq!(
            kinds("@define-color x #1c3049; /* c */ url(\"icons/a.svg\") url(b.svg) 50% calc("),
            vec![
                Token::AtKeyword("define-color".into()),
                Token::Space,
                Token::Ident("x".into()),
                Token::Space,
                Token::Hash("1c3049".into(), false),
                Token::Semicolon,
                Token::Space,
                Token::Url("icons/a.svg".into()),
                Token::Space,
                Token::Url("b.svg".into()),
                Token::Space,
                Token::Percentage(50.0),
                Token::Space,
                Token::Function("calc".into()),
            ]
        );
    }

    #[test]
    fn places_count_lines_and_columns() {
        let tokens = tokenize("a\n  b");
        let last = tokens.last().map(|(_, place)| (place.line, place.column));
        assert_eq!(last, Some((2, 3)));
    }
}
