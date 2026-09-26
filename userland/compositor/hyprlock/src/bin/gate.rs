//! `hyprlock-gate`: hyprlock as the gate boot runs it, and nothing else.
//!
//! The same program as `/bin/hyprlock` but for its authentication backend:
//! [`Gate`] accepts the one secret in [`SECRET_PATH`], which only the gate
//! boot's image carries (`cargo xtask test-compositor --boot hyprlock`). It
//! lets that boot type a wrong password and then the right one through
//! hyprlock's own authentication interface while Ferrix's authentication
//! service is still being written (`docs/AUTH.md` phase 1). When `authd`
//! lands, the gate seeds a real store entry instead and this binary goes
//! (§4.4). No other image carries it, and `/bin/hyprlock` has no way to
//! reach this backend.

use std::process::ExitCode;
use std::sync::Arc;

use compositor_hyprlock::auth::{Backend, Next, Prompt, REJECTED, Secret, Verdict};
use compositor_hyprlock::session::FAIL_DELAY_MS;

/// Where the gate's secret is.
const SECRET_PATH: &str = "/etc/hyprlock/gate.secret";

/// The variable a host run names another file with.
const SECRET_VARIABLE: &str = "HYPRLOCK_GATE_SECRET_FILE";

/// One fixed secret, refused as `pam_unix` refuses: after two seconds.
#[derive(Debug)]
struct Gate {
    secret: Vec<u8>,
}

impl Backend for Gate {
    fn begin(&self) -> Result<Prompt, Verdict> {
        Ok(Prompt {
            text: "Password: ".to_owned(),
            secret: true,
        })
    }

    fn respond(&self, secret: &Secret) -> Next {
        let typed = secret.bytes();
        // Compared without stopping at the first difference.
        let same = typed.len() == self.secret.len()
            && typed
                .iter()
                .zip(&self.secret)
                .fold(0u8, |differ, (a, b)| differ | (a ^ b))
                == 0;
        Next::Verdict(if same {
            Verdict::Accepted
        } else {
            Verdict::Failed {
                text: REJECTED.to_owned(),
                retry_after_ms: FAIL_DELAY_MS,
            }
        })
    }
}

fn main() -> ExitCode {
    let path = std::env::var(SECRET_VARIABLE).unwrap_or_else(|_| SECRET_PATH.to_owned());
    match std::fs::read_to_string(&path) {
        Ok(secret) => compositor_hyprlock::cli::main(Arc::new(Gate {
            secret: secret.trim_end_matches('\n').as_bytes().to_vec(),
        })),
        Err(error) => {
            compositor_hyprlock::say(&format!("hyprlock-gate: {path}: {error}"));
            ExitCode::FAILURE
        }
    }
}
