//! Fuzz the compositor's `hyprland.conf` parser.
//!
//! The configuration is a file any user writes, read at startup and again on
//! every reload; `hyprctl keyword` feeds the same code a line at a time over
//! a socket. The parser promises to answer every input with a configuration
//! and diagnostics and never to stop.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **Diagnostics point into the file**: every one names the file parsed
//!    or one `source` resolved, at a line that file has.
//! 2. **Parsing is a function**: the same bytes parse to the same result.
//! 3. **A file with no diagnostics replays as keywords**: applying each of its
//!    top-level `key = value` lines through `Config::keyword` in order gives
//!    the same configuration, which is what `hyprctl keyword` relies on.
//! 4. **`source` ends**: a file that sources itself, however many times,
//!    stops within the depth and file limits.

#![no_main]

use std::collections::BTreeMap;

use compositor_config::{Config, SourceFile, Sources, parse};
use libfuzzer_sys::fuzz_target;

/// Every `source` resolves to the fuzzed file itself, under a second name.
struct Loop<'a> {
    text: &'a str,
    seen: BTreeMap<String, usize>,
}

impl Sources for Loop<'_> {
    fn resolve(&mut self, spec: &str, _from: &str) -> Result<Vec<SourceFile>, String> {
        *self.seen.entry(spec.to_owned()).or_default() += 1;
        Ok(vec![SourceFile {
            name: "sourced.conf".to_owned(),
            text: self.text.to_owned(),
        }])
    }
}

fuzz_target!(|bytes: &[u8]| {
    let Ok(text) = core::str::from_utf8(bytes) else {
        return;
    };
    if text.len() > 4096 {
        return;
    }

    let lines = text.lines().count();
    let mut sources = Loop {
        text,
        seen: BTreeMap::new(),
    };
    let parsed = parse("hyprland.conf", text, &mut sources);

    // 1.
    for diagnostic in &parsed.diagnostics {
        assert!(
            diagnostic.file == "hyprland.conf" || diagnostic.file == "sourced.conf",
            "a diagnostic names {}",
            diagnostic.file
        );
        assert!(
            (1..=lines).contains(&diagnostic.line),
            "line {} of a {lines}-line file",
            diagnostic.line
        );
    }

    // 2.
    let again = parse(
        "hyprland.conf",
        text,
        &mut Loop {
            text,
            seen: BTreeMap::new(),
        },
    );
    assert_eq!(again, parsed, "the same bytes parsed differently");

    // 4. The parser stops asking at 256 files; the limit check comes before
    // the request, so one more request is never made.
    let resolved: usize = sources.seen.values().sum();
    assert!(resolved <= 256, "{resolved} files sourced");

    // 3. Only for files the replay can express: top level, no categories, no
    // variables, no source or submap, all of which are the file parser's.
    if parsed.diagnostics.is_empty()
        && !text.contains(['{', '}', '$', '#'])
        && !text.contains("source")
        && !text.contains("submap")
    {
        let mut replayed = Config::default();
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                assert_eq!(replayed.keyword(key, value), Ok(()), "{line:?} replayed");
            }
        }
        assert_eq!(replayed, parsed.config, "the keyword replay differs");
    }
});
