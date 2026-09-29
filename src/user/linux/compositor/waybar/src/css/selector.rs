//! Selectors, as GTK3 matches them against its CSS node tree.
//!
//! A GTK3 node has an element name (`window`, `box`, `label`, `tooltip`,
//! `widget` for an event box), at most one name set with
//! `gtk_widget_set_name` -- which CSS writes as `#name` -- any number of
//! style classes, and state flags. So `window#waybar .ws:hover` is the
//! window named `waybar`, then any descendant with the class `ws` in the
//! prelight state.
//!
//! Specificity is CSS's: names, then classes and pseudo-classes, then
//! element names, with `*` counting nothing. Equal specificity falls to
//! source order, which is what the user's style.css leans on for its
//! `window#waybar .ws` rules.

use super::token::Token;

/// What a node looks like to a selector.
pub trait Node: Sized {
    /// The element name.
    fn element(&self) -> &str;
    /// The `#name`, if it has one.
    fn name(&self) -> Option<&str>;
    /// Whether it has a style class.
    fn has_class(&self, class: &str) -> bool;
    /// Whether it is in a state (`hover`, `active`, `focus`, `checked`,
    /// `disabled`, `selected`, `backdrop`, …).
    fn in_state(&self, state: &str) -> bool;
    /// Its parent, if it has one.
    fn parent(&self) -> Option<Self>;
    /// Its place among its visible siblings, from 0, and how many there are.
    fn position(&self) -> (usize, usize);
    /// The visible sibling just before it.
    fn previous(&self) -> Option<Self>;
}

/// A pseudo-class.
#[derive(Clone, Debug, PartialEq)]
pub enum Pseudo {
    /// A state: `:hover` is `hover`.
    State(String),
    /// `:first-child`.
    FirstChild,
    /// `:last-child`.
    LastChild,
    /// `:only-child`.
    OnlyChild,
    /// `:nth-child(an+b)`.
    NthChild(i32, i32),
    /// `:nth-last-child(an+b)`.
    NthLastChild(i32, i32),
    /// `:not(compound)`.
    Not(Box<Compound>),
}

/// A compound selector: one element's tests.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Compound {
    /// The element name, `None` for `*` or none written.
    pub element: Option<String>,
    /// `#name`.
    pub name: Option<String>,
    /// `.class`es.
    pub classes: Vec<String>,
    /// `:pseudo`s.
    pub pseudos: Vec<Pseudo>,
}

/// How a compound relates to the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    /// ` `: an ancestor.
    Descendant,
    /// `>`: the parent.
    Child,
    /// `+`: the sibling just before.
    Adjacent,
    /// `~`: any sibling before.
    Sibling,
}

/// A complex selector: compounds joined by combinators, left to right.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    /// The first compound.
    pub first: Compound,
    /// Each later compound and how it relates to the one before.
    pub rest: Vec<(Combinator, Compound)>,
}

/// The states GTK3's CSS parser knows as pseudo-classes.
const STATES: [&str; 13] = [
    "active",
    "hover",
    "selected",
    "disabled",
    "indeterminate",
    "focus",
    "backdrop",
    "dir(ltr)",
    "dir(rtl)",
    "link",
    "visited",
    "checked",
    "drop(active)",
];

impl Compound {
    fn specificity(&self) -> (u32, u32, u32) {
        let mut ids = u32::from(self.name.is_some());
        let mut classes = u32::try_from(self.classes.len()).unwrap_or(u32::MAX);
        let mut elements = u32::from(self.element.is_some());
        for pseudo in &self.pseudos {
            if let Pseudo::Not(inner) = pseudo {
                let (a, b, c) = inner.specificity();
                ids += a;
                classes += b;
                elements += c;
            } else {
                classes += 1;
            }
        }
        (ids, classes, elements)
    }

    /// Whether `node` passes every test.
    pub fn matches<N: Node>(&self, node: &N) -> bool {
        if let Some(element) = &self.element
            && !node.element().eq_ignore_ascii_case(element)
        {
            return false;
        }
        if let Some(name) = &self.name
            && node.name() != Some(name.as_str())
        {
            return false;
        }
        if !self.classes.iter().all(|class| node.has_class(class)) {
            return false;
        }
        self.pseudos.iter().all(|pseudo| {
            let (index, count) = node.position();
            match pseudo {
                Pseudo::State(state) => node.in_state(state),
                Pseudo::FirstChild => index == 0,
                Pseudo::LastChild => index + 1 == count,
                Pseudo::OnlyChild => count == 1,
                Pseudo::NthChild(a, b) => nth(*a, *b, index + 1),
                Pseudo::NthLastChild(a, b) => nth(*a, *b, count - index),
                Pseudo::Not(inner) => !inner.matches(node),
            }
        })
    }
}

