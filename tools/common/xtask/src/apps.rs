//! Apps: optional programs, each a folder of its own under `src/user/apps/`
//! (`docs/APPS.md`).
//!
//! Nothing here names an app. Every folder in [`PLACE`] is one, described by
//! its `app.toml`, which `ferrix-pkg` reads; this module builds each into a
//! package, installs packages into the images a person runs, gates each in
//! `check`, and boots them all once in `test-apps`. Adding an app is adding a
//! folder, and the check that nothing outside the folder names it is here
//! too (`docs/APPS.md` §4, rule 1).

mod new;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) use new::new_app;

use ferrix_cpio::{Archive, FileType};
use ferrix_pkg::manifest::{self, Abi, Build, FileSpec, Recipe};
use ferrix_pkg::plan::plan;
use ferrix_pkg::record::{self, Installed, Record};

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::{Error, Result, cargo, fat, initramfs, native, ports, qemu, shell, zinc};

/// Where apps are, from the workspace's root.
pub(crate) const PLACE: &str = "src/user/apps";

/// An app's manifest, in its folder.
const MANIFEST: &str = "app.toml";

/// The lints every app is held to, passed to its clippy: an app's manifest
/// carries no table of its own (`docs/APPS.md` §4, rule 3). The workspace's
/// rules that matter in a program nobody supervises -- no panics by
/// `unwrap`, `expect`, indexing or `panic!`, every `unsafe` argued and
/// alone -- and documentation. The root's `.clippy.toml` still applies, so
/// tests may `expect`.
const LINTS: &[&str] = &[
    "-D",
    "warnings",
    "-D",
    "missing_docs",
    "-D",
    "unsafe_op_in_unsafe_fn",
    "-D",
    "clippy::unwrap_used",
    "-D",
    "clippy::expect_used",
    "-D",
    "clippy::panic",
    "-D",
    "clippy::indexing_slicing",
    "-D",
    "clippy::undocumented_unsafe_blocks",
    "-D",
    "clippy::missing_safety_doc",
    "-D",
    "clippy::multiple_unsafe_ops_per_block",
];

/// An app: its folder and what its manifest says.
#[derive(Debug, Clone)]
pub(crate) struct App {
    /// Its folder.
    pub(crate) dir: PathBuf,
    /// Its `app.toml`.
    pub(crate) recipe: Recipe,
}

impl App {
    fn name(&self) -> &str {
        &self.recipe.package.name
    }

    fn builds_for(&self, arch: Arch) -> bool {
        self.recipe
            .package
            .arches
            .iter()
            .any(|name| name == arch.name())
    }
}

/// Every app, by name.
///
/// # Errors
///
/// A folder in [`PLACE`] without an `app.toml`, a manifest that does not
/// read, or one whose name is not its folder's.
pub(crate) fn discover() -> Result<Vec<App>> {
    let place = paths::workspace_root().join(PLACE);
    let Ok(entries) = fs::read_dir(&place) else {
        return Ok(Vec::new());
    };
    let mut apps = Vec::new();
    for entry in entries {
        let dir = entry
            .map_err(|error| Error::new(format!("{}: {error}", place.display())))?
            .path();
        if !dir.is_dir() {
            continue;
        }
        let path = dir.join(MANIFEST);
        let text = fs::read_to_string(&path).map_err(|error| {
            Error::new(format!(
                "{}: {error}; every folder in {PLACE} is an app, described by its {MANIFEST}",
                path.display()
            ))
        })?;
        let recipe = manifest::recipe(&text)
            .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
        let folder = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if recipe.package.name != folder {
            return Err(Error::new(format!(
                "{}: the app is named `{}`, and its folder `{folder}`; they are one name",
                path.display(),
                recipe.package.name
            )));
        }
        apps.push(App { dir, recipe });
    }
    apps.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(apps)
}

/// The folder of the app named `name`: for a system gate that builds an app
/// its own way (`test-badapple`'s negative control), and finds it by its
/// name, never its path (rule 1).
///
/// # Errors
///
/// No app of that name, or a manifest that does not read.
pub(crate) fn folder(name: &str) -> Result<PathBuf> {
    discover()?
        .into_iter()
        .find(|app| app.name() == name)
        .map(|app| app.dir)
        .ok_or_else(|| Error::new(format!("there is no app `{name}` in {PLACE}")))
}

