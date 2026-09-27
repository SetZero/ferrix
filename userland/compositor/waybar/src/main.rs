//! `waybar`: `-c config`, `-s style`, `-l level`, `-b bar`, `-v`, `-h`, as
//! upstream's `Client::main` takes them; then the bar, for as long as the
//! compositor is there.

use std::io::Write;
use std::process::ExitCode;

use compositor_toolkit::Client;
use compositor_toolkit::tiny_skia;
use compositor_waybar::app::App;
use compositor_waybar::cli::{self, Parsed};
use compositor_waybar::config::{self, System};
use compositor_waybar::css::Stylesheet;
use compositor_waybar::diag::Diagnostics;
use compositor_waybar::engine::{ImageCache, TextEngine};
use compositor_waybar::render;

fn say(lines: Vec<String>) {
    let mut err = std::io::stderr().lock();
    for line in lines {
        let _ = writeln!(err, "{line}");
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match cli::parse(&args) {
        Ok(Parsed::Run(options)) => options,
        Ok(Parsed::Help) => {
            let _ = writeln!(std::io::stdout(), "{}", cli::HELP);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            let _ = writeln!(
                std::io::stdout(),
                "Waybar v{} (Ferrix)",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            say(vec![format!("[error] Error in command line: {message}")]);
            return ExitCode::from(1);
        }
    };
    let mut diag = Diagnostics::default();
    let loaded = config::load(options.config.as_deref(), &System, &mut diag);
    let style = options
        .style
        .clone()
        .or_else(|| config::find(&["style.css"], &System));
    let sheet = style.as_ref().and_then(|path| {
        diag.info(format!("Using CSS file {}", path.display()));
        match std::fs::read_to_string(path) {
            Ok(text) => Some(Stylesheet::parse(
                &text,
                path,
                &|p| std::fs::read_to_string(p).ok(),
                &mut diag,
            )),
            Err(_) => {
                diag.error("Can't open style file".to_owned());
                None
            }
        }
    });
    say(diag.drain(options.level));
    let Ok(loaded) = loaded.map_err(|message| say(vec![format!("[error] {message}")])) else {
        return ExitCode::from(1);
    };
    let Some(sheet) = sheet else {
        if style.is_none() {
            say(vec!["[error] Missing required resource files".to_owned()]);
        }
        return ExitCode::from(1);
    };
    let mut engine = match &options.fonts_dir {
        Some(dir) => {
            let mut fonts = compositor_text::Fonts::new();
            let _ = fonts.add_dir(dir);
            TextEngine::with_fonts(fonts)
        }
        None => TextEngine::system(),
    };
    if let Some(path) = &options.render {
        let ground = render::colour(&options.over).unwrap_or(tiny_skia::Color::BLACK);
        let mut images = ImageCache::default();
        let mut diag = Diagnostics::default();
        let drawn = render::render(
            &loaded.root,
            &sheet,
            &mut engine,
            &mut images,
            &options.output,
            options.size,
            ground,
            &mut diag,
        );
        say(diag.drain(options.level));
        say(engine.diag.drain(options.level));
        return match drawn.and_then(|picture| {
            std::fs::write(path, render::ppm(&picture))
                .map(|()| picture)
                .map_err(|error| format!("{}: {error}", path.display()))
        }) {
            Ok(picture) => {
                let _ = writeln!(
                    std::io::stdout(),
                    "waybar: rendered {}x{} into {}",
                    picture.width(),
                    picture.height(),
                    path.display()
                );
                ExitCode::SUCCESS
            }
            Err(message) => {
                say(vec![format!("[error] {message}")]);
                ExitCode::from(1)
            }
        };
    }
    let client = match Client::connect() {
        Ok(client) => client,
        Err(error) => {
            say(vec![format!(
                "[error] Bar need to run under Wayland: {error}"
            )]);
            return ExitCode::from(1);
        }
    };
    let mut app = match App::new(
        client,
        loaded.root,
        sheet,
        engine,
        Box::new(ImageCache::default()),
        options.level,
    ) {
        Ok(app) => app,
        Err(message) => {
            say(vec![format!("[error] {message}")]);
            return ExitCode::from(1);
        }
    };
    loop {
        let turned = app.turn(None);
        say(app.take_lines());
        if let Err(message) = turned {
            say(vec![format!("[error] {message}")]);
            return ExitCode::from(1);
        }
    }
}
