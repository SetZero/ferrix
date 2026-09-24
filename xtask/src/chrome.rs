//! `test-chrome`: Google's headless Chrome on Ferrix, rendering a page.
//!
//! The browser is Chrome for Testing's prebuilt `chrome-headless-shell`, not
//! one built here: a 198 MB position-independent glibc program that loads
//! forty of the system's libraries, run by Debian's `ld-linux` on Debian's
//! glibc, all of it on a btrfs volume `scripts/fetch-chrome.sh` makes from
//! pinned downloads. `docs/CHROME.md` says why this is the first Chrome and
//! what ferrousli standing in for that glibc would add.
//!
//! Three steps, each a proof of more than the one before: `--version`, which
//! says the program loaded and its libraries resolved; `--dump-dom` of a
//! page whose script computes a number, which says Chrome's processes
//! started, talked over Mojo, and V8 ran; and `--screenshot`, which says
//! Blink laid out and Skia drew a picture into a file.
//!
//! Chrome runs as Chrome, by `execve`, not as an argument to the loader: it
//! finds its ICU data and its resource packs beside `/proc/self/exe`, and it
//! starts its own renderer and GPU processes by running that again.
//!
//! # `--no-sandbox --no-zygote`
//!
//! `--no-sandbox` because the sandbox is namespaces and seccomp, which Ferrix
//! has neither of: `clone` refuses `CLONE_NEW*` rather than pretend
//! (`docs/CHROME.md` §2.4). `--no-zygote` because the zygote, the process
//! Chrome forks its children from instead of starting each afresh, reports
//! that it could not fork on Ferrix, and its children never start; with it
//! off, the browser starts each child itself, by `/proc/self/exe`, and they
//! do. Why the zygote's fork fails is the next thing to find.
//!
//! # Where Chrome lives
//!
//! The volume carries no `ferrix-root` label, so the kernel mounts it at
//! `/data`, as test-rustc's is, attached under QEMU's `snapshot=on` so a run
//! never changes it. glibc names its paths absolutely, so the initramfs
//! carries a symbolic link for each into `/data` ([`LINKS`]).
//!
//! # Why x86-64 only
//!
//! Chrome for Testing publishes linux64 only.

use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result, cargo, fat, initramfs, native, qemu, rustc, zinc};

/// The page whose DOM Chrome is asked for: its script writes a number only
/// V8 could have computed into an element, so the DOM carries it as text the
/// script's own source does not.
const PAGE: &str = "data:text/html,<p id=v8></p><script>document.getElementById(\"v8\").textContent=\"computed \"+6*7</script>";

/// What that DOM holds once the script ran.
const COMPUTED: &str = "computed 42";

/// The page Chrome takes a picture of.
const PICTURE: &str =
    "data:text/html,<body style=background:%23fc0><h1>Hello from Chrome on Ferrix</h1>";

/// The script the shell runs. The version first, so a failure says whether
/// Chrome loaded at all or only a later step failed.
const SCRIPT: &str = r#"export PATH=/bin HOME=/tmp
cd /tmp
chrome=/data/chrome/chrome-headless-shell
$chrome --version || exit 3
$chrome --no-sandbox --no-zygote --dump-dom 'PAGE' || exit 4
$chrome --no-sandbox --no-zygote --screenshot=/tmp/shot.png --window-size=640,360 'PICTURE' || exit 5
[ -s /tmp/shot.png ] || exit 6
echo chrome-gate: screenshot written
exit 16
"#;

/// What `--version` prints, which says Chrome and its libraries loaded.
const VERSION: &str = "Google Chrome for Testing 154.0.8037.57";

/// What the script says once the screenshot is on the disk.
const SHOT: &str = "chrome-gate: screenshot written";

/// The status the script exits with when every step succeeded.
const STATUS: i32 = 16;