/// `cargo xtask apps`: every app, and whether its manifest reads.
pub(crate) fn list() -> Result<()> {
    let apps = discover()?;
    for app in &apps {
        let package = &app.recipe.package;
        let image = if app.recipe.default {
            "default"
        } else {
            "opt-in"
        };
        println!(
            "{:<16} {:<10} {:<6} {:<7} {}  {}",
            package.name,
            package.version,
            package.abi.as_str(),
            image,
            package.arches.join(","),
            package.description
        );
    }
    println!("{} apps in {PLACE}", apps.len());
    Ok(())
}

/// Every `--app` names a folder in [`PLACE`].
fn every_app_named_is_one(apps: &[App], args: &Args) -> Result<()> {
    match args
        .apps
        .iter()
        .find(|name| !apps.iter().any(|app| app.name() == name.as_str()))
    {
        Some(unknown) => Err(Error::new(format!(
            "--app {unknown}: there is no {PLACE}/{unknown}"
        ))),
        None => Ok(()),
    }
}

/// The apps an image a person runs carries: the `default` ones and every
/// `--app`, or none with `--no-apps`; under `--everything`, every app; with
/// `defaults` false, only the `--app` ones.
fn selected(args: &Args, defaults: bool) -> Result<Vec<App>> {
    let apps = discover()?;
    every_app_named_is_one(&apps, args)?;
    if args.no_apps {
        return Ok(Vec::new());
    }
    Ok(apps
        .into_iter()
        .filter(|app| {
            (defaults && (app.recipe.default || args.everything))
                || args.apps.iter().any(|name| name == app.name())
        })
        .collect())
}

/// The files of the apps `args` select, built for `arch`, as an image a
/// person runs carries them: each app's package, installed.
///
/// # Errors
///
/// A build that fails, or a set of packages that does not install.
pub(crate) fn installed(arch: Arch, args: &Args) -> Result<Vec<ports::File>> {
    install_selected(arch, args, true)
}

/// [`installed`], but only the apps `--app` names: for an image a test
/// boots too, which carries no app it was not asked for.
///
/// # Errors
///
/// As [`installed`].
pub(crate) fn named(arch: Arch, args: &Args) -> Result<Vec<ports::File>> {
    install_selected(arch, args, false)
}

fn install_selected(arch: Arch, args: &Args, defaults: bool) -> Result<Vec<ports::File>> {
    // `--everything` is everything: a script app with no package is built,
    // and a build that fails stops the run.
    let fresh = if args.everything {
        Fresh::UnlessBuilt
    } else {
        Fresh::UnlessScript
    };
    let mut packages = Vec::new();
    for app in selected(args, defaults)? {
        if let Some(path) = package(&app, arch, args.release, fresh)? {
            packages.push(read(&path)?);
        }
    }
    let files = install(&packages)?;
    if !packages.is_empty() {
        println!("  {} apps installed, {} files", packages.len(), files.len());
    }
    Ok(files)
}

/// Whether a package is built for the asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fresh {
    /// Always: `build-apps` and `test-apps`.
    Always,
    /// Unless the app is built by a script, which is a download and minutes
    /// of C that no image starts on its own, as no image starts a port: the
    /// last package built is taken, and its absence said.
    UnlessScript,
    /// As [`Fresh::UnlessScript`], but a script app with no package yet is
    /// built: `--everything`, which leaves nothing out.
    UnlessBuilt,
}

/// Where `app`'s package for `arch` is written.
fn package_path(app: &App, arch: Arch) -> PathBuf {
    let name = app.name();
    paths::target_dir()
        .join("apps")
        .join(name)
        .join(arch.name())
        .join(format!(
            "{name}-{}-{}.fxpkg",
            app.recipe.package.version,
            arch.name()
        ))
}

