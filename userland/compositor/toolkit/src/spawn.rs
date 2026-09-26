//! Starting a program and forgetting it, the way upstream's `exec` does.

use std::process::Stdio;

/// Start `line` with `/bin/sh -c`, detached: in a session of its own, its
/// standard input from `/dev/null`, and not this program's child for long
/// (it is started from a child that exits at once, so nothing is left to
/// reap and it outlives this program).
///
/// That is what waybar's `on-click` and hyprlock's and hypridle's commands
/// do upstream (a `fork`/`setsid`/`execl("/bin/sh", "-c")`), so a command
/// line from the user's file means here what it means there -- `pidof
/// hyprlock || hyprlock` included. fuzzel is not one of them: it splits a
/// desktop entry's `Exec` itself and calls `execvp`.
///
/// # Errors
///
/// The `fork` failing. What the command itself does is not known here.
pub fn spawn(line: &str) -> std::io::Result<()> {
    // The outer shell puts the command in the background and exits; the
    // command is then init's to reap. The line goes in braces on a line of
    // its own so that a `||`, a `;` or a trailing comment stays inside.
    let script = format!("{{\n{line}\n}} &");
    let mut command = std::process::Command::new("/bin/sh");
    let _ = command
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    crate::sources::prepare_child(&mut command, false);
    detach(&mut command);
    let mut child = command.spawn()?;
    let _ = child.wait()?;
    Ok(())
}

/// A session of its own, so the command is not in this program's process
/// group or on its terminal.
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    let hook = || {
        #[expect(
            unsafe_code,
            reason = "AUDIT: setsid in the forked child is async-signal-safe and takes no arguments"
        )]
        // SAFETY: as the reason says.
        let _ = unsafe { libc::setsid() };
        Ok(())
    };
    #[expect(
        unsafe_code,
        reason = "AUDIT: pre_exec runs the hook in the forked child; setsid is async-signal-safe"
    )]
    // SAFETY: the hook allocates nothing and takes no lock.
    unsafe {
        let _ = command.pre_exec(hook);
    }
}
