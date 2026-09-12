//! Stage 7's exit criterion, as a script with an expected transcript.
//!
//! "A static musl `busybox sh` starts, runs a script, and exits." The script
//! is given with `-c`, which puts it in `argv` and needs no filesystem: the
//! roadmap's criterion read as "runs a script", not "runs a script *file*",
//! because a file is stage 8's and would make this stage's exit wait on it.
//!
//! # What the script exercises
//!
//! The parts of a shell that are the shell rather than somebody else's
//! program: variables and arithmetic, a loop, a function with an argument,
//! `test`, `case`, `printf`, the positional parameters, and an exit status
//! the kernel reports back. Nothing that forks, because `clone`, `execve` and
//! `wait4` do not exist yet -- a command substitution or an external command
//! would fail, and the test would be measuring that rather than the ABI.
//!
//! `printf` is missing for a reason of its own: busybox's builtin asks
//! `fcntl(1, F_GETFL)` before it writes and gives up when that fails, and
//! `fcntl` belongs to the file descriptor table stage 8 brings. When it
//! answers, `printf 'script: %s %d\n' printf 42` and its line go back in.
//!
//! # Why the program is not in the repository
//!
//! A static busybox is a binary built by somebody else, for each architecture,
//! and which one to trust is a decision for whoever runs this. `--init` names
//! it. Alpine's `busybox-static` package is what this was first run against:
//! static musl, one build per architecture, and no knowledge of Ferrix.

/// The script `sh -c` runs.
pub(crate) const SCRIPT: &str = r#"echo "script: started"
n=0
for i in 1 2 3 4 5; do n=$((n + i)); done
echo "script: the sum is $n"
greet() { echo "script: hello, $1"; }
greet ferrix
if [ "$n" -eq 15 ]; then echo "script: test agrees"; fi
case ferrix in fer*) echo "script: case matched";; esac
set -- a b c
echo "script: $# positional parameters"
exit 7
"#;

/// The lines the script must print, in this order.
pub(crate) const EXPECTED: &[&str] = &[
    "script: started",
    "script: the sum is 15",
    "script: hello, ferrix",
    "script: test agrees",
    "script: case matched",
    "script: 3 positional parameters",
];

/// The status the script exits with. Not zero, so a shell that died and
/// reported success cannot pass.
pub(crate) const STATUS: i32 = 7;

/// The kernel's line when the first program exits, before the status.
pub(crate) const EXITED: &str = "init     the shell exited with";

/// The kernel's line when the first program could not be started at all.
pub(crate) const NOT_STARTED: &str = "init     the shell could not be started";
