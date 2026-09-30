//! `/bin/passwd [account]`: change a password, through `authd`
//! (`docs/AUTH.md` §3.3, service `passwd`).
//!
//! It asks nothing itself. `authd` sends the prompts: the current password,
//! unless the caller is root, then the new one twice. `passwd` shows them,
//! reads the answers from the terminal with its echo off (or a line of
//! standard input each, for a script), and says what `authd` decided. It
//! reads no hash and writes no file, so it needs no privilege: root may set
//! anyone's password because `authd` sees the caller is root, not because
//! `passwd` is set-uid.

use std::io::Write as _;
use std::process::ExitCode;

use ferrix_auth_client::{Connection, Terminal, Verdict, converse};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let account = match arguments.as_slice() {
        [] => "",
        [account] if !account.starts_with('-') => account.as_str(),
        _ => {
            say("usage: passwd [account]");
            return ExitCode::from(2);
        }
    };
    let verdict = Connection::open()
        .and_then(|connection| converse(&connection, "passwd", account, &mut Terminal::new()));
    match verdict {
        Ok(Verdict::Accepted { account, .. }) => {
            say(&format!("passwd: the password for {account} is changed"));
            ExitCode::SUCCESS
        }
        Ok(Verdict::Failed { text, .. }) => {
            say(&format!("passwd: {text}"));
            ExitCode::FAILURE
        }
        Ok(Verdict::Unavailable(text)) => {
            say(&format!("passwd: {text}"));
            ExitCode::FAILURE
        }
        Err(error) => {
            say(&format!("passwd: cannot reach authd: {error}"));
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}
