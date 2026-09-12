//! The panic report, drawn on the framebuffer firmware left behind.
//!
//! Serial is where a panic is reported first, and on a machine with a cable
//! attached nothing here matters. A machine with a screen and no cable — a
//! board on a desk — shows nothing at all without this, and that is the case
//! it exists for.
//!
//! What it draws is the console's recent output, the same bytes the serial
//! port carried, which by the time this runs end with the report itself. So
//! the screen and the log cannot disagree, and there is no second copy of the
//! report to keep in step. The report is picked out from the boot log above it
//! by colour, from its `FERRIX-PANIC` line on.
//!
//! This is the kernel's second output device after the serial port, and it is
//! the same kind of exception `docs/ARCHITECTURE.md` makes for that one: used
//! for panic output only, written once, never read.

use core::cell::UnsafeCell;
use core::fmt::Write;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use ferrix_bootinfo::{Framebuffer, PixelFormat};
use ferrix_fbtext::{BYTES_PER_PIXEL, GLYPH_HEIGHT, PixelOrder, Rect, Rgb, Surface, TextArea};

use crate::{console, mm};

/// Where [`install`] recorded the framebuffer: its mapping, the memory behind
/// it, and how many bytes of it are mapped.
static VIRT: AtomicU64 = AtomicU64::new(0);
/// The physical address the mapping at [`VIRT`] must still resolve to.
static PHYS: AtomicU64 = AtomicU64::new(0);
/// Bytes mapped at [`VIRT`].
static LEN: AtomicU64 = AtomicU64::new(0);
/// Visible width in pixels.
static WIDTH: AtomicU32 = AtomicU32::new(0);
/// Lines that are both visible and mapped.
static HEIGHT: AtomicU32 = AtomicU32::new(0);
/// Pixels from one line to the next.
static STRIDE: AtomicU32 = AtomicU32::new(0);
/// The pixel format, as its discriminant. Stored last, so a reader that sees
/// a format it can draw in also sees everything above.
static FORMAT: AtomicU32 = AtomicU32::new(PixelFormat::Unknown as u32);

/// The screen behind everything.
const BACKGROUND: Rgb = Rgb::new(0x12, 0x12, 0x1A);
/// The banner across the top, which is what says "panic" from across a room.
const BANNER: Rgb = Rgb::new(0xA8, 0x1C, 0x1C);
/// The boot log above the report.
const LOG_TEXT: Rgb = Rgb::new(0x8C, 0x8C, 0x96);
/// The report.
const REPORT_TEXT: Rgb = Rgb::new(0xF2, 0xF2, 0xF2);
/// Space around the edges, in pixels.
const MARGIN: usize = 16;
/// What begins the report, as `panic.rs` prints it.
const MARKER: &[u8] = b"FERRIX-PANIC";

/// How much recent console output is copied out to draw.
const TEXT_BYTES: usize = 4096;

/// Where the recent output is copied: static rather than on the stack, because
/// a panic may be reporting that the stack is nearly gone.
struct Scratch(UnsafeCell<[u8; TEXT_BYTES]>);

// SAFETY: only `draw` touches it, and only the first panic reaches `draw`:
// `panic.rs` lets exactly one caller past its `REPORTING` swap, and every
// later panic halts before drawing. There is never a second accessor.
unsafe impl Sync for Scratch {}

/// The one scratch buffer.
static SCRATCH: Scratch = Scratch(UnsafeCell::new([0; TEXT_BYTES]));

/// Record the framebuffer boot mapped, for a panic to draw on.
///
/// `mapped` bytes of `framebuffer` are mapped at `virt`. A format this kernel
/// cannot draw in records nothing, and so does a mapping too small for a
/// single line.
pub(crate) fn install(framebuffer: &Framebuffer, virt: u64, mapped: u64) {
    if framebuffer.format == PixelFormat::Unknown {
        return;
    }
    let line = u64::from(framebuffer.stride).saturating_mul(BYTES_PER_PIXEL as u64);
    let lines = mapped.checked_div(line).unwrap_or(0);
    let height = u32::try_from(lines)
        .unwrap_or(u32::MAX)
        .min(framebuffer.height);
    if height == 0 {
        return;
    }
    VIRT.store(virt, Ordering::Relaxed);
    PHYS.store(framebuffer.phys, Ordering::Relaxed);
    LEN.store(mapped, Ordering::Relaxed);
    WIDTH.store(framebuffer.width, Ordering::Relaxed);
    HEIGHT.store(height, Ordering::Relaxed);
    STRIDE.store(framebuffer.stride, Ordering::Relaxed);
    FORMAT.store(framebuffer.format as u32, Ordering::Release);
}

