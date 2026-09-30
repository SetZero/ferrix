//! The two pointer boots: the arrow drawn into the frame, and the arrow on
//! the card's cursor plane, where no screendump can see it.
//!
//! Both sweep the pointer through four hundred places, the way a hand moves a
//! mouse, before they judge where it was put down: a compositor that drew a
//! frame for every report passed a single movement and stuttered under a
//! hand.

use std::path::Path;
use std::time::{Duration, Instant};

use super::{
    Carried, EITHER, FAILED, MARKER, Programs, SETTLE, Wanted, ask_for_state, boot_and_dump,
    build_image, differences, expected, say_the_marker, settle, undithered, unexpected,
    with_the_transcript,
};
use crate::args::Args;
use crate::display::{Qmp, free_port};
use crate::paths::{self, Arch};
use crate::qemu::Watching;
use crate::{Error, Result};

/// The two pictures the pointer boot requires: the windows, and the same
/// windows with the pointer on them.
///
/// The pointer is not drawn until it has moved, because until then the
/// compositor has only its own guess at where the mouse is -- the middle of
/// the screen -- and an arrow drawn at a guess is worse than none. So the
/// first picture is every other boot's, and the second is the one the
/// movement makes.
pub(super) const POINTER_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled, with no pointer drawn because none has moved",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the pointer over the windows, its tip where the mouse was put",
        "src/user/linux/compositor/render/tests/data/pointer-on-two-clients.xrle",
    ),
];

/// Where the pointer is put, in QMP's 0..0x7FFF across the screen.
///
/// `src/user/linux/compositor/render` blesses the picture with the arrow's tip at
/// (700, 300) on a 1024x768 screen, and these are those two as a fraction
/// of the axis QEMU's virtio tablet reports.
///
/// Inside the window that already has the focus, because
/// `input:follow_mouse` is on: a pointer moved into the other one would
/// take the focus with it and draw the active border somewhere else, which
/// is a different picture and not the one this boot is about.
pub(super) const POINTER_AT: (i32, i32) = (22401, 12800);

/// How many places the pointer is swept through on its way to
/// [`POINTER_AT`], and how long after each the next is sent.
///
/// A hand moving a mouse, as a guest sees it: a few hundred reports a
/// second, each a few pixels on from the last. The boot used to put the
/// pointer down once, and a compositor that drew a whole frame for every
/// report -- each ending in the whole framebuffer sent to the host -- passed
/// it and stuttered under a hand. So the pointer is swept first, and two
/// things are required of the sweep. The picture at the end is the one a
/// single movement makes, to the pixel: no frame of the four hundred left
/// an arrow behind on the host's copy of the screen, which is what sending
/// only what changed would do if what changed were worked out wrong. And
/// the compositor drew at the screen's rate and not the mouse's:
/// [`MOST_FRAMES`].
pub(super) const SWEEP: (u32, Duration) = (400, Duration::from_millis(3));

/// The most frames one of the compositor's frame reports may count.
///
/// A report is printed by the first frame a second or more after the last,
/// so at sixty frames a second it counts sixty or so however long the quiet
/// before it was. Three hundred reports a second drawn one for one count
/// three hundred.
const MOST_FRAMES: u32 = 75;

/// Where the pointer is at `step` of the sweep, in QMP's 0..0x7FFF.
///
/// Inside the focused window for the reason [`POINTER_AT`] gives, down and
/// back up its whole height so that it crosses the half of the gradient that
/// can be seen through, which is the half with a blur behind it.
pub(super) fn swept(step: u32) -> (i32, i32) {
    let (steps, _) = SWEEP;
    let along = i32::try_from(step).unwrap_or(0);
    let of = i32::try_from(steps).unwrap_or(1).max(1);
    // x from 56% to 94% of the screen, y a triangle wave from 10% to 90%.
    let x = 18_350 + (12_450 * along) / of;
    let wave = (along * 4) % (of * 2);
    let up = if wave < of { wave } else { of * 2 - wave };
    let y = 3_277 + (26_213 * up) / of;
    (x, y)
}

/// The configuration the seventeenth boot is given: the two windows, and
/// the arrow drawn into the frame, which is what a screendump can see -- a
/// pointer on the card's cursor plane is the `cursor` boot's to judge.
const POINTER_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
cursor:no_hardware_cursors = 1
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
";

