//! The Spawn backend (§5.2): a [`SpawnSpec`] turned into a process in its
//! unit's cgroup.
//!
//! Everything the child needs is worked out first, in the parent, where a
//! failure can be reported as [`Event::SpawnFailed`]: the program found on
//! the search path, `$VAR` expanded, the user looked up, the environment
//! files read, and every string made a C string. Then `clone3` puts the
//! child in the cgroup from its first instruction. The child only makes
//! system calls: it unblocks its signals, leads a session of its own, takes
//! its terminal, sets up its three streams, changes directory and user, and
//! calls `execve`.
//!
//! A pipe closed on exec tells the parent how that went (§5.2 step 5). The
//! child writes the step that failed and its error number to it; a success
//! closes it with nothing written, which is
//! [`Event::Execed`](ferrix_svc::event::Event). A child that fails ends with
//! systemd's exit code for the step, so its unit fails as the unit of a
//! program that could not run fails there.
//!
//! [`Event::SpawnFailed`]: ferrix_svc::event::Event

use std::collections::BTreeMap;
use std::ffi::{CString, c_char};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt as _;
use std::ptr;

use ferrix_svc::event::SpawnSpec;
use ferrix_svc::exec::Command;
use ferrix_svc::kind::{Input, Output};

use crate::sys::{self, Forked};

/// The search path for a command that is not an absolute path, and the
/// `PATH` a service starts with: systemd's.
pub(crate) const SEARCH_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// A step of the child's, and systemd's exit code for its failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Step {
    /// Changing directory: `EXIT_CHDIR`.
    Chdir = 200,
    /// `execve`: `EXIT_EXEC`.
    Exec = 203,
    /// Standard input: `EXIT_STDIN`.
    Stdin = 208,
    /// Standard output or error: `EXIT_STDOUT`.
    Stdout = 209,
    /// The user: `EXIT_USER`.
    User = 217,
    /// The session: `EXIT_SETSID`.
    Setsid = 220,
}

impl Step {
    /// The step a report names.
    fn from_code(code: u32) -> Option<Step> {
        [
            Step::Chdir,
            Step::Exec,
            Step::Stdin,
            Step::Stdout,
            Step::User,
            Step::Setsid,
        ]
        .into_iter()
        .find(|step| *step as u32 == code)
    }

    /// What the step was doing, for the log.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Step::Chdir => "changing directory",
            Step::Exec => "execve",
            Step::Stdin => "opening standard input",
            Step::Stdout => "opening standard output",
            Step::User => "setting the user and groups",
            Step::Setsid => "setsid",
        }
    }
}

/// What the report pipe said.
#[derive(Debug)]
pub(crate) enum Report {
    /// Nothing: `execve` succeeded.
    Execed,
    /// A step failed.
    Failed(Step, io::Error),
}

/// Read what a child wrote to its report pipe before it closed.
pub(crate) fn read_report(pipe: &OwnedFd) -> Report {
    let mut bytes = [0_u8; 8];
    let mut file = fs::File::from(match pipe.try_clone() {
        Ok(fd) => fd,
        Err(error) => return Report::Failed(Step::Exec, error),
    });
    let got = io::Read::read(&mut file, &mut bytes).unwrap_or(0);
    if got < bytes.len() {
        return Report::Execed;
    }
    let [a, b, c, d, e, f, g, h] = bytes;
    let code = u32::from_le_bytes([a, b, c, d]);
    let errno = i32::from_le_bytes([e, f, g, h]);
    Report::Failed(
        Step::from_code(code).unwrap_or(Step::Exec),
        io::Error::from_raw_os_error(errno),
    )
}

/// Where one of the child's streams comes from.
#[derive(Debug)]
enum Stream {
    /// Open this path with these flags.
    Open(CString, libc::c_int),
    /// The terminal the child took.
    Tty,
    /// A copy of a stream set up before it: standard error as output.
    Same(RawFd),
}

