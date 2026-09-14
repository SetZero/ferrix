//! The lexer: zsh's `lex.c`, token for token.
//!
//! Which characters end a word, whether `(` opens a subshell or a glob group,
//! whether `}` closes a brace group — all of it depends on where the parser
//! is, which zsh passes to the lexer in globals (`incmdpos`, `incond`,
//! `incasepat`, `infor`, `intypeset`). They are fields here, set by the parser
//! the same way, so the two can be compared line by line.

use std::cell::RefCell;
use std::rc::Rc;

use crate::input::Input;
use crate::tok;

/// A lexical token (zsh's `enum lextok`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tok {
    Nulltok,
    Seper,
    Newlin,
    Semi,
    Dsemi,
    Amper,
    Inpar,
    Outpar,
    Dbar,
    Damper,
    Outang,
    Outangbang,
    Doutang,
    Doutangbang,
    Inang,
    Inoutang,
    Dinang,
    Dinangdash,
    Inangamp,
    Outangamp,
    Ampoutang,
    Outangampbang,
    Doutangamp,
    Doutangampbang,
    Trinang,
    Bar,
    Baramp,
    Inoutpar,
    Dinpar,
    Doutpar,
    Amperbang,
    Semiamp,
    Semibar,
    Doutbrack,
    String,
    Envstring,
    Envarray,
    Endinput,
    Lexerr,
    Bang,
    Dinbrack,
    Inbrace,
    Outbrace,
    Case,
    Coproc,
    Doloop,
    Done,
    Elif,
    Else,
    Zend,
    Esac,
    Fi,
    For,
    Foreach,
    Func,
    If,
    Nocorrect,
    Repeat,
    Select,
    Then,
    Time,
    Until,
    While,
    Typeset,
}

impl Tok {
    /// True for a redirection operator (`OUTANG..=TRINANG`).
    pub(crate) fn is_redir(self) -> bool {
        use Tok::*;
        matches!(
            self,
            Outang
                | Outangbang
                | Doutang
                | Doutangbang
                | Inang
                | Inoutang
                | Dinang
                | Dinangdash
                | Inangamp
                | Outangamp
                | Ampoutang
                | Outangampbang
                | Doutangamp
                | Doutangampbang
                | Trinang
        )
    }

    /// The text of a punctuation token, for error messages.
    pub(crate) fn text(self) -> &'static str {
        use Tok::*;
        match self {
            Seper | Semi => ";",
            Newlin => "\\n",
            Dsemi => ";;",
            Amper => "&",
            Inpar => "(",
            Outpar => ")",
            Dbar => "||",
            Damper => "&&",
            Outang => ">",
            Outangbang => ">|",
            Doutang => ">>",
            Doutangbang => ">>|",
            Inang => "<",
            Inoutang => "<>",
            Dinang => "<<",
            Dinangdash => "<<-",
            Inangamp => "<&",
            Outangamp => ">&",
            Ampoutang => "&>",
            Outangampbang => "&>|",
            Doutangamp => ">>&",
            Doutangampbang => ">>&|",
            Trinang => "<<<",
            Bar => "|",
            Baramp => "|&",
            Inoutpar => "()",
            Dinpar => "((",
            Doutpar => "))",
            Amperbang => "&|",
            Semiamp => ";&",
            Semibar => ";|",
            _ => "",
        }
    }
}

/// The reserved words and their tokens (zsh's `reswds`).
pub(crate) fn reserved(word: &[u8]) -> Option<Tok> {
    use Tok::*;
    Some(match word {
        b"!" => Bang,
        b"[[" => Dinbrack,
        b"{" => Inbrace,
        b"}" => Outbrace,
        b"case" => Case,
        b"coproc" => Coproc,
        b"declare" | b"export" | b"float" | b"integer" | b"local" | b"readonly" | b"typeset" => {
            Typeset
        }
        b"do" => Doloop,
        b"done" => Done,
        b"elif" => Elif,
        b"else" => Else,
        b"end" => Zend,
        b"esac" => Esac,
        b"fi" => Fi,
        b"for" => For,
        b"foreach" => Foreach,
        b"function" => Func,
        b"if" => If,
        b"nocorrect" => Nocorrect,
        b"repeat" => Repeat,
        b"select" => Select,
        b"then" => Then,
        b"time" => Time,
        b"until" => Until,
        b"while" => While,
        _ => return None,
    })
}