/// Build `app` for `arch` and write its package, or `None` for an
/// architecture it is not built for or a build this host cannot make.
///
/// # Errors
///
/// A build that fails, a file its manifest names that the build did not
/// make, or a native program the kernel could not start.
pub(crate) fn package(
    app: &App,
    arch: Arch,
    release: bool,
    fresh: Fresh,
) -> Result<Option<PathBuf>> {
    let name = app.name();
    if !app.builds_for(arch) {
        println!("  {name} is not built for {arch}");
        return Ok(None);
    }
    let out = package_path(app, arch);
    if fresh != Fresh::Always && app.recipe.build == Build::Script {
        if out.is_file() {
            println!("  {name}: the package built last, {}", out.display());
            return Ok(Some(out));
        }
        if fresh == Fresh::UnlessScript {
            println!(
                "  {name} is not built for {arch}: `cargo xtask build-apps --arch {arch} --app {name}`"
            );
            return Ok(None);
        }
    }
    let Some(built) = build(app, arch, release)? else {
        return Ok(None);
    };
    let mut package = app.recipe.package.clone();
    package.arches = vec![arch.name().to_owned()];
    let record = Record {
        package,
        files: built
            .iter()
            .map(|(spec, bytes)| Installed::of(&spec.to, spec.mode, bytes))
            .collect(),
    };
    let text = record::render(&record);
    let record_path = record::path(name);
    let mut files: Vec<(&str, u32, &[u8])> = built
        .iter()
        .map(|(spec, bytes)| (spec.to.as_str(), spec.mode, bytes.as_slice()))
        .collect();
    files.push((&record_path, 0o644, text.as_bytes()));
    let mut directories: Vec<String> = Vec::new();
    for (path, _, _) in &files {
        let mut at = *path;
        while let Some((parent, _)) = at.rsplit_once('/') {
            directories.push(parent.to_owned());
            at = parent;
        }
    }
    directories.sort();
    directories.dedup();
    let directories: Vec<&str> = directories.iter().map(String::as_str).collect();
    let archive = initramfs::plain(&directories, &files)?;
    write(&out, &archive)?;
    Ok(Some(out))
}

/// `cargo xtask build-apps`: every app's package, or `--app`'s, built for
/// each `--arch`, scripts too.
///
/// # Errors
///
/// A build that fails, or an `--app` there is no folder for.
pub(crate) fn build_apps(args: &Args) -> Result<()> {
    let apps = discover()?;
    every_app_named_is_one(&apps, args)?;
    let wanted: Vec<&App> = apps
        .iter()
        .filter(|app| args.apps.is_empty() || args.apps.iter().any(|name| name == app.name()))
        .collect();
    for arch in args.arches()? {
        for app in &wanted {
            if let Some(path) = package(app, arch, args.release, Fresh::Always)? {
                println!("{}", path.display());
            }
        }
    }
    Ok(())
}

/// The files a build made: each as its manifest names it, and its bytes.
type Built = Vec<(FileSpec, Vec<u8>)>;

