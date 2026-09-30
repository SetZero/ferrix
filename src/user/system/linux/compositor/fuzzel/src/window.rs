//! The program: fuzzel's `main` and `wayland.c`, on `src/user/linux/compositor/toolkit`.
//!
//! The command line, then the configuration, then the entries (from the
//! `.desktop` files, or in dmenu mode from standard input), then a
//! `zwlr_layer_surface_v1` on `layer=` with fuzzel's namespace, sized from
//! the font, and the loop: a key goes through `keys::resolve` to the
//! launcher, a redraw is a frame, and the launcher finishing is a program
//! started (or a line printed) and fuzzel's exit status.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use compositor_toolkit::{
    Client, Event, KeyboardEvent, KeyboardInteractivity, LayerOptions, Margin, PointerEvent,
    SurfaceId,
};

use crate::cli::{self, LogLevel};
use crate::config::{self, Config, DpiAware, Env, KeyboardFocus, Layer, Level, ScalingFilter};
use crate::desktop::{self, Locale, Search};
use crate::geometry::{Geometry, Scaling};
use crate::keys::{self, Press};
use crate::launcher::{Finish, Launcher, Outcome};
use crate::paint::{Look, Painter, Pictures};
use crate::{cache, dmenu, exec, icon};

/// fuzzel's log, on standard error, above `level`.
#[derive(Clone, Copy, Debug)]
struct Log {
    level: LogLevel,
}

impl Log {
    fn say(&self, level: LogLevel, text: &str) {
        if level > self.level || self.level == LogLevel::None {
            return;
        }
        let class = match level {
            LogLevel::Error | LogLevel::None => " err",
            LogLevel::Warning => "warn",
            LogLevel::Info => "info",
        };
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{class}: {text}");
    }

    fn error(&self, text: &str) {
        self.say(LogLevel::Error, text);
    }

    fn warn(&self, text: &str) {
        self.say(LogLevel::Warning, text);
    }

    fn info(&self, text: &str) {
        self.say(LogLevel::Info, text);
    }
}

/// `--print-timing-info`: how long a stage took, as fuzzel's `time_finish`
/// says it (a warning, so it shows at the default log level).
struct Timing {
    enabled: bool,
    log: Log,
}

impl Timing {
    fn since(&self, what: &str, started: std::time::Instant) {
        if !self.enabled {
            return;
        }
        let took = started.elapsed();
        self.log.warn(&format!(
            "{what} in {}s {}µs",
            took.as_secs(),
            took.subsec_micros()
        ));
    }
}

/// Print a line on standard output, as fuzzel's `printf` does.
fn print(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// The lines of `config` this program reads and does not carry out, each
/// said by name, as the desktop clients' rules require.
#[must_use]
pub fn unsupported(config: &Config) -> Vec<String> {
    let mut out = Vec::new();
    if config.gamma_correct {
        // fuzzel's own words when the compositor lacks the protocol, which
        // hyprix does.
        out.push(
            "gamma-corrected-blending: disabling; compositor does not implement the \
             color-management protocol"
                .to_owned(),
        );
    }
    if config.scaling_filter != ScalingFilter::Box {
        out.push(
            "scaling-filter: PNG icons are scaled with a bilinear filter here, whichever is named"
                .to_owned(),
        );
    }
    if config.list_executables_in_path {
        out.push(
            "list-executables-in-path: not carried out yet; only .desktop entries are listed"
                .to_owned(),
        );
    }
    if config.message.is_some() && config.message_mode == config::MessageMode::Expand {
        out.push(
            "message-mode=expand: the message is not measured; lines are cut at the width"
                .to_owned(),
        );
    }
    out
}

/// Where fuzzel's single-instance lock is.
fn lock_path() -> Option<PathBuf> {
    let display = std::env::var("WAYLAND_DISPLAY").ok()?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_owned());
    Some(PathBuf::from(format!("{runtime}/fuzzel-{display}.lock")))
}

