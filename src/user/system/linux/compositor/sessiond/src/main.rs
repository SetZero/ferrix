//! `sessiond --user NAME -- PROGRAM [ARGUMENT...]`: seat0's owner
//! (`docs/AUTH.md` §6.2, P2.4).
//!
//! It runs as root, as a unit of `graphical.target`, and starts the
//! graphical session: `PROGRAM`, the compositor, as the account `NAME` --
//! its groups, its gid and its uid, its home as the working directory, and
//! `HOME`, `USER`, `LOGNAME`, `SHELL` and `XDG_RUNTIME_DIR` set, the last a
//! `/run/user/<uid>` it makes `0700` and the account's own. Every program the
//! compositor starts is then that account's too.
//!
//! The devices stay `0660 root`. The compositor inherits one end of a socket
//! pair as descriptor 3, named by `FERRIX_SEAT_FD`, and asks over it for a
//! card, a render node or an input node; this opens it and hands the
//! descriptor back (`compositor_seat`). No path leads to the channel, so no
//! other program of the account can ask for the keyboard.
//!
//! The session ends with its compositor (§6.4): this exits with the
//! compositor's status, and init's stop of the unit ends whatever the
//! session left running. An image that logs a named user in at once
//! (decision 8) is the one case built; a greeter would choose the account
//! instead of `--user`.

use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, chown};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitCode};
use std::time::Duration;

use compositor_socket::{Connection, RecvError};
use compositor_wire::Fd;

const USAGE: &str = "usage: sessiond --user NAME -- PROGRAM [ARGUMENT...]\n\
    \n\
    Start PROGRAM, the session's compositor, as the account NAME, and hand it\n\
    seat0's card, render node and input nodes when it asks.\n";

/// Where each account's runtime directory goes.
const RUNTIME: &str = "/run/user";

/// What a home starts with: each file copied in once, when it is not there.
const SKEL: &str = "/etc/skel";

/// What the image puts under `/home` itself -- the host paths a carried
/// configuration names -- which the home disk's mount hides: copied back
/// in place at every start.
const HOME_COPY: &str = "/usr/share/ferrix/home";

/// The deepest a seeded tree goes, against a loop of links.
const DEEPEST: usize = 16;

/// An account, from `/etc/passwd` and `/etc/group`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Account {
    name: String,
    uid: u32,
    gid: u32,
    home: String,
    shell: String,
    /// Its supplementary groups: every group naming it, and its own.
    groups: Vec<u32>,
}

/// Find `name` in the text of `/etc/passwd` and `/etc/group`.
fn account(name: &str, passwd: &str, group: &str) -> Option<Account> {
    let (uid, gid, home, shell) = passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let [user, _, uid, gid, _, home, shell, ..] = fields.as_slice() else {
            return None;
        };
        (*user == name).then(|| {
            Some((
                uid.parse::<u32>().ok()?,
                gid.parse::<u32>().ok()?,
                (*home).to_owned(),
                (*shell).to_owned(),
            ))
        })?
    })?;
    let mut groups = vec![gid];
    for line in group.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        let [_, _, id, members, ..] = fields.as_slice() else {
            continue;
        };
        if members.split(',').any(|member| member == name)
            && let Ok(id) = id.parse::<u32>()
            && !groups.contains(&id)
        {
            groups.push(id);
        }
    }
    Some(Account {
        name: name.to_owned(),
        uid,
        gid,
        home,
        shell,
        groups,
    })
}

/// The command line: the account and the program with its arguments.
fn parse(args: &[String]) -> Option<(String, Vec<String>)> {
    let [flag, user, dashes, program @ ..] = args else {
        return None;
    };
    (flag == "--user" && dashes == "--" && !program.is_empty() && !user.is_empty())
        .then(|| (user.clone(), program.to_vec()))
}

fn say(line: &str) {
    let _ = writeln!(std::io::stdout(), "sessiond: {line}");
}

/// `/run/user/<uid>`, made if it is not there, `0700` and the account's.
fn runtime_dir(account: &Account) -> std::io::Result<String> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(RUNTIME)?;
    let path = format!("{RUNTIME}/{}", account.uid);
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    chown(&path, Some(account.uid), Some(account.gid))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