/// A seventeenth boot: the pointer, drawn.
///
/// A compositor with a mouse and no arrow on the screen is one a person
/// cannot use. This moves the pointer with QMP and requires the screen to
/// become the picture `src/user/linux/compositor/render` blesses for the arrow at that
/// point -- and to have been the ordinary tiled pair before it, because the
/// pointer is not drawn until it has moved.
pub(super) fn test_pointer(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (screens, said) = boot_and_dump(
        arch,
        programs,
        POINTER_CONFIG,
        &Wanted {
            states: &POINTER_EXPECTED,
            others: &[],
            moving: None,
            pointer: Some(POINTER_AT),
            awaiting: &[],
        },
        &[],
        args,
    )?;
    let (Some(before), Some(after)) = (screens.first(), screens.get(1)) else {
        return Err(Error::new(format!(
            "{arch}: the pointer boot took {} pictures",
            screens.len()
        )));
    };
    if before.pixels == after.pixels {
        return Err(Error::new(format!(
            "{arch}: moving the pointer drew nothing"
        )));
    }
    // The compositor's own count of what the sweep cost: `hyprix: frames 412
    // slowest of the last 58 1312 us`, a second or more apart.
    let counted: Vec<u32> = said
        .iter()
        .filter_map(|line| {
            line.split_once("slowest of the last ")?
                .1
                .split(' ')
                .next()?
                .parse()
                .ok()
        })
        .collect();
    let Some(most) = counted.iter().copied().max() else {
        return Err(Error::new(format!(
            "{arch}: the compositor never said how many frames it drew"
        )));
    };
    // The sweep lasts a second and is drawn: a report that counted a
    // handful is a sweep that was not seen at all.
    if most < 20 {
        return Err(Error::new(format!(
            "{arch}: the most frames a report counted is {most}, so the sweep was not drawn"
        )));
    }
    if most > MOST_FRAMES {
        return Err(Error::new(format!(
            "{arch}: one report counted {most} frames, over {MOST_FRAMES}: the compositor is \
             drawing a frame for every report of the mouse rather than at the screen's rate"
        )));
    }
    println!(
        "  {arch}: the pointer was swept over the windows and put down, drawn at the screen's \
         rate ({most} frames the most in a report) and left where the mouse was put, in every \
         one of {} pixels",
        after.width * after.height
    );
    Ok(())
}

/// The pictures the cursor boot requires: the two windows before the pointer
/// is used, and after it has been swept over them and put down -- the same
/// picture both times, because a pointer on the card's cursor plane is not
/// drawn into the frame at all.
const CURSOR_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled, with no pointer drawn because none has moved",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "the pointer swept over the windows and put down, and still not in the frame",
        "src/user/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
];

/// The configuration the cursor boot is given: the pointer boot's two
/// windows, with the pointer left to the card's cursor plane, which is
/// Hyprland's default.
const CURSOR_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
";

/// The most frames the compositor may draw while the pointer is swept over
/// the windows and put down. None is owed: a few for whatever the clients
/// do is all that is allowed, where the same sweep drawn into the frame is
/// sixty and more.
const PLANE_FRAMES: u32 = 10;

/// Where the pointer boot's arrow is drawn, in the screen's pixels:
/// [`POINTER_AT`] on a 1024 x 768 screen.
const POINTER_PIXEL: (usize, usize) = (700, 300);

/// The most frames the compositor has said it drew, over every frame report
/// in `lines`: `hyprix: frames 412 slowest of the last 58 ...`.
fn frames_drawn<'a>(lines: impl Iterator<Item = &'a String>) -> u32 {
    lines
        .filter_map(|line| {
            line.split_once("hyprix: frames ")?
                .1
                .split(' ')
                .next()?
                .parse()
                .ok()
        })
        .max()
        .unwrap_or(0)
}

/// The twenty-second boot: the pointer on the card's cursor plane.
///
/// A pointer on a plane is the host's to show over the frame, so a
/// screendump -- which is the frame -- cannot see it. The judge is a VNC
/// viewer asking QEMU for the pointer's shape, which is how QEMU hands a
/// plane to anything that shows the screen, and how a served desktop's
/// pointer comes to have no lag at all (`docs/GPU.md` §3.9). Four things are
/// required: the frame is the two windows and nothing else, before the
/// pointer moved and after it was swept through four hundred places and put
/// down; the sweep cost the compositor no frames to speak of; the
/// compositor says the pointer is on the plane; and the shape the viewer is
/// handed is, pixel for pixel, the arrow the pointer boot's picture has drawn
/// into it, hotspot and all.
pub(super) fn test_cursor(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let (image, kernel) = build_image(
        arch,
        programs,
        &undithered(CURSOR_CONFIG),
        Carried::none(),
        args,
    )?;
    let port = free_port()?;
    // A VNC display is a port above 5900; the loopback's free ports are.
    let vnc = loop {
        let candidate = free_port()?;
        if candidate > 5900 {
            break candidate;
        }
    };
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    qemu_args.judge_vnc = Some(vnc);
    let dump = paths::build_dir(arch).join("compositor.ppm");
    let mut shape = None;
    let mut said = Vec::new();
    let mut before = 0;
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        watching.stop_when_done();
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        if let Some(line) = watching
            .lines()
            .iter()
            .rev()
            .find(|line| line.contains(FAILED))
        {
            return Err(Error::new(format!("{arch}: {}", line.trim())));
        }
        let up = watching.read_more(Instant::now() + SETTLE, |lines| {
            lines.iter().any(|line| line.contains(MARKER))
        })?;
        if !up && !watching.lines().iter().any(|line| line.contains(MARKER)) {
            return Err(Error::new(format!(
                "{arch}: the compositor never printed `{MARKER}`"
            )));
        }
        say_the_marker(watching, arch);
        before = cursor_states(watching, &mut qmp, arch, &dump)?;
        let mut viewer =
            crate::vnc::Viewer::connect(vnc, Instant::now() + Duration::from_secs(10))?;
        shape = viewer.cursor(Instant::now() + Duration::from_secs(10))?;
        let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
        said = watching
            .lines()
            .iter()
            .chain(watching.after())
            .cloned()
            .collect();
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    on_the_plane(arch, &said, before, shape)
}

