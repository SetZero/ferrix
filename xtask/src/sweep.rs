//! A measurement, not a test: run a script's snippets as programs and report
//! every system call the kernel answered `ENOSYS`, by the step that asked.
//!
//! # The script
//!
//! Lines before the first `### NAME` are a prelude, put in front of every
//! snippet. Each `### NAME` line starts a snippet, which runs to the next one.
//! The kernel starts each snippet as `sh -c PRELUDE+BODY`, in order and over
//! one tmpfs, through `kernel/src/init.rs`'s command list: the same path
//! `test-vfs` uses, and the one that brackets each command in the log and
//! reports the calls it was refused.
//!
//! # Steps
//!
//! A snippet may run several programs. A line `rc NAME STATUS`, printed after
//! each, closes a step, and the refusals reported since the previous step are
//! charged to `NAME`. That is sound because the kernel prints a refusal as it
//! answers it, and the program prints the line only after its step has ended.
//! Refusals after a snippet's last step are charged to the snippet.
//!
//! # What this cannot see
//!
//! `init.rs` reports a bounded number of refusals per command, so a snippet
//! refused more often than that loses the rest. Keep snippets short.

use std::fmt::Write as _;

use crate::vfs::{self, Ending, Marker};
use crate::{Error, Result};

/// What starts a snippet's line in the script.
const SNIPPET: &str = "### ";
/// What starts the line closing a step, in a program's output.
const STEP: &str = "rc ";
/// The kernel's report of a refused call, from its indent to the name.
const UNANSWERED: &str = "  syscall  ";
/// How that report ends.
const ENOSYS: &str = " answered ENOSYS";

/// One snippet: its name and the whole script `sh -c` is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snippet {
    /// The name after `### `.
    pub(crate) name: String,
    /// The prelude and the body.
    pub(crate) script: String,
}

/// Split a script into its snippets, the prelude put in front of each.
pub(crate) fn parse(text: &str) -> Result<Vec<Snippet>> {
    let mut prelude = String::new();
    let mut named: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix(SNIPPET) {
            let name = name.trim();
            if name.is_empty() {
                return Err(Error::new("a `### ` line names no snippet"));
            }
            named.push((name.to_owned(), String::new()));
            continue;
        }
        let body = named.last_mut().map_or(&mut prelude, |(_, body)| body);
        body.push_str(line);
        body.push('\n');
    }
    if named.is_empty() {
        return Err(Error::new("the script has no `### NAME` snippets"));
    }
    named
        .into_iter()
        .map(|(name, body)| {
            // An empty argument ends a command in the list, so an empty
            // script would silently merge two commands.
            if body.trim().is_empty() {
                return Err(Error::new(format!("snippet `{name}` is empty")));
            }
            let script = format!("{prelude}{body}");
            if script.contains('\0') {
                return Err(Error::new(format!("snippet `{name}` holds a NUL")));
            }
            Ok(Snippet { name, script })
        })
        .collect()
}

/// The command list `kernel/build.rs` embeds: `sh -c SCRIPT` per snippet.
pub(crate) fn encode(snippets: &[Snippet]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for snippet in snippets {
        for arg in ["sh", "-c", snippet.script.as_str()] {
            bytes.extend_from_slice(arg.as_bytes());
            bytes.push(0);
        }
        bytes.push(0);
    }
    bytes
}

/// What a log said, gathered.
#[derive(Debug, Default)]
struct Findings {
    /// Each refused call, in order of first report, with every step that asked.
    calls: Vec<(String, Vec<String>)>,
    /// Steps that closed with a status other than zero.
    steps: Vec<(String, i32)>,
    /// Snippets that ended other than with status zero, and how.
    snippets: Vec<(String, String)>,
    /// How many snippets ended at all.
    ended: usize,
    /// The snippet running when the log stopped, if one was.
    running: Option<String>,
    /// Refusals not yet charged to a step.
    pending: Vec<String>,
}

impl Findings {
    /// Charge every pending refusal to `asker`.
    fn charge(&mut self, asker: &str) {
        for call in std::mem::take(&mut self.pending) {
            if !self.calls.iter().any(|(known, _)| *known == call) {
                self.calls.push((call.clone(), Vec::new()));
            }
            let askers = self
                .calls
                .iter_mut()
                .find(|(known, _)| *known == call)
                .map(|(_, askers)| askers);
            if let Some(askers) = askers.filter(|askers| !askers.iter().any(|a| a == asker)) {
                askers.push(asker.to_owned());
            }
        }
    }

