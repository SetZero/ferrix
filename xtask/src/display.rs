//! `cargo xtask test-display`: iteration 1 of the compositor, a blank screen
//! on Ferrix, judged pixel by pixel.
//!
//! `docs/DISPLAY.md` §3. The compositor's first program, `compositor/blank`,
//! is built static for the architecture and booted as init with a virtio-gpu
//! device on the bus. It sets the connector's preferred mode and fills a dumb
//! buffer with one colour, then prints [`MARKER`]. At that line this module
//! asks QEMU, over its QMP socket, for a screendump of the virtio-gpu head,
//! and requires every pixel to be that colour.
//!
//! A check that cannot fail proves nothing, so the test runs twice: once as
//! above, and once with the program built with `negative-control`, which
//! draws pixel (0, 0) in another colour. The second boot must fail the check
//! on exactly that pixel and no other.
//!
//! QMP is JSON lines over a TCP socket on localhost, on every host, so this
//! works the same on Linux and Windows. xtask takes no crates, so what QMP
//! needs of JSON is written here: two commands out and a `return` or `error`
//! back. The screendump is QEMU's default format, binary PPM.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::{Error, Result};

/// What `--init` names `compositor/blank` by, built for the architecture
/// first, so `run --display --init blank` shows its screen in a window.
pub(crate) const INIT_NAME: &str = "blank";

/// What the program prints once the colour is on the screen. The same string
/// as `compositor/blank/src/card.rs`'s `MARKER`.
pub(crate) const MARKER: &str = "compositor: scanout";

/// What the program prints when it could not.
pub(crate) const FAILED: &str = "compositor: failed";

/// What both of its lines start with, which is what the boot is watched for.
const EITHER: &str = "compositor: ";

/// The colour the program fills with, as red, green and blue: its
/// `BACKGROUND`, `0x1E1E2E`.
pub(crate) const BACKGROUND: [u8; 3] = [0x1E, 0x1E, 0x2E];

/// The virtio-gpu device's QEMU id, which the screendump names.
pub(crate) const DEVICE_ID: &str = "gpu0";

/// How long the screen may take to show the colour after the marker: the
/// program's `SETCRTC` returns once the flush is queued, not once QEMU has
/// drawn it.
const SETTLE: Duration = Duration::from_secs(5);

/// The Rust target the program is built for on `arch`, if the architecture
/// has virtio-gpu.
fn target(arch: Arch) -> Option<&'static str> {
    match arch {
        Arch::X86_64 => Some("x86_64-unknown-linux-musl"),
        Arch::AArch64 => Some("aarch64-unknown-linux-musl"),
        Arch::Armv7a => None,
    }
}

/// Build `compositor/blank` for `arch`, with the negative control or without,
/// and return where the program is.
pub(crate) fn build_blank(arch: Arch, negative: bool) -> Result<PathBuf> {
    let target = target(arch).ok_or_else(|| {
        Error::new(format!(
            "{arch} has no virtio-gpu in QEMU; the display test runs on x86_64 and aarch64"
        ))
    })?;
    let flavour = if negative { "negative" } else { "plain" };
    let target_dir = paths::target_dir().join("compositor").join(flavour);
    println!("  building compositor/blank ({flavour}) for {target}");
    let mut command = Command::new(crate::cargo::cargo());
    let _ = command
        .current_dir(paths::workspace_root().join("compositor"))
        .args([
            "build",
            "--release",
            "-p",
            "compositor-blank",
            "--target",
            target,
        ])
        .env("CARGO_TARGET_DIR", &target_dir);
    if negative {
        let _ = command.args(["--features", "negative-control"]);
    }
    crate::cargo::run(command, "cargo build (compositor/blank)")?;
    Ok(target_dir.join(target).join("release").join("blank"))
}

/// A free TCP port on localhost for QEMU's QMP server. The listener is closed
/// before QEMU binds the port, so another program could take it in between;
/// QEMU then fails to start and says why.
pub(crate) fn free_port() -> Result<u16> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