/// An alias as the lexer needs it.
#[derive(Debug, Clone)]
pub(crate) struct AliasDef {
    /// The replacement text, metafied.
    pub(crate) text: Vec<u8>,
    /// `alias -g`: expands in any position.
    pub(crate) global: bool,
}

/// The option settings the lexer and parser read.
#[derive(Debug, Clone, Copy, Default)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per zsh option")]
pub(crate) struct LexOpts {
    pub(crate) shglob: bool,
    pub(crate) kshglob: bool,
    pub(crate) ignorebraces: bool,
    pub(crate) ignoreclosebraces: bool,
    /// Whether `#` starts a comment here (INTERACTIVE_COMMENTS and friends).
    pub(crate) comments: bool,
    pub(crate) rcquotes: bool,
    pub(crate) cshjunkiequotes: bool,
    pub(crate) cshjunkieloops: bool,
    pub(crate) aliases: bool,
    pub(crate) posixaliases: bool,
    pub(crate) shortloops: bool,
    pub(crate) shortrepeat: bool,
    pub(crate) multifuncdef: bool,
    pub(crate) aliasfuncdef: bool,
    pub(crate) execopt: bool,
}

/// What the lexer asks of the shell: aliases and options.
pub(crate) trait LexEnv {
    /// The alias named `name` (metafied, untokenized).
    fn alias(&self, name: &[u8]) -> Option<AliasDef>;
    /// The suffix alias for extension `ext`.
    fn suffix_alias(&self, ext: &[u8]) -> Option<AliasDef>;
    /// Current option settings.
    fn opts(&self) -> LexOpts;
}

/// A here-document waiting for its body: filled when the lexer reaches the
/// end of the line the redirection was on.
#[derive(Debug, Default)]
pub(crate) struct HereDoc {
    /// The terminator word as written, tokenized.
    pub(crate) term: Vec<u8>,
    /// `<<-`: leading tabs are stripped.
    pub(crate) strip_tabs: bool,
    /// The body; tokenized as in double quotes unless the terminator was quoted.
    pub(crate) body: Vec<u8>,
    /// The terminator was quoted: the body is literal.
    pub(crate) quoted: bool,
}

/// Shared slot for a here-document body.
pub(crate) type HereDocSlot = Rc<RefCell<HereDoc>>;

/// Lexer state (zsh's lexer and parser globals).
#[derive(Debug)]
pub(crate) struct Lexer {
    pub(crate) input: Input,
    pub(crate) tok: Tok,
    /// Text of a `String`-like token, tokenized and metafied.
    pub(crate) tokstr: Option<Vec<u8>>,
    /// The file descriptor written before a redirection (`2>`), or -1.
    pub(crate) tokfd: i32,
    pub(crate) incmdpos: bool,
    pub(crate) incond: i32,
    pub(crate) incasepat: i32,
    pub(crate) infor: i32,
    pub(crate) intypeset: bool,
    inredir: bool,
    oldpos: bool,
    pub(crate) dbparens: bool,
    inrepeat: i32,
    pub(crate) noaliases: bool,
    /// `1`: the last newline token ended the input's line; `-1`: more follows.
    pub(crate) isnewlin: i32,
    /// Set by the lexer when a newline should not become `SEPER`.
    pub(crate) keep_newline: bool,
    /// The current `SEPER` was written `;` rather than as a newline.
    pub(crate) sep_was_semi: bool,
    /// Line of the current token's first character.
    pub(crate) toklineno: u64,
    /// The first lexical error, reported by the parser.
    pub(crate) error: Option<String>,
    pub(crate) opts: LexOpts,
    /// Here-documents whose bodies follow the current line.
    pending_heredocs: Vec<HereDocSlot>,
    /// The next word's alias expansion ended in a blank: expand it too.
    inalmore: bool,
}

