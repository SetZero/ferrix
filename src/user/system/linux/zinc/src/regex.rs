//! A small backtracking regular-expression matcher for `[[ x =~ re ]]`:
//! POSIX extended syntax with `^ $ . [] * + ? {m,n} | ()` and the escapes
//! `\d \w \s`. Unanchored search, as `=~` is.

#[derive(Debug, Clone)]
enum Node {
    Char(u8),
    Any,
    Class(bool, Vec<(u8, u8)>),
    Start,
    End,
    Group(Vec<Vec<Node>>),
    Repeat(Box<Node>, usize, usize),
}

fn parse_alt(p: &[u8], i: &mut usize) -> Result<Vec<Vec<Node>>, String> {
    let mut alts = vec![Vec::new()];
    while let Some(&c) = p.get(*i) {
        *i += 1;
        let node = match c {
            b'|' => {
                alts.push(Vec::new());
                continue;
            }
            b')' => {
                *i -= 1;
                return Ok(alts);
            }
            b'(' => {
                let g = parse_alt(p, i)?;
                if p.get(*i) != Some(&b')') {
                    return Err("unmatched ( in regex".to_owned());
                }
                *i += 1;
                Node::Group(g)
            }
            b'.' => Node::Any,
            b'^' => Node::Start,
            b'$' => Node::End,
            b'[' => parse_class(p, i)?,
            b'\\' => {
                let e = p.get(*i).copied().unwrap_or(b'\\');
                *i += 1;
                match e {
                    b'd' => Node::Class(false, vec![(b'0', b'9')]),
                    b'w' => Node::Class(
                        false,
                        vec![(b'a', b'z'), (b'A', b'Z'), (b'0', b'9'), (b'_', b'_')],
                    ),
                    b's' => Node::Class(false, vec![(b' ', b' '), (b'\t', b'\r')]),
                    other => Node::Char(other),
                }
            }
            b'*' | b'+' | b'?' | b'{' => {
                let Some(last) = alts.last_mut().and_then(Vec::pop) else {
                    return Err("nothing to repeat in regex".to_owned());
                };
                let (lo, hi) = match c {
                    b'*' => (0, usize::MAX),
                    b'+' => (1, usize::MAX),
                    b'?' => (0, 1),
                    _ => {
                        let end = p
                            .get(*i..)
                            .and_then(|r| r.iter().position(|&x| x == b'}'))
                            .map(|e| *i + e);
                        let Some(end) = end else {
                            return Err("unmatched { in regex".to_owned());
                        };
                        let body =
                            String::from_utf8_lossy(p.get(*i..end).unwrap_or(&[])).into_owned();
                        *i = end + 1;
                        match body.split_once(',') {
                            Some((a, "")) => (a.parse().unwrap_or(0), usize::MAX),
                            Some((a, b)) => {
                                (a.parse().unwrap_or(0), b.parse().unwrap_or(usize::MAX))
                            }
                            None => {
                                let n = body.parse().unwrap_or(0);
                                (n, n)
                            }
                        }
                    }
                };
                Node::Repeat(Box::new(last), lo, hi)
            }
            other => Node::Char(other),
        };
        if let Some(a) = alts.last_mut() {
            a.push(node);
        }
    }
    Ok(alts)
}