/// Each file `app`'s manifest installs, with its bytes, built for `arch`.
fn build(app: &App, arch: Arch, release: bool) -> Result<Option<Built>> {
    let name = app.name();
    let target_dir = paths::target_dir().join("apps").join(name);
    let out = match (app.recipe.build, app.recipe.package.abi) {
        (Build::Cargo, Abi::Native) => {
            let target = arch.kernel_target();
            let profile = if release { "release" } else { "debug" };
            let out = target_dir.join(target).join(profile);
            let mut build = crate::builds::Build::cargo(
                format!("cargo build ({name}) --target {target}"),
                &app.dir,
            )
            .args(["build", "--bins", "--target", target])
            .env("CARGO_TARGET_DIR", &target_dir);
            if release {
                build = build.args(["--release"]);
            }
            for spec in &app.recipe.files {
                build = build.output(out.join(&spec.from));
            }
            println!("  building {name} for {target}");
            build.run()?;
            out
        }
        (Build::Cargo, Abi::Linux) => {
            let Some(target) = zinc::target(arch) else {
                println!("  {name} is not built for {arch} yet: there is no musl target");
                return Ok(None);
            };
            let out = target_dir.join(target).join("release");
            let mut build = crate::builds::Build::cargo(
                format!("cargo build ({name}) --target {target}"),
                &app.dir,
            )
            .args(["build", "--release", "--bins", "--target", target])
            .env("CARGO_TARGET_DIR", &target_dir)
            // For zinc's reason: RUSTFLAGS replaces the flags every config
            // file up the tree would otherwise merge in.
            .env("RUSTFLAGS", zinc::RUSTFLAGS)
            // And the linker, so an app needs no `.cargo/config.toml` of
            // its own: the musl targets carry their C runtime, so rust-lld
            // is the whole toolchain on any host.
            .env(&linker_variable(target), "rust-lld");
            for spec in &app.recipe.files {
                build = build.output(out.join(&spec.from));
            }
            println!("  building {name} for {target}");
            build.run()?;
            out
        }
        (Build::Script, _) => {
            let out = target_dir.join(arch.name()).join("out");
            fs::create_dir_all(&out)
                .map_err(|error| Error::new(format!("{}: {error}", out.display())))?;
            if cfg!(windows) {
                // In WSL, as ferrousli's shared library is built, writing
                // into this checkout's target directory through `/mnt`.
                crate::wsl::require_toolchain(&format!(
                    "{name} is built by its build.sh with a Linux host's tools"
                ))?;
                let status = crate::wsl::bash(
                    &app.dir,
                    "exec bash build.sh \"$1\" \"$(wslpath -u \"$2\")\"",
                    &[arch.name(), &out.to_string_lossy()],
                )
                .stdin(std::process::Stdio::null())
                .status()
                .map_err(|error| Error::new(format!("could not run wsl.exe: {error}")))?;
                if !status.success() {
                    return Err(Error::new(format!(
                        "{name}'s build.sh {arch} in WSL: {status}"
                    )));
                }
            } else {
                let mut command = Command::new("bash");
                let _ = command
                    .current_dir(&app.dir)
                    .arg("build.sh")
                    .arg(arch.name())
                    .arg(&out);
                cargo::run(command, &format!("{name}'s build.sh"))?;
            }
            out
        }
    };
    let mut files = Vec::new();
    for spec in &app.recipe.files {
        let bytes = read(&out.join(&spec.from))?;
        if app.recipe.package.abi == Abi::Native && bytes.starts_with(b"\x7fELF") {
            native::verify(arch, &bytes).map_err(|why| {
                Error::new(format!(
                    "{name}'s {} for {arch} is not a program the kernel can start: {why}",
                    spec.from
                ))
            })?;
        }
        files.push((spec.clone(), bytes));
    }
    Ok(Some(files))
}

/// Install `packages` into a root: what an image carries, as `ports` files.
///
/// Read back out of each archive with the kernel's own cpio reader, as the
/// package manager will read them: every file must be its record's, with
/// the size and digest the record says, and the set must install whole
/// (`ferrix_pkg::plan`) before one file is taken.
fn install(packages: &[Vec<u8>]) -> Result<Vec<ports::File>> {
    let mut records = Vec::new();
    for bytes in packages {
        records.push(package_record(bytes)?);
    }
    let order = plan(&records).map_err(|error| Error::new(format!("apps: {error}")))?;
    let mut files = Vec::new();
    for at in order {
        let (Some(bytes), Some(record)) = (packages.get(at), records.get(at)) else {
            continue;
        };
        let name = &record.package.name;
        let record_path = record::path(name);
        for entry in Archive::new(bytes).entries() {
            let entry =
                entry.map_err(|error| Error::new(format!("{name}'s package: {error:?}")))?;
            if entry.file_type() != FileType::Regular {
                continue;
            }
            if entry.name != record_path {
                let listed = record
                    .files
                    .iter()
                    .find(|file| file.path == entry.name)
                    .ok_or_else(|| {
                        Error::new(format!(
                            "{name}'s package holds {}, which its record does not list",
                            entry.name
                        ))
                    })?;
                if *listed != Installed::of(entry.name, entry.mode & 0o7777, entry.data) {
                    return Err(Error::new(format!(
                        "{name}'s package holds {} unlike its record says",
                        entry.name
                    )));
                }
            }
            files.push(ports::File {
                path: entry.name.to_owned(),
                mode: entry.mode & 0o7777,
                content: ports::Content::Bytes(entry.data.to_vec()),
            });
        }
    }
    Ok(files)
}

