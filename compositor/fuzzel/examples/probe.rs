//! `cargo run -p compositor-fuzzel --example probe -- [QUERY...]`: what this
//! fuzzel makes of the machine it runs on.
//!
//! The host-side probe the desktop-clients rules ask for: it reads the
//! user's real `~/.config/fuzzel/fuzzel.ini` and the real `.desktop` files
//! and icon themes, and prints what the launcher would -- the diagnostics,
//! the window's size for a stand-in font, the first page of entries with the
//! icon each would draw, and the ranking of each query given. Nothing of the
//! user's files is committed; this reads them where they are.

use std::io::Write as _;

use compositor_fuzzel::config::{self, Env};
use compositor_fuzzel::desktop::{self, Locale, Search};
use compositor_fuzzel::geometry::{FontMetrics, Geometry, Scaling};
use compositor_fuzzel::icon;
use compositor_fuzzel::launcher::Launcher;

fn main() {
    let queries: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout().lock();
    let env = Env::from_process();
    let loaded = config::load(None, &env, &[], false);
    let _ = writeln!(out, "config: {:?}", loaded.path);
    for diagnostic in &loaded.diagnostics {
        let _ = writeln!(out, "  {diagnostic}");
    }
    let c = &loaded.config;
    let _ = writeln!(
        out,
        "font {:?}, {} chars x {} lines, namespace {:?}, layer {:?}, anchor {:?}, icons {} (theme {:?}), terminal {:?}",
        c.font,
        c.chars,
        c.lines,
        c.namespace,
        c.layer,
        c.anchor,
        c.icons_enabled,
        c.icon_theme,
        c.terminal
    );
    // A stand-in for GFS Didot 16pt at 96 DPI; the window measures the real
    // face.
    let font = FontMetrics {
        ascent: 17,
        descent: 6,
        height: 26,
        o_advance: 11,
        space_advance: 5,
        underline_thickness: 1,
    };
    let geometry = Geometry::new(c, &font, Scaling::PLAIN, 0, 0);
    let _ = writeln!(
        out,
        "window {}x{}, row {}, icon {}",
        geometry.width, geometry.height, geometry.row_height, geometry.icon_size
    );

    let var = |name: &str| std::env::var(name).ok();
    let data_dirs = desktop::data_dirs(
        var("XDG_DATA_HOME").as_deref(),
        var("HOME").as_deref(),
        var("XDG_DATA_DIRS").as_deref(),
    );
    let search = Search {
        data_dirs: data_dirs.clone(),
        terminal: c.terminal.clone(),
        include_actions: c.show_actions,
        filter_desktop: c.filter_desktop,
        desktops: var("XDG_CURRENT_DESKTOP")
            .map(|d| d.split(':').map(str::to_owned).collect())
            .unwrap_or_default(),
        locale: Locale::parse(
            &var("LC_ALL")
                .or_else(|| var("LC_MESSAGES"))
                .or_else(|| var("LANG"))
                .unwrap_or_else(|| "C".to_owned()),
        ),
        path: var("PATH"),
    };
    let apps = desktop::find_programs(&search);
    let visible = apps.iter().filter(|a| a.visible).count();
    let _ = writeln!(out, "{} entries, {visible} shown", apps.len());

    let dirs = icon::icon_dirs(&data_dirs, var("HOME").as_deref());
    let themes = icon::load_themes(&c.icon_theme, &dirs, true);
    let names: Vec<Option<&str>> = apps.iter().map(|a| a.icon_name.as_deref()).collect();
    let icons = icon::lookup(&themes, &dirs, geometry.icon_size, &names);
    let theme_names: Vec<&str> = themes.iter().map(|t| t.name.as_str()).collect();
    let _ = writeln!(out, "icon themes: {theme_names:?}");

    let launcher = Launcher::new(c, apps);
    let show = |out: &mut std::io::StdoutLock<'_>, launcher: &Launcher| {
        for m in launcher.matches.on_page() {
            let Some(app) = launcher.apps.get(m.app) else {
                continue;
            };
            let icon = icons
                .get(m.app)
                .cloned()
                .flatten()
                .map_or_else(|| "-".to_owned(), |f| f.path.display().to_string());
            let _ = writeln!(out, "  {:<40} {icon}", app.title_string());
        }
    };
    let _ = writeln!(out, "first page:");
    show(&mut out, &launcher);
    for query in queries {
        let mut l = launcher.clone();
        let _ = l.type_text(&query);
        let _ = writeln!(out, "{query:?}: {} matches", l.matches.list.len());
        show(&mut out, &l);
    }
}