/// A spawn worked out in the parent: nothing the child does allocates.
#[derive(Debug)]
pub(crate) struct Prepared {
    path: CString,
    argv: Vec<CString>,
    envp: Vec<CString>,
    /// The terminal to take, and whether to take it from another session.
    tty: Option<(CString, bool)>,
    streams: [Stream; 3],
    directory: Option<(CString, bool)>,
    user: Option<(u32, u32, Vec<u32>)>,
}

/// Why a spawn could not be worked out.
#[derive(Debug)]
pub(crate) struct Unprepared {
    /// The error number to report.
    pub(crate) errno: i32,
    /// What went wrong, for the log.
    pub(crate) why: String,
}

impl Unprepared {
    fn new(errno: i32, why: String) -> Unprepared {
        Unprepared { errno, why }
    }
}

/// A C string, or the reason a string with a NUL in it cannot be one.
fn c_string(text: &str) -> Result<CString, Unprepared> {
    CString::new(text).map_err(|_| Unprepared::new(libc::EINVAL, format!("{text:?} holds a NUL")))
}

/// Work out everything the child needs from `spec`, with `terminal` the
/// `TERM` a service on a terminal gets unless it sets its own.
pub(crate) fn prepare(spec: &SpawnSpec, terminal: &str) -> Result<Prepared, Unprepared> {
    let user = match &spec.user {
        Some(name) => Some(lookup_user(name)?),
        None => None,
    };
    let mut environment: BTreeMap<String, String> = BTreeMap::new();
    let _ = environment.insert("PATH".to_owned(), SEARCH_PATH.to_owned());
    if let Some(account) = &user {
        let _ = environment.insert("HOME".to_owned(), account.home.clone());
        let _ = environment.insert("USER".to_owned(), account.name.clone());
        let _ = environment.insert("LOGNAME".to_owned(), account.name.clone());
        let _ = environment.insert("SHELL".to_owned(), account.shell.clone());
    }
    if spec.tty.is_some() {
        let _ = environment.insert("TERM".to_owned(), terminal.to_owned());
    }
    for (name, value) in &spec.environment {
        let _ = environment.insert(name.clone(), value.clone());
    }
    for (path, missing_ok) in &spec.environment_files {
        match fs::read_to_string(path) {
            Ok(text) => environment.extend(environment_file(&text)),
            Err(_) if *missing_ok => {}
            Err(error) => {
                return Err(Unprepared::new(
                    error.raw_os_error().unwrap_or(libc::EIO),
                    format!("reading EnvironmentFile={path}: {error}"),
                ));
            }
        }
    }

    let argv = arguments(&spec.command, &environment);
    let path = find(&spec.command.path)?;
    let group_ids = match (&spec.group_name, &user) {
        (Some(name), _) => lookup_group(name)?,
        (None, Some(account)) => account.gid,
        (None, None) => 0,
    };
    let supplementary = spec
        .supplementary_groups
        .iter()
        .map(|name| lookup_group(name))
        .collect::<Result<Vec<u32>, Unprepared>>()?;
    let user_ids = user
        .as_ref()
        .map(|account| (account.uid, group_ids, supplementary.clone()))
        .or_else(|| {
            spec.group_name
                .as_ref()
                .map(|_| (0, group_ids, supplementary))
        });

    let tty = match &spec.tty {
        Some(path) => {
            let force = matches!(spec.stdin, Input::TtyForce);
            Some((c_string(path)?, force))
        }
        None => None,
    };
    let stdin = match &spec.stdin {
        Input::Null => Stream::Open(c"/dev/null".to_owned(), libc::O_RDONLY),
        Input::Tty | Input::TtyForce | Input::TtyFail => Stream::Tty,
        Input::File(path) => Stream::Open(c_string(path)?, libc::O_RDONLY),
    };
    let stdin_is_tty = matches!(stdin, Stream::Tty);
    let stdout = output(&spec.stdout, stdin_is_tty, None)?;
    let stderr = output(&spec.stderr, stdin_is_tty, Some(1))?;

    let directory = match &spec.working_directory {
        Some(directory) => {
            let path = match (directory.path.as_str(), &user) {
                ("~", Some(account)) => account.home.clone(),
                ("~", None) => "/root".to_owned(),
                (path, _) => path.to_owned(),
            };
            Some((c_string(&path)?, directory.missing_ok))
        }
        None => Some((c"/".to_owned(), false)),
    };

    Ok(Prepared {
        path: c_string(&path)?,
        argv: argv
            .iter()
            .map(|word| c_string(word))
            .collect::<Result<_, _>>()?,
        envp: environment
            .iter()
            .map(|(name, value)| c_string(&format!("{name}={value}")))
            .collect::<Result<_, _>>()?,
        tty,
        streams: [stdin, stdout, stderr],
        directory,
        user: user_ids,
    })
}

