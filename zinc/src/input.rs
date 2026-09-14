//! The character source the lexer reads from.
//!
//! zsh's lexer reads with `hgetc` and pushes back with `hungetc`, often
//! several characters deep, and alias expansion pushes whole strings in front
//! of the remaining input. [`Input`] provides both: a stack of pushed-back
//! bytes over a metafied buffer, and a marker for the aliases currently being
//! expanded so an alias cannot expand into itself.

/// A pushed-in alias text and the alias it came from.
#[derive(Debug)]
struct Pushed {
    text: Vec<u8>,
    pos: usize,
    alias: Option<Vec<u8>>,
}

/// Metafied input with unlimited push-back.
#[derive(Debug)]
pub(crate) struct Input {
    buf: Vec<u8>,
    pos: usize,
    /// Pushed-back single bytes, last pushed first.
    back: Vec<u8>,
    /// Alias texts in front of `buf`, innermost last.
    pushed: Vec<Pushed>,
    /// Line number of the next byte from `buf`.
    pub(crate) lineno: u64,
    /// Set when a read found nothing left.
    pub(crate) stop: bool,
}

impl Input {
    /// Input over `text`, which must already be metafied.
    pub(crate) fn new(text: Vec<u8>) -> Input {
        Input { buf: text, pos: 0, back: Vec::new(), pushed: Vec::new(), lineno: 1, stop: false }
    }

    /// Next byte, or `None` at the end (and `stop` is set).
    pub(crate) fn get(&mut self) -> Option<u8> {
        if let Some(c) = self.back.pop() {
            self.stop = false;
            return Some(c);
        }
        while let Some(top) = self.pushed.last_mut() {
            if let Some(&c) = top.text.get(top.pos) {
                top.pos += 1;
                self.stop = false;
                return Some(c);
            }
            let _done = self.pushed.pop();
        }
        match self.buf.get(self.pos) {
            Some(&c) => {
                self.pos += 1;
                if c == b'\n' {
                    self.lineno += 1;
                }
                self.stop = false;
                Some(c)
            }
            None => {
                self.stop = true;
                None
            }
        }
    }

    /// Push `c` back so the next [`get`](Self::get) returns it.
    pub(crate) fn unget(&mut self, c: u8) {
        self.back.push(c);
        self.stop = false;
    }

    /// Push back an optional byte; `None` (end of input) pushes nothing.
    pub(crate) fn unget_opt(&mut self, c: Option<u8>) {
        if let Some(c) = c {
            self.unget(c);
        }
    }

    /// Look at the next byte without consuming it.
    pub(crate) fn peek(&mut self) -> Option<u8> {
        let c = self.get();
        self.unget_opt(c);
        c
    }

    /// Put `text` in front of the remaining input, as the expansion of
    /// `alias` if one is named.
    pub(crate) fn push_text(&mut self, text: &[u8], alias: Option<&[u8]>) {
        // Bytes already pushed back belong to the level they were read from,
        // as hungetc'd characters do in zsh, so they are read after the new
        // text: move them into a pushed string of their own beneath it.
        if !self.back.is_empty() {
            let mut rest: Vec<u8> = self.back.drain(..).collect();
            rest.reverse();
            self.pushed.push(Pushed { text: rest, pos: 0, alias: None });
        }
        self.pushed.push(Pushed { text: text.to_vec(), pos: 0, alias: alias.map(<[u8]>::to_vec) });
    }

    /// True while the text of `alias` is still being read.
    pub(crate) fn alias_in_use(&self, alias: &[u8]) -> bool {
        self.pushed.iter().any(|p| p.alias.as_deref() == Some(alias) && p.pos <= p.text.len())
    }

    /// True if the byte about to be read comes from an alias expansion.
    pub(crate) fn in_alias(&self) -> bool {
        self.back.is_empty() && self.pushed.last().is_some_and(|p| p.alias.is_some())
    }

    /// The unread remainder of the underlying buffer, for errors.
    pub(crate) fn rest(&self) -> &[u8] {
        self.buf.get(self.pos..).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushed_text_comes_before_pushed_back_bytes() {
        // zsh's hungetc returns a byte to the level it came from, and inpush
        // stacks new text above that level: `c = hgetc(); hungetc(c);
        // inpush(alias)` reads the alias first, then `c`.
        let mut i = Input::new(b"xyz".to_vec());
        assert_eq!(i.get(), Some(b'x'));
        i.unget(b'x');
        i.push_text(b"ab", Some(b"al"));
        assert!(i.alias_in_use(b"al"));
        let got: Vec<u8> = std::iter::from_fn(|| i.get()).collect();
        assert_eq!(got, b"abxyz");
        assert!(i.stop);
    }
}
