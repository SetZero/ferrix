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
//! # `--no-sandbox`, and the zygote
//!
//! `--no-sandbox` because the sandbox is namespaces and seccomp, which Ferrix
//! has neither of: `clone` refuses `CLONE_NEW*` rather than pretend
//! (`docs/CHROME.md` §2.4). The zygote, the process Chrome forks its
//! renderers from instead of starting each afresh, runs as it does on Linux
//! since 2026-09-26. Until then it reported that it could not fork, and the
//! tests ran with `--no-zygote`: it learns each child's pid from the
//! credentials the child's first message carries (`SCM_CREDENTIALS`, on a
//! socket whose reader set `SO_PASSCRED`), and Ferrix passed none.
//!
//! # Where Chrome lives
//!
//! The volume carries no `ferrix-root` label, so the kernel mounts it at
//! `/data`, as test-rustc's is, attached under QEMU's `snapshot=on` so a run
//! never changes it. glibc names its paths absolutely, so the initramfs
//! carries a symbolic link for each into `/data` ([`LINKS`]).
//!
//! # On ferrousli
//!
//! With `--interpreter ferrousli --library ferrousli`, as `test-shell` takes
//! them, the same program runs on ferrousli's loader and `libc.so.6` in
//! glibc's place: the loader goes where Chrome's `PT_INTERP` names, instead
//! of the volume's link to Debian's, and `libc.so.6` into `/lib`, which
//! `LD_LIBRARY_PATH` puts before the volume's libraries. glibc's other names,
//! `libm.so.6` and the rest, the loader answers with ferrousli whatever the
//! volume holds. The forty other libraries are Debian's as before.
//!
//! # Why x86-64 only
//!
//! Chrome for Testing publishes linux64 only.