/// A QMP session.
pub(crate) struct Qmp {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Qmp {
    /// Connect to QEMU's QMP server on `port`, retrying until `deadline`,
    /// read its greeting and leave capabilities negotiation.
    pub(crate) fn connect(port: u16, deadline: Instant) -> Result<Self> {
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let stream = loop {
            match TcpStream::connect(address) {
                Ok(stream) => break stream,
                Err(error) if Instant::now() >= deadline => {
                    return Err(Error::new(format!(
                        "could not reach QMP on {address}: {error}"
                    )));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let writer = stream.try_clone()?;
        let mut session = Self {
            reader: BufReader::new(stream),
            writer,
        };
        let greeting = session.line()?;
        if !greeting.contains("\"QMP\"") {
            return Err(Error::new(format!("QMP greeted with `{greeting}`")));
        }
        let _ = session.execute("qmp_capabilities", None)?;
        Ok(session)
    }

    fn line(&mut self) -> Result<String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(Error::new("QMP closed the connection"));
        }
        Ok(line)
    }

    /// Run `command` with `arguments`, a JSON object's text, and return the
    /// reply line, skipping the asynchronous events QEMU interleaves.
    pub(crate) fn execute(&mut self, command: &str, arguments: Option<&str>) -> Result<String> {
        let request = match arguments {
            Some(arguments) => format!("{{\"execute\":\"{command}\",\"arguments\":{arguments}}}\n"),
            None => format!("{{\"execute\":\"{command}\"}}\n"),
        };
        self.writer.write_all(request.as_bytes())?;
        loop {
            let line = self.line()?;
            if line.contains("\"event\"") {
                continue;
            }
            if line.contains("\"error\"") {
                return Err(Error::new(format!(
                    "QMP `{command}` failed: {}",
                    line.trim()
                )));
            }
            if line.contains("\"return\"") {
                return Ok(line);
            }
        }
    }

    /// Ask for a screendump of `device`'s first head, or of QEMU's first
    /// console when `device` is `None`, into `file`.
    pub(crate) fn screendump(&mut self, device: Option<&str>, file: &Path) -> Result<()> {
        let file = json_string(&file.display().to_string());
        let arguments = match device {
            Some(device) => format!(
                "{{\"filename\":{file},\"device\":{},\"head\":0}}",
                json_string(device)
            ),
            None => format!("{{\"filename\":{file}}}"),
        };
        self.execute("screendump", Some(&arguments)).map(drop)
    }
}

/// `text` as a JSON string literal.
pub(crate) fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A screendump: width, height, and three bytes a pixel, red first.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Image {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) pixels: Vec<u8>,
}

/// Parse a binary PPM (`P6`) with a maximum value of 255.
pub(crate) fn parse_ppm(bytes: &[u8]) -> Result<Image> {
    let bad = |why: &str| Error::new(format!("not a screendump PPM: {why}"));
    let mut at = 0;
    let mut fields = Vec::new();
    while fields.len() < 4 {
        // Whitespace and comments between the header's fields.
        while let Some(&byte) = bytes.get(at) {
            if byte == b'#' {
                while bytes.get(at).is_some_and(|&b| b != b'\n') {
                    at += 1;
                }
            } else if byte.is_ascii_whitespace() {
                at += 1;
            } else {
                break;
            }
        }
        let start = at;
        while bytes.get(at).is_some_and(|b| !b.is_ascii_whitespace()) {
            at += 1;
        }
        let field = bytes
            .get(start..at)
            .filter(|field| !field.is_empty())
            .ok_or_else(|| bad("the header ends early"))?;
        fields.push(String::from_utf8_lossy(field).into_owned());
    }
    // Exactly one whitespace byte separates the header from the pixels.
    at += 1;
    let [magic, width, height, max] = fields.as_slice() else {
        return Err(bad("the header ends early"));
    };
    if magic != "P6" {
        return Err(bad("not P6"));
    }
    let number = |field: &str| {
        field
            .parse::<usize>()
            .map_err(|_| bad("a size is not a number"))
    };
    let (width, height) = (number(width)?, number(height)?);
    if max != "255" {
        return Err(bad("the maximum value is not 255"));
    }
    let len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| bad("the size overflows"))?;
    let pixels = at
        .checked_add(len)
        .and_then(|end| bytes.get(at..end))
        .ok_or_else(|| bad("fewer pixels than the header says"))?
        .to_vec();
    Ok(Image {
        width,
        height,
        pixels,
    })
}

/// Every pixel that is not `color`, as `(x, y, [r, g, b])`, up to `limit`,
/// and how many there are in all.
pub(crate) fn mismatches(
    image: &Image,
    color: [u8; 3],
    limit: usize,
) -> (Vec<(usize, usize, [u8; 3])>, usize) {
    let width = image.width.max(1);
    let mut found = Vec::new();
    let mut count = 0;
    for (index, pixel) in image.pixels.chunks_exact(3).enumerate() {
        if let [red, green, blue] = *pixel
            && [red, green, blue] != color
        {
            count += 1;
            if found.len() < limit {
                found.push((index % width, index / width, [red, green, blue]));
            }
        }
    }
    (found, count)
}

/// Take a screendump of `device` into `file` and parse it.
fn read_dump(qmp: &mut Qmp, device: Option<&str>, file: &Path) -> Result<Image> {
    let _ = std::fs::remove_file(file);
    qmp.screendump(device, file)?;
    let bytes = std::fs::read(file)
        .map_err(|error| Error::new(format!("reading {}: {error}", file.display())))?;
    parse_ppm(&bytes)
}

