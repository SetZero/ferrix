//! Reading what the loader did about KASLR off a boot's serial output.
//!
//! The loader moves the kernel image, the direct map and the vmap arena each
//! boot (`boot/src/kaslr.rs`, `docs/certification/SPECULATION.md` §6) and says
//! where on the serial line:
//!
//! ```text
//!   kaslr    kernel at 0xffffffffa3912000, slide 0x23912000, 18 bits from EFI_RNG
//!   kaslr    direct map at 0xffff9b9900000000, 16 bits; vmap arena top at 0xffffffed57140000, 17 bits
//! ```
//!
//! and the kernel, after checking it, says how well. Three things here read
//! that: every `test-boot`, which requires the layout to have moved when it
//! should and to have stayed when it should not; `test-kaslr`, which boots
//! twice and requires two layouts; and the symboliser and the coverage report,
//! which take the slide off a run-time address before looking it up in the ELF,
//! which knows only link-time ones.

use std::path::{Path, PathBuf};

use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result};

/// What the loader's first line begins with, after its indentation.
const MOVED: &str = "kaslr    kernel at ";

/// What its second line begins with.
const DIRECT_MAP: &str = "kaslr    direct map at ";

/// What the kernel prints once it has checked a randomised layout.
const KERNEL_MOVED: &str = "kaslr    image, direct map and arena moved";

/// What the loader prints for a kernel built `--mitigations off`.
const FIXED_IMAGE: &str = "kaslr    NOT randomised: the kernel is a fixed-address image";

/// What the loader prints for `nokaslr`.
const DECLINED: &str = "kaslr    NOT randomised: nokaslr on the command line";

/// The word before the slide, on the loader's line and on a panic's.
const SLIDE: &str = "slide 0x";

/// The slide a line gives, if it is a `kaslr` line that gives one: the
/// loader's, or the one a panic report prints before its backtrace.
pub(crate) fn slide_in(line: &str) -> Option<u64> {
    let after = line.split_once("kaslr ")?.1;
    let digits = after.split_once(SLIDE)?.1;
    let end = digits
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(digits.len());
    u64::from_str_radix(digits.get(..end)?, 16).ok()
}

/// Where the loader put the three regions a boot moves: the image's slide,
/// and the lines that name the direct map and the arena, as they were printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// How far the image moved from its link address.
    pub(crate) slide: u64,
    /// The loader's line naming the direct map and the arena's top.
    pub(crate) rest: String,
}

/// The layout the last boot in `lines` got, if the loader moved it.
pub(crate) fn layout(lines: &[String]) -> Option<Layout> {
    let slide = lines
        .iter()
        .rev()
        .find(|line| line.contains(MOVED))
        .and_then(|line| slide_in(line))?;
    let rest = lines
        .iter()
        .rev()
        .find_map(|line| Some(line.split_once(DIRECT_MAP)?.1.trim().to_owned()))?;
    Some(Layout { slide, rest })
}

/// Why a boot's layout is not what its build and command line should give,
/// if it is not.
///
/// A kernel built `--mitigations on` moves, from `EFI_RNG`, which every QEMU
/// machine here offers; one built `off` is a fixed-address image and does
/// not; `nokaslr` keeps even a movable one where it was linked.
pub(crate) fn problem(lines: &[String], mitigations_off: bool, declined: bool) -> Option<String> {
    let has = |text: &str| lines.iter().any(|line| line.contains(text));
    if mitigations_off {
        return (!has(FIXED_IMAGE)).then(|| {
            "a kernel built --mitigations off was not reported at its fixed address".to_owned()
        });
    }
    if declined {
        return (!has(DECLINED)).then(|| {
            "nokaslr was given, and the loader did not say it kept the layout".to_owned()
        });
    }
    let Some(line) = lines.iter().rev().find(|line| line.contains(MOVED)) else {
        return Some("the loader did not say where it moved the kernel".to_owned());
    };
    if !line.contains("from EFI_RNG") {
        return Some(format!(
            "the layout moved without firmware's randomness: `{}`",
            line.trim()
        ));
    }
    if slide_in(line).is_none_or(|slide| slide == 0) {
        return Some(format!("the kernel did not move: `{}`", line.trim()));
    }
    (!has(KERNEL_MOVED)).then(|| "the kernel did not confirm the layout it was given".to_owned())
}

/// Whether the image a run boots is told `nokaslr`.
pub(crate) fn declined(args: &Args) -> bool {
    args.gdb || args.kernel_options.iter().any(|option| option == "nokaslr")
}

/// Write the slide of the boot in `lines` beside the trace a coverage plugin
/// wrote, as `<trace>.slide`, for `scripts/coverage-report.py`.
///
/// `plugin` is QEMU's `-plugin` argument, whose `filename=` names the trace.
/// A drcov trace records the addresses the code ran at, and the report looks
/// them up in the ELF at the addresses it was linked at: this is the
/// difference, per boot. Nothing is written for a plugin that names no file.
pub(crate) fn record_slide(plugin: &str, lines: &[String]) -> Result<()> {
    let Some(trace) = plugin
        .split(',')
        .find_map(|part| part.strip_prefix("filename="))
    else {
        return Ok(());
    };
    let slide = layout(lines).map_or(0, |layout| layout.slide);
    std::fs::write(sidecar(Path::new(trace)), format!("{slide:#x}\n"))?;
    Ok(())
}

