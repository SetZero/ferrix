//! zinc: a zsh-compatible shell.
//!
//! The acceptance criterion is that oh-my-zsh runs in it. Behaviour is
//! measured against zsh 5.9, whose source is the reference for every rule a
//! module here ports; each module names the zsh file it follows.

#![allow(
    dead_code,
    reason = "the shell is being built bottom-up; stages land before their callers"
)]

mod arith;
mod ast;
mod builtins;
mod cond;
mod dquote;
mod exec;
mod expand;
mod input;
mod lex;
mod lexword;
mod param;
mod parse;
mod parsectl;
mod pattern;
mod prompt;
mod qual;
mod regex;
mod shell;
mod tok;
mod zle;

use std::os::unix::ffi::OsStrExt;

use lex::Lexer;
use shell::{Flow, Shell};

/// `zinc --tokens FILE`: one token per line, for comparing with zsh's lexer.
fn dump_tokens(sh: &Shell, text: &[u8]) {
    let mut lx = Lexer::new(tok::metafy(text), sh.lex_opts());
    let mut outb = Vec::new();
    loop {
        lx.zshlex(sh);
        outb.extend_from_slice(format!("{:?}", lx.tok).as_bytes());
        if let Some(s) = &lx.tokstr {
            let mut shown = s.clone();
            tok::untokenize(&mut shown);
            outb.push(b' ');
            outb.extend(tok::unmetafy(&shown));
        }
        outb.push(b'\n');
        if matches!(lx.tok, lex::Tok::Endinput | lex::Tok::Lexerr) {
            break;
        }
    }
    let _ok = exec::write_fd(1, &outb);
}

/// `zinc --ast FILE`: parse and print each event's tree.
fn dump_ast(sh: &Shell, text: &[u8]) -> i32 {
    let mut lx = Lexer::new(tok::metafy(text), sh.lex_opts());
    loop {
        let mut p = parse::Parser::new(&mut lx, sh);
        match p.parse_event() {
            Ok(Some(list)) => {
                let _ok = exec::write_fd(1, format!("{list:#?}\n").as_bytes());
            }
            Ok(None) => return 0,
            Err(e) => {
                let _ok = exec::write_fd(1, format!("zinc:{}: {}\n", e.lineno, e.msg).as_bytes());
                return 1;
            }
        }
    }
}

/// Leave the shell: the EXIT trap, then the process.
fn finish(sh: &mut Shell) -> ! {
    if let Some(trap) = sh.exit_trap.take() {
        let status = sh.status;
        sh.flow = Flow::Normal;
        exec::run_string(sh, &trap);
        sh.status = status;
    }
    exec::exit_now(sh.status)
}

/// True if `text` ends inside an unfinished construct and needs more lines.
fn incomplete(sh: &Shell, text: &[u8]) -> bool {
    let mut lx = Lexer::new(text.to_vec(), sh.lex_opts());
    let r = {
        let mut p = parse::Parser::new(&mut lx, sh);
        p.parse_all()
    };
    r.is_err() && lx.input.rest().is_empty() && lx.input.stop
}

/// Read one line from fd 0, unbuffered so programs see the rest of the input.
fn read_line() -> Option<Vec<u8>> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        // SAFETY: b is one writable byte.
        let n = unsafe { libc::read(0, b.as_mut_ptr().cast(), 1) };
        if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if n <= 0 {
            return if line.is_empty() { None } else { Some(line) };
        }
        line.push(b[0]);
        if b[0] == b'\n' {
            return Some(line);
        }
    }
}

fn interactive_loop(sh: &mut Shell) -> ! {
    let mut buffer: Vec<u8> = Vec::new();
    let mut editor = zle::Editor::default();
    sh.at_prompt = true;
    loop {
        let which: &[u8] = if buffer.is_empty() { b"PS1" } else { b"PS2" };
        let ps = sh.get(which).map(|v| v.joined()).unwrap_or_default();
        let prompt = prompt::expand(sh, &ps);
        let Some(line) = editor.read_line(sh, &prompt) else {
            if !buffer.is_empty() {
                exec::run_string(sh, &buffer);
            }
            let _ok = exec::write_fd(2, b"\n");
            finish(sh);
        };
        buffer.extend(tok::metafy(&line));
        if incomplete(sh, &buffer) {
            continue;
        }
        let text = std::mem::take(&mut buffer);
        editor.add_history(&tok::unmetafy(&text));
        exec::run_string(sh, &text);
        sh.at_prompt = true;
        reap(sh);
        match sh.flow {
            Flow::Exit => finish(sh),
            _ => sh.flow = Flow::Normal,
        }
    }
}

/// Collect background children that have finished.
fn reap(sh: &mut Shell) {
    sh.jobs.retain(|&pid| {
        let mut st = 0;
        // SAFETY: st is a valid out-pointer.
        let r = unsafe { libc::waitpid(pid, &raw mut st, libc::WNOHANG) };
        r == 0
    });
}