    /// Take one line a snippet's program or the kernel printed while it ran.
    fn take(&mut self, line: &str) {
        if let Some((_, report)) = line.split_once(UNANSWERED) {
            let call = report.strip_suffix(ENOSYS).unwrap_or(report);
            // "number N, in no table, answered ENOSYS" keeps its comma.
            self.pending
                .push(call.trim().trim_end_matches(',').to_owned());
            return;
        }
        let Some((step, status)) = line
            .strip_prefix(STEP)
            .and_then(|rest| rest.rsplit_once(' '))
        else {
            return;
        };
        let Ok(status) = status.parse::<i32>() else {
            return;
        };
        self.charge(step);
        if status != 0 {
            self.steps.push((step.to_owned(), status));
        }
    }
}

/// Read the log of a sweep and say what it found.
pub(crate) fn report(snippets: &[Snippet], lines: &[String]) -> String {
    let name = |index: usize| {
        snippets.get(index).map_or_else(
            || format!("command {index}"),
            |snippet| snippet.name.clone(),
        )
    };
    let mut found = Findings::default();
    let mut current = None;
    for line in lines {
        let line = line.trim_end();
        match vfs::marker(line) {
            Some((index, Marker::Started)) => {
                found.pending.clear();
                current = Some(index);
            }
            Some((index, Marker::Ended(ending))) => {
                let snippet = name(index);
                found.charge(&format!("{snippet} (outside a step)"));
                found.ended += 1;
                match ending {
                    Ending::Exited(0) => {}
                    Ending::Exited(status) => found
                        .snippets
                        .push((snippet, format!("exited with {status}"))),
                    Ending::NotStarted(why) => {
                        found
                            .snippets
                            .push((snippet, format!("not started: {why}")));
                    }
                }
                current = None;
            }
            None if current.is_some() => found.take(line),
            None => {}
        }
    }
    found.running = current.map(name);
    render(snippets.len(), &found)
}

/// The findings as text.
fn render(total: usize, found: &Findings) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} of {total} snippets ended", found.ended);
    if let Some(running) = &found.running {
        let _ = writeln!(out, "still running when the log ended: {running}");
    }
    let _ = writeln!(
        out,
        "\n{} calls answered ENOSYS, in order of first report:",
        found.calls.len()
    );
    for (call, askers) in &found.calls {
        let _ = writeln!(out, "  {call}\n      {}", askers.join(", "));
    }
    let _ = writeln!(out, "\n{} snippets did not exit 0:", found.snippets.len());
    for (snippet, how) in &found.snippets {
        let _ = writeln!(out, "  {snippet}: {how}");
    }
    let _ = writeln!(out, "\n{} steps did not exit 0:", found.steps.len());
    for (step, status) in &found.steps {
        let _ = writeln!(out, "  {step}: {status}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn a_script_splits_into_snippets_each_with_the_prelude() {
        let snippets = parse("p() { :; }\n### one\necho 1\n### two\necho 2\n").unwrap();
        assert_eq!(snippets.len(), 2);
        assert_eq!(snippets[0].name, "one");
        assert_eq!(snippets[1].script, "p() { :; }\necho 2\n");
        assert_eq!(
            encode(&snippets[..1]),
            b"sh\0-c\0p() { :; }\necho 1\n\0\0".to_vec()
        );
    }

    #[test]
    fn a_script_without_snippets_or_with_an_empty_one_is_refused() {
        assert!(parse("echo nothing named\n").is_err());
        assert!(parse("### empty\n\n### full\ntrue\n").is_err());
    }

    #[test]
    fn refusals_are_charged_to_the_step_that_closes_after_them() {
        let snippets = parse("### net\nping\n### tail\ntrue\n").unwrap();
        let log = lines(&[
            "  init     command 0: sh -c <99 bytes>",
            "  syscall  Setitimer (number 38) answered ENOSYS",
            "rc ping 1",
            "  syscall  Setitimer (number 38) answered ENOSYS",
            "rc top 0",
            "partial  syscall  number 999, in no table, answered ENOSYS",
            "  init     command 0 exited with 139",
            "  init     command 1: sh -c <99 bytes>",
        ]);
        let text = report(&snippets, &log);
        assert!(text.contains("1 of 2 snippets ended"), "{text}");
        assert!(
            text.contains("still running when the log ended: tail"),
            "{text}"
        );
        assert!(
            text.contains("  Setitimer (number 38)\n      ping, top\n"),
            "{text}"
        );
        assert!(
            text.contains("  number 999, in no table\n      net (outside a step)\n"),
            "{text}"
        );
        assert!(text.contains("  net: exited with 139"), "{text}");
        assert!(text.contains("  ping: 1"), "{text}");
    }
}
