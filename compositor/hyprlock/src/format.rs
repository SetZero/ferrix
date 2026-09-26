//! `IWidget::formatString`: the variables a label or a field's text may
//! hold, and `cmd[…]`.
//!
//! The replacements, in upstream's order: `$DESC` (the GECOS field),
//! `$USER`, `<br/>` (a line break), `$TIME12`, `$TIME`, `$ATTEMPTS` (or
//! `$ATTEMPTS[text shown at zero]`), `$LAYOUT` (or `$LAYOUT[a,b,…]` by
//! group, `!` for nothing), `$FAIL`, `$PAMFAIL`, `$PAMPROMPT`, `$FPRINTFAIL`
//! and `$FPRINTPROMPT`. A text that starts `cmd[update:N]` is a command
//! whose output is the text, run again every N milliseconds.
//!
//! **The time.** Upstream asks the C++ library for the local zone through
//! `TZ` or `/etc/localtime`, and falls back to UTC with a warning when
//! there is none. Ferrix's desktop image has neither, so the clock there is
//! UTC; [`Context::utc_offset`] is the seam a zone would plug into.

/// What the variables stand for at the moment of formatting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// `$USER`.
    pub user: String,
    /// `$DESC`.
    pub gecos: String,
    /// Seconds since the epoch, now.
    pub now: i64,
    /// Seconds east of UTC the clock is shown in.
    pub utc_offset: i64,
    /// `$ATTEMPTS`: failed attempts so far.
    pub attempts: usize,
    /// `$FAIL`: the current failure's text.
    pub fail: String,
    /// `$PAMFAIL`: the password check's last failure, `None` with the check
    /// off.
    pub pam_fail: Option<String>,
    /// `$PAMPROMPT`.
    pub pam_prompt: Option<String>,
    /// `$LAYOUT`: the keyboard layout's name, and which group is in force.
    pub layout: (String, usize),
}

/// A text with its variables replaced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Formatted {
    /// The text: markup, or for a command the command line.
    pub text: String,
    /// How often it must be formatted again, in milliseconds; 0 for never.
    pub update_every_ms: u64,
    /// Whether an authentication event should redraw it at once.
    pub allow_force_update: bool,
    /// Whether it is redrawn even when the text did not change.
    pub always_update: bool,
    /// Whether `text` is a command whose output is what is shown.
    pub cmd: bool,
}

/// Replace every `from` in `text` with `to`, as `replaceInString` does.
fn replace(text: &mut String, from: &str, to: &str) {
    if text.contains(from) {
        *text = text.replace(from, to);
    }
}

/// `$VAR[...]` or `$VAR`: the bracketed text, and where the whole ends.
fn bracketed<'a>(text: &'a str, at: usize, name: &str) -> Option<(&'a str, usize)> {
    let after = at + name.len();
    let rest = text.get(after..)?;
    let inner = rest.strip_prefix('[')?;
    let close = inner.find(']')?;
    Some((inner.get(..close)?, after + 1 + close + 1))
}

/// `replaceAllAttempts`.
fn attempts(text: &mut String, count: usize) {
    let number = count.to_string();
    let mut from = 0;
    while let Some(found) = text.get(from..).and_then(|rest| rest.find("$ATTEMPTS")) {
        let at = from + found;
        let (replacement, end) = match bracketed(text, at, "$ATTEMPTS") {
            Some((zero, end)) => (
                if count == 0 {
                    zero.to_owned()
                } else {
                    number.clone()
                },
                end,
            ),
            None => (number.clone(), at + "$ATTEMPTS".len()),
        };
        text.replace_range(at..end, &replacement);
        from = at + replacement.len();
    }
}

/// `replaceAllLayout`.
fn layout(text: &mut String, name: &str, group: usize) {
    let mut from = 0;
    while let Some(found) = text.get(from..).and_then(|rest| rest.find("$LAYOUT")) {
        let at = from + found;
        let (replacement, end) = match bracketed(text, at, "$LAYOUT") {
            Some((list, end)) => {
                let Some(chosen) = list.split(',').map(str::trim).nth(group) else {
                    // Upstream logs the index out of bounds and leaves the
                    // rest of the text as it is.
                    return;
                };
                let chosen = match chosen {
                    "" => name.to_owned(),
                    "!" => String::new(),
                    other => other.to_owned(),
                };
                (chosen, end)
            }
            None => (name.to_owned(), at + "$LAYOUT".len()),
        };
        text.replace_range(at..end, &replacement);
        from = at + replacement.len();
    }
}

/// The time of day at `now` shifted by `offset` seconds: hours and minutes.
fn clock(now: i64, offset: i64) -> (i64, i64) {
    let seconds = (now + offset).rem_euclid(86_400);
    (seconds / 3600, seconds % 3600 / 60)
}

