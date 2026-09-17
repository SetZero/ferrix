//! Iteration 1 of the compositor: fill the screen with one colour.
//!
//! `docs/DISPLAY.md` puts a blank screen on Ferrix first, so the path from a
//! Linux program through `/dev/dri/card0`, the kernel's display core and the
//! ring-3 virtio-gpu driver to QEMU's window is proven before a compositor
//! draws anything on it. This program is that proof, and it uses nothing a
//! real compositor's DRM backend does not: the legacy mode-setting calls and
//! a dumb buffer.
//!
//! It runs as init in the display test, so it never exits: it prints one
//! line saying what it showed, or where it failed, and waits.

#[cfg(target_os = "linux")]
fn main() {
    use std::io::Write;

    use compositor_drm::{Card, show};

    let line = match Card::open().and_then(|card| show(&card)) {
        Ok(line) => line,
        Err(error) => format!("compositor: failed: {error}"),
    };
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    // As init, exiting would end the machine; as a program, the scanout lasts
    // only while the card is open.
    loop {
        // SAFETY: pause has no preconditions.
        let _ = unsafe { libc::pause() };
    }
}

/// Only Linux, and Ferrix through its Linux ABI, have `/dev/dri`; elsewhere the
/// crate builds so its modeset logic is tested on any host.
#[cfg(not(target_os = "linux"))]
fn main() {}
