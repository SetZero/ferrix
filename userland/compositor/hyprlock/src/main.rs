//! `hyprlock`: lock the screen until the user's password is typed.
//!
//! Ferrix authenticates through `authd` (`docs/AUTH.md`), whose client is
//! phase 1's work; until it lands this one has [`Missing`], and hyprlock
//! says there is no authentication service and does not take the lock.

use std::process::ExitCode;
use std::sync::Arc;

use compositor_hyprlock::auth::Missing;

fn main() -> ExitCode {
    compositor_hyprlock::cli::main(Arc::new(Missing))
}
