//! Negative controls for `cargo xtask test-auth` (`docs/AUTH.md` §7, P1.6).
//!
//! Each refusal the gate requires has a sabotage that turns it off: a build
//! of `authd` with `FERRIX_AUTH_SABOTAGE` set when it was compiled. The gate
//! is then run against that build and must fail on exactly the line the
//! sabotage breaks.
//!
//! An ordinary build has no sabotage in it at all: the name is read with
//! `option_env!`, and the marker and banner below exist only under
//! `cfg(ferrix_auth_sabotage)`, which `build.rs` sets only for a named
//! sabotage. xtask looks for [`MARKER`] in every `authd` it packages, and
//! refuses one that carries it unless `test-auth --sabotage` asked for it
//! (ferrix-55's review, 2026-09-26).
//!
//! | Name | What it turns off |
//! |---|---|
//! | `accept-any` | the password check: any password opens a known account |
//! | `tell-unknown` | the decoy and the phantom tally: an unknown account says it is unknown |
//! | `let-anyone-name` | the rule that only root may name another account |
//! | `no-throttle` | the throttle |

/// The sabotage this build was made with, if any.
pub(crate) const BUILT: Option<&str> = option_env!("FERRIX_AUTH_SABOTAGE");

/// The bytes xtask refuses to package: in a sabotaged build only.
#[cfg(ferrix_auth_sabotage)]
pub(crate) const MARKER: &str = "FERRIX-AUTH-SABOTAGED-BUILD";

/// Whether this build was sabotaged as `name`.
pub(crate) fn is(name: &str) -> bool {
    cfg!(ferrix_auth_sabotage) && BUILT == Some(name)
}

/// Say so, loudly, first thing: only a sabotaged build has this to say.
pub(crate) fn announce() {
    #[cfg(ferrix_auth_sabotage)]
    crate::audit::say(&format!(
        "authd: {MARKER} ({}): for a gate's negative control; it must never ship",
        BUILT.unwrap_or_default()
    ));
}
