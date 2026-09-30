//! `caption [--font DESC] [--size POINTS] [--fonts-dir DIR] [--render FILE.ppm] TEXT...`:
//! draw TEXT on a layer surface near the screen's top-left corner, or with
//! `--render`, into a picture file on this machine. `--fonts-dir` takes the
//! faces from that directory alone rather than from every font directory,
//! so the host and the guest choose between the same files.

use std::io::Write as _;
use std::time::Duration;

use compositor_caption::{MARGIN, Request, draw, ppm};
use compositor_toolkit::{Anchor, Client, Event, Layer, LayerOptions, Margin};

fn main() {
    match run() {
        Ok(()) => {}
        Err(error) => {
            say(&format!("caption: {error}"));
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let mut request = Request {
        font: "sans-serif".to_owned(),
        points: 24.0,
        text: String::new(),
    };
    let mut render = None;
    let mut dir = None;
    let mut words = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--font" => request.font = arguments.next().ok_or("--font wants a description")?,
            "--size" => {
                request.points = arguments
                    .next()
                    .and_then(|size| size.parse().ok())
                    .ok_or("--size wants a number of points")?;
            }
            "--render" => render = Some(arguments.next().ok_or("--render wants a file")?),
            "--fonts-dir" => dir = Some(arguments.next().ok_or("--fonts-dir wants a directory")?),
            _ => words.push(argument),
        }
    }
    request.text = words.join(" ");
    let mut fonts = match &dir {
        Some(dir) => {
            let mut fonts = compositor_text::Fonts::new();
            let _ = fonts.add_dir(std::path::Path::new(dir));
            fonts
        }
        None => compositor_text::Fonts::system(),
    };
    let (pixmap, said) = draw(&mut fonts, &request)?;
    say(&format!("caption: {said}"));
    if let Some(path) = render {
        return std::fs::write(&path, ppm(&pixmap)).map_err(|error| format!("{path}: {error}"));
    }
    let mut client = Client::connect().map_err(|error| error.to_string())?;
    let surface = client
        .layer_surface(&LayerOptions {
            layer: Layer::Overlay,
            namespace: "caption".to_owned(),
            size: (pixmap.width(), pixmap.height()),
            anchor: Anchor::TOP.with(Anchor::LEFT),
            exclusive_zone: -1,
            margin: Margin {
                top: MARGIN,
                left: MARGIN,
                ..Margin::default()
            },
            ..LayerOptions::default()
        })
        .map_err(|error| error.to_string())?;
    loop {
        for event in client
            .dispatch(Some(Duration::from_secs(60)))
            .map_err(|error| error.to_string())?
        {
            match event {
                Event::Configure { surface: which, .. } if which == surface => {
                    let _ = client
                        .draw(surface, |target| {
                            target.draw_pixmap(
                                0,
                                0,
                                pixmap.as_ref(),
                                &compositor_caption::tiny_skia::PixmapPaint::default(),
                                compositor_caption::tiny_skia::Transform::identity(),
                                None,
                            );
                        })
                        .map_err(|error| error.to_string())?;
                    client.request_frame(surface);
                }
                Event::Frame { surface: which, .. } if which == surface => {
                    say(&format!(
                        "caption: drawn {}x{} at {MARGIN},{MARGIN}",
                        pixmap.width(),
                        pixmap.height()
                    ));
                }
                Event::Closed(which) if which == surface => return Ok(()),
                _ => {}
            }
        }
    }
}

/// A line on the standard output, flushed.
fn say(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