/// The record inside a package.
fn package_record(bytes: &[u8]) -> Result<Record> {
    let prefix = format!("{}/", record::RECORDS);
    for entry in Archive::new(bytes).entries() {
        let entry = entry.map_err(|error| Error::new(format!("a package: {error:?}")))?;
        if entry.file_type() == FileType::Regular && entry.name.starts_with(&prefix) {
            let text = std::str::from_utf8(entry.data)
                .map_err(|_| Error::new(format!("{} is not text", entry.name)))?;
            return record::parse(text)
                .map_err(|error| Error::new(format!("{}: {error}", entry.name)));
        }
    }
    Err(Error::new("a package without a record"))
}

/// Cargo's variable for `target`'s linker: `CARGO_TARGET_<TRIPLE>_LINKER`.
pub(crate) fn linker_variable(target: &str) -> String {
    format!(
        "CARGO_TARGET_{}_LINKER",
        target.to_ascii_uppercase().replace('-', "_")
    )
}

/// `cargo` in `app`'s folder, into the app's own target directory under
/// the tree's, where its builds go and CI's cache finds it: on Windows, a
/// Linux app's through WSL, which keeps its own.
fn cargo_in(app: &App, arguments: &[&str]) -> Command {
    if cfg!(windows) && app.recipe.package.abi == Abi::Linux {
        return crate::wsl::cargo(&app.dir, arguments);
    }
    let mut command = Command::new(cargo::cargo());
    let _ = command.current_dir(&app.dir).args(arguments).env(
        "CARGO_TARGET_DIR",
        paths::target_dir().join("apps").join(app.name()),
    );
    command
}

