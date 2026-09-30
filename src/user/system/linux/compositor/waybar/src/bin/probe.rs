//! `waybar-probe`: read a waybar config and stylesheet -- by default the
//! user's own, found the way waybar finds them -- and list everything in
//! them this waybar does not carry out, with the reason.
//!
//! Host-side only: it is how the real `~/.config/waybar` files are checked
//! without committing them. `waybar-probe [-c config] [-s style]`.

use std::io::Write;
use std::path::PathBuf;

use compositor_waybar::probe;

fn main() -> std::process::ExitCode {
    let mut config = None;
    let mut style = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--config" => config = args.next().map(PathBuf::from),
            "-s" | "--style" => style = args.next().map(PathBuf::from),
            _ => {
                let _ = writeln!(
                    std::io::stderr(),
                    "usage: waybar-probe [-c config] [-s style]"
                );
                return std::process::ExitCode::from(2);
            }
        }
    }
    let report = probe::run(config.as_deref(), style.as_deref());
    let mut out = std::io::stdout().lock();
    for line in &report {
        let _ = writeln!(out, "{line}");
    }
    std::process::ExitCode::SUCCESS
}