/// [`test_cursor`]'s two pictures, each required to be the two windows and
/// nothing else, with the sweep before the second: what the compositor had
/// drawn before the sweep, by its own count.
fn cursor_states(
    watching: &mut Watching<'_>,
    qmp: &mut Qmp,
    arch: Arch,
    dump: &Path,
) -> Result<u32> {
    let mut before = 0;
    for (index, (what, path)) in CURSOR_EXPECTED.iter().enumerate() {
        if index == 1 {
            let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
            before = frames_drawn(watching.lines().iter().chain(watching.after()));
        }
        ask_for_state(qmp, arch, index, &[], Some(POINTER_AT))?;
        let want = expected(path)?;
        let screen = settle(qmp, dump, &want)?;
        let (found, count) = differences(&screen, &want);
        if count != 0 {
            let _ = watching.read_more(Instant::now() + Duration::from_secs(2), |_| false)?;
            return Err(with_the_transcript(
                &unexpected(arch, what, &screen, found, count),
                watching,
            ));
        }
        println!(
            "  {arch}: {what}, every one of {} pixels as the renderer draws them",
            screen.width * screen.height
        );
    }
    Ok(before)
}

/// [`test_cursor`]'s verdict on what the boot said and what the viewer was
/// handed.
fn on_the_plane(
    arch: Arch,
    said: &[String],
    before: u32,
    shape: Option<crate::vnc::Shape>,
) -> Result<()> {
    if let Some(line) = said.iter().find(|line| line.contains("FERRIX-PANIC")) {
        return Err(Error::new(format!(
            "{arch}: the kernel stopped while the compositor ran: {}",
            line.trim()
        )));
    }
    if !said
        .iter()
        .any(|line| line.contains("the pointer is on the card's cursor plane"))
    {
        return Err(Error::new(format!(
            "{arch}: the compositor never put the pointer on the card's cursor plane"
        )));
    }
    let swept = frames_drawn(said.iter()).saturating_sub(before);
    if swept > PLANE_FRAMES {
        return Err(Error::new(format!(
            "{arch}: sweeping the pointer cost {swept} frames, over {PLANE_FRAMES}: the pointer \
             is being drawn into the frame"
        )));
    }
    let Some(shape) = shape else {
        return Err(Error::new(format!(
            "{arch}: a VNC viewer was never handed the pointer's shape"
        )));
    };
    same_arrow(arch, &shape)?;
    println!(
        "  {arch}: the pointer is on the card's cursor plane: {} frames for a sweep of {}, and a \
         viewer handed the {}x{} arrow the frame would have drawn, hotspot ({}, {})",
        swept, SWEEP.0, shape.size.0, shape.size.1, shape.hot.0, shape.hot.1
    );
    Ok(())
}

/// Whether `shape` is the arrow the pointer boot's picture has drawn into it
/// at [`POINTER_PIXEL`]: every pixel the shape shows is that picture's pixel
/// there, and every pixel it does not is the picture without a pointer.
fn same_arrow(arch: Arch, shape: &crate::vnc::Shape) -> Result<()> {
    let plain = expected(POINTER_EXPECTED[0].1)?;
    let pointed = expected(POINTER_EXPECTED[1].1)?;
    const WIDTH: usize = 1024;
    let at = |x: u16, y: u16| -> Option<usize> {
        let x = (POINTER_PIXEL.0 + usize::from(x)).checked_sub(usize::from(shape.hot.0))?;
        let y = (POINTER_PIXEL.1 + usize::from(y)).checked_sub(usize::from(shape.hot.1))?;
        Some((y * WIDTH + x) * 3)
    };
    let mut shown = 0;
    for y in 0..shape.size.1 {
        for x in 0..shape.size.0 {
            let Some(index) = at(x, y) else { continue };
            let (Some(drawn), Some(under)) =
                (pointed.get(index..index + 3), plain.get(index..index + 3))
            else {
                continue;
            };
            if shape.shows(x, y) {
                shown += 1;
                let Some([blue, green, red]) = shape.pixel(x, y) else {
                    continue;
                };
                if drawn != [red, green, blue] {
                    return Err(Error::new(format!(
                        "{arch}: the viewer's pointer has {:?} at ({x}, {y}) where the frame's \
                         arrow has {drawn:?}",
                        [red, green, blue]
                    )));
                }
            } else if drawn != under {
                return Err(Error::new(format!(
                    "{arch}: the viewer's pointer shows nothing at ({x}, {y}), where the frame's \
                     arrow is drawn"
                )));
            }
        }
    }
    if shown == 0 {
        return Err(Error::new(format!(
            "{arch}: the viewer was handed a pointer with nothing in it"
        )));
    }
    Ok(())
}
