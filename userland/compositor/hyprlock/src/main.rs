//! `hyprlock`: lock the screen until the user's password is typed.
//!
//! The password is checked by `authd` (`docs/AUTH.md`) through
//! [`Service`]. With no `authd` running, or no password set for this
//! account, hyprlock does not take the lock, and says why (decision 4).

use std::process::ExitCode;
use std::sync::Arc;

use compositor_hyprlock::auth::Service;

fn main() -> ExitCode {
    compositor_hyprlock::cli::main(Arc::new(Service::new()))
}
