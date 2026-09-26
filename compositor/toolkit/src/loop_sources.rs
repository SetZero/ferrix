//! What the event loop waits on besides the compositor: timers, children's
//! pipes, signals and descriptors a program hands it.

/// A timer [`crate::Client::add_timer`] made.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TimerId(pub u32);

/// A child [`crate::Client::run`] started.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ChildId(pub u32);

/// A descriptor [`crate::Client::watch_fd`] watches.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct WatchId(pub u32);

/// How a child's standard output comes back.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ChildOutput {
    /// A line at a time, as [`crate::Event::ChildLine`], as it is written:
    /// a script that runs for ever and prints when something changes
    /// (waybar's `exec` with no `interval`, hyprlock's nothing).
    #[default]
    Lines,
    /// All of it at once, in [`crate::Event::ChildExited`]'s `output`, when
    /// the child is done: a script run on an interval, hyprlock's
    /// `cmd[update:…]`.
    Whole,
}

/// A program to start with `/bin/sh -c`, the way waybar's `exec`, hyprlock's
/// `cmd[]` and hypridle's `on-timeout` run theirs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    /// The shell command line, as the configuration wrote it.
    pub line: String,
    /// Variables added to the environment it inherits
    /// (`WAYBAR_OUTPUT_NAME`).
    pub env: Vec<(String, String)>,
    /// How its output comes back.
    pub output: ChildOutput,
}

impl Command {
    /// `line`, with its output a line at a time and nothing added.
    #[must_use]
    pub fn new(line: &str) -> Self {
        Self {
            line: line.to_owned(),
            env: Vec::new(),
            output: ChildOutput::Lines,
        }
    }
}

/// Wakes a [`crate::Client::dispatch`] from another thread: it returns an
/// [`crate::Event::Woken`]. Cheap to clone; every clone wakes the same loop.
#[derive(Clone, Debug)]
pub struct Waker {
    pub(crate) fd: std::sync::Arc<std::os::fd::OwnedFd>,
}

impl Waker {
    /// Wake the loop. Wakes that arrive before it next looks are one
    /// [`crate::Event::Woken`].
    pub fn wake(&self) {
        use std::os::fd::AsRawFd as _;
        let one: u64 = 1;
        #[expect(
            unsafe_code,
            reason = "AUDIT: write of eight bytes from a local to an eventfd this value keeps open"
        )]
        // SAFETY: the pointer is to a local u64 and the length is its size;
        // the descriptor is owned by the Arc for as long as `self` lives.
        let _ = unsafe {
            libc::write(
                self.fd.as_raw_fd(),
                (&raw const one).cast(),
                size_of::<u64>(),
            )
        };
    }
}
