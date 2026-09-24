//! Functions busybox calls that this library does not have yet. Each one ends
//! the program.
//!
//! busybox links only when every symbol it names is defined. Its applets
//! still call pattern matching and a few math functions that are not written
//! yet. Their stubs remain under the terms agreed for the busybox row of
//! `docs/BACKLOG.md`:
//!
//! * Only those three areas may be stubs. System-call wrappers, terminals,
//!   users and groups, and the rest of stdio are implemented properly.
//! * A stub writes `ferrousli: <name> is not implemented yet` and a newline to
//!   standard error, then aborts. It never returns and never reports `ENOSYS`,
//!   so a program cannot take it for an answer.
//! * This file is the list that has to shrink. A function leaves it in the
//!   commit that implements it, and the README's count is checked by a unit
//!   test.
//! * The busybox that `test-shell` and `test-vfs` run must reach no stub.
//!
//! A stub is defined without parameters. The caller's arguments go unread,
//! and whatever it expected back never comes.

use crate::signal::abort;
use crate::syscall::{self, nr};

/// Writes `message` to standard error and aborts.
fn missing(message: &str) -> ! {
    // SAFETY: the kernel only reads the message.
    let _ = unsafe { syscall::syscall3(nr::WRITE, 2, message.as_ptr().addr(), message.len()) };
    abort()
}

/// Defines a stub for each name, and [`NAMES`], the list of them.
macro_rules! stubs {
    ($($name:ident)*) => {
        $(
            #[doc = concat!("`", stringify!($name), "`, which is not implemented yet: it ends the program.")]
            #[cfg_attr(not(test), unsafe(no_mangle))]
            pub extern "C" fn $name() -> ! {
                missing(concat!("ferrousli: ", stringify!($name), " is not implemented yet\n"))
            }
        )*

        /// The name of every stub, in byte order.
        pub const NAMES: &[&str] = &[$(stringify!($name)),*];
    };
}

stubs! {
    atan2
    cos
    dirname
    exp
    log
    pow
    regcomp
    regerror
    regexec
    regfree
    sin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_in_order_without_repeats() {
        for pair in NAMES.windows(2) {
            if let [before, after] = pair {
                assert!(before < after, "{before} must come before {after}");
            }
        }
    }

    #[test]
    fn the_readme_counts_the_stubs() {
        let readme = include_str!("../README.md");
        let count = format!("{} functions in `src/stubs.rs`", NAMES.len());
        assert!(readme.contains(&count), "README.md must say \"{count}\"");
    }
}
