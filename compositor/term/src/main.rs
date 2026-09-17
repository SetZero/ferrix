//! The terminal's command line: `term [--headless] [--] <program> [args...]`.
//!
//! With a window, it connects to the compositor `WAYLAND_DISPLAY` names and
//! draws the program's output in it. With `--headless` it has no window at
//! all and prints what the program wrote on the terminal's own output, which
//! is how `cargo xtask test-pty` sees that a pseudoterminal works without a
//! compositor in the way.

use std::io::Write as _;

fn main() {
    // A program that is init is handed a shell's arguments, because the
    // kernel starts `sh -c <script>` and this is what it starts instead:
    // `compositor/evecho`'s `unshell` turns them back into its own.
    let arguments =
        compositor_evecho::init::unshell(std::env::args().skip(1).collect::<Vec<String>>());
    let headless = arguments.iter().any(|word| word == "--headless");
    let rest: Vec<String> = arguments
        .into_iter()
        .filter(|word| word != "--headless" && word != "--")
        .collect();
    let Some(program) = rest.first().cloned() else {
        say("term: usage: term [--headless] <program> [arguments...]");
        std::process::exit(2);
    };
    let program_arguments: Vec<String> = rest.into_iter().skip(1).collect();

    let answer = if headless {
        headless_run(&program, &program_arguments)
    } else {
        windowed(&program, &program_arguments)
    };
    match answer {
        Ok(line) => say(&line),
        Err(error) => {
            say(&format!("term: failed: {error}"));
            std::process::exit(1);
        }
    }
}

/// A terminal with a window.
fn windowed(program: &str, arguments: &[String]) -> Result<String, String> {
    let display =
        std::env::var("WAYLAND_DISPLAY").map_err(|_| "WAYLAND_DISPLAY is not set".to_owned())?;
    let path =
        compositor_socket::socket_path(&display).map_err(|error| format!("the socket: {error}"))?;
    compositor_term::client::run(&path, program, arguments)
}

/// A terminal with no window: the program's output is printed, and what it
/// drew is described.
///
/// The grid is still kept, so what is printed at the end is what a window
/// would have shown -- which is what proves the pseudoterminal and the
/// escape sequences without a compositor.
fn headless_run(program: &str, arguments: &[String]) -> Result<String, String> {
    use std::time::{Duration, Instant};

    const COLUMNS: usize = 80;
    const ROWS: usize = 24;
    /// How long to wait for the program after it has stopped writing.
    const PATIENCE: Duration = Duration::from_secs(20);

    let mut grid = compositor_term::grid::Grid::new(COLUMNS, ROWS);
    let mut pty = compositor_term::pty::Pty::start(
        program,
        arguments,
        (
            u16::try_from(COLUMNS).unwrap_or(80),
            u16::try_from(ROWS).unwrap_or(24),
        ),
    )
    .map_err(|error| format!("the pseudoterminal: {error}"))?;

    let started = Instant::now();
    let mut buffer = [0u8; 4096];
    let mut read_bytes = 0usize;
    let mut done: Option<Instant> = None;
    while started.elapsed() < PATIENCE {
        let read = pty
            .read(&mut buffer)
            .map_err(|error| format!("reading the pseudoterminal: {error}"))?;
        if read > 0 {
            read_bytes += read;
            grid.write(buffer.get(..read).unwrap_or(&[]));
            continue;
        }
        if pty.done() {
            // One more pass to take whatever it wrote as it ended, then
            // stop: a program's last line is written before it exits and
            // read after.
            match done {
                Some(when) if when.elapsed() > Duration::from_millis(200) => break,
                Some(_) => {}
                None => done = Some(Instant::now()),
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    // What a window would have shown, line by line, so that a test can look
    // for the program's own words in the grid rather than in the pipe.
    for row in 0..ROWS {
        let line = grid.line(row);
        if !line.is_empty() {
            say(&format!("term: | {line}"));
        }
    }
    Ok(format!(
        "term: {program} wrote {read_bytes} bytes; the cursor is at {:?}",
        grid.cursor()
    ))
}

/// Say a line on the standard output, flushed: a terminal started by the
/// compositor has the console for its output, and a line held in a buffer is
/// a line a test never sees.
fn say(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