impl Lexer {
    /// A lexer over metafied `text`.
    pub(crate) fn new(text: Vec<u8>, opts: LexOpts) -> Lexer {
        Lexer {
            input: Input::new(text),
            tok: Tok::Endinput,
            tokstr: None,
            tokfd: -1,
            incmdpos: true,
            incond: 0,
            incasepat: 0,
            infor: 0,
            intypeset: false,
            inredir: false,
            oldpos: false,
            dbparens: false,
            inrepeat: 0,
            noaliases: false,
            isnewlin: 0,
            keep_newline: false,
            sep_was_semi: false,
            toklineno: 1,
            error: None,
            opts,
            pending_heredocs: Vec::new(),
            inalmore: false,
        }
    }

    /// Record a lexical error; the first one wins.
    pub(crate) fn err(&mut self, msg: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(msg.into());
        }
    }

    /// Register a here-document whose body is read at the next newline.
    pub(crate) fn add_heredoc(&mut self, slot: HereDocSlot) {
        self.pending_heredocs.push(slot);
    }

    /// Read the next token, expanding aliases (zsh's `zshlex`).
    pub(crate) fn zshlex(&mut self, env: &dyn LexEnv) {
        if self.tok == Tok::Lexerr {
            return;
        }
        loop {
            if self.inrepeat > 0 {
                self.inrepeat += 1;
            }
            if self.inrepeat == 3 && (self.opts.shortloops || self.opts.shortrepeat) {
                self.incmdpos = true;
            }
            self.tok = self.gettok();
            if self.tok == Tok::Endinput || !self.exalias(env) {
                break;
            }
        }
        if matches!(self.tok, Tok::Newlin | Tok::Endinput) {
            self.read_heredocs();
        }
        if self.tok != Tok::Newlin {
            self.isnewlin = 0;
        } else {
            self.isnewlin = if self.input.rest().is_empty() { 1 } else { -1 };
        }
        self.sep_was_semi = self.tok == Tok::Semi;
        if self.tok == Tok::Semi || (self.tok == Tok::Newlin && !self.keep_newline) {
            self.tok = Tok::Seper;
        }
    }

    /// [`zshlex`](Self::zshlex) plus the command-position bookkeeping
    /// (zsh's `ctxtlex`).
    pub(crate) fn ctxtlex(&mut self, env: &dyn LexEnv) {
        use Tok::*;
        self.zshlex(env);
        match self.tok {
            Seper | Newlin | Semi | Dsemi | Semiamp | Semibar | Amper | Amperbang | Inpar
            | Inbrace | Dbar | Damper | Bar | Baramp | Inoutpar | Doloop | Then | Elif | Else
            | Doutbrack => self.incmdpos = true,
            String | Typeset | Envarray | Outpar | Case | Dinbrack => self.incmdpos = false,
            _ => {}
        }
        if self.tok != Dinpar {
            self.infor = if self.tok == For { 2 } else { 0 };
        }
        if self.tok.is_redir() || matches!(self.tok, For | Foreach | Select) {
            self.inredir = true;
            self.oldpos = self.incmdpos;
            self.incmdpos = false;
        } else if self.inredir {
            self.incmdpos = self.oldpos;
            self.inredir = false;
        }
    }

    fn read_heredocs(&mut self) {
        let docs = std::mem::take(&mut self.pending_heredocs);
        for slot in docs {
            let (term, strip) = {
                let d = slot.borrow();
                (d.term.clone(), d.strip_tabs)
            };
            let quoted = term.iter().any(|&c| tok::is_null(c));
            let mut word = tok::remove_nulls(&term);
            if strip {
                while word.first() == Some(&b'\t') {
                    let _tab = word.remove(0);
                }
            }
            let body = self.gethere(&word, strip, quoted);
            let mut d = slot.borrow_mut();
            d.body = body;
            d.quoted = quoted;
        }
    }

    /// Read a here-document body up to a line equal to `term` (zsh's
    /// `gethere`). Unquoted bodies are tokenized as in double quotes.
    fn gethere(&mut self, term: &[u8], strip: bool, quoted: bool) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let start = buf.len();
            let mut c = self.input.get();
            while strip && c == Some(b'\t') {
                c = self.input.get();
            }
            loop {
                match c {
                    None | Some(b'\n') => break,
                    Some(b'\\') if !quoted => {
                        buf.push(b'\\');
                        c = self.input.get();
                        if c == Some(b'\n') {
                            let _bs = buf.pop();
                            c = self.input.get();
                            continue;
                        }
                    }
                    Some(ch) => {
                        buf.push(ch);
                        c = self.input.get();
                    }
                }
            }
            if buf.get(start..) == Some(term) {
                buf.truncate(start);
                break;
            }
            if c.is_none() {
                break;
            }
            buf.push(b'\n');
        }
        if quoted {
            buf
        } else {
            match crate::dquote::parse_dquote_string(&buf, self.opts) {
                Ok(t) => t,
                Err(_) => {
                    self.err("parse error in here-document");
                    buf
                }
            }
        }
    }

    /// Expand an alias or recognise a reserved word (zsh's `exalias`).
    /// Returns true if an alias was pushed and the token must be read again.
    fn exalias(&mut self, env: &dyn LexEnv) -> bool {
        let text: Vec<u8> = match &self.tokstr {
            None => {
                if self.tok == Tok::Newlin {
                    return false;
                }
                self.tok.text().as_bytes().to_vec()
            }
            Some(s) => s.iter().map(|&c| tok::detok(c)).collect(),
        };
        if self.tokstr.is_none() {
            return self.checkalias(env, &text);
        }
        if self.tok == Tok::String {
            let has_tok = self.tokstr.as_deref().is_some_and(tok::has_token);
            if (!has_tok || !self.opts.posixaliases) && self.checkalias(env, &text) {
                return true;
            }
            let close_brace =
                !self.opts.ignorebraces && !self.opts.ignoreclosebraces && text.as_slice() == b"}";
            if let Some(rw) = (self.incmdpos || close_brace)
                .then(|| reserved(&text))
                .flatten()
            {
                self.tok = rw;
                self.inrepeat = i32::from(rw == Tok::Repeat);
                if rw == Tok::Dinbrack {
                    self.incond = 1;
                }
            } else if self.incond > 0 && text.as_slice() == b"]]" {
                self.tok = Tok::Doutbrack;
                self.incond = 0;
            } else if self.incond == 1 && text.as_slice() == b"!" {
                self.tok = Tok::Bang;
            }
        }
        self.inalmore = false;
        false
    }

    fn checkalias(&mut self, env: &dyn LexEnv, text: &[u8]) -> bool {
        if self.noaliases || !self.opts.aliases {
            return false;
        }
        if self.opts.posixaliases && !(self.tok == Tok::String && reserved(text).is_none()) {
            return false;
        }
        if let Some(an) = env.alias(text)
            && !self.input.alias_in_use(text)
            && (an.global || (self.incmdpos && self.tok == Tok::String) || self.inalmore)
        {
            if let Some(c) = self.input.peek()
                && !is_blank(c)
            {
                self.input.push_text(b" ", None);
            }
            self.input.push_text(&an.text, Some(text));
            if an.text.first() == Some(&b' ') && !an.global {
                self.inalmore = true;
            }
            return true;
        }
        if self.incmdpos
            && let Some(dot) = text.iter().rposition(|&c| c == b'.')
            && dot > 0
            && dot + 1 < text.len()
            && text.get(dot - 1) != Some(&tok::META)
        {
            let ext = text.get(dot + 1..).unwrap_or(&[]);
            if let Some(an) = env.suffix_alias(ext)
                && !self.input.alias_in_use(ext)
            {
                // Pushed in reverse: the alias text, a space, then the word.
                self.input.push_text(text, Some(ext));
                self.input.push_text(b" ", None);
                self.input.push_text(&an.text, None);
                return true;
            }
        }
        false
    }
}

/// zsh's `iblank`: space or tab (and the other non-newline blanks).
pub(crate) fn is_blank(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

/// zsh's `inblank`: a blank or a newline.
pub(crate) fn is_inblank(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\n'
}