/// Boot `arch` with `program` as init and a virtio-gpu, and return the
/// screendump taken once the program has printed its marker and the screen
/// has had [`SETTLE`] to show it — or as soon as it is all `BACKGROUND`.
fn boot_and_dump(arch: Arch, program: &Path, args: &Args, name: &str) -> Result<Image> {
    let loader = crate::cargo::build_loader(arch, args.release)?;
    let kernel = crate::cargo::build_kernel_with_init(arch, args.release, program, "")?;
    let natives = crate::native::build(arch, args.release)?;
    let image = crate::fat::write_image(arch, &loader, &kernel, &natives, None)?;

    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join(format!("{name}.ppm"));
    let mut taken = None;
    let hook = |lines: &[String]| -> Result<()> {
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        if let Some(line) = lines.iter().rev().find(|line| line.contains(FAILED)) {
            // Say what QEMU's first console showed, which is also the proof
            // that the screendump path works while the card does not.
            let firmware = read_dump(&mut qmp, None, &dump)
                .map(|screen| {
                    format!(
                        "QEMU's first console was {}x{}",
                        screen.width, screen.height
                    )
                })
                .unwrap_or_else(|error| format!("no screendump either: {error}"));
            return Err(Error::new(format!("{arch}: {} ({firmware})", line.trim())));
        }
        let settle = Instant::now() + SETTLE;
        loop {
            let screen = read_dump(&mut qmp, Some(DEVICE_ID), &dump)?;
            let clean = mismatches(&screen, BACKGROUND, 0).1 == 0;
            if clean || Instant::now() >= settle {
                taken = Some(screen);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    taken.ok_or_else(|| {
        Error::new(format!(
            "{arch}: the program never printed `{MARKER}` within {}s",
            args.timeout
        ))
    })
}

/// `test-display` on each architecture asked for that has virtio-gpu.
pub(crate) fn test_display(args: &Args) -> Result<()> {
    for arch in args.arches()? {
        if target(arch).is_none() {
            println!("  {arch}: no virtio-gpu in QEMU's machine; skipped");
            continue;
        }
        let plain = build_blank(arch, false)?;
        let screen = boot_and_dump(arch, &plain, args, "display")?;
        let (found, count) = mismatches(&screen, BACKGROUND, 8);
        if count != 0 {
            return Err(Error::new(format!(
                "{arch}: {count} of {} pixels are not 0x1e1e2e; the first: {found:?}",
                screen.width * screen.height
            )));
        }
        println!(
            "  {arch}: all {} pixels of the {}x{} screen are 0x1e1e2e",
            screen.width * screen.height,
            screen.width,
            screen.height
        );

        let negative = build_blank(arch, true)?;
        let screen = boot_and_dump(arch, &negative, args, "display-negative")?;
        let (found, count) = mismatches(&screen, BACKGROUND, 8);
        if count != 1 || found.first().map(|&(x, y, _)| (x, y)) != Some((0, 0)) {
            return Err(Error::new(format!(
                "{arch}: the negative control should differ at exactly pixel (0, 0); \
                 {count} pixels differ, the first: {found:?}"
            )));
        }
        println!("  {arch}: the negative control failed the check at exactly pixel (0, 0)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ppm(width: usize, height: usize, fill: [u8; 3]) -> Vec<u8> {
        let mut bytes = format!("P6\n# QEMU\n{width} {height}\n255\n").into_bytes();
        for _ in 0..width * height {
            bytes.extend_from_slice(&fill);
        }
        bytes
    }

    #[test]
    fn a_screendump_parses_and_is_judged_pixel_by_pixel() {
        let mut bytes = ppm(4, 3, BACKGROUND);
        let screen = parse_ppm(&bytes).expect("parses");
        assert_eq!(
            (screen.width, screen.height, screen.pixels.len()),
            (4, 3, 36)
        );
        assert_eq!(mismatches(&screen, BACKGROUND, 8), (vec![], 0));

        // Pixel (1, 2) wrong.
        let header = bytes.len() - 36;
        bytes[header + (2 * 4 + 1) * 3] = 0xFF;
        let screen = parse_ppm(&bytes).expect("parses");
        assert_eq!(
            mismatches(&screen, BACKGROUND, 8),
            (vec![(1, 2, [0xFF, 0x1E, 0x2E])], 1)
        );
    }

    #[test]
    fn a_malformed_screendump_is_refused() {
        let good = ppm(2, 2, BACKGROUND);
        assert!(parse_ppm(&good[..good.len() - 1]).is_err(), "short");
        assert!(parse_ppm(b"P5\n2 2\n255\n").is_err(), "greyscale");
        assert!(parse_ppm(b"P6\n2 2\n65535\n").is_err(), "wide samples");
        assert!(parse_ppm(b"P6\n2").is_err(), "no header");
        assert!(parse_ppm(b"P6\nx 2\n255\n").is_err(), "not a number");
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(
            json_string(r"C:\build\display.ppm"),
            r#""C:\\build\\display.ppm""#
        );
        assert_eq!(json_string("a\"b\n"), "\"a\\\"b\\u000a\"");
    }
}