fn parse_class(p: &[u8], i: &mut usize) -> Result<Node, String> {
    let neg = p.get(*i) == Some(&b'^');
    if neg {
        *i += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    loop {
        let Some(&c) = p.get(*i) else {
            return Err("unmatched [ in regex".to_owned());
        };
        *i += 1;
        if c == b']' && !first {
            break;
        }
        first = false;
        if c == b'[' && p.get(*i) == Some(&b':') {
            let rest = p.get(*i + 1..).unwrap_or(&[]);
            if let Some(e) = rest.windows(2).position(|w| w == b":]") {
                let name = rest.get(..e).unwrap_or(&[]);
                *i += 1 + e + 2;
                let ranges: &[(u8, u8)] = match name {
                    b"digit" => &[(b'0', b'9')],
                    b"alpha" => &[(b'a', b'z'), (b'A', b'Z')],
                    b"alnum" => &[(b'a', b'z'), (b'A', b'Z'), (b'0', b'9')],
                    b"space" => &[(b' ', b' '), (b'\t', b'\r')],
                    b"upper" => &[(b'A', b'Z')],
                    b"lower" => &[(b'a', b'z')],
                    b"xdigit" => &[(b'0', b'9'), (b'a', b'f'), (b'A', b'F')],
                    b"punct" => &[(b'!', b'/'), (b':', b'@'), (b'[', b'`'), (b'{', b'~')],
                    _ => &[],
                };
                items.extend_from_slice(ranges);
                continue;
            }
        }
        if p.get(*i) == Some(&b'-') && p.get(*i + 1).is_some_and(|&n| n != b']') {
            let hi = p.get(*i + 1).copied().unwrap_or(c);
            *i += 2;
            items.push((c, hi));
        } else {
            items.push((c, c));
        }
    }
    Ok(Node::Class(neg, items))
}

fn step(node: &Node, s: &[u8], pos: usize) -> Vec<usize> {
    match node {
        Node::Char(c) => {
            if s.get(pos) == Some(c) {
                vec![pos + 1]
            } else {
                vec![]
            }
        }
        Node::Any => {
            if pos < s.len() && s.get(pos) != Some(&b'\n') {
                vec![pos + 1]
            } else {
                vec![]
            }
        }
        Node::Class(neg, items) => match s.get(pos) {
            Some(&c) if items.iter().any(|&(lo, hi)| lo <= c && c <= hi) != *neg => vec![pos + 1],
            _ => vec![],
        },
        Node::Start => {
            if pos == 0 {
                vec![pos]
            } else {
                vec![]
            }
        }
        Node::End => {
            if pos == s.len() {
                vec![pos]
            } else {
                vec![]
            }
        }
        Node::Group(alts) => alts.iter().flat_map(|a| run(a, s, pos)).collect(),
        Node::Repeat(inner, lo, hi) => {
            let mut out = Vec::new();
            let mut frontier = vec![pos];
            let mut count = 0;
            if *lo == 0 {
                out.push(pos);
            }
            while !frontier.is_empty() && count < *hi {
                count += 1;
                let mut next = Vec::new();
                for p in frontier {
                    for q in step(inner, s, p) {
                        if q != p && !next.contains(&q) {
                            next.push(q);
                        }
                    }
                }
                if count >= *lo {
                    out.extend(next.iter().copied());
                }
                frontier = next;
            }
            out.sort_unstable();
            out.dedup();
            out.reverse();
            out
        }
    }
}

fn run(nodes: &[Node], s: &[u8], pos: usize) -> Vec<usize> {
    let Some((first, rest)) = nodes.split_first() else {
        return vec![pos];
    };
    let mut out = Vec::new();
    for p in step(first, s, pos) {
        for q in run(rest, s, p) {
            if !out.contains(&q) {
                out.push(q);
            }
        }
    }
    out
}

/// True if `re` matches somewhere in `s`.
pub(crate) fn is_match(re: &[u8], s: &[u8]) -> Result<bool, String> {
    let mut i = 0;
    let alts = parse_alt(re, &mut i)?;
    if i < re.len() {
        return Err("unmatched ) in regex".to_owned());
    }
    let node = Node::Group(alts);
    Ok((0..=s.len()).any(|start| !step(&node, s, start).is_empty()))
}

#[cfg(test)]
mod tests {
    use super::is_match;

    #[test]
    fn regexes_search_like_posix_ere() {
        assert_eq!(is_match(b"^[0-9]+$", b"123"), Ok(true));
        assert_eq!(is_match(b"^[0-9]+$", b"12a"), Ok(false));
        assert_eq!(is_match(b"b(a|o)t", b"xboty"), Ok(true));
        assert_eq!(is_match(b"^a{2,3}$", b"aaa"), Ok(true));
        assert_eq!(is_match(b"[[:alpha:]]", b"12z"), Ok(true));
    }
}