/// [`record_slide`] for the plugin this boot was given, if it was given one:
/// a coverage trace records the addresses the kernel ran at, which KASLR
/// moved, and its report needs this boot's slide to look them up. The boot's
/// own trace, which after a gate's first boot is a numbered one
/// ([`crate::coverage::plugin_for_boot`]).
pub(crate) fn record_coverage_slide(lines: &[String]) -> Result<()> {
    match crate::coverage::latest_plugin() {
        Some(plugin) => record_slide(&plugin, lines),
        None => Ok(()),
    }
}

/// Where [`record_slide`] writes a trace's slide.
fn sidecar(trace: &Path) -> PathBuf {
    let mut name = trace.as_os_str().to_owned();
    name.push(".slide");
    PathBuf::from(name)
}

/// `test-kaslr`: boot `arch`'s image twice and require two layouts.
///
/// Each boot is a whole `test-boot`, so each has already been held to
/// [`problem`]. The image's slide repeats by chance once in 2^bits pairs of
/// boots — 2^18 on the 64-bit pair, 2^11 on ARMv7-A — so a repeat of the
/// slide alone is reported, not failed, and the boot fails only when all
/// three regions came back where they were: a loader that did not
/// randomise, rather than one that was unlucky.
pub(crate) fn test_kaslr(arch: Arch, boot: impl Fn() -> Result<Vec<String>>) -> Result<()> {
    let first = boot()?;
    let second = boot()?;
    let (Some(one), Some(two)) = (layout(&first), layout(&second)) else {
        return Err(Error::new(format!(
            "{arch}: a boot did not say where the loader put the kernel"
        )));
    };
    println!("  {arch}: first boot  slide {:#x}; {}", one.slide, one.rest);
    println!("  {arch}: second boot slide {:#x}; {}", two.slide, two.rest);
    if one == two {
        return Err(Error::new(format!(
            "{arch}: two boots got the same layout, so the loader is not randomising"
        )));
    }
    if one.slide == two.slide {
        println!(
            "  {arch}: the image's slide repeated, which happens once in 2^bits pairs; \
             the direct map or the arena moved"
        );
    }
    println!("  {arch}: two boots, two layouts");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    const LOADER: &str =
        "  kaslr    kernel at 0xffffffffa3912000, slide 0x23912000, 18 bits from EFI_RNG";
    const LOADER_REST: &str = "  kaslr    direct map at 0xffff9b9900000000, 16 bits; \
                               vmap arena top at 0xffffffed57140000, 17 bits";
    const KERNEL: &str = "  kaslr    image, direct map and arena moved, 18, 16 and 17 bits \
                          from EFI_RNG";

    #[test]
    fn the_slide_is_read_from_the_loader_and_from_a_panic() {
        assert_eq!(slide_in(LOADER), Some(0x2391_2000));
        assert_eq!(
            slide_in("    3.10 |   kaslr     slide 0x7a30000"),
            Some(0x7a3_0000)
        );
        assert_eq!(slide_in("  trace     #0  0xffffffff80001234"), None);
        assert_eq!(slide_in("  kaslr    NOT randomised: nokaslr"), None);
    }

    #[test]
    fn a_boot_that_moved_passes_and_one_that_did_not_is_named() {
        let moved = lines(&[LOADER, LOADER_REST, KERNEL]);
        assert_eq!(problem(&moved, false, false), None);
        assert_eq!(
            layout(&moved),
            Some(Layout {
                slide: 0x2391_2000,
                rest: LOADER_REST.split_once("at ").unwrap().1.trim().to_owned()
            })
        );

        let counter = lines(&[
            "  kaslr    kernel at 0xf7a30000, slide 0x7a30000, 11 bits from the cycle counter",
            KERNEL,
        ]);
        assert!(
            problem(&counter, false, false)
                .unwrap()
                .contains("randomness")
        );
        assert!(problem(&lines(&[KERNEL]), false, false).is_some());
        assert!(
            problem(&lines(&[LOADER]), false, false).is_some(),
            "the kernel must confirm"
        );
    }

    #[test]
    fn a_fixed_build_and_nokaslr_must_say_they_stayed() {
        let fixed = lines(&[
            "  kaslr    NOT randomised: the kernel is a fixed-address image \
             (--mitigations off); kernel at its link address 0xffffffff80000000",
        ]);
        assert_eq!(problem(&fixed, true, false), None);
        assert!(problem(&lines(&[LOADER, KERNEL]), true, false).is_some());
        let declined = lines(&[
            "  kaslr    NOT randomised: nokaslr on the command line; kernel at its link \
             address 0xffffffff80000000",
        ]);
        assert_eq!(problem(&declined, false, true), None);
        assert!(problem(&fixed, false, true).is_some());
    }

    #[test]
    fn a_coverage_trace_gets_its_boots_slide_beside_it() {
        let directory = std::env::temp_dir().join(format!("ferrix-kaslr-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let trace = directory.join("boot.drcov");
        let plugin = format!("/q/libdrcov.so,filename={}", trace.display());
        record_slide(&plugin, &lines(&[LOADER, LOADER_REST])).unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join("boot.drcov.slide")).unwrap(),
            "0x23912000\n"
        );
        record_slide("/q/libdrcov.so", &lines(&[LOADER])).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