/// Where standard output or error goes. `Inherit` is standard input's
/// terminal if it has one, and otherwise the log -- which reaches the
/// console (§10), and until L6 gives the log a pipe of its own is the
/// console itself. Standard error's `Inherit` is standard output.
fn output(
    output: &Output,
    stdin_is_tty: bool,
    inherit_from: Option<RawFd>,
) -> Result<Stream, Unprepared> {
    let write = libc::O_WRONLY | libc::O_NOCTTY;
    Ok(match output {
        Output::Inherit => match inherit_from {
            Some(fd) => Stream::Same(fd),
            None if stdin_is_tty => Stream::Tty,
            None => Stream::Open(c"/dev/console".to_owned(), write),
        },
        Output::Null => Stream::Open(c"/dev/null".to_owned(), write),
        Output::Tty => Stream::Tty,
        Output::Console | Output::Log => Stream::Open(c"/dev/console".to_owned(), write),
        Output::File(path) => Stream::Open(c_string(path)?, write | libc::O_CREAT),
        Output::Append(path) => {
            Stream::Open(c_string(path)?, write | libc::O_CREAT | libc::O_APPEND)
        }
        Output::Truncate(path) => {
            Stream::Open(c_string(path)?, write | libc::O_CREAT | libc::O_TRUNC)
        }
    })
}

/// A command's words, `$VAR` and `${VAR}` expanded unless `:` said not.
/// A word that is only `$VAR` becomes that variable's words, split at
/// whitespace; anywhere else the value goes in as it is (systemd's rule).
fn arguments(command: &Command, environment: &BTreeMap<String, String>) -> Vec<String> {
    if !command.expand_environment {
        return command.argv.clone();
    }
    let mut argv = Vec::new();
    for word in &command.argv {
        if let Some(name) = word.strip_prefix('$')
            && ferrix_svc::value::is_env_name(name)
        {
            if let Some(value) = environment.get(name) {
                argv.extend(value.split_whitespace().map(str::to_owned));
            }
            continue;
        }
        argv.push(expand(word, environment));
    }
    argv
}

/// `$VAR` and `${VAR}` in `word`, replaced by their values; an unset one is
/// empty. `$$` is one `$`.
fn expand(word: &str, environment: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = word;
    while let Some(at) = rest.find('$') {
        out.push_str(rest.get(..at).unwrap_or_default());
        let after = rest.get(at + 1..).unwrap_or_default();
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
        } else if let Some(braced) = after.strip_prefix('{')
            && let Some(end) = braced.find('}')
        {
            let name = braced.get(..end).unwrap_or_default();
            out.push_str(environment.get(name).map_or("", String::as_str));
            rest = braced.get(end + 1..).unwrap_or_default();
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            let name = after.get(..end).unwrap_or_default();
            if name.is_empty() {
                out.push('$');
            } else {
                out.push_str(environment.get(name).map_or("", String::as_str));
            }
            rest = after.get(end..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
}

/// `KEY=value` lines, as `EnvironmentFile=` reads them: blank lines and
/// `#` or `;` comments skipped, one level of quotes taken off.
fn environment_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with(';'))
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| {
            let value = value.trim();
            let unquoted = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            (name.trim().to_owned(), unquoted.to_owned())
        })
        .filter(|(name, _)| ferrix_svc::value::is_env_name(name))
        .collect()
}