/// `formatString`.
#[must_use]
pub fn format(input: &str, context: &Context) -> Formatted {
    let mut text = input.to_owned();
    let mut result = Formatted::default();
    replace(&mut text, "$DESC", &context.gecos);
    replace(&mut text, "$USER", &context.user);
    replace(&mut text, "<br/>", "\n");
    let second = |result: &mut Formatted| {
        result.update_every_ms = if result.update_every_ms != 0 && result.update_every_ms < 1000 {
            result.update_every_ms
        } else {
            1000
        };
    };
    let (hours, minutes) = clock(context.now, context.utc_offset);
    if text.contains("$TIME12") {
        let twelve = if hours % 12 == 0 { 12 } else { hours % 12 };
        let half = if hours < 12 { "AM" } else { "PM" };
        replace(
            &mut text,
            "$TIME12",
            &format!("{twelve:02}:{minutes:02} {half}"),
        );
        second(&mut result);
    }
    if text.contains("$TIME") {
        replace(&mut text, "$TIME", &format!("{hours:02}:{minutes:02}"));
        second(&mut result);
    }
    if text.contains("$ATTEMPTS") {
        attempts(&mut text, context.attempts);
        result.allow_force_update = true;
    }
    if text.contains("$LAYOUT") {
        layout(&mut text, &context.layout.0, context.layout.1);
        result.allow_force_update = true;
    }
    if text.contains("$FAIL") {
        replace(&mut text, "$FAIL", &context.fail);
        result.allow_force_update = true;
    }
    for (name, value) in [
        ("$PAMFAIL", &context.pam_fail),
        ("$PAMPROMPT", &context.pam_prompt),
        // No fingerprint reader: fprintd is a D-Bus service, and Ferrix has
        // no D-Bus.
        ("$FPRINTFAIL", &None),
        ("$FPRINTPROMPT", &None),
    ] {
        if text.contains(name) {
            replace(&mut text, name, value.as_deref().unwrap_or(""));
            result.allow_force_update = true;
        }
    }
    if let Some(rest) = text.strip_prefix("cmd[")
        && let Some(close) = rest.find(']')
    {
        for property in rest.get(..close).unwrap_or("").split(',') {
            let property = property.trim();
            if let Some(update) = property.strip_prefix("update:") {
                let (every, force) = match update.split_once(':') {
                    Some((every, force)) => (every, Some(force)),
                    None => (update, None),
                };
                if let Ok(every) = every.trim().parse::<u64>() {
                    result.update_every_ms = every;
                }
                if let Some(force) = force {
                    result.allow_force_update = force == "true" || force.trim() == "1";
                }
            } else if !property.is_empty() {
                crate::say(&format!("Unknown prop in string format {property}"));
            }
        }
        result.always_update = true;
        result.cmd = true;
        text = rest.get(close + 1..).unwrap_or("").to_owned();
    }
    result.text = text;
    result
}

#[cfg(test)]
mod tests {
    use super::{Context, format};

    fn context() -> Context {
        Context {
            user: "ferrix".to_owned(),
            gecos: "The Ferrix User".to_owned(),
            // 2026-09-26 18:07:30 UTC.
            now: 1_790_446_050,
            pam_fail: Some("Authentication failed".to_owned()),
            layout: ("German (no dead keys)".to_owned(), 0),
            ..Context::default()
        }
    }

    #[test]
    fn the_clock_and_its_update() {
        let formatted = format("$TIME", &context());
        assert_eq!(formatted.text, "18:07");
        assert_eq!(formatted.update_every_ms, 1000);
        assert!(!formatted.cmd);
        assert_eq!(format("$TIME12", &context()).text, "06:07 PM");
        let midnight = Context {
            now: 1_790_380_800,
            ..context()
        };
        assert_eq!(format("$TIME12 $TIME", &midnight).text, "12:00 AM 00:00");
    }

    #[test]
    fn a_command_label() {
        let formatted = format("cmd[update:60000] date +\"%A, %-d %B\"", &context());
        assert!(formatted.cmd);
        assert!(formatted.always_update);
        assert_eq!(formatted.update_every_ms, 60_000);
        assert_eq!(formatted.text, " date +\"%A, %-d %B\"");
        let forced = format("cmd[update:1000:true] echo $USER", &context());
        assert!(forced.allow_force_update);
        assert_eq!(forced.text, " echo ferrix");
    }

    #[test]
    fn the_failure_variables() {
        let formatted = format("<span foreground=\"#ff8888\">$PAMFAIL</span>", &context());
        assert_eq!(
            formatted.text,
            "<span foreground=\"#ff8888\">Authentication failed</span>"
        );
        assert!(formatted.allow_force_update);
        assert_eq!(format("$ATTEMPTS[none]", &context()).text, "none");
        let twice = Context {
            attempts: 2,
            ..context()
        };
        assert_eq!(
            format("$ATTEMPTS[none] and $ATTEMPTS", &twice).text,
            "2 and 2"
        );
        assert_eq!(
            format("$USER is $DESC<br/>ok", &context()).text,
            "ferrix is The Ferrix User\nok"
        );
    }

    #[test]
    fn the_layout() {
        assert_eq!(format("$LAYOUT", &context()).text, "German (no dead keys)");
        assert_eq!(format("$LAYOUT[de,us]", &context()).text, "de");
        assert_eq!(format("x$LAYOUT[!,us]y", &context()).text, "xy");
    }
}
