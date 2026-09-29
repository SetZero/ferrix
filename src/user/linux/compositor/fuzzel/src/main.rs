//! `fuzzel`: the launcher.

fn main() {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_else(|| "fuzzel".to_owned());
    let rest: Vec<String> = args.collect();
    std::process::exit(compositor_fuzzel::window::run(&program, &rest));
}