/// Each path glibc and fontconfig name absolutely, and where on the volume
/// it is.
const LINKS: &[(&str, &str)] = &[
    ("lib64", "/data/usr/lib64"),
    ("lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("usr/lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("etc/fonts", "/data/etc/fonts"),
    ("usr/share/fonts", "/data/usr/share/fonts"),
    ("usr/share/fontconfig", "/data/usr/share/fontconfig"),
];

/// Guest memory unless `--memory` says otherwise: Chrome wants about two
/// GiB to open a page, and the page cache holds its 260 MiB of code.
const MEMORY: u32 = 4096;

/// Seconds to wait unless `--timeout` says otherwise: three starts of a
/// browser, emulated when there is no KVM.
const TIMEOUT: u64 = 1800;

/// Where `scripts/fetch-chrome.sh` writes, unless `FERRIX_CHROME_VOLUME`
/// names another directory.
fn volume() -> Result<std::path::PathBuf> {
    let directory = match std::env::var_os("FERRIX_CHROME_VOLUME") {
        Some(directory) => std::path::PathBuf::from(directory),
        None => {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .ok_or_else(|| Error::new("neither HOME nor USERPROFILE is set"))?;
            std::path::PathBuf::from(home).join(".local/share/ferrix/chrome")
        }
    };
    let image = directory.join("chrome.img");
    if !image.is_file() {
        return Err(Error::new(format!(
            "{} is not there: scripts/fetch-chrome.sh makes it",
            image.display()
        )));
    }
    Ok(image)
}

/// The script, with the pages in it.
fn script() -> String {
    SCRIPT.replace("PAGE", PAGE).replace("PICTURE", PICTURE)
}

/// Boot a shell whose script runs Chrome three times.
///
/// # Errors
///
/// When the volume is missing, the image cannot be built, the boot fails, or
/// any step of the script does not do what it must.
pub(crate) fn test_chrome(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    if args.arches()?.iter().any(|&asked| asked != arch) {
        return Err(Error::new(
            "test-chrome runs on x86-64 only: Chrome for Testing publishes linux64 alone",
        ));
    }
    let mut args = args.clone();
    args.data_image = Some(volume()?);
    if !args.memory_given {
        args.memory = MEMORY;
    }
    if !args.timeout_given {
        args.timeout = TIMEOUT;
    }

    let shell =
        zinc::built(arch)?.ok_or_else(|| Error::new("zinc could not be built for x86-64"))?;
    println!("  {arch}: building an image whose shell runs headless Chrome");
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &shell, &script())?;
    let natives = native::build(arch, args.release)?;
    let bytes = std::fs::read(&shell)
        .map_err(|error| Error::new(format!("reading {}: {error}", shell.display())))?;
    let links = rustc::files(LINKS);
    // zinc alone: the script is builtins, and every program it runs is on
    // the volume.
    let archive = initramfs::build(None, &natives, Some(&bytes), &links)?;
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, None)?;

    println!(
        "  {arch}: running Chrome on Ferrix with {} MiB (timeout {}s)",
        args.memory, args.timeout
    );
    let lines = qemu::watch_then(arch, &image, &kernel, &args, crate::shell::EXITED, |_| {
        Ok(())
    })?;
    judge(arch, &lines)
}

/// Whether the transcript is a Chrome that loaded, ran a page's script,
/// drew a picture, and a script that got to its end.
fn judge(arch: Arch, lines: &[String]) -> Result<()> {
    let after_boot = lines
        .iter()
        .position(|line| line.contains(qemu::SUCCESS_MARKER))
        .and_then(|at| lines.get(at..))
        .unwrap_or_default();
    let exited = after_boot
        .iter()
        .find_map(|line| line.trim().strip_prefix(crate::shell::EXITED))
        .map(str::trim);
    let loaded = after_boot.iter().any(|line| line.contains(VERSION));
    let computed = after_boot.iter().any(|line| line.contains(COMPUTED));
    let drew = after_boot.iter().any(|line| line.trim_end() == SHOT);
    match exited {
        Some(status) if status == STATUS.to_string() && loaded && computed && drew => {
            println!(
                "  {arch}: Chrome {VERSION} loaded, ran a page's script and drew a screenshot \
                 on Ferrix"
            );
            Ok(())
        }
        Some("3") => Err(Error::new(format!("{arch}: `chrome --version` failed"))),
        Some("4") => Err(Error::new(format!(
            "{arch}: Chrome loaded but `--dump-dom` failed"
        ))),
        Some("5") => Err(Error::new(format!(
            "{arch}: Chrome ran a page but `--screenshot` failed"
        ))),
        Some("6") => Err(Error::new(format!(
            "{arch}: Chrome said it took a screenshot, and the file is empty or missing"
        ))),
        Some(status) => Err(Error::new(format!(
            "{arch}: the script exited with {status}; version {loaded}, script ran {computed}, \
             screenshot {drew}"
        ))),
        None => Err(Error::new(format!("{arch}: the shell never exited"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn the_script_carries_both_pages_and_no_placeholder() {
        let script = script();
        assert!(script.contains("\"computed \"+6*7"));
        // The source must not already hold what the DOM is required to.
        assert!(!script.contains(COMPUTED));
        assert!(script.contains("Hello from Chrome on Ferrix"));
        assert!(!script.contains("'PAGE'") && !script.contains("'PICTURE'"));
    }

    #[test]
    fn a_chrome_that_ran_passes() {
        let lines = transcript(&[
            qemu::SUCCESS_MARKER,
            VERSION,
            "<html><head></head><body><p id=\"v8\">computed 42</p><script>document.\
             getElementById(\"v8\").textContent=\"computed \"+6*7</script></body></html>",
            SHOT,
            "  init     the shell exited with 16",
        ]);
        assert!(judge(Arch::X86_64, &lines).is_ok());
    }

    #[test]
    fn a_dom_whose_script_never_ran_fails() {
        let lines = transcript(&[
            qemu::SUCCESS_MARKER,
            VERSION,
            "<html><head></head><body><p id=\"v8\"></p><script>document.getElementById(\"v8\").\
             textContent=\"computed \"+6*7</script></body></html>",
            SHOT,
            "  init     the shell exited with 16",
        ]);
        assert!(judge(Arch::X86_64, &lines).is_err());
    }

    #[test]
    fn each_failing_step_is_named() {
        for (status, words) in [
            ("3", "--version"),
            ("4", "--dump-dom"),
            ("5", "--screenshot"),
            ("6", "empty"),
        ] {
            let exit = format!("  init     the shell exited with {status}");
            let lines = transcript(&[qemu::SUCCESS_MARKER, &exit]);
            let error = judge(Arch::X86_64, &lines).unwrap_err().to_string();
            assert!(error.contains(words), "{status}: {error}");
        }
    }
}