/// Run fuzzel with `args`, as `program`; answers the exit status.
#[must_use]
pub fn run(program: &str, args: &[String]) -> i32 {
    let cli = match cli::parse(program, args) {
        Ok(cli) => cli,
        Err(message) => {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{message}");
            return 1;
        }
    };
    if cli.version {
        print(&format!("fuzzel {}", cli::version()));
        return 0;
    }
    if cli.help {
        print(&cli::usage(program));
        return 0;
    }
    let log = Log {
        level: cli.log_level,
    };
    let env = Env::from_process();
    let loaded = config::load(
        cli.config.as_deref().map(Path::new),
        &env,
        &cli.overrides,
        cli.check_config,
    );
    for diagnostic in &loaded.diagnostics {
        let level = match diagnostic.level {
            Level::Error => LogLevel::Error,
            Level::Warning => LogLevel::Warning,
            Level::Info => LogLevel::Info,
        };
        log.say(level, &format!("config: {}", diagnostic.text));
    }
    if !loaded.ok {
        return 1;
    }
    if cli.check_config {
        return 0;
    }
    let mut config = loaded.config;
    cli.apply(&mut config);
    for line in unsupported(&config) {
        log.warn(&line);
    }

    // One fuzzel a Wayland session.
    let lock = lock_path().and_then(|path| match std::fs::File::create(&path) {
        Ok(file) => Some((path, file)),
        Err(error) => {
            log.warn(&format!(
                "{}: failed to create lock file: {}",
                path.display(),
                exec::error_text(&error)
            ));
            None
        }
    });
    if let Some((path, file)) = &lock
        && file.try_lock().is_err()
    {
        log.error(&format!(
            "{}: failed to acquire lock: fuzzel already running?",
            path.display()
        ));
        return 1;
    }
    let status = session(&config, &cli, log);
    if let Some((path, _)) = lock {
        let _ = std::fs::remove_file(path);
    }
    status
}

