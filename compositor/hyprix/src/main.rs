//! The compositor's command line.
//!
//! Everything it does is in the library beside this, so a test can run the
//! compositor in a thread rather than as a process and compare the pixels it
//! drew.

use std::io::Write;

use hyprix::Options;

fn main() {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            report(&format!("hyprix: {message}"));
            report(Options::USAGE);
            std::process::exit(2);
        }
    };
    match hyprix::run(&options) {
        Ok(line) => report(&line),
        Err(error) => {
            report(&format!("hyprix: failed: {error}"));
            std::process::exit(1);
        }
    }
}

/// Say one line on standard output.
///
/// A compositor's own output is a log, not a client's, so it goes out
/// whatever happens to the screen; the display test reads it.
fn report(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
