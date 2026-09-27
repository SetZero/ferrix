//! `/bin/authctl`: ask `authd` about accounts, and run a conversation by
//! hand (`docs/AUTH.md` §3.3).
//!
//! ```text
//! authctl status [account]         what an account has, and its throttle
//! authctl try SERVICE [account]    authenticate, as a lock screen or login would
//! authctl reset ACCOUNT            root: clear a throttle
//! authctl unlock-seat              root: let the seat's lock go (phase 2)
//! ```
//!
//! `try` is what any client does, and is throttled and audited like one:
//! it is how a script, or a gate, asks "is this the password".

use std::io::Write as _;
use std::process::ExitCode;

use ferrix_auth_client::{Answer, Connection, State, Terminal, Verdict, converse};
use ferrix_auth_proto::{Record, method};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let connection = match Connection::open() {
        Ok(connection) => connection,
        Err(error) => {
            say(&format!("authctl: cannot reach authd: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let result = match words.as_slice() {
        ["status"] => status(&connection, ""),
        ["status", account] => status(&connection, account),
        ["try", service] => attempt(&connection, service, ""),
        ["try", service, account] => attempt(&connection, service, account),
        ["reset", account] => request(&connection, &Record::Reset { account }, account),
        ["unlock-seat"] => request(&connection, &Record::UnlockSeat, ""),
        _ => {
            say(
                "usage: authctl status [account] | try SERVICE [account] | reset ACCOUNT | unlock-seat",
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            say(&format!("authctl: {error}"));
            ExitCode::FAILURE
        }
    }
}

fn status(connection: &Connection, account: &str) -> std::io::Result<bool> {
    request(connection, &Record::Status { account }, account)
}

fn request(connection: &Connection, record: &Record<'_>, account: &str) -> std::io::Result<bool> {
    match connection.request(record)? {
        Answer::State(state) => {
            say(&describe(account, state));
            Ok(true)
        }
        Answer::Verdict(verdict) => Ok(report(&verdict)),
    }
}

fn describe(account: &str, state: State) -> String {
    let who = if account.is_empty() { "you" } else { account };
    let set = if state.credential && state.methods & method::PASSWORD != 0 {
        "a password is set"
    } else {
        "no password is set"
    };
    if state.throttled_ms > 0 {
        format!(
            "authctl: {who}: {set}; throttled for {} ms",
            state.throttled_ms
        )
    } else {
        format!("authctl: {who}: {set}; not throttled")
    }
}

fn attempt(connection: &Connection, service: &str, account: &str) -> std::io::Result<bool> {
    let verdict = converse(connection, service, account, &mut Terminal::new())?;
    Ok(report(&verdict))
}

fn report(verdict: &Verdict) -> bool {
    match verdict {
        Verdict::Accepted { uid, account } => {
            say(&format!("authctl: accepted {account} ({uid})"));
            true
        }
        Verdict::Failed {
            retry_after_ms,
            text,
        } => {
            say(&format!(
                "authctl: failed: {text} (retry after {retry_after_ms} ms)"
            ));
            false
        }
        Verdict::Unavailable(text) => {
            say(&format!("authctl: unavailable: {text}"));
            false
        }
    }
}

fn say(line: &str) {
    let _ = writeln!(std::io::stdout(), "{line}");
}
