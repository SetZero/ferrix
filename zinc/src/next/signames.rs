//! Signal names and messages (zsh's generated `signames.c`) for Linux.

/// `SIGCOUNT`: the highest real signal number named.
pub(crate) const SIGCOUNT: usize = 31;
/// `SIGEXIT`: the pseudo-signal for the `EXIT` trap.
pub(crate) const SIGEXIT: usize = 0;
/// `SIGZERR`: the `ZERR` trap.
pub(crate) const SIGZERR: usize = SIGCOUNT + 1;
/// `SIGDEBUG`: the `DEBUG` trap.
pub(crate) const SIGDEBUG: usize = SIGCOUNT + 2;
/// `VSIGCOUNT`: real and pseudo-signals.
pub(crate) const VSIGCOUNT: usize = SIGCOUNT + 3;

/// `sigs[]`: names without the `SIG` prefix.
pub(crate) const SIGS: [&str; VSIGCOUNT] = [
    "EXIT", "HUP", "INT", "QUIT", "ILL", "TRAP", "IOT", "BUS", "FPE", "KILL", "USR1", "SEGV",
    "USR2", "PIPE", "ALRM", "TERM", "STKFLT", "CHLD", "CONT", "STOP", "TSTP", "TTIN", "TTOU",
    "URG", "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "POLL", "PWR", "SYS", "ZERR", "DEBUG",
];

/// `sig_msg[]`: the text `jobs` prints for a signal.
const SIG_MSG: [&str; SIGCOUNT + 1] = [
    "done",
    "hangup",
    "interrupt",
    "quit",
    "illegal hardware instruction",
    "trace trap",
    "IOT instruction",
    "bus error",
    "floating point exception",
    "killed",
    "user-defined signal 1",
    "segmentation fault",
    "user-defined signal 2",
    "broken pipe",
    "alarm",
    "terminated",
    "SIGSTKFLT",
    "death of child",
    "continued",
    "suspended (signal)",
    "suspended",
    "suspended (tty input)",
    "suspended (tty output)",
    "urgent condition",
    "cpu limit exceeded",
    "file size limit exceeded",
    "virtual time alarm",
    "profile signal",
    "window size changed",
    "pollable event occurred",
    "power fail",
    "invalid system call",
];

/// zsh's `sigmsg(sig)`.
pub(crate) fn sigmsg(sig: i32) -> &'static str {
    usize::try_from(sig)
        .ok()
        .and_then(|s| SIG_MSG.get(s))
        .copied()
        .unwrap_or("unknown signal")
}

/// zsh's `alt_sigs[]`.
pub(crate) const ALT_SIGS: [(&str, usize); 3] = [("CLD", 17), ("IO", 29), ("ERR", SIGZERR)];
