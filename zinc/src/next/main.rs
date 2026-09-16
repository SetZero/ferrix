//! zinc-next: zinc's runtime as a faithful port of zsh 5.9's C runtime.
//!
//! The lexer and parser are shared with the current zinc; everything that
//! runs a parsed command is ported module by module from zsh's `Src/`, each
//! module naming the file it follows. When this reaches parity with zsh on
//! oh-my-zsh it replaces the runtime in `src/*.rs` and becomes `zinc`.

#![allow(
    dead_code,
    reason = "the port lands bottom-up; modules land before their callers"
)]

#[path = "../ast.rs"]
mod ast;
#[path = "../dquote.rs"]
mod dquote;
#[path = "../input.rs"]
mod input;
#[path = "../lex.rs"]
mod lex;
#[path = "../lexword.rs"]
mod lexword;
#[path = "../parse.rs"]
mod parse;
#[path = "../parsectl.rs"]
mod parsectl;
#[path = "../tok.rs"]
mod tok;

mod glob;
mod hashtable;
mod hist;
mod math;
mod misc;
mod modules;
mod options;
mod params;
mod params_value;
mod pattern;
mod pending;
mod shell;
mod signames;
mod sort;
mod subst;
mod utils;

use pending::{builtin, exec, jobs, prompt, signals, sysutil, tables};

fn main() {
    let mut sh = shell::Shell::new();
    let status = sh.main_entry(std::env::args_os().collect());
    shell::exit_now(status);
}
