//! The stack the kernel starts a process on, and the auxiliary vector at the
//! end of it.
//!
//! ```text
//!   sp -> argc
//!         argv[0] .. argv[argc-1]
//!         NULL
//!         envp[0] .. envp[n-1]
//!         NULL
//!         a_type, a_val        the auxiliary vector
//!         ...
//!         AT_NULL, 0
//! ```
//!
//! Everything the loader needs to start is in there and nowhere else: where
//! the program's headers are, how many there are, where it starts, and where
//! this loader itself was put.

use core::ffi::c_char;

/// The end of the vector.
pub const AT_NULL: usize = 0;
/// The program's program headers.
pub const AT_PHDR: usize = 3;
/// How many there are.
pub const AT_PHNUM: usize = 5;
/// The page size.
pub const AT_PAGESZ: usize = 6;
/// Where the dynamic loader was placed, and zero when there is none — which
/// is how the loader knows it was run as a program rather than as somebody's
/// interpreter.
pub const AT_BASE: usize = 7;
/// The program's entry point.
pub const AT_ENTRY: usize = 9;
/// Whether the program runs with privileges it did not have.
pub const AT_SECURE: usize = 23;

/// The stack as the process was entered, read once.
#[derive(Debug, Clone, Copy)]
pub struct Stack {
    /// The stack pointer the process was entered with, which is the one the
    /// program's own `_start` has to be given.
    pub sp: *const usize,
    /// `argc`.
    pub argc: usize,
    /// `argv`, with its terminating null.
    pub argv: *const *const c_char,
    /// The environment, with its terminating null.
    pub envp: *const *const c_char,
    /// The auxiliary vector, with its terminating `AT_NULL`.
    pub auxv: *const usize,
}

impl Stack {
    /// Read the three arrays off the stack.
    ///
    /// # Safety
    ///
    /// `sp` must be the stack pointer the kernel entered the process with,
    /// whose layout is the one in this module's documentation.
    #[must_use]
    pub unsafe fn read(sp: *const usize) -> Stack {
        // SAFETY: the first word of the entry stack is `argc`.
        let argc = unsafe { sp.read() };
        // SAFETY: `argv` follows it.
        let argv = unsafe { sp.add(1) }.cast::<*const c_char>();
        // SAFETY: `argc` pointers and a null terminator follow, so the
        // environment starts one past the null.
        let envp = unsafe { argv.add(argc + 1) };

        let mut end = envp;
        // SAFETY: the environment is null-terminated, and this stops there.
        while !unsafe { end.read() }.is_null() {
            // SAFETY: the entry just read was not the terminator.
            end = unsafe { end.add(1) };
        }
        // SAFETY: the vector begins one past the environment's null.
        let auxv = unsafe { end.add(1) }.cast::<usize>();

        Stack {
            sp,
            argc,
            argv,
            envp,
            auxv,
        }
    }

    /// The value of `key`, or `None` when the vector does not carry it.
    ///
    /// The vector is walked per lookup rather than copied into a table: it has
    /// a couple of dozen entries and is read a handful of times, and a table
    /// would be a `static` — which is the one thing the code that runs before
    /// relocation may not have.
    #[must_use]
    pub fn get(&self, key: usize) -> Option<usize> {
        let mut at = self.auxv;
        loop {
            // SAFETY: the vector runs to an `AT_NULL` entry, and this stops
            // there.
            let tag = unsafe { at.read() };
            if tag == AT_NULL {
                return None;
            }
            // SAFETY: a tag that is not the terminator is followed by its
            // value.
            let value = unsafe { at.add(1).read() };
            if tag == key {
                return Some(value);
            }
            // SAFETY: and by the next pair.
            at = unsafe { at.add(2) };
        }
    }
}
