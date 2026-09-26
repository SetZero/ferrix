//! Every run's record under `~/.local/share/ferrix/pixel7`: the phone runs'
//! `run.log`s, the helper's `launcher-*` ones, and the guests this app ran.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// The directory the runs are in.
pub fn root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local/share/ferrix/pixel7")
}

/// One run, as the history lists it.
#[derive(Serialize)]
pub struct Run {
    name: String,
    path: String,
    /// Seconds since the epoch.
    when: f64,
    /// Its `FERRIX-BOOT-OK` or `FERRIX-PANIC` line, if it has one.
    result: Option<String>,
    /// How long it took, as the run's `elapsed.txt` says.
    elapsed: Option<String>,
    /// Whether it was a guest.
    vm: bool,
}

/// The first line of `text` that says how the boot ended.
fn result(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.starts_with("FERRIX-BOOT-OK") || line.starts_with("FERRIX-PANIC"))
        .map(str::to_string)
}

/// Every run, newest first.
pub fn list() -> Vec<Run> {
    let mut runs = Vec::new();
    let Ok(entries) = fs::read_dir(root()) else {
        return runs;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let log = dir.join("run.log");
        let Ok(meta) = fs::metadata(&log) else {
            continue;
        };
        let when = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64());
        let text = fs::read(&log).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        runs.push(Run {
            vm: name.starts_with("vm-"),
            result: result(&text),
            elapsed: fs::read_to_string(dir.join("elapsed.txt")).ok().map(|s| s.trim().to_string()),
            path: log.to_string_lossy().into_owned(),
            name,
            when,
        });
    }
    runs.sort_by(|a, b| b.when.total_cmp(&a.when));
    runs
}

/// A run's record, if `path` is one of them.
pub fn read(path: &str) -> Result<String, String> {
    let path = Path::new(path).canonicalize().map_err(|error| error.to_string())?;
    let root = root().canonicalize().map_err(|error| error.to_string())?;
    if !path.starts_with(&root) {
        return Err("not a run record".into());
    }
    fs::read(&path).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|error| error.to_string())
}

/// `seconds` since the epoch as `YYYYMMDD-HHMMSS`, in UTC, as the helper
/// names its runs (in local time; close enough to sort and to read).
fn stamp(seconds: u64) -> String {
    let (days, of_day) = (seconds / 86_400, seconds % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    )
}

/// Keep a guest's console as a run record, `vm-<time>/run.log`.
pub fn save_vm(console: &str) -> Result<String, String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = root().join(format!("vm-{}", stamp(now)));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let log = dir.join("run.log");
    fs::write(&log, console).map_err(|error| error.to_string())?;
    Ok(log.to_string_lossy().into_owned())
}