fn source_if_exists(sh: &mut Shell, path: &[u8]) {
    if let Ok(text) = std::fs::read(String::from_utf8_lossy(path).as_ref()) {
        exec::run_string(sh, &tok::metafy(&text));
        if sh.flow == Flow::Exit {
            finish(sh);
        }
        sh.flow = Flow::Normal;
    }
}

fn main() {
    // Rust starts programs with SIGPIPE ignored, which every child would
    // inherit; a shell's children expect the default.
    // SAFETY: SIG_DFL is a valid disposition for SIGPIPE.
    let _old = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let args: Vec<Vec<u8>> = std::env::args_os().map(|a| a.as_bytes().to_vec()).collect();
    let argv0 = args.first().cloned().unwrap_or_else(|| b"zinc".to_vec());
    let base = argv0
        .rsplit(|&c| c == b'/')
        .next()
        .unwrap_or(b"zinc")
        .to_vec();
    let base = base.strip_prefix(b"-").unwrap_or(&base).to_vec();
    let name = if base == b"zinc" {
        "zsh".to_owned()
    } else {
        String::from_utf8_lossy(&base).into_owned()
    };
    let mut sh = Shell::new(name);
    sh.argzero = tok::metafy(&argv0);

    let mut command: Option<Vec<u8>> = None;
    let mut force_interactive = false;
    let mut no_rcs = false;
    let mut k = 1;
    while let Some(a) = args.get(k) {
        match a.as_slice() {
            b"--tokens" | b"--ast" => {
                let file = args.get(k + 1).cloned().unwrap_or_default();
                let Ok(text) = std::fs::read(String::from_utf8_lossy(&file).as_ref()) else {
                    sh.error("cannot read file");
                    exec::exit_now(1);
                };
                if a == b"--tokens" {
                    dump_tokens(&sh, &text);
                    exec::exit_now(0);
                }
                exec::exit_now(dump_ast(&sh, &text));
            }
            b"--version" => {
                let _ok = exec::write_fd(1, b"zsh 5.9 (zinc)\n");
                exec::exit_now(0);
            }
            b"--" => {
                k += 1;
                break;
            }
            _ if a.first() == Some(&b'-') && a.len() > 1 => {
                for &c in a.iter().skip(1) {
                    match c {
                        b'c' => {
                            k += 1;
                            command = args.get(k).map(|c| tok::metafy(c));
                        }
                        b'i' => force_interactive = true,
                        b'f' => no_rcs = true,
                        b'x' => drop(sh.set_option(b"xtrace", true)),
                        b'e' => drop(sh.set_option(b"errexit", true)),
                        _ => {}
                    }
                }
                k += 1;
            }
            _ => break,
        }
    }
    let rest: Vec<Vec<u8>> = args
        .get(k..)
        .unwrap_or(&[])
        .iter()
        .map(|a| tok::metafy(a))
        .collect();

    if let Some(cmd) = command {
        if let Some((zero, pos)) = rest.split_first() {
            sh.argzero = zero.clone();
            sh.positional = pos.to_vec();
        }
        exec::run_string(&mut sh, &cmd);
        finish(&mut sh);
    }
    if let Some((file, pos)) = rest.split_first() {
        sh.argzero = file.clone();
        sh.positional = pos.to_vec();
        let Ok(text) = std::fs::read(String::from_utf8_lossy(&tok::unmetafy(file)).as_ref()) else {
            sh.error(&format!(
                "can't open input file: {}",
                String::from_utf8_lossy(&tok::unmetafy(file))
            ));
            exec::exit_now(127);
        };
        sh.script = file.clone();
        exec::run_string(&mut sh, &tok::metafy(&text));
        finish(&mut sh);
    }
    // SAFETY: isatty has no memory-safety preconditions.
    let tty = unsafe { libc::isatty(0) } == 1;
    sh.interactive = force_interactive || tty;
    if !sh.interactive {
        let mut text = Vec::new();
        while let Some(line) = read_line() {
            text.extend(line);
        }
        exec::run_string(&mut sh, &tok::metafy(&text));
        finish(&mut sh);
    }
    for sig in [
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
    ] {
        // SAFETY: SIG_IGN is a valid disposition for these signals.
        let _old = unsafe { libc::signal(sig, libc::SIG_IGN) };
    }
    if !no_rcs {
        let dir = sh
            .get(b"ZDOTDIR")
            .or_else(|| sh.get(b"HOME"))
            .map(|v| v.joined())
            .unwrap_or_default();
        for f in [&b"/.zshenv"[..], b"/.zshrc"] {
            let path = [dir.as_slice(), f].concat();
            source_if_exists(&mut sh, &tok::unmetafy(&path));
        }
    }
    interactive_loop(&mut sh);
}
