//! `bench-ipc`: a channel round trip between two native processes, timed.
//!
//! `docs/OPAQUE-KERNEL.md`'s trip is a disk read through the block ring and a
//! ring-3 driver, device and all. The figure an IPC design is compared by --
//! seL4's, Zircon's -- is narrower: a message to another process and back,
//! nothing else. This boots `--init`'s shell with a script that runs
//! `/sbin/ipc-bench`
//! (`src/user/system/native/ipc-bench`), which starts its own echo server in
//! a cgroup of its own and prints the floor (a native call that does not
//! sleep) and the trip, in nanoseconds.

use crate::args::Args;
use crate::{Error, Result, cargo, fat, initramfs, native, qemu, shell};

/// What the shell runs.
const SCRIPT: &str = r#"/sbin/ipc-bench
echo "ipc-bench: exit $?"
"#;

/// Boot, run the benchmark, and print its lines.
///
/// # Errors
///
/// No `--init`, a build or boot that fails, or a benchmark that did not
/// finish.
pub(crate) fn bench_ipc(args: &Args) -> Result<()> {
    let init = args.init.as_deref().ok_or_else(|| {
        Error::new("bench-ipc needs --init, a static busybox for each architecture")
    })?;
    for arch in args.arches()? {
        let program = crate::program_for(init, arch)?;
        let loader = cargo::build_loader(arch, args.release)?;
        let kernel = cargo::build_kernel_with_init(arch, args.release, &program, SCRIPT)?;
        let natives = native::build(arch, args.release)?;
        let carried = shell::carried_for(arch, &program, args)?;
        let initramfs = initramfs::build(None, &natives, None, &carried)?;
        let image = fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
        let lines = qemu::watch_lines(arch, &image, &kernel, args, shell::EXITED)?;
        let mut finished = false;
        for line in lines.iter().filter(|line| line.contains("ipc-bench")) {
            println!("  {arch}: {}", line.trim());
            finished |= line.contains("ipc-bench: exit 0");
        }
        if !finished {
            return Err(Error::new(format!("{arch}: ipc-bench did not finish")));
        }
    }
    Ok(())
}