/// The program: an absolute path as it is, a bare name looked for on
/// [`SEARCH_PATH`].
fn find(program: &str) -> Result<String, Unprepared> {
    if program.starts_with('/') {
        return Ok(program.to_owned());
    }
    SEARCH_PATH
        .split(':')
        .map(|dir| format!("{dir}/{program}"))
        .find(|path| {
            fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .ok_or_else(|| Unprepared::new(libc::ENOENT, format!("{program} is not on {SEARCH_PATH}")))
}

/// An account from `/etc/passwd`.
#[derive(Debug)]
struct Account {
    name: String,
    uid: u32,
    gid: u32,
    home: String,
    shell: String,
}

/// `User=`: a name in `/etc/passwd`, or a number.
fn lookup_user(user: &str) -> Result<Account, Unprepared> {
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let found = passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let [name, _, uid, gid, _, home, shell, ..] = fields.as_slice() else {
            return None;
        };
        (*name == user || *uid == user).then(|| Account {
            name: (*name).to_owned(),
            uid: uid.parse().unwrap_or(u32::MAX),
            gid: gid.parse().unwrap_or(u32::MAX),
            home: (*home).to_owned(),
            shell: (*shell).to_owned(),
        })
    });
    match (found, user.parse::<u32>()) {
        (Some(account), _) => Ok(account),
        (None, Ok(uid)) => Ok(Account {
            name: user.to_owned(),
            uid,
            gid: uid,
            home: "/".to_owned(),
            shell: "/bin/sh".to_owned(),
        }),
        (None, Err(_)) => Err(Unprepared::new(
            libc::ESRCH,
            format!("User={user} is not in /etc/passwd"),
        )),
    }
}

/// `Group=`: a name in `/etc/group`, or a number.
fn lookup_group(group: &str) -> Result<u32, Unprepared> {
    if let Ok(gid) = group.parse() {
        return Ok(gid);
    }
    fs::read_to_string("/etc/group")
        .unwrap_or_default()
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let gid = fields.nth(1)?;
            (name == group).then(|| gid.parse().ok()).flatten()
        })
        .ok_or_else(|| Unprepared::new(libc::ESRCH, format!("Group={group} is not in /etc/group")))
}

/// Start `prepared` in the cgroup `cgroup` is open on, and return the
/// child's pid and the read end of its report pipe.
pub(crate) fn start(prepared: &Prepared, cgroup: BorrowedFd<'_>) -> io::Result<(u32, OwnedFd)> {
    let (report, write_end) = sys::pipe()?;
    // Built before the clone: the child must not allocate.
    let argv = pointers(&prepared.argv);
    let envp = pointers(&prepared.envp);
    match sys::fork_into(cgroup)? {
        Forked::Child => child(prepared, &argv, &envp, write_end.as_raw_fd()),
        Forked::Parent(pid) => {
            drop(write_end);
            Ok((pid, report))
        }
    }
}

/// A null-terminated array of pointers to `strings`.
fn pointers(strings: &[CString]) -> Vec<*const c_char> {
    strings
        .iter()
        .map(|string| string.as_ptr())
        .chain(std::iter::once(ptr::null()))
        .collect()
}

