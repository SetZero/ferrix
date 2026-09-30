//! The parse tree.
//!
//! zsh compiles to a flat word code; zinc builds a tree with the same shape.
//! Words stay tokenized byte strings (see [`crate::tok`]); expansion happens
//! when a node runs.

use std::rc::Rc;

use crate::lex::HereDocSlot;

/// A tokenized, metafied word.
pub(crate) type Word = Vec<u8>;

/// A sequence of sublists, each run in the foreground or background.
#[derive(Debug, Clone, Default)]
pub(crate) struct List {
    pub(crate) items: Vec<ListItem>,
}

/// One entry of a [`List`].
#[derive(Debug, Clone)]
pub(crate) struct ListItem {
    pub(crate) sublist: Sublist,
    pub(crate) mode: ListMode,
}

/// How a list item runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListMode {
    /// Terminated by `;`, a newline or the end.
    Sync,
    /// `&`.
    Async,
    /// `&|` or `&!`.
    Disown,
}

/// Pipelines joined by `&&` and `||`, evaluated left to right.
#[derive(Debug, Clone)]
pub(crate) struct Sublist {
    pub(crate) first: Sublist2,
    pub(crate) rest: Vec<(AndOr, Sublist2)>,
}

/// The connector before a pipeline in a [`Sublist`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AndOr {
    And,
    Or,
}

/// A pipeline with its `!` or `coproc` prefix.
#[derive(Debug, Clone)]
pub(crate) struct Sublist2 {
    pub(crate) not: bool,
    pub(crate) coproc: bool,
    /// Absent for a bare `!` or `coproc`.
    pub(crate) pipeline: Option<Pipeline>,
}

/// Commands joined by `|`.
#[derive(Debug, Clone)]
pub(crate) struct Pipeline {
    pub(crate) cmds: Vec<Command>,
    pub(crate) lineno: u64,
}

/// One command and its redirections, in the order written.
#[derive(Debug, Clone)]
pub(crate) struct Command {
    pub(crate) kind: CmdKind,
    pub(crate) redirs: Vec<Redir>,
    pub(crate) lineno: u64,
}

/// The kinds of command.
#[derive(Debug, Clone)]
pub(crate) enum CmdKind {
    /// Assignments and words; either may be empty (not both, unless there
    /// are redirections).
    Simple {
        assigns: Vec<Assign>,
        words: Vec<Word>,
    },
    /// `typeset` and its relatives, whose arguments may be array assignments.
    Typeset {
        assigns: Vec<Assign>,
        words: Vec<Word>,
        args: Vec<Assign>,
    },
    /// `( list )`.
    Subsh(List),
    /// `{ list }`.
    Cursh(List),
    /// `{ try } always { always }`.
    Try { body: List, always: List },
    /// `for name ... in words`, `foreach`, `for name (words)`.
    For {
        vars: Vec<Word>,
        words: Option<Vec<Word>>,
        body: List,
    },
    /// `for (( init; cond; step ))`.
    ForArith {
        init: Word,
        cond: Word,
        step: Word,
        body: List,
    },
    /// `select name in words`.
    Select {
        var: Word,
        words: Option<Vec<Word>>,
        body: List,
    },
    /// `case word in arms esac`.
    Case { word: Word, arms: Vec<CaseArm> },
    /// `if`/`elif` pairs and an optional `else`.
    If {
        branches: Vec<(List, List)>,
        otherwise: Option<List>,
    },
    /// `while` or `until`.
    While { until: bool, cond: List, body: List },
    /// `repeat count body`.
    Repeat { count: Word, body: List },
    /// A function definition; with no names, an anonymous function run at
    /// once with `args`.
    FuncDef {
        names: Vec<Word>,
        body: Rc<List>,
        tracing: bool,
        args: Vec<Word>,
    },
    /// `time pipeline`; `None` for a bare `time`.
    Time(Option<Box<Sublist2>>),
    /// `[[ cond ]]`.
    Cond(Cond),
    /// `(( expr ))`.
    Arith(Word),
}

/// What follows a `case` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaseTerm {
    /// `;;` or the end.
    Break,
    /// `;&`: run the next arm's body too.
    Fallthrough,
    /// `;|`: go on testing the following patterns.
    TestNext,
}

/// One arm of a `case`.
#[derive(Debug, Clone)]
pub(crate) struct CaseArm {
    pub(crate) patterns: Vec<Word>,
    pub(crate) body: List,
    pub(crate) term: CaseTerm,
}

/// An assignment `name=value`, `name+=value`, `name[key]=value`,
/// `name=(words)`.
#[derive(Debug, Clone)]
pub(crate) struct Assign {
    /// The name, possibly with a tokenized subscript.
    pub(crate) name: Word,
    pub(crate) value: AssignValue,
    pub(crate) append: bool,
}

/// The right-hand side of an [`Assign`].
#[derive(Debug, Clone)]
pub(crate) enum AssignValue {
    Scalar(Word),
    Array(Vec<Word>),
    /// A bare name among typeset's arguments.
    None,
}

/// A conditional expression inside `[[ ]]`.
#[derive(Debug, Clone)]
pub(crate) enum Cond {
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    /// `-X word` for one of zsh's single-letter tests.
    Unary(u8, Word),
    /// A binary test.
    Binary(CondOp, Word, Word),
    /// A condition provided by a module: `-name args...` or `a -name b`.
    Module {
        name: Word,
        args: Vec<Word>,
        infix: bool,
    },
}

/// Binary operators in `[[ ]]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CondOp {
    StrEq,
    StrDeq,
    StrNeq,
    StrLt,
    StrGt,
    Regex,
    Nt,
    Ot,
    Ef,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

/// A redirection.
#[derive(Debug, Clone)]
pub(crate) struct Redir {
    pub(crate) kind: RedirKind,
    pub(crate) fd: i32,
    pub(crate) target: Word,
    /// `{name}>file`: the shell picks the descriptor and stores it in `name`.
    pub(crate) varid: Option<Word>,
    pub(crate) heredoc: Option<HereDocSlot>,
}

/// Redirection operators (zsh's `REDIR_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedirKind {
    Write,
    WriteNow,
    App,
    AppNow,
    ErrWrite,
    ErrWriteNow,
    ErrApp,
    ErrAppNow,
    ReadWrite,
    Read,
    HereDoc,
    HereDocDash,
    HereStr,
    MergeIn,
    MergeOut,
    InPipe,
    OutPipe,
}

impl RedirKind {
    /// True for the redirections whose default descriptor is 0.
    pub(crate) fn reads(self) -> bool {
        matches!(
            self,
            RedirKind::ReadWrite
                | RedirKind::Read
                | RedirKind::HereDoc
                | RedirKind::HereDocDash
                | RedirKind::HereStr
                | RedirKind::MergeIn
                | RedirKind::InPipe
        )
    }
}
