//! How often each entry was started: fuzzel's `read_cache` and
//! `write_cache`.
//!
//! One line an entry, `<desktop-file ID>|<count>` (in dmenu mode, the
//! entry's text instead of an ID), in `cache=` or `$XDG_CACHE_HOME/fuzzel`.
//! The count is what orders the list before anything is typed and breaks
//! ties after. dmenu mode keeps no cache unless `cache=` names one, so a
//! script's menu does not disturb the launcher's.

use std::io::Write as _;
use std::path::PathBuf;

use crate::desktop::Application;

/// `$XDG_CACHE_HOME`, else `~/.cache`.
#[must_use]
pub fn cache_dir(xdg_cache_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    match (
        xdg_cache_home.filter(|s| !s.is_empty()),
        home.filter(|s| !s.is_empty()),
    ) {
        (Some(dir), _) => Some(PathBuf::from(dir)),
        (None, Some(home)) => Some(PathBuf::from(format!("{home}/.cache"))),
        (None, None) => None,
    }
}

/// Where the cache is, or why there is none; `Ok(None)` is dmenu mode with
/// no `cache=`, which keeps none and says nothing.
///
/// # Errors
///
/// fuzzel's complaint when the cache directory cannot be opened.
pub fn cache_path(
    explicit: Option<&str>,
    dmenu: bool,
    dir: Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    if let Some(path) = explicit {
        return Ok(Some(PathBuf::from(path)));
    }
    if dmenu {
        return Ok(None);
    }
    let Some(dir) = dir else {
        return Err("failed to get cache directory: not saving popularity cache".to_owned());
    };
    if !dir.is_dir() {
        return Err(format!(
            "{}: failed to open: No such file or directory (2)",
            dir.display()
        ));
    }
    Ok(Some(dir.join("fuzzel")))
}

/// Read the counts into `apps`, matching by ID (or by title in dmenu mode).
/// Returns the lines fuzzel would complain about.
pub fn read(text: &str, apps: &mut [Application], dmenu: bool) -> Vec<String> {
    let mut complaints = Vec::new();
    let mut entries: Vec<(String, u32)> = Vec::new();
    for line in text.lines() {
        // `strtok_r(line, "|")` twice: empty fields are skipped.
        let mut fields = line.split('|').filter(|f| !f.is_empty());
        let (Some(id), Some(count)) = (fields.next(), fields.next()) else {
            complaints.push(format!("invalid cache entry (cache corrupt?): {line}"));
            continue;
        };
        let digits: String = count.chars().take_while(char::is_ascii_digit).collect();
        entries.push((id.to_owned(), digits.parse().unwrap_or(0)));
    }
    for app in apps.iter_mut() {
        let key = if dmenu {
            Some(app.title_string())
        } else {
            app.id.clone()
        };
        let Some(key) = key else { continue };
        if let Some(at) = entries.iter().position(|(id, _)| *id == key) {
            let (_, count) = entries.remove(at);
            app.count = count;
        }
    }
    complaints
}

/// The cache's text for `apps`: every visible entry started at least once.
#[must_use]
pub fn write(apps: &[Application], dmenu: bool) -> String {
    let mut out = String::new();
    for app in apps {
        if app.count == 0 || !app.visible {
            continue;
        }
        let key = if dmenu {
            Some(app.title_string())
        } else {
            app.id.clone()
        };
        if let Some(key) = key {
            out.push_str(&format!("{key}|{}\n", app.count));
        }
    }
    out
}

/// Write `text` to `path`.
///
/// # Errors
///
/// fuzzel's complaint when it cannot.
pub fn save(path: &std::path::Path, text: &str) -> Result<(), String> {
    let mut file = std::fs::File::create(path)
        .map_err(|error| format!("{}: failed to open: {error}", path.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|error| format!("failed to write cache: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{cache_dir, cache_path, read, write};
    use crate::desktop::Application;

    fn app(id: &str) -> Application {
        Application {
            id: Some(id.to_owned()),
            title: id.chars().collect(),
            visible: true,
            ..Application::default()
        }
    }

    #[test]
    fn round_trip() {
        let mut apps = vec![app("a.desktop"), app("b.desktop"), app("c.desktop")];
        let complaints = read("b.desktop|4\nbroken\nc.desktop|x\n|\n", &mut apps, false);
        assert_eq!(
            complaints,
            vec![
                "invalid cache entry (cache corrupt?): broken",
                "invalid cache entry (cache corrupt?): |"
            ]
        );
        let counts: Vec<u32> = apps.iter().map(|a| a.count).collect();
        assert_eq!(counts, vec![0, 4, 0]);
        if let Some(a) = apps.first_mut() {
            a.count = 1;
        }
        assert_eq!(write(&apps, false), "a.desktop|1\nb.desktop|4\n");
    }

    #[test]
    fn where_it_is() {
        assert_eq!(
            cache_dir(None, Some("/home/u")).map(|p| p.display().to_string()),
            Some("/home/u/.cache".to_owned())
        );
        assert_eq!(cache_path(None, true, None), Ok(None));
        assert_eq!(
            cache_path(Some("/dev/null"), true, None)
                .ok()
                .flatten()
                .map(|p| p.display().to_string()),
            Some("/dev/null".to_owned())
        );
        assert_eq!(
            cache_path(None, false, Some("/nonexistent-cache".into())),
            Err("/nonexistent-cache: failed to open: No such file or directory (2)".to_owned())
        );
    }
}