/// Copy the tree at `from` into `into`: a file that is not there, or every
/// file when `refresh`; directories made as needed. With `owner`, everything
/// made is given to it, so a home's seeds are the account's. Gives how many
/// files were copied. What cannot be copied is said and passed over: a home
/// missing a default is still a session.
fn seed(
    from: &std::path::Path,
    into: &std::path::Path,
    owner: Option<(u32, u32)>,
    refresh: bool,
    depth: usize,
) -> usize {
    let Ok(entries) = std::fs::read_dir(from) else {
        return 0;
    };
    if depth > DEEPEST {
        return 0;
    }
    let give = |path: &std::path::Path| {
        if let Some((uid, gid)) = owner {
            let _ = std::os::unix::fs::lchown(path, Some(uid), Some(gid));
        }
    };
    let mut copied = 0;
    for entry in entries.flatten() {
        let source = entry.path();
        let target = into.join(entry.file_name());
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !target.exists() {
                if let Err(error) = std::fs::create_dir_all(&target) {
                    say(&format!("{}: {error}", target.display()));
                    continue;
                }
                give(&target);
            }
            copied += seed(&source, &target, owner, refresh, depth + 1);
        } else if kind.is_symlink() {
            if std::fs::symlink_metadata(&target).is_err()
                && let Ok(link) = std::fs::read_link(&source)
                && std::os::unix::fs::symlink(&link, &target).is_ok()
            {
                give(&target);
                copied += 1;
            }
        } else if refresh || std::fs::symlink_metadata(&target).is_err() {
            match std::fs::copy(&source, &target) {
                Ok(_) => {
                    give(&target);
                    copied += 1;
                }
                Err(error) => say(&format!("{}: {error}", target.display())),
            }
        }
    }
    copied
}

fn run(args: &[String]) -> Result<i32, String> {
    let (name, program) = parse(args).ok_or_else(|| USAGE.to_owned())?;
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let group = std::fs::read_to_string("/etc/group").unwrap_or_default();
    let account =
        account(&name, &passwd, &group).ok_or_else(|| format!("{name} is not in /etc/passwd"))?;
    if account.uid == 0 {
        return Err(format!(
            "{name} is root, whose session needs no sessiond: start the compositor itself"
        ));
    }
    let runtime = runtime_dir(&account).map_err(|error| format!("{RUNTIME}: {error}"))?;
    let seeded = seed(
        std::path::Path::new(SKEL),
        std::path::Path::new(&account.home),
        Some((account.uid, account.gid)),
        false,
        0,
    );
    if seeded > 0 {
        say(&format!(
            "{seeded} file(s) from {SKEL} put in {}",
            account.home
        ));
    }
    let _ = seed(
        std::path::Path::new(HOME_COPY),
        std::path::Path::new("/home"),
        None,
        true,
        0,
    );
    let (ours, theirs) = UnixStream::pair().map_err(|error| format!("socketpair: {error}"))?;
    let child = start(&account, &program, &runtime, theirs)?;
    say(&format!(
        "seat0's session for {} (uid {}): {}, pid {}",
        account.name,
        account.uid,
        program.join(" "),
        child.id()
    ));
    serve(ours, child)
}

/// Start the compositor as `account`, with `theirs` as its descriptor 3.
fn start(
    account: &Account,
    program: &[String],
    runtime: &str,
    theirs: UnixStream,
) -> Result<std::process::Child, String> {
    let (first, rest) = program.split_first().ok_or_else(|| USAGE.to_owned())?;
    let channel = OwnedFd::from(theirs);
    let raw = channel.as_raw_fd();
    let ids = (account.uid, account.gid, account.groups.clone());
    let mut command = Command::new(first);
    let _ = command
        .args(rest)
        .current_dir(&account.home)
        .env("HOME", &account.home)
        .env("USER", &account.name)
        .env("LOGNAME", &account.name)
        .env("SHELL", &account.shell)
        .env("XDG_RUNTIME_DIR", runtime)
        .env(
            compositor_seat::FD_VARIABLE,
            compositor_seat::CHANNEL_FD.to_string(),
        );
    #[expect(
        unsafe_code,
        reason = "AUDIT: pre_exec runs between fork and exec; the closure makes only async-signal-safe calls (dup2, fcntl, setgroups, setgid, setuid, getuid)"
    )]
    // SAFETY: the closure allocates nothing and calls only the system calls
    // the reason names, each on values captured before the fork.
    unsafe {
        let _ = command.pre_exec(move || become_account(raw, &ids));
    }
    let child = command
        .spawn()
        .map_err(|error| format!("starting {first}: {error}"))?;
    drop(channel);
    Ok(child)
}

/// In the child, before `exec`: the channel at descriptor 3 without
/// close-on-exec, then the account's groups, gid and uid, in that order,
/// checked.
fn become_account(channel: i32, (uid, gid, groups): &(u32, u32, Vec<u32>)) -> std::io::Result<()> {
    let check = |result: libc::c_int| {
        if result < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    };
    let target = compositor_seat::CHANNEL_FD;
    if channel == target {
        // SAFETY: fcntl on a descriptor this process holds, with constants.
        check(unsafe { libc::fcntl(target, libc::F_SETFD, 0) })?;
    } else {
        // SAFETY: dup2 of a descriptor this process holds; the copy has no
        // close-on-exec.
        check(unsafe { libc::dup2(channel, target) })?;
    }
    // SAFETY: the pointer and length are the vector's own, read only.
    check(unsafe { libc::setgroups(groups.len(), groups.as_ptr()) })?;
    // SAFETY: setgid and setuid take plain integers.
    check(unsafe { libc::setgid(*gid) })?;
    // SAFETY: as above.
    check(unsafe { libc::setuid(*uid) })?;
    // SAFETY: getuid cannot fail.
    if unsafe { libc::getuid() } != *uid {
        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
    }
    Ok(())
}