/// The child, from `clone3` to `execve`.
fn child(prepared: &Prepared, argv: &[*const c_char], envp: &[*const c_char], report: RawFd) -> ! {
    let fail = |step: Step, error: io::Error| -> ! {
        let errno = error.raw_os_error().unwrap_or(libc::EIO);
        let mut bytes = [0_u8; 8];
        let (code, number) = bytes.split_at_mut(4);
        code.copy_from_slice(&(step as u32).to_le_bytes());
        number.copy_from_slice(&errno.to_le_bytes());
        sys::write_once(report, &bytes);
        sys::exit_now(step as i32)
    };

    // Init blocks the signals it reads through a signalfd and ignores the
    // rest; a program starts with neither.
    let _ = sys::unblock_all();
    for signal in 1..=64 {
        sys::disposition(signal, libc::SIG_DFL);
    }
    // Every service leads a session of its own, as under systemd.
    if let Err(error) = sys::setsid()
        && error.raw_os_error() != Some(libc::EPERM)
    {
        fail(Step::Setsid, error);
    }
    let mut terminal = None;
    if let Some((path, force)) = &prepared.tty {
        match sys::open(path, libc::O_RDWR) {
            Ok(fd) => {
                if let Err(error) = sys::take_terminal(fd, *force) {
                    fail(Step::Stdin, error);
                }
                terminal = Some(fd);
            }
            Err(error) => fail(Step::Stdin, error),
        }
    }
    for (target, stream) in (0..).zip(&prepared.streams) {
        let step = if target == 0 {
            Step::Stdin
        } else {
            Step::Stdout
        };
        let from = match stream {
            Stream::Open(path, flags) => match sys::open(path, *flags) {
                Ok(fd) => fd,
                Err(error) => fail(step, error),
            },
            Stream::Tty => match terminal {
                Some(fd) => fd,
                None => fail(step, io::Error::from_raw_os_error(libc::ENOTTY)),
            },
            Stream::Same(fd) => *fd,
        };
        if let Err(error) = sys::dup2(from, target) {
            fail(step, error);
        }
    }
    if let Some((path, missing_ok)) = &prepared.directory
        && let Err(error) = sys::chdir(path)
        && !*missing_ok
    {
        fail(Step::Chdir, error);
    }
    if let Some((uid, gid, groups)) = &prepared.user
        && let Err(error) = sys::become_user(*uid, *gid, groups)
    {
        fail(Step::User, error);
    }
    let error = sys::execve(&prepared.path, argv, envp);
    fail(Step::Exec, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn command(argv: &[&str], expand: bool) -> Command {
        Command {
            path: argv
                .first()
                .map_or_else(String::new, |word| (*word).to_owned()),
            argv: argv.iter().map(|word| (*word).to_owned()).collect(),
            ignore_failure: false,
            expand_environment: expand,
            privilege: ferrix_svc::exec::Privilege::Unit,
        }
    }

    #[test]
    fn a_whole_word_variable_splits_and_a_braced_one_does_not() {
        let env = environment(&[("OPTS", "-a -b"), ("DIR", "/x y")]);
        let argv = arguments(&command(&["/bin/p", "$OPTS", "--in=${DIR}/z"], true), &env);
        assert_eq!(argv, ["/bin/p", "-a", "-b", "--in=/x y/z"]);
    }

    #[test]
    fn a_colon_prefix_expands_nothing() {
        let env = environment(&[("OPTS", "-a")]);
        let argv = arguments(&command(&["/bin/p", "$OPTS"], false), &env);
        assert_eq!(argv, ["/bin/p", "$OPTS"]);
    }

    #[test]
    fn an_unset_variable_is_empty_and_a_doubled_dollar_is_one() {
        let env = environment(&[]);
        assert_eq!(expand("a$NOPEb", &env), "a");
        assert_eq!(expand("cost $$5", &env), "cost $5");
        assert_eq!(expand("${NOPE}x", &env), "x");
    }

    #[test]
    fn environment_files_skip_comments_and_unquote() {
        let read = environment_file("# c\n; c\nA=1\nB=\"two words\"\n bad name=3\nC='x'\n");
        assert_eq!(
            read,
            [
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "two words".to_owned()),
                ("C".to_owned(), "x".to_owned()),
            ]
        );
    }

    #[test]
    fn a_report_names_its_step() {
        assert_eq!(Step::from_code(203), Some(Step::Exec));
        assert_eq!(Step::from_code(217), Some(Step::User));
        assert_eq!(Step::from_code(1), None);
    }
}
