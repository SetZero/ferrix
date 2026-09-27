//! A bar drawn without a compositor, into a picture: what a boot's screen
//! must show.
//!
//! `waybar --render FILE.ppm --size WxH --over RRGGBB` makes the bars the
//! config asks for on one made-up output of that size, runs every module to
//! its first answer -- each script to its end, or one that keeps running
//! to its first line, in order, here and now -- lays the first bar out as the running bar would and draws it over a
//! ground of that colour, as the compositor composites it. So the picture
//! is the one a boot of the same config, style and fonts shows at the top of
//! its screen, and `xtask`'s waybar boot compares the two.
//!
//! Nothing here waits: a module's timers never fire, and no server is asked
//! -- a `hyprland/window` sees no windows, as on a fresh desktop, and a
//! `pulseaudio` module waybar's starting values, as where there is no sound
//! server.

use std::collections::VecDeque;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use tiny_skia::{Color, Pixmap, PixmapPaint, Transform};

use crate::app::Engine;
use crate::bar;
use crate::config::{self, Output, System};
use crate::css::Stylesheet;
use crate::diag::Diagnostics;
use crate::json::Value;
use crate::layout;
use crate::modules::{self, ChildKey, Host, Module, TimerKey, network::Interface};
use crate::paint::{self, Images};
use crate::view::{self, BarView};

/// What a script left for its module.
enum Pending {
    Line(ChildKey, String),
    Exit(ChildKey, Option<i32>, String),
}

/// A host that runs each command at once and hands its output back in
/// order.
struct Now<'a> {
    output: &'a str,
    next: u64,
    pending: VecDeque<Pending>,
    diag: &'a mut Diagnostics,
}

impl Now<'_> {
    /// Hand `module` what its scripts left, and what those runs left, until
    /// nothing is left.
    fn settle(&mut self, module: &mut dyn Module) {
        while let Some(done) = self.pending.pop_front() {
            let _ = match done {
                Pending::Line(key, line) => module.child_line(self, key, &line),
                Pending::Exit(key, status, out) => module.child_exit(self, key, status, &out),
            };
        }
    }
}

