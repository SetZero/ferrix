//! zinc: a zsh-compatible shell.
//!
//! The acceptance criterion is that oh-my-zsh runs in it. Behaviour is
//! measured against zsh 5.9, whose source is the reference for every rule a
//! module here ports; each module names the zsh file it follows.

#![allow(dead_code, reason = "the shell is being built bottom-up; stages land before their callers")]

mod ast;
mod dquote;
mod input;
mod lex;
mod lexword;
mod parse;
mod parsectl;
mod tok;

use std::io::Write;

use lex::{AliasDef, LexEnv, LexOpts, Lexer, Tok};

/// A lexing environment with zsh's default options and no aliases.
#[derive(Debug)]
struct Defaults;

impl LexEnv for Defaults {
    fn alias(&self, _name: &[u8]) -> Option<AliasDef> {
        None
    }
    fn suffix_alias(&self, _ext: &[u8]) -> Option<AliasDef> {
        None
    }
    fn opts(&self) -> LexOpts {
        default_opts()
    }
}

/// zsh's defaults for the options the lexer reads, in a script.
fn default_opts() -> LexOpts {
    LexOpts {
        comments: true,
        aliases: true,
        shortloops: true,
        multifuncdef: true,
        aliasfuncdef: false,
        execopt: true,
        ..LexOpts::default()
    }
}

/// `zinc --tokens FILE`: print one token per line, for comparing the lexer
/// with zsh's by hand while the parser is written.
fn dump_tokens(text: &[u8]) -> std::io::Result<()> {
    let env = Defaults;
    let mut lx = Lexer::new(tok::metafy(text), env.opts());
    let mut out = std::io::stdout().lock();
    loop {
        lx.ctxtlex(&env);
        let mut line = format!("{:?}", lx.tok).into_bytes();
        if let Some(s) = &lx.tokstr {
            line.push(b' ');
            let mut shown = s.clone();
            tok::untokenize(&mut shown);
            line.extend_from_slice(&tok::unmetafy(&shown));
        }
        if lx.tokfd >= 0 && lx.tok.is_redir() {
            line.extend_from_slice(format!(" fd={}", lx.tokfd).as_bytes());
        }
        line.push(b'\n');
        out.write_all(&line)?;
        lx.tokfd = -1;
        if matches!(lx.tok, Tok::Endinput | Tok::Lexerr) {
            if let Some(e) = &lx.error {
                writeln!(out, "error: {e}")?;
            }
            return Ok(());
        }
    }
}

/// `zinc --ast FILE`: parse event by event and print each tree.
fn dump_ast(text: &[u8]) -> std::io::Result<bool> {
    let env = Defaults;
    let mut lx = Lexer::new(tok::metafy(text), env.opts());
    let mut out = std::io::stdout().lock();
    loop {
        let mut p = parse::Parser::new(&mut lx, &env);
        match p.parse_event() {
            Ok(Some(list)) => writeln!(out, "{list:#?}")?,
            Ok(None) => return Ok(true),
            Err(e) => {
                writeln!(out, "zinc:{}: {}", e.lineno, e.msg)?;
                return Ok(false);
            }
        }
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    match args.get(1..).unwrap_or(&[]) {
        [flag, file] if flag == "--ast" => match std::fs::read(file).and_then(|t| dump_ast(&t)) {
            Ok(true) => std::process::ExitCode::SUCCESS,
            Ok(false) => std::process::ExitCode::from(1),
            Err(e) => {
                let _ignored = writeln!(std::io::stderr(), "zinc: {e}");
                std::process::ExitCode::from(1)
            }
        },
        [flag, file] if flag == "--tokens" => {
            let result = std::fs::read(file).and_then(|text| dump_tokens(&text));
            match result {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(e) => {
                    let _ignored = writeln!(std::io::stderr(), "zinc: {e}");
                    std::process::ExitCode::from(1)
                }
            }
        }
        _ => {
            let _ignored = writeln!(std::io::stderr(), "zinc: usage: zinc --tokens FILE");
            std::process::ExitCode::from(2)
        }
    }
}