/// Whether the 1-based `place` is `a·n + b` for some `n >= 0`.
fn nth(a: i32, b: i32, place: usize) -> bool {
    let Ok(place) = i32::try_from(place) else {
        return false;
    };
    if a == 0 {
        return place == b;
    }
    let n = place - b;
    n % a == 0 && n / a >= 0
}

impl Selector {
    /// `(names, classes, elements)`, compared in that order.
    #[must_use]
    pub fn specificity(&self) -> (u32, u32, u32) {
        let mut total = self.first.specificity();
        for (_, compound) in &self.rest {
            let (a, b, c) = compound.specificity();
            total.0 += a;
            total.1 += b;
            total.2 += c;
        }
        total
    }

    /// Whether `node` is what this selects.
    pub fn matches<N: Node>(&self, node: &N) -> bool {
        // Right to left: the last compound is the node itself.
        let mut chain: Vec<(Option<Combinator>, &Compound)> = vec![(None, &self.first)];
        for (combinator, compound) in &self.rest {
            chain.push((Some(*combinator), compound));
        }
        matches_from(&chain, chain.len(), node)
    }
}

/// Whether `node` matches `chain[..len]`, whose last entry is `node`'s own.
fn matches_from<N: Node>(chain: &[(Option<Combinator>, &Compound)], len: usize, node: &N) -> bool {
    let Some(&(combinator, compound)) = len.checked_sub(1).and_then(|last| chain.get(last)) else {
        return true;
    };
    if !compound.matches(node) {
        return false;
    }
    let before = len - 1;
    match combinator {
        None => true,
        Some(Combinator::Child) => node
            .parent()
            .is_some_and(|parent| matches_from(chain, before, &parent)),
        Some(Combinator::Descendant) => {
            let mut ancestor = node.parent();
            while let Some(candidate) = ancestor {
                if matches_from(chain, before, &candidate) {
                    return true;
                }
                ancestor = candidate.parent();
            }
            false
        }
        Some(Combinator::Adjacent) => node
            .previous()
            .is_some_and(|sibling| matches_from(chain, before, &sibling)),
        Some(Combinator::Sibling) => {
            let mut sibling = node.previous();
            while let Some(candidate) = sibling {
                if matches_from(chain, before, &candidate) {
                    return true;
                }
                sibling = candidate.previous();
            }
            false
        }
    }
}

/// Parse a selector list: the tokens before a rule's `{`.
///
/// # Errors
///
/// GTK3's message for the first thing that is not a selector.
pub fn parse_list(tokens: &[Token]) -> Result<Vec<Selector>, String> {
    let mut out = Vec::new();
    for part in split_top(tokens) {
        out.push(parse_one(part)?);
    }
    Ok(out)
}