/// Draw the panic screen, if there is a framebuffer to draw it on.
///
/// For the one panic that got past `REPORTING`, after it has printed its
/// report: what is drawn is the console's recent output, which by then ends
/// with that report.
pub(crate) fn draw() {
    let order = match FORMAT.load(Ordering::Acquire) {
        format if format == PixelFormat::Bgrx8888 as u32 => PixelOrder::Bgrx,
        format if format == PixelFormat::Rgbx8888 as u32 => PixelOrder::Rgbx,
        _ => return,
    };
    let virt = VIRT.load(Ordering::Relaxed);
    // Still mapped where boot left it, and to the same memory. Nothing since
    // has had a reason to change that, and a panic is not the moment to
    // assume it.
    if mm::translate_in(mm::root_table(), virt) != Some(PHYS.load(Ordering::Relaxed)) {
        return;
    }
    let (Ok(address), Ok(len), Ok(width), Ok(height), Ok(stride)) = (
        usize::try_from(virt),
        usize::try_from(LEN.load(Ordering::Relaxed)),
        usize::try_from(WIDTH.load(Ordering::Relaxed)),
        usize::try_from(HEIGHT.load(Ordering::Relaxed)),
        usize::try_from(STRIDE.load(Ordering::Relaxed)),
    ) else {
        return;
    };

    // SAFETY: `install` recorded `len` bytes mapped at `address`, and the
    // check above shows the mapping still stands. Nothing else writes the
    // framebuffer during a panic: the other processors have been asked to
    // stop, and this is the only report that draws.
    let pixels = unsafe { core::slice::from_raw_parts_mut(address as *mut u8, len) };
    let Some(mut surface) = Surface::new(pixels, width, height, stride, order) else {
        return;
    };
    // SAFETY: the one accessor, as the `Sync` impl for `Scratch` argues.
    let text = unsafe { &mut *SCRATCH.0.get() };
    let count = console::recent(text);
    paint(&mut surface, text.get(..count).unwrap_or_default());
}

/// Paint the banner and as much of `text` as fits below it, newest last.
fn paint(surface: &mut Surface<'_>, text: &[u8]) {
    surface.fill(BACKGROUND);
    let width = surface.width();
    let height = surface.height();
    let inner = width.saturating_sub(MARGIN.saturating_mul(2));
    let banner = GLYPH_HEIGHT.saturating_mul(2).saturating_add(MARGIN);
    surface.fill_rect(0, 0, width, banner, BANNER);

    let title = Rect {
        x: MARGIN,
        y: MARGIN / 2,
        width: inner,
        height: GLYPH_HEIGHT.saturating_mul(2),
    };
    let _ = TextArea::new(surface, title, 2, Rgb::WHITE, None)
        .write_str("Ferrix kernel panic - this processor has stopped");

    let body = Rect {
        x: MARGIN,
        y: banner.saturating_add(MARGIN),
        width: inner,
        height: height.saturating_sub(banner.saturating_add(MARGIN.saturating_mul(2))),
    };
    let mut area = TextArea::new(surface, body, 1, LOG_TEXT, None);
    let shown = tail(text, area.rows(), area.columns());
    let report = report_start(shown);
    for (offset, &byte) in shown.iter().enumerate() {
        if Some(offset) == report {
            area.set_color(REPORT_TEXT);
        }
        match byte {
            b'\r' => {}
            b'\n' => area.newline(),
            0x20..=0x7E => area.put_char(char::from(byte)),
            _ => area.put_char(char::REPLACEMENT_CHARACTER),
        }
    }
}

/// The end of `text` that fits in `rows` rows of `columns` columns, starting
/// at the beginning of a line.
///
/// Counted from the end, because the newest lines are the report and they are
/// the ones that must be on the screen; the boot log above fills whatever is
/// left.
fn tail(text: &[u8], rows: usize, columns: usize) -> &[u8] {
    let columns = columns.max(1);
    // Output normally ends with a newline, which begins no line of its own.
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    let mut used = 0_usize;
    let mut end = body.len();
    let mut from = text.len();
    for line in body.rsplit(|&byte| byte == b'\n') {
        let visible = line.iter().filter(|&&byte| byte != b'\r').count();
        let needs = visible.div_ceil(columns).max(1);
        if used.saturating_add(needs) > rows {
            break;
        }
        used = used.saturating_add(needs);
        from = end.saturating_sub(line.len());
        end = from.saturating_sub(1);
    }
    text.get(from..).unwrap_or_default()
}

/// Where the line holding the last report marker in `text` begins.
fn report_start(text: &[u8]) -> Option<usize> {
    let marker = text
        .windows(MARKER.len())
        .rposition(|window| window == MARKER)?;
    let before = text.get(..marker)?;
    Some(
        before
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |newline| newline + 1),
    )
}