use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result, cargo, fat, initramfs, native, qemu, rustc, shell, zinc};

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
$chrome --no-sandbox --dump-dom 'PAGE' || exit 4
$chrome --no-sandbox --screenshot=/tmp/shot.png --window-size=640,360 'PICTURE' || exit 5
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
pub(crate) const LINKS: &[(&str, &str)] = &[
    ("lib64", "/data/usr/lib64"),
    ("lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("usr/lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("etc/fonts", "/data/etc/fonts"),
    ("usr/share/fonts", "/data/usr/share/fonts"),
    ("usr/share/fontconfig", "/data/usr/share/fontconfig"),
];

/// Guest memory unless `--memory` says otherwise: Chrome wants about two
/// GiB to open a page, and the page cache holds its 260 MiB of code.
pub(crate) const MEMORY: u32 = 4096;

/// Seconds to wait unless `--timeout` says otherwise: three starts of a
/// browser, emulated when there is no KVM.
const TIMEOUT: u64 = 1800;

/// What the browser in a window says it is: Chrome's own user agent, reduced
/// as Chrome reduces it, with Ferrix in the platform, which says it is not
/// Linux in words a site that looks for `Linux x86_64` still finds.
///
/// `(Ferrix x86_64)` alone had Google's search answer with its page for a
/// browser it no longer supports, every time: a site that knows the
/// platforms it serves does not know Ferrix. With the words `Linux x86_64`
/// in it, as Ubuntu's Firefox said `X11; Ubuntu; Linux x86_64` for years,
/// the same search is answered as Chrome's own user agent is.
pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (X11; Ferrix; not Linux x86_64) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36";

/// `--user-agent` changes the user agent and the `User-Agent` header, and
/// nothing else. `navigator.platform` stays `Linux x86_64`, and the client
/// hints -- `navigator.userAgentData` and the `Sec-CH-UA-Platform` header --
/// stay `Linux`. Those are constants in Chrome's build, whatever `uname`
/// says, and no switch reaches them: saying Ferrix there takes a Chromium
/// built from source.
/// Where the image carries the tree's `fonts/`: Inter, Liberation and the
/// fontconfig file that adds them to the system's, which `FONTCONFIG_FILE`
/// names ([`WINDOW_ENV`]). `fonts/README.md` says what each face is for and
/// where it came from.
const FONTS: &str = "usr/share/ferrix/fonts";

/// `fonts/`'s files, by their path under it.
const FONT_FILES: &[(&str, &[u8])] = &[
    ("fonts.conf", include_bytes!("../../fonts/fonts.conf")),
    (
        "inter/InterVariable.ttf",
        include_bytes!("../../fonts/inter/InterVariable.ttf"),
    ),
    (
        "inter/InterVariable-Italic.ttf",
        include_bytes!("../../fonts/inter/InterVariable-Italic.ttf"),
    ),
    ("inter/LICENSE", include_bytes!("../../fonts/inter/LICENSE")),
    (
        "liberation/LiberationSans-Regular.ttf",
        include_bytes!("../../fonts/liberation/LiberationSans-Regular.ttf"),
    ),
    (
        "liberation/LiberationSans-Bold.ttf",
        include_bytes!("../../fonts/liberation/LiberationSans-Bold.ttf"),
    ),
    (
        "liberation/LiberationSans-Italic.ttf",
        include_bytes!("../../fonts/liberation/LiberationSans-Italic.ttf"),
    ),
    (
        "liberation/LiberationSans-BoldItalic.ttf",
        include_bytes!("../../fonts/liberation/LiberationSans-BoldItalic.ttf"),
    ),
    (
        "liberation/LiberationSerif-Regular.ttf",
        include_bytes!("../../fonts/liberation/LiberationSerif-Regular.ttf"),
    ),
    (
        "liberation/LiberationSerif-Bold.ttf",
        include_bytes!("../../fonts/liberation/LiberationSerif-Bold.ttf"),
    ),
    (
        "liberation/LiberationSerif-Italic.ttf",
        include_bytes!("../../fonts/liberation/LiberationSerif-Italic.ttf"),
    ),
    (
        "liberation/LiberationSerif-BoldItalic.ttf",
        include_bytes!("../../fonts/liberation/LiberationSerif-BoldItalic.ttf"),
    ),
    (
        "liberation/LiberationMono-Regular.ttf",
        include_bytes!("../../fonts/liberation/LiberationMono-Regular.ttf"),
    ),
    (
        "liberation/LiberationMono-Bold.ttf",
        include_bytes!("../../fonts/liberation/LiberationMono-Bold.ttf"),
    ),
    (
        "liberation/LiberationMono-Italic.ttf",
        include_bytes!("../../fonts/liberation/LiberationMono-Italic.ttf"),
    ),
    (
        "liberation/LiberationMono-BoldItalic.ttf",
        include_bytes!("../../fonts/liberation/LiberationMono-BoldItalic.ttf"),
    ),
    (
        "liberation/LICENSE",
        include_bytes!("../../fonts/liberation/LICENSE"),
    ),
];

/// The files an image with the browser in a window carries beside the
/// volume's links: the fonts.
pub(crate) fn window_files() -> Vec<crate::ports::File> {
    FONT_FILES
        .iter()
        .map(|(name, bytes)| crate::ports::File {
            path: format!("{FONTS}/{name}"),
            mode: 0o644,
            content: crate::ports::Content::Bytes(bytes.to_vec()),
        })
        .collect()
}

/// The command that starts the full browser in a window on the compositor,
/// showing `page`, which may hold no spaces or quotes: it is one word of the
/// command, which the compositor splits as a shell would.
///
/// `--ozone-platform=wayland` makes Chrome a Wayland client, drawing through
/// `wl_shm`; `--disable-gpu` keeps its GPU process to software, since the
/// render node is the compositor's. `--user-data-dir` is in `/dev/shm`,
/// which is tmpfs whatever the root is -- on the desktop's persistent btrfs
/// root too since 2026-09-26, when the kernel began mounting one there; before
/// that, it was devfs's bare directory there, and Chrome stopped at once.
/// `--no-sandbox` for the reason at the top of this file. `--disable-infobars`
/// takes away the bar Chrome for Testing shows under the toolbar, saying it
/// is for automated testing only. [`USER_AGENT`] says Ferrix where Chrome
/// says Linux.
pub(crate) fn window_command(page: &str) -> String {
    format!(
        "/data/chrome-window/chrome --no-sandbox --ozone-platform=wayland \
         --user-data-dir=/dev/shm/chrome --no-first-run --disable-gpu --disable-crash-reporter \
         --disable-breakpad --enable-logging=stderr --disable-infobars \
         '--user-agent={USER_AGENT}' {page}"
    )
}

/// The environment the compositor gives Chrome, as `env =` lines: a home in
/// tmpfs, for [`window_command`]'s reason -- NSS keeps its database there --
/// a runtime directory that can be written, and [`FONTS`]'s fontconfig file,
/// which includes the system's. The compositor gives these to every program
/// it starts, so foot draws with the same fonts file.
pub(crate) const WINDOW_ENV: &str = "env = HOME,/dev/shm\nenv = XDG_RUNTIME_DIR,/tmp\n\
     env = FONTCONFIG_FILE,/usr/share/ferrix/fonts/fonts.conf\n";

/// Where `scripts/fetch-chrome.sh` writes, unless `FERRIX_CHROME_VOLUME`
/// names another directory.
pub(crate) fn volume() -> Result<std::path::PathBuf> {
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

/// The script, with the pages in it, and for ferrousli the search path that
/// finds its `libc.so.6` in `/lib` before the volume's.
fn script(ferrousli: bool) -> String {
    let script = SCRIPT.replace("PAGE", PAGE).replace("PICTURE", PICTURE);
    if ferrousli {
        format!("export LD_LIBRARY_PATH=/lib:/lib/x86_64-linux-gnu\n{script}")
    } else {
        script
    }
}

/// Chrome as `scripts/fetch-chrome.sh` unpacked it beside the volume, whose
/// `PT_INTERP` says where a loader of ferrousli's must go.
fn program_on_host(volume: &std::path::Path) -> Result<std::path::PathBuf> {
    let program = volume
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("tree/chrome/chrome-headless-shell");
    if program.is_file() {
        Ok(program)
    } else {
        Err(Error::new(format!(
            "{} is not there: scripts/fetch-chrome.sh leaves it beside the volume",
            program.display()
        )))
    }
}

/// `test-chrome` or `test-chrome-window`, whichever `command` names: the
/// browser headless, or in a window on the compositor
/// ([`crate::compositor::test_chrome_window`]).
///
/// # Errors
///
/// As the one it runs.
pub(crate) fn run(command: &str, args: &Args) -> Result<()> {
    if command == "test-chrome-window" {
        crate::compositor::test_chrome_window(args)
    } else {
        test_chrome(args)
    }
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
    let volume = volume()?;
    args.data_image = Some(volume.clone());
    let ferrousli = args.interpreter.is_some() || !args.libraries.is_empty();
    if !args.memory_given {
        args.memory = MEMORY;
    }
    if !args.timeout_given {
        args.timeout = TIMEOUT;
    }

    let shell =
        zinc::built(arch)?.ok_or_else(|| Error::new("zinc could not be built for x86-64"))?;
    let libc = if ferrousli { "ferrousli" } else { "glibc" };
    println!("  {arch}: building an image whose shell runs headless Chrome on {libc}");
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &shell, &script(ferrousli))?;
    let natives = native::build(arch, args.release)?;
    let bytes = std::fs::read(&shell)
        .map_err(|error| Error::new(format!("reading {}: {error}", shell.display())))?;
    let links = if ferrousli {
        // The loader takes `/lib64`'s place, so the link to the volume's goes.
        let kept: Vec<_> = LINKS
            .iter()
            .copied()
            .filter(|(path, _)| *path != "lib64")
            .collect();
        let mut files = rustc::files(&kept);
        files.extend(shell::carried_for(arch, &program_on_host(&volume)?, &args)?);
        files
    } else {
        rustc::files(LINKS)
    };
    // zinc alone: the script is builtins, and every program it runs is on
    // the volume.
    let archive = initramfs::build(None, &natives, Some(&bytes), &links)?;
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, None)?;

    println!(
        "  {arch}: running Chrome on Ferrix with {} MiB (timeout {}s)",
        args.memory, args.timeout
    );
    let lines = qemu::watch_then(arch, &image, &kernel, &args, shell::EXITED, |_| Ok(()))?;
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
        .find_map(|line| line.trim().strip_prefix(shell::EXITED))
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
        let script = script(false);
        assert!(script.contains("\"computed \"+6*7"));
        // The source must not already hold what the DOM is required to.
        assert!(!script.contains(COMPUTED));
        assert!(script.contains("Hello from Chrome on Ferrix"));
        assert!(!script.contains("'PAGE'") && !script.contains("'PICTURE'"));
    }

    #[test]
    fn the_user_agent_is_one_quoted_word_of_the_window_command() {
        let command = window_command("data:text/html,x");
        let quoted: Vec<&str> = command.split('\'').collect();
        assert_eq!(quoted.len(), 3, "{command}");
        assert_eq!(quoted[1], format!("--user-agent={USER_AGENT}"));
        // Ferrix by name, and the words a site looking for Linux looks for.
        assert!(USER_AGENT.contains("(X11; Ferrix; not Linux x86_64)"));
    }

    #[test]
    fn fontconfig_is_pointed_at_the_carried_fonts() {
        let conf = String::from_utf8_lossy(FONT_FILES[0].1);
        assert_eq!(FONT_FILES[0].0, "fonts.conf");
        assert!(WINDOW_ENV.contains(&format!("env = FONTCONFIG_FILE,/{FONTS}/fonts.conf\n")));
        assert!(conf.contains(&format!("<dir>/{FONTS}</dir>")));
        assert!(conf.contains("<include ignore_missing=\"yes\">/etc/fonts/fonts.conf</include>"));
        let paths: Vec<_> = window_files().into_iter().map(|file| file.path).collect();
        for face in [
            "inter/InterVariable.ttf",
            "liberation/LiberationSans-Regular.ttf",
        ] {
            assert!(paths.contains(&format!("{FONTS}/{face}")), "{face}");
        }
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