fn split_top(tokens: &[Token]) -> Vec<&[Token]> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate() {
        match token {
            Token::Function(_) | Token::OpenParen => depth += 1,
            Token::CloseParen => depth = depth.saturating_sub(1),
            Token::Comma if depth == 0 => {
                parts.push(tokens.get(start..at).unwrap_or_default());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(tokens.get(start..).unwrap_or_default());
    parts
}

fn trim(tokens: &[Token]) -> &[Token] {
    let start = tokens
        .iter()
        .position(|t| *t != Token::Space)
        .unwrap_or(tokens.len());
    let end = tokens
        .iter()
        .rposition(|t| *t != Token::Space)
        .map_or(start, |at| at + 1);
    tokens.get(start..end).unwrap_or_default()
}

fn parse_one(tokens: &[Token]) -> Result<Selector, String> {
    let tokens = trim(tokens);
    if tokens.is_empty() {
        return Err("Expected a valid selector".to_owned());
    }
    let mut at = 0usize;
    let first = compound(tokens, &mut at)?;
    let mut rest = Vec::new();
    while at < tokens.len() {
        let mut combinator = Combinator::Descendant;
        while let Some(token) = tokens.get(at) {
            match token {
                Token::Space => {}
                Token::Delim('>') => combinator = Combinator::Child,
                Token::Delim('+') => combinator = Combinator::Adjacent,
                Token::Delim('~') => combinator = Combinator::Sibling,
                _ => break,
            }
            at += 1;
        }
        if at >= tokens.len() {
            return Err("Expected a valid selector".to_owned());
        }
        rest.push((combinator, compound(tokens, &mut at)?));
    }
    Ok(Selector { first, rest })
}

fn compound(tokens: &[Token], at: &mut usize) -> Result<Compound, String> {
    let mut out = Compound::default();
    let start = *at;
    match tokens.get(*at) {
        Some(Token::Ident(name)) => {
            out.element = Some(name.clone());
            *at += 1;
        }
        Some(Token::Delim('*')) => *at += 1,
        _ => {}
    }
    loop {
        match tokens.get(*at) {
            Some(Token::Hash(name, _)) => {
                out.name = Some(name.clone());
                *at += 1;
            }
            Some(Token::Delim('.')) => match tokens.get(*at + 1) {
                Some(Token::Ident(class)) => {
                    out.classes.push(class.clone());
                    *at += 2;
                }
                _ => return Err("Expected a valid name for class".to_owned()),
            },
            Some(Token::Colon) => {
                *at += 1;
                out.pseudos.push(pseudo(tokens, at)?);
            }
            _ => break,
        }
    }
    if *at == start {
        return Err("Expected a valid selector".to_owned());
    }
    Ok(out)
}

fn pseudo(tokens: &[Token], at: &mut usize) -> Result<Pseudo, String> {
    match tokens.get(*at) {
        Some(Token::Ident(name)) => {
            *at += 1;
            let lower = name.to_ascii_lowercase();
            match lower.as_str() {
                "first-child" => Ok(Pseudo::FirstChild),
                "last-child" => Ok(Pseudo::LastChild),
                "only-child" => Ok(Pseudo::OnlyChild),
                // GTK3's old names for states.
                "prelight" => Ok(Pseudo::State("hover".to_owned())),
                "insensitive" => Ok(Pseudo::State("disabled".to_owned())),
                "inconsistent" => Ok(Pseudo::State("indeterminate".to_owned())),
                state if STATES.contains(&state) => Ok(Pseudo::State(state.to_owned())),
                _ => Err(format!("Unknown name of pseudo-class: '{name}'")),
            }
        }
        Some(Token::Function(name)) => {
            let lower = name.to_ascii_lowercase();
            *at += 1;
            let start = *at;
            let mut depth = 0usize;
            while let Some(token) = tokens.get(*at) {
                match token {
                    Token::Function(_) | Token::OpenParen => depth += 1,
                    Token::CloseParen if depth == 0 => break,
                    Token::CloseParen => depth -= 1,
                    _ => {}
                }
                *at += 1;
            }
            let inner = trim(tokens.get(start..*at).unwrap_or_default());
            if tokens.get(*at) != Some(&Token::CloseParen) {
                return Err("Missing closing bracket for pseudo-class".to_owned());
            }
            *at += 1;
            match lower.as_str() {
                "not" => {
                    let mut inner_at = 0usize;
                    let negated = compound(inner, &mut inner_at)?;
                    if inner_at != inner.len() {
                        return Err("Invalid selector in :not()".to_owned());
                    }
                    Ok(Pseudo::Not(Box::new(negated)))
                }
                "nth-child" | "nth-last-child" => {
                    let (a, b) = an_plus_b(inner)?;
                    Ok(if lower == "nth-child" {
                        Pseudo::NthChild(a, b)
                    } else {
                        Pseudo::NthLastChild(a, b)
                    })
                }
                "dir" | "drop" => {
                    let text = match inner {
                        [Token::Ident(word)] => word.to_ascii_lowercase(),
                        _ => String::new(),
                    };
                    let state = format!("{lower}({text})");
                    if STATES.contains(&state.as_str()) {
                        Ok(Pseudo::State(state))
                    } else {
                        Err(format!("Unknown name of pseudo-class: '{lower}'"))
                    }
                }
                _ => Err(format!("Unknown name of pseudo-class: '{name}'")),
            }
        }
        _ => Err("Expected a valid name of a pseudo-class".to_owned()),
    }
}

/// `an+b`, `odd`, `even` or a number.
fn an_plus_b(tokens: &[Token]) -> Result<(i32, i32), String> {
    let text: String = tokens
        .iter()
        .map(|token| match token {
            Token::Ident(word) => word.clone(),
            Token::Number(value) => format!("{value}"),
            Token::Dimension(value, unit) => format!("{value}{unit}"),
            Token::Delim(c) => c.to_string(),
            _ => String::new(),
        })
        .collect::<String>()
        .to_ascii_lowercase();
    let text = text.replace('+', " + ").replace(" + ", "+");
    let bad = || "Expected a valid argument for :nth-child()".to_owned();
    match text.as_str() {
        "odd" => return Ok((2, 1)),
        "even" => return Ok((2, 0)),
        _ => {}
    }
    if let Some((a, b)) = text.split_once('n') {
        let a = match a {
            "" | "+" => 1,
            "-" => -1,
            a => a.parse().map_err(|_| bad())?,
        };
        let b = if b.is_empty() {
            0
        } else {
            b.trim_start_matches('+').parse().map_err(|_| bad())?
        };
        Ok((a, b))
    } else {
        text.parse().map(|b| (0, b)).map_err(|_| bad())
    }
}

#[cfg(test)]
mod tests {
    use super::super::token::{Token, tokenize};
    use super::{Node, Selector, parse_list};

    /// A node in a made-up tree: element, name, classes, states, and the
    /// chain above it.
    #[derive(Debug)]
    struct Fake {
        element: &'static str,
        name: Option<&'static str>,
        classes: Vec<&'static str>,
        states: Vec<&'static str>,
        parent: Option<Box<Fake>>,
    }

    impl Node for &Fake {
        fn element(&self) -> &str {
            self.element
        }
        fn name(&self) -> Option<&str> {
            self.name
        }
        fn has_class(&self, class: &str) -> bool {
            self.classes.contains(&class)
        }
        fn in_state(&self, state: &str) -> bool {
            self.states.contains(&state)
        }
        fn parent(&self) -> Option<Self> {
            self.parent.as_deref()
        }
        fn position(&self) -> (usize, usize) {
            (0, 1)
        }
        fn previous(&self) -> Option<Self> {
            None
        }
    }

    fn one(text: &str) -> Selector {
        let tokens: Vec<Token> = tokenize(text).into_iter().map(|(t, _)| t).collect();
        parse_list(&tokens)
            .ok()
            .and_then(|mut list| list.pop())
            .unwrap_or(Selector {
                first: super::Compound::default(),
                rest: Vec::new(),
            })
    }

    fn chip(states: Vec<&'static str>, classes: Vec<&'static str>) -> Fake {
        let window = Fake {
            element: "window",
            name: Some("waybar"),
            classes: vec!["top"],
            states: vec![],
            parent: None,
        };
        let section = Fake {
            element: "box",
            name: None,
            classes: vec!["modules-right"],
            states: vec![],
            parent: Some(Box::new(window)),
        };
        Fake {
            element: "box",
            name: Some("custom-ws-1"),
            classes,
            states,
            parent: Some(Box::new(section)),
        }
    }

    #[test]
    fn descendant_names_classes_and_states() {
        let hovered = chip(vec!["hover"], vec!["module", "ws", "active"]);
        assert!(one("window#waybar .ws:hover").matches(&&hovered));
        assert!(one("window#waybar .ws.active").matches(&&hovered));
        assert!(one("#custom-ws-1").matches(&&hovered));
        assert!(one("*").matches(&&hovered));
        assert!(!one("window#waybar > .ws").matches(&&hovered));
        assert!(one("window#waybar > box > .ws").matches(&&hovered));
        assert!(!one("label").matches(&&hovered));
        let idle = chip(vec![], vec!["module", "ws"]);
        assert!(!one("window#waybar .ws:hover").matches(&&idle));
        assert!(one(".ws:not(.active)").matches(&&idle));
    }

    #[test]
    fn specificity_ties_between_the_users_ws_rules() {
        // One name, two classes and an element each: the rules tie, and
        // source order decides, as the user's comment says.
        assert_eq!(one("window#waybar .ws.active").specificity(), (1, 2, 1));
        assert_eq!(one("window#waybar .ws:hover").specificity(), (1, 2, 1));
        assert_eq!(one("*").specificity(), (0, 0, 0));
        assert_eq!(one("tooltip label").specificity(), (0, 0, 2));
    }

    #[test]
    fn unknown_pseudo_classes_are_errors() {
        let tokens: Vec<Token> = tokenize("label:frobbed")
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(
            parse_list(&tokens).err(),
            Some("Unknown name of pseudo-class: 'frobbed'".to_owned())
        );
    }
}