/// The entries: standard input in dmenu mode, else the `.desktop` files,
/// with their launch counts.
fn entries(config: &Config, log: Log) -> (Vec<desktop::Application>, Option<PathBuf>) {
    let var = |name: &str| std::env::var(name).ok();
    let cache_path = match cache::cache_path(
        config.cache.as_deref(),
        config.dmenu.enabled,
        cache::cache_dir(var("XDG_CACHE_HOME").as_deref(), var("HOME").as_deref()),
    ) {
        Ok(path) => path,
        Err(message) => {
            log.error(&message);
            None
        }
    };
    let mut apps = if config.dmenu.enabled {
        let mut input = Vec::new();
        if !config.prompt_only {
            let _ = std::io::stdin().read_to_end(&mut input);
        }
        dmenu::entries(
            &input,
            config.dmenu.delim,
            config.dmenu.with_nth.as_deref(),
            config.dmenu.match_nth.as_deref(),
            config.dmenu.nth_delim,
        )
    } else {
        let search = Search {
            data_dirs: desktop::data_dirs(
                var("XDG_DATA_HOME").as_deref(),
                var("HOME").as_deref(),
                var("XDG_DATA_DIRS").as_deref(),
            ),
            terminal: config.terminal.clone(),
            include_actions: config.show_actions,
            filter_desktop: config.filter_desktop,
            desktops: if config.filter_desktop {
                var("XDG_CURRENT_DESKTOP")
                    .map(|d| {
                        d.split(':')
                            .filter(|d| !d.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            locale: Locale::parse(
                &var("LC_ALL")
                    .or_else(|| var("LC_MESSAGES"))
                    .or_else(|| var("LANG"))
                    .unwrap_or_else(|| "C".to_owned()),
            ),
            path: var("PATH"),
        };
        let mut found = desktop::find_programs(&search);
        if found.is_empty() {
            log.warn("No applications found. See SEARCH PATHS in `man fuzzel` for details.");
        }
        if config.list_executables_in_path
            && let Some(path) = var("PATH")
        {
            let (programs, warnings) = desktop::path_programs(&path);
            for warning in warnings {
                log.warn(&warning);
            }
            found.extend(programs);
        }
        found
    };
    if let Some(path) = &cache_path
        && let Ok(text) = std::fs::read_to_string(path)
    {
        for complaint in cache::read(&text, &mut apps, config.dmenu.enabled) {
            log.error(&complaint);
        }
    }
    (apps, cache_path)
}

/// How fonts are sized on `client`'s screens: fuzzel's `update_size`.
fn scaling(client: &Client, config: &Config) -> Scaling {
    let outputs = client.outputs();
    let chosen = config
        .output
        .as_ref()
        .and_then(|name| {
            outputs
                .iter()
                .find(|o| &o.name == name || &o.xdg_name == name)
        })
        .or_else(|| outputs.first())
        .copied();
    let scale = chosen.map_or(1, |o| o.scale.max(1)) as f32;
    let by_dpi = match config.dpi_aware {
        DpiAware::Yes => true,
        DpiAware::No => false,
        DpiAware::Auto => outputs.iter().all(|o| o.scale <= 1),
    };
    // fuzzel's DPI is the diagonal's, in logical pixels, times the scale;
    // an output with no physical size is 96.
    let dpi = chosen
        .and_then(|o| {
            let (mm_w, mm_h) = o.physical_mm;
            if mm_w <= 0 || mm_h <= 0 {
                return None;
            }
            let (w, h) = o.logical_size();
            let px = f64::from(w).hypot(f64::from(h));
            let inches = f64::from(mm_w).hypot(f64::from(mm_h)) / 25.4;
            Some((px / inches * f64::from(o.scale.max(1))) as f32)
        })
        .unwrap_or(96.0);
    Scaling { scale, dpi, by_dpi }
}

/// Everything after the configuration: the entries, the window and the loop.
fn session(config: &Config, cli: &cli::Cli, log: Log) -> i32 {
    let timing = Timing {
        enabled: config.print_timing_info,
        log,
    };
    let started = std::time::Instant::now();
    let (apps, cache_path) = entries(config, log);
    timing.since("apps loaded", started);
    let default_status = if config.dmenu.enabled { 1 } else { 0 };
    if config.dmenu.exit_immediately_if_empty && apps.is_empty() {
        return default_status;
    }
    let mut launcher = Launcher::new(config, apps);
    if let Some(index) = cli.select_index.filter(|index| *index != 0) {
        if !launcher.matches.select(index) {
            log.error(&format!("couldn't select entry at index {index}"));
            return default_status;
        }
    } else if let Some(select) = &cli.select {
        let _ = launcher.matches.select_containing(&launcher.apps, select);
    }

    let started = std::time::Instant::now();
    let mut client = match Client::connect() {
        Ok(client) => client,
        Err(error) => {
            log.error(&format!(
                "failed to connect to wayland; no compositor running? ({error})"
            ));
            return 1;
        }
    };
    let scaling = scaling(&client, config);
    timing.since("connected to the compositor", started);
    let started = std::time::Instant::now();
    let fonts = compositor_text::Fonts::system();
    timing.since("fonts found", started);
    let started = std::time::Instant::now();
    let look = Look::new(fonts, config, scaling);
    timing.since("font loaded", started);
    let started = std::time::Instant::now();
    let (lines, longest) = config.message.as_ref().map_or((0, 0), |m| {
        let lines: Vec<&str> = m.split('\n').collect();
        let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        (
            i32::try_from(lines.len()).unwrap_or(0),
            i32::try_from(longest).unwrap_or(0),
        )
    });
    let longest = if config.message_mode == config::MessageMode::Expand {
        longest
    } else {
        0
    };
    let geometry = Geometry::new(config, &look.metrics, scaling, lines, longest);

    // The icons, at the row's size.
    let var = |name: &str| std::env::var(name).ok();
    let icons = if config.icons_enabled {
        let data_dirs = desktop::data_dirs(
            var("XDG_DATA_HOME").as_deref(),
            var("HOME").as_deref(),
            var("XDG_DATA_DIRS").as_deref(),
        );
        let dirs = icon::icon_dirs(&data_dirs, var("HOME").as_deref());
        let themes = icon::load_themes(&config.icon_theme, &dirs, !config.dmenu.enabled);
        match themes.first() {
            Some(theme) => log.info(&format!("theme: {}", theme.name)),
            None => log.warn(&format!("{}: icon theme not found", config.icon_theme)),
        }
        let names: Vec<Option<&str>> = launcher
            .apps
            .iter()
            .map(|a| a.icon_name.as_deref())
            .collect();
        icon::lookup(&themes, &dirs, geometry.icon_size, &names)
    } else {
        vec![None; launcher.apps.len()]
    };
    timing.since("icon paths resolved", started);
    let have_icons = launcher.apps.iter().any(|a| a.icon_name.is_some());
    let painter = Painter {
        look,
        geometry,
        icons,
        have_icons,
        pictures: Pictures::default(),
        input_offset: 0,
    };

    let output = config.output.as_ref().and_then(|name| {
        client
            .outputs()
            .into_iter()
            .find(|o| &o.name == name || &o.xdg_name == name)
            .and_then(|o| o.id)
    });
    let options = LayerOptions {
        output,
        layer: match config.layer {
            Layer::Top => compositor_toolkit::Layer::Top,
            Layer::Overlay => compositor_toolkit::Layer::Overlay,
        },
        namespace: config.namespace.clone(),
        size: geometry.logical_size(scaling.scale),
        anchor: compositor_toolkit::Anchor(config.anchor.0),
        exclusive_zone: 0,
        margin: Margin {
            top: i32::try_from(config.y_margin).unwrap_or(0),
            right: i32::try_from(config.x_margin).unwrap_or(0),
            bottom: i32::try_from(config.y_margin).unwrap_or(0),
            left: i32::try_from(config.x_margin).unwrap_or(0),
        },
        keyboard: match config.keyboard_focus {
            KeyboardFocus::Exclusive => KeyboardInteractivity::Exclusive,
            KeyboardFocus::OnDemand => KeyboardInteractivity::OnDemand,
        },
    };
    let surface = match client.layer_surface(&options) {
        Ok(surface) => surface,
        Err(error) => {
            log.error(&format!("failed to create layer shell surface: {error}"));
            return 1;
        }
    };
    // Said once the window exists, so a boot can wait for it.
    log.info(&format!(
        "fuzzel: {} entries, window {}x{} on layer {:?}, namespace {}",
        launcher.apps.len(),
        geometry.width,
        geometry.height,
        config.layer,
        config.namespace
    ));

    let mut state = Loop {
        client,
        surface,
        painter,
        launcher,
        pointer: None,
        configured: false,
        log,
    };
    let finish = state.run(config);
    let Loop { launcher, .. } = state;
    let (status, update_cache) = match finish {
        Some((status, update)) => (status, update),
        None => (default_status, false),
    };
    if (update_cache || launcher.force_cache_update)
        && let Some(path) = cache_path
    {
        let text = cache::write(&launcher.apps, config.dmenu.enabled);
        if let Err(message) = cache::save(&path, &text) {
            log.error(&message);
        }
    }
    status
}

/// The running window.
struct Loop {
    client: Client,
    surface: SurfaceId,
    painter: Painter,
    launcher: Launcher,
    /// Where the pointer is on the surface, in buffer pixels.
    pointer: Option<(i32, i32)>,
    configured: bool,
    log: Log,
}

impl Loop {
    fn redraw(&mut self, config: &Config) {
        if !self.configured {
            return;
        }
        let painter = &mut self.painter;
        let launcher = &self.launcher;
        if let Err(error) = self.client.draw(self.surface, |pixmap| {
            painter.draw(pixmap, launcher, config);
        }) {
            self.log.error(&format!("failed to draw: {error}"));
        }
    }

    /// Turn the loop until the launcher finishes; answers the exit status
    /// and whether the cache is to be written, or `None` for the window
    /// going away.
    fn run(&mut self, config: &Config) -> Option<(i32, bool)> {
        loop {
            let events = match self.client.dispatch(None) {
                Ok(events) => events,
                Err(error) => {
                    self.log.error(&format!("wayland: {error}"));
                    return None;
                }
            };
            let mut redraw = false;
            for event in events {
                let outcome = match event {
                    Event::Configure { surface, .. } if surface == self.surface => {
                        self.configured = true;
                        Outcome {
                            redraw: true,
                            finish: None,
                        }
                    }
                    Event::Closed(surface) if surface == self.surface => return None,
                    Event::Keyboard(KeyboardEvent::Key(key)) if key.pressed => {
                        self.key(&key, config)
                    }
                    Event::Keyboard(KeyboardEvent::Leave(surface))
                        if surface == self.surface && config.exit_on_keyboard_focus_loss =>
                    {
                        self.launcher.focus_lost()
                    }
                    Event::Pointer(pointer) if config.enable_mouse => self.pointer(pointer),
                    _ => Outcome::default(),
                };
                redraw |= outcome.redraw;
                if let Some(finish) = outcome.finish {
                    return Some(self.finish(finish, config));
                }
            }
            if redraw {
                self.redraw(config);
            }
        }
    }

    /// A key pressed (or repeating): a binding's action, or text.
    fn key(&mut self, key: &compositor_toolkit::Key, config: &Config) -> Outcome {
        let press = Press {
            keysym: key.keysym,
            plain: key.plain,
            mods: key.modifiers.mask,
            consumed: key.consumed,
            text: &key.text,
        };
        match keys::resolve(&config.bindings, &press) {
            keys::Outcome::Action(action) => {
                if matches!(
                    action,
                    config::Action::ClipboardPaste | config::Action::PrimaryPaste
                ) {
                    self.log.warn(&format!(
                        "{}: pasting is not carried out yet",
                        action.name()
                    ));
                }
                self.launcher.action(action)
            }
            keys::Outcome::Text(text) => self.launcher.type_text(&text),
            keys::Outcome::Nothing => Outcome::default(),
        }
    }

    /// A pointer event: hovering selects, a left click executes, a right
    /// click cancels, the wheel moves the selection.
    fn pointer(&mut self, event: PointerEvent) -> Outcome {
        let scale = self.client.scale(self.surface) as f64;
        let rows = i32::try_from(self.launcher.matches.on_page().len()).unwrap_or(0);
        let geometry = self.painter.geometry;
        match event {
            PointerEvent::Enter { surface, x, y } | PointerEvent::Motion { surface, x, y }
                if surface == self.surface =>
            {
                let at = ((x * scale).round() as i32, (y * scale).round() as i32);
                let moved = self.pointer.is_some();
                self.pointer = Some(at);
                let row = geometry.row_at(at.0, at.1, rows);
                match row {
                    // fuzzel ignores the first position it is told, so a
                    // window that opens under the pointer does not move the
                    // selection.
                    Some(row) if moved => Outcome {
                        redraw: self.launcher.hover(usize::try_from(row).unwrap_or(0)),
                        finish: None,
                    },
                    _ => Outcome::default(),
                }
            }
            PointerEvent::Leave { .. } => {
                self.pointer = None;
                Outcome::default()
            }
            // BTN_LEFT and BTN_RIGHT, on release.
            PointerEvent::Button {
                surface,
                x,
                y,
                button,
                pressed: false,
                ..
            } if surface == self.surface => {
                let at = ((x * scale).round() as i32, (y * scale).round() as i32);
                match button {
                    0x110 => match geometry.row_at(at.0, at.1, rows) {
                        Some(row) => self.launcher.click(usize::try_from(row).unwrap_or(0)),
                        None => Outcome::default(),
                    },
                    0x111 => self.launcher.right_click(),
                    _ => Outcome::default(),
                }
            }
            PointerEvent::Axis {
                surface, vertical, ..
            } if surface == self.surface && vertical != 0.0 => {
                let action = if vertical > 0.0 {
                    config::Action::Next
                } else {
                    config::Action::Prev
                };
                self.launcher.action(action)
            }
            _ => Outcome::default(),
        }
    }

    /// Do what finishing asks: start the entry, or print it in dmenu mode.
    fn finish(&mut self, finish: Finish, config: &Config) -> (i32, bool) {
        match finish {
            Finish::Cancel(status) => (status, false),
            Finish::Execute { app, code } => {
                let chosen = app.and_then(|at| self.launcher.apps.get(at));
                let typed = self.launcher.prompt.text_string();
                if config.dmenu.enabled {
                    print(&dmenu::output(
                        chosen,
                        &typed,
                        config.dmenu.mode == config::DmenuMode::Index,
                        config.dmenu.accept_nth.as_deref(),
                        config.dmenu.nth_delim,
                    ));
                    self.launcher.started(app);
                    return (code, true);
                }
                let plan = match exec::plan(chosen, &typed, config.launch_prefix.as_deref()) {
                    Ok(plan) => plan,
                    Err(refused) => {
                        self.log.error(&refused.to_string());
                        return (1, false);
                    }
                };
                self.log.info(&format!(
                    "executing {}: \"{}\"",
                    chosen
                        .and_then(|a| a.id.clone())
                        .unwrap_or_else(|| "(null)".to_owned()),
                    plan.line
                ));
                let mut warnings = Vec::new();
                let started = exec::start(&plan, &mut warnings);
                for warning in warnings {
                    self.log.error(&warning);
                }
                match started {
                    Ok(()) => {
                        self.launcher.started(app);
                        (code, true)
                    }
                    Err(message) => {
                        self.log.error(&message);
                        (1, false)
                    }
                }
            }
        }
    }
}