/// Rule 1: nothing outside `app`'s folder names it.
pub(crate) fn stays_in_its_folder(app: &App) -> Result<()> {
    let folder = format!("{PLACE}/{}", app.name());
    let output = Command::new("git")
        .current_dir(paths::workspace_root())
        .args(["grep", "--untracked", "-l", "-F", &folder, "--", "."])
        .arg(format!(":(exclude){folder}"))
        .output()
        .map_err(|error| Error::new(format!("could not run git grep: {error}")))?;
    match output.status.code() {
        Some(1) => Ok(()),
        Some(0) => Err(Error::new(format!(
            "these files name {folder}, and an app changes nothing outside its folder \
             (docs/APPS.md §4):\n{}",
            String::from_utf8_lossy(&output.stdout)
        ))),
        _ => Err(Error::new(format!(
            "git grep failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))),
    }
}

/// `cargo fmt --check` in `app`'s folder; for an app built by a script,
/// `bash -n` over the script, which is what can be said of it without
/// running it.
pub(crate) fn formatting(app: &App) -> Result<()> {
    if app.recipe.build == Build::Script {
        let mut command = Command::new("bash");
        let _ = command.current_dir(&app.dir).args(["-n", "build.sh"]);
        return cargo::run(command, &format!("{}: bash -n build.sh", app.name()));
    }
    cargo::run(
        cargo_in(app, &["fmt", "--check"]),
        &format!("{}: cargo fmt", app.name()),
    )
}

/// The host's clippy and tests over `app`'s lib target, when its manifest
/// asks for them.
pub(crate) fn host(app: &App) -> Result<()> {
    if !app.recipe.host_tests {
        println!("  {} has no host tests", app.name());
        return Ok(());
    }
    let mut clippy = vec!["clippy", "--lib", "--tests", "--"];
    clippy.extend(LINTS);
    cargo::run(
        cargo_in(app, &clippy),
        &format!("{}: cargo clippy", app.name()),
    )?;
    cargo::run(
        cargo_in(app, &["test", "--lib"]),
        &format!("{}: cargo test", app.name()),
    )
}

/// Clippy over `app`'s programs for each target it is built for: none for
/// an app built by a script, which has no cargo workspace.
pub(crate) fn targets(app: &App) -> Result<()> {
    if app.recipe.build == Build::Script {
        println!(
            "  {} is built by its build.sh: nothing for clippy",
            app.name()
        );
        return Ok(());
    }
    for arch in Arch::ALL {
        if !app.builds_for(arch) {
            continue;
        }
        let target = match app.recipe.package.abi {
            Abi::Native => arch.kernel_target(),
            Abi::Linux => match zinc::target(arch) {
                Some(target) => target,
                None => continue,
            },
        };
        let mut clippy = vec!["clippy", "--bins", "--target", target, "--"];
        clippy.extend(LINTS);
        let command = cargo_in(app, &clippy);
        cargo::run(
            command,
            &format!("{}: cargo clippy --target {target}", app.name()),
        )?;
    }
    Ok(())
}

/// What `test-apps` prints before the apps run.
const STARTED: &str = "apps: started";

/// The line an app's `index`th smoke check prints when it passes.
fn passed(name: &str, index: usize) -> String {
    format!("apps: ok {name} {index}")
}

/// `text` quoted for the shell, whole.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The script `test-apps`' shell runs: each smoke command, its output read
/// line by line for the one it expects.
fn script(apps: &[App]) -> String {
    let mut script = format!("PATH=/bin:/sbin:/usr/bin\nexport PATH\necho \"{STARTED}\"\n");
    for app in apps {
        for (index, smoke) in app.recipe.smoke.iter().enumerate() {
            script.push_str(&format!(
                "{} | while read -r line; do case \"$line\" in {}*) echo \"{}\";; esac; done\n",
                smoke.run,
                quoted(&smoke.expect),
                passed(app.name(), index)
            ));
        }
    }
    script.push_str(&format!("exit {}\n", shell::STATUS));
    script
}

/// `cargo xtask test-apps`: one boot that runs every app's smoke checks.
///
/// # Errors
///
/// A build that fails, or a check whose line did not come.
pub(crate) fn test_apps(args: &Args) -> Result<()> {
    for arch in args.arches()? {
        let apps: Vec<App> = discover()?
            .into_iter()
            .filter(|app| app.builds_for(arch) && !app.recipe.smoke.is_empty())
            .collect();
        let mut packages = Vec::new();
        let mut tested = Vec::new();
        for app in apps {
            if let Some(path) = package(&app, arch, args.release, Fresh::Always)? {
                packages.push(read(&path)?);
                tested.push(app);
            }
        }
        if tested.is_empty() {
            println!("{arch}: no app has a smoke check");
            continue;
        }
        let files = install(&packages)?;
        let program = zinc::built(arch)?.ok_or_else(|| {
            Error::new(format!(
                "zinc is not built for {arch}, and test-apps runs its checks in it"
            ))
        })?;
        let loader = cargo::build_loader(arch, args.release)?;
        let kernel = cargo::build_kernel_with_init(arch, args.release, &program, &script(&tested))?;
        let natives = native::build(arch, args.release)?;
        let initramfs = initramfs::build(None, &natives, None, &files)?;
        let image = fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
        let lines = qemu::watch_then(arch, &image, &kernel, args, shell::EXITED, |_| Ok(()))?;
        let mut missing: Vec<String> = Vec::new();
        let mut wanted = vec![STARTED.to_owned()];
        for app in &tested {
            wanted.extend((0..app.recipe.smoke.len()).map(|index| passed(app.name(), index)));
        }
        for want in wanted {
            if !lines.iter().any(|line| line.trim_end().ends_with(&want)) {
                missing.push(want);
            }
        }
        if !missing.is_empty() {
            let tail: Vec<&str> = lines
                .iter()
                .rev()
                .take(30)
                .rev()
                .map(String::as_str)
                .collect();
            return Err(Error::new(format!(
                "{arch}: test-apps is missing {}\n  The boot's last lines:\n    {}",
                missing.join(", "),
                tail.join("\n    ")
            )));
        }
        println!("{arch}: {} apps' smoke checks passed", tested.len());
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| Error::new(format!("{}: {error}", parent.display())))?;
    }
    fs::write(path, bytes)
        .map_err(|error| Error::new(format!("writing {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::{App, install, quoted, script};
    use ferrix_pkg::manifest;
    use ferrix_pkg::record::{self, Installed, Record};

    fn app(name: &str, smoke: &str) -> App {
        let text = format!(
            "[package]\nname = \"{name}\"\nversion = \"1.0\"\ndescription = \"\"\nabi = \"native\"\n\
             arches = [\"x86_64\"]\n\n[[package.files]]\nfrom = \"{name}\"\nto = \"bin/{name}\"\nmode = \"755\"\n{smoke}"
        );
        App {
            dir: std::path::PathBuf::new(),
            recipe: manifest::recipe(&text).expect("a manifest"),
        }
    }

    #[test]
    fn the_real_apps_read_and_stay_in_their_folders() {
        for app in super::discover().expect("every app's manifest reads") {
            super::stays_in_its_folder(&app).expect("nothing outside the folder names it");
        }
    }

    #[test]
    fn everything_selects_every_app() {
        let every = super::discover().expect("every app's manifest reads").len();
        let mut args = crate::args::Args::default();
        let defaults = super::selected(&args, true).expect("the defaults").len();
        assert!(defaults < every, "an opt-in app to leave out");
        args.everything = true;
        assert_eq!(
            super::selected(&args, true).expect("every app").len(),
            every
        );
        assert!(super::selected(&args, false).expect("none").is_empty());
    }

    #[test]
    fn the_smoke_script_quotes_what_it_expects() {
        assert_eq!(quoted("it's"), "'it'\\''s'");
        let smoke = "\n[[smoke]]\nrun = \"a --version\"\nexpect = \"a 1.0 'x'\"\n";
        let text = script(&[app("a", smoke)]);
        assert!(text.contains(
            "a --version | while read -r line; do case \"$line\" in 'a 1.0 '\\''x'\\'''*) \
             echo \"apps: ok a 0\";; esac; done"
        ));
        assert!(text.starts_with("PATH=/bin:/sbin:/usr/bin\n"));
        assert!(text.ends_with("exit 7\n"));
    }

    /// A package of `name` holding `files`, with `listed` as its record's.
    fn package(name: &str, files: &[(&str, &[u8])], listed: &[(&str, &[u8])]) -> Vec<u8> {
        let mut package = app(name, "").recipe.package;
        package.arches = vec!["x86_64".to_owned()];
        let record = Record {
            package,
            files: listed
                .iter()
                .map(|(path, bytes)| Installed::of(path, 0o755, bytes))
                .collect(),
        };
        let text = record::render(&record);
        let path = record::path(name);
        let mut entries: Vec<(&str, u32, &[u8])> = files
            .iter()
            .map(|(path, bytes)| (*path, 0o755, *bytes))
            .collect();
        entries.push((&path, 0o644, text.as_bytes()));
        crate::initramfs::plain(
            &["bin", "lib", "lib/ferrix", "lib/ferrix/packages"],
            &entries,
        )
        .expect("an archive")
    }

    #[test]
    fn install_takes_what_the_record_vouches_for() {
        let good = package("a", &[("bin/a", b"one")], &[("bin/a", b"one")]);
        let files = install(&[good]).expect("it installs");
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["bin/a", "lib/ferrix/packages/a.toml"]);
        let changed = package("a", &[("bin/a", b"two")], &[("bin/a", b"one")]);
        assert!(install(&[changed]).is_err(), "a file unlike its record");
        let unlisted = package(
            "a",
            &[("bin/a", b"one"), ("bin/b", b"")],
            &[("bin/a", b"one")],
        );
        assert!(
            install(&[unlisted]).is_err(),
            "a file its record does not list"
        );
        let both = [
            package("a", &[("bin/x", b"")], &[("bin/x", b"")]),
            package("b", &[("bin/x", b"")], &[("bin/x", b"")]),
        ];
        assert!(install(&both).is_err(), "two packages owning one path");
    }
}