impl Host for Now<'_> {
    fn run(&mut self, command: &str, lines: bool) -> Option<ChildKey> {
        self.next += 1;
        let key = self.next;
        let child = Command::new("/bin/sh")
            .args(["-c", command])
            .env("WAYBAR_OUTPUT_NAME", self.output)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return None;
        };
        let mut stdout = child.stdout.take()?;
        if lines {
            // A script that runs for ever and prints a line whenever
            // something changes (the user's `ba-calendar daemon`): its first
            // line is its answer. It is not waited for past that, and it is
            // not said to have ended, which would hide the module.
            let (send, receive) = std::sync::mpsc::channel();
            let _ = std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(&mut stdout);
                let mut line = String::new();
                let _ = std::io::BufRead::read_line(&mut reader, &mut line);
                let _ = send.send(line);
            });
            match receive.recv_timeout(Duration::from_secs(5)) {
                Ok(line) if line.ends_with('\n') => {
                    self.pending
                        .push_back(Pending::Line(key, line.trim_end_matches('\n').to_owned()));
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Ok(rest) => {
                    // It ended without a whole line.
                    let status = child.wait().ok().and_then(|status| status.code());
                    self.pending.push_back(Pending::Exit(key, status, rest));
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            return Some(key);
        }
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let status = child.wait().ok().and_then(|status| status.code());
        self.pending.push_back(Pending::Exit(key, status, out));
        Some(key)
    }

    fn kill(&mut self, _: ChildKey) {}

    fn spawn(&mut self, _: &str) {}

    fn timer(&mut self, _: Duration) -> TimerKey {
        self.next += 1;
        self.next
    }

    fn cancel(&mut self, _: TimerKey) {}

    fn watch(&mut self, _: i32) -> modules::WatchKey {
        self.next += 1;
        self.next
    }

    fn unwatch(&mut self, _: modules::WatchKey) {}

    fn read(&mut self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn now(&self) -> f64 {
        0.0
    }

    fn wall(&self) -> (i64, i64) {
        (0, 0)
    }

    fn hyprland(&mut self, _: &str) -> Option<String> {
        None
    }

    fn var(&self, name: &str) -> Option<String> {
        // A render asks no server: not Hyprland, and not a sound server,
        // which the machine drawing the expected picture may have and the
        // machine it is compared with has not.
        if matches!(
            name,
            "PULSE_SERVER" | "PULSE_RUNTIME_PATH" | "XDG_RUNTIME_DIR"
        ) {
            return None;
        }
        std::env::var(name).ok()
    }

    fn diag(&mut self) -> &mut Diagnostics {
        self.diag
    }

    fn interface(&mut self, _: &str) -> Option<Interface> {
        None
    }

    fn sigrtmin(&self) -> i32 {
        crate::app::sigrtmin()
    }
}

/// `RRGGBB` as a colour.
#[must_use]
pub fn colour(text: &str) -> Option<Color> {
    let text = text.trim_start_matches('#');
    if text.len() != 6 {
        return None;
    }
    let byte = |at: usize| {
        text.get(at..at + 2)
            .and_then(|b| u8::from_str_radix(b, 16).ok())
    };
    Some(Color::from_rgba8(byte(0)?, byte(2)?, byte(4)?, 255))
}

/// Draw the first bar `root` makes on an output named `name` of `size`,
/// over `ground`, into a picture the output's width by the bar's height.
///
/// # Errors
///
/// No bar for that output, or a size of nothing.
#[expect(
    clippy::too_many_arguments,
    reason = "a bar's whole setting, spelled out"
)]
pub fn render(
    root: &Value,
    sheet: &Stylesheet,
    engine: &mut dyn Engine,
    images: &mut dyn Images,
    name: &str,
    size: (u32, u32),
    ground: Color,
    diag: &mut Diagnostics,
) -> Result<Pixmap, String> {
    let output = Output {
        name: name.to_owned(),
        description: name.to_owned(),
        width: i32::try_from(size.0).unwrap_or(0),
        height: i32::try_from(size.1).unwrap_or(0),
    };
    let config = config::bars_for(root, &output, &System, diag)
        .into_iter()
        .next()
        .cloned()
        .ok_or_else(|| format!("no bar in the config asks for output {name}"))?;
    let options = bar::Options::read(&config, diag);
    let mut host = Now {
        output: name,
        next: 0,
        pending: VecDeque::new(),
        diag,
    };
    let mut made: Vec<Box<dyn Module>> = Vec::new();
    let mut sections: [Vec<usize>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (section, names) in options.modules.iter().enumerate() {
        if section == 1 && options.no_center {
            continue;
        }
        for module in names {
            match modules::make(module, config.get(module), name, &mut host) {
                Ok(mut module) => {
                    module.start(&mut host);
                    // Every script this start ran, to its end.
                    host.settle(module.as_mut());
                    if let Some(list) = sections.get_mut(section) {
                        list.push(made.len());
                    }
                    made.push(module);
                }
                Err(error) => host.diag.warn(format!("module {module}: {error}")),
            }
        }
    }
    let mut classes = vec![options.position.name().to_owned(), name.to_owned()];
    if !options.name.is_empty() {
        classes.push(options.name.clone());
    }
    classes.push("mode-default".to_owned());
    for module in &made {
        for (class, on) in module.window_classes() {
            if on && !classes.contains(&class) {
                classes.push(class);
            } else if !on {
                classes.retain(|c| *c != class);
            }
        }
    }
    let views: Vec<_> = made.iter().map(|m| m.view().clone()).collect();
    let bar_view = BarView {
        window_classes: classes,
        spacing: f32::from(i16::try_from(options.spacing).unwrap_or(0)),
        center: !options.no_center,
        fixed_center: options.fixed_center,
        sections,
    };
    let (mut tree, _) = view::build(&bar_view, &views);
    tree.style(sheet);
    let (_, natural) = layout::natural_size(&tree, engine);
    #[expect(clippy::cast_possible_truncation, reason = "a bar's height in pixels")]
    #[expect(clippy::cast_sign_loss, reason = "a size is not negative")]
    let height = options.height.max(natural.ceil().max(0.0) as u32);
    #[expect(clippy::cast_precision_loss, reason = "a screen's size in pixels")]
    let placement = layout::layout(&tree, engine, size.0 as f32, height as f32);
    let mut bar = Pixmap::new(size.0, height).ok_or("a bar of no size")?;
    paint::draw_tree(&mut bar.as_mut(), &tree, &placement, images, engine, 1.0);
    let mut picture = Pixmap::new(size.0, height).ok_or("a bar of no size")?;
    picture.fill(ground);
    picture.draw_pixmap(
        0,
        0,
        bar.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    Ok(picture)
}

/// A picture as binary PPM, which `xtask` reads screendumps as.
#[must_use]
pub fn ppm(pixmap: &Pixmap) -> Vec<u8> {
    let mut out = format!("P6\n{} {}\n255\n", pixmap.width(), pixmap.height()).into_bytes();
    for pixel in pixmap.pixels() {
        let color = pixel.demultiply();
        out.extend([color.red(), color.green(), color.blue()]);
    }
    out
}