/// Answer the compositor's requests until it exits, and give its status.
fn serve(ours: UnixStream, mut child: std::process::Child) -> Result<i32, String> {
    let mut connection = Some(Connection::new(ours).map_err(|error| format!("{error}"))?);
    loop {
        if let Some(status) = child.try_wait().map_err(|error| format!("{error}"))? {
            let code = status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
            say(&format!(
                "the session ended: its compositor exited with {code}"
            ));
            return Ok(code);
        }
        let Some(open) = connection.as_mut() else {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        };
        wait_readable(open.as_raw_fd(), Duration::from_millis(200));
        match open.receive() {
            Ok(_) | Err(RecvError::WouldBlock) => {}
            Err(RecvError::Closed) => {
                connection = None;
                continue;
            }
            Err(error) => {
                say(&format!("the seat channel failed: {error:?}"));
                connection = None;
                continue;
            }
        }
        while let Some(end) = open.bytes().iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(open.bytes().get(..end).unwrap_or(&[])).into_owned();
            open.consume(end + 1, 0);
            let (said, fd) = compositor_seat::serve(&line);
            if said != "ok\n" {
                say(&format!("refused `{line}`: {}", said.trim_end()));
            }
            let fds: Vec<Fd> = fd.iter().map(|fd| Fd(fd.as_raw_fd())).collect();
            if let Err(error) = open.send(said.as_bytes(), &fds) {
                say(&format!("answering the compositor failed: {error:?}"));
            }
        }
    }
}

/// Wait until `fd` is readable or `left` has gone.
fn wait_readable(fd: i32, left: Duration) {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = libc::c_int::try_from(left.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: one pollfd, which lives for the call.
    let _ = unsafe { libc::poll(&raw mut poll, 1, millis) };
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "sessiond: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str =
        "root:x:0:0:root:/:/bin/sh\nferrix:x:1000:1000:ferrix:/home/ferrix:/bin/zsh\n";
    const GROUP: &str = "root:x:0:\nferrix:x:1000:\naudio:x:29:ferrix,other\nvideo:x:44:other\n";

    #[test]
    fn an_account_has_its_ids_home_shell_and_groups() {
        assert_eq!(
            account("ferrix", PASSWD, GROUP),
            Some(Account {
                name: "ferrix".to_owned(),
                uid: 1000,
                gid: 1000,
                home: "/home/ferrix".to_owned(),
                shell: "/bin/zsh".to_owned(),
                groups: vec![1000, 29],
            })
        );
        assert_eq!(account("nobody", PASSWD, GROUP), None);
    }

    /// A seed is copied where nothing is, and an edited file is left.
    #[test]
    fn seeds_fill_gaps_and_keep_edits() {
        let root = std::env::temp_dir().join(format!("sessiond-seed-{}", std::process::id()));
        let skel = root.join("skel");
        let home = root.join("home");
        std::fs::create_dir_all(skel.join(".config/waybar")).unwrap();
        std::fs::write(skel.join(".config/waybar/config.jsonc"), "seed").unwrap();
        std::fs::write(skel.join(".zshrc"), "seed").unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".zshrc"), "edited").unwrap();
        assert_eq!(seed(&skel, &home, None, false, 0), 1);
        assert_eq!(
            std::fs::read_to_string(home.join(".config/waybar/config.jsonc")).unwrap(),
            "seed"
        );
        assert_eq!(
            std::fs::read_to_string(home.join(".zshrc")).unwrap(),
            "edited"
        );
        assert_eq!(
            seed(&skel, &home, None, false, 0),
            0,
            "the second start copies nothing"
        );
        assert_eq!(
            seed(&skel, &home, None, true, 0),
            2,
            "a refresh copies everything"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_command_line_names_the_account_and_the_program() {
        let words = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            parse(&words(
                "--user ferrix -- /bin/hyprix --config /etc/hyprland.conf"
            )),
            Some((
                "ferrix".to_owned(),
                words("/bin/hyprix --config /etc/hyprland.conf")
            ))
        );
        assert_eq!(parse(&words("--user ferrix --")), None);
        assert_eq!(parse(&words("ferrix -- /bin/hyprix")), None);
    }
}
