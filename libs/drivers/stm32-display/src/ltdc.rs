//! The LTDC, the STM32MP15's display controller: timing, its two layers --
//! the frame on the first, the pointer on the second -- and the shadow
//! registers that make a change take effect at vertical blanking.
//!
//! Register offsets and fields are the STM32MP15's (RM0436, "LCD-TFT display
//! controller"), which Linux's `drivers/gpu/drm/stm/ltdc.c` calls hardware
//! version 1.2/1.3 and gives the layer layout `ltdc_layer_regs_a0`.
//!
//! # How a frame changes
//!
//! Every timing and layer register is shadowed. Writing one changes nothing
//! on screen until a reload copies the shadows in: `SRCR.IMR` at once,
//! `SRCR.VBR` at the next vertical blanking. This driver only ever asks for
//! the second, so the controller never switches buffers halfway down the
//! screen, and the reload's interrupt (`ISR.RRIF`) is when a page flip is
//! done. A reload that changes nothing still raises it, which is what paces a
//! flush of the buffer already shown.
//!
//! # The pointer on the second layer
//!
//! The controller blends its second layer over its first, in a window of
//! its own that can be anywhere on the screen, from a buffer of its own.
//! That is a cursor plane: a compositor's pointer image shown there moves
//! by rewriting the window's two position registers, and no frame is
//! composed, turned and flipped for it -- which on the DK1, whose frames
//! take 25 ms on average and 150 ms at worst in software, is the
//! difference between a pointer that keeps up with the hand and one that
//! trails it.
//!
//! Its registers are shadowed like the first layer's and reloaded by the
//! same `SRCR.VBR`: a move asked for while a flip waits for its blanking
//! lands at that blanking with it, and neither waits for the other. The
//! one reload has one cost: a move written in the instant a reload happens
//! can land half this frame and half the next -- the window's position
//! before its buffer's start, say, when the image crosses the screen's edge
//! -- which is one frame of a pointer one step off, and nothing more.
//!
//! The image is premultiplied `ARGB8888`, as a Wayland client's pixels and
//! `hyprix`'s cursor plane are, so the layer blends with a factor of one
//! for its own colour and one less its pixel's alpha for what is beneath
//! ([`BFCR_PREMULTIPLIED`]). Outside the window the layer contributes its
//! default colour, which is left transparent black: nothing.

use crate::Registers;
use crate::mode::Mode;

/// Identification.
pub const IDR: u32 = 0x00;
/// Layer count.
pub const LCR: u32 = 0x04;
/// Synchronization size.
pub const SSCR: u32 = 0x08;
/// Back porch, accumulated.
pub const BPCR: u32 = 0x0C;
/// Active width and height, accumulated.
pub const AWCR: u32 = 0x10;
/// Total width and height.
pub const TWCR: u32 = 0x14;
/// Global control.
pub const GCR: u32 = 0x18;
/// Global configuration 2: the bus width.
pub const GC2R: u32 = 0x20;
/// Shadow reload.
pub const SRCR: u32 = 0x24;
/// Background colour.
pub const BCCR: u32 = 0x2C;
/// Interrupt enable.
pub const IER: u32 = 0x34;
/// Interrupt status.
pub const ISR: u32 = 0x38;
/// Interrupt clear.
pub const ICR: u32 = 0x3C;

/// The first layer's registers start here; the second's 0x80 further on.
pub const LAYER: u32 = 0x84;
/// Distance from one layer's registers to the next's.
pub const LAYER_STRIDE: u32 = 0x80;
/// A layer's control, from [`LAYER`].
pub const L_CR: u32 = 0x00;
/// Window horizontal position.
pub const L_WHPCR: u32 = 0x04;
/// Window vertical position.
pub const L_WVPCR: u32 = 0x08;
/// Pixel format.
pub const L_PFCR: u32 = 0x10;
/// Constant alpha.
pub const L_CACR: u32 = 0x14;
/// Default colour.
pub const L_DCCR: u32 = 0x18;
/// Blending factors.
pub const L_BFCR: u32 = 0x1C;
/// Colour frame buffer address.
pub const L_CFBAR: u32 = 0x28;
/// Colour frame buffer pitch and line length.
pub const L_CFBLR: u32 = 0x2C;
/// Colour frame buffer line count.
pub const L_CFBLNR: u32 = 0x30;

/// `GCR`: the controller is on.
pub const GCR_LTDCEN: u32 = 1 << 0;
/// `GCR`: horizontal sync active high.
pub const GCR_HSPOL: u32 = 1 << 31;
/// `GCR`: vertical sync active high.
pub const GCR_VSPOL: u32 = 1 << 30;
/// `SRCR`: reload now.
pub const SRCR_IMR: u32 = 1 << 0;
/// `SRCR`: reload at the next vertical blanking.
pub const SRCR_VBR: u32 = 1 << 1;
/// `IER`/`ISR`/`ICR`: a transfer error on the bus.
pub const INT_TERR: u32 = 1 << 2;
/// `IER`/`ISR`/`ICR`: a reload happened.
pub const INT_RR: u32 = 1 << 3;
/// `IER`/`ISR`/`ICR`: the FIFO ran dry and a pixel went out wrong.
pub const INT_FUE: u32 = 1 << 6;
/// `IER`/`ISR`/`ICR`: the FIFO ran dry on the first layer (a warning on
/// this version).
pub const INT_FUW: u32 = 1 << 1;
/// A layer's `CR`: the layer is shown.
pub const L_CR_LEN: u32 = 1 << 0;
/// `PFCR`'s ARGB8888, the format DRM's XRGB8888 is scanned out as.
pub const PF_ARGB8888: u32 = 0;
/// `BFCR`: blend with the constant alpha alone, ignoring the pixels' own
/// alpha byte, which in XRGB8888 is whatever the program left there.
pub const BFCR_CONSTANT: u32 = (0b100 << 8) | 0b101;
/// `BFCR` for premultiplied pixels: `BF1` the constant alpha (0xFF, so
/// one) times the layer's colour, which already carries its alpha, and
/// `BF2` one less pixel alpha times constant alpha times what is beneath --
/// RM0436's `BF1 = 100` and `BF2 = 111`, the Porter-Duff "over" for
/// premultiplied colour.
pub const BFCR_PREMULTIPLIED: u32 = (0b100 << 8) | 0b111;
/// The layer the frame is on.
pub const FRAME_LAYER: u32 = 0;
/// The layer the pointer is on.
pub const CURSOR_LAYER: u32 = 1;

/// The versions whose register layout this module is written for: the
/// STM32MP15's, which Linux calls 1.2 and 1.3.
pub const KNOWN_VERSIONS: [u32; 2] = [0x0001_0200, 0x0001_0300];

/// Why the controller cannot be driven.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LtdcError {
    /// `IDR` names a version with another register layout.
    Version(u32),
    /// Fewer than one layer, or a bus width that makes no sense.
    Layout,
    /// The mode does not fit the controller's counters.
    Mode,
    /// A buffer that does not fit the mode or the controller's fields, or an
    /// address the controller cannot reach.
    Buffer,
}

/// The four timing words and the polarity bits for `mode`, in the
/// controller's accumulated form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timing {
    /// `SSCR`.
    pub sscr: u32,
    /// `BPCR`.
    pub bpcr: u32,
    /// `AWCR`.
    pub awcr: u32,
    /// `TWCR`.
    pub twcr: u32,
    /// `GCR`'s polarity bits.
    pub polarity: u32,
}

impl Timing {
    /// The words for `mode`, or `None` if it does not fit: widths are twelve
    /// bits and heights eleven.
    ///
    /// The controller counts sync, then back porch, then active, then front
    /// porch, each field one less than the running total at its end, which
    /// is Linux's `ltdc_crtc_mode_set_nofb` arithmetic.
    #[must_use]
    pub fn of(mode: &Mode) -> Option<Timing> {
        if !mode.is_consistent() {
            return None;
        }
        let hsync = u32::from(mode.hsync_end - mode.hsync_start) - 1;
        let vsync = u32::from(mode.vsync_end - mode.vsync_start) - 1;
        let hbp = u32::from(mode.htotal - mode.hsync_start) - 1;
        let vbp = u32::from(mode.vtotal - mode.vsync_start) - 1;
        let active_w = hbp + u32::from(mode.hdisplay);
        let active_h = vbp + u32::from(mode.vdisplay);
        let total_w = u32::from(mode.htotal) - 1;
        let total_h = u32::from(mode.vtotal) - 1;
        if total_w > 0xFFF || total_h > 0x7FF {
            return None;
        }
        let pair = |w: u32, h: u32| (w << 16) | h;
        Some(Timing {
            sscr: pair(hsync, vsync),
            bpcr: pair(hbp, vbp),
            awcr: pair(active_w, active_h),
            twcr: pair(total_w, total_h),
            polarity: if mode.hsync_high { GCR_HSPOL } else { 0 }
                | if mode.vsync_high { GCR_VSPOL } else { 0 },
        })
    }
}

/// A buffer the first layer can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Frame {
    /// Bus address of the first pixel: the buffer is one contiguous run from
    /// here, since the controller reads it with nothing to translate.
    pub address: u32,
    /// Bytes from one row to the next.
    pub pitch: u32,
    /// Pixels a row.
    pub width: u32,
    /// Rows.
    pub height: u32,
}

/// A pointer image the second layer can show: premultiplied `ARGB8888`,
/// one contiguous run like a [`Frame`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CursorImage {
    /// Bus address of its top-left pixel.
    pub address: u32,
    /// Bytes from one row to the next.
    pub pitch: u32,
    /// Pixels a row.
    pub width: u32,
    /// Rows.
    pub height: u32,
}

/// The part of a pointer image that is on the screen, and where: what the
/// second layer's window and buffer registers are made from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Clip {
    /// Bus address of the first pixel shown: the image's own, moved on by
    /// the rows cut off above and the columns cut off to the left.
    pub address: u32,
    /// The screen column of the first pixel shown, from 0.
    pub x: u32,
    /// The screen row of the first pixel shown, from 0.
    pub y: u32,
    /// Columns shown.
    pub width: u32,
    /// Rows shown.
    pub height: u32,
}

impl Clip {
    /// What of `image`, its top-left corner at `at` on a screen `screen`
    /// pixels big, is on the screen: `None` when nothing is.
    ///
    /// The window cannot start left of the screen or above it, so an image
    /// partly off the left or the top edge is shown from the first column or
    /// row that is on it, read from that far into the buffer; off the right
    /// or the bottom edge the window is only narrower or shorter.
    #[must_use]
    pub fn of(image: &CursorImage, at: (i32, i32), screen: (u32, u32)) -> Option<Clip> {
        let span = |at: i32, length: u32, limit: u32| -> Option<(u32, u32, u32)> {
            let (at, length, limit) = (i64::from(at), i64::from(length), i64::from(limit));
            let first = at.max(0);
            let end = at.saturating_add(length).min(limit);
            if first >= end {
                return None;
            }
            // Where on the screen, how many, and how many of the image's
            // were cut off before it.
            Some((
                u32::try_from(first).ok()?,
                u32::try_from(end - first).ok()?,
                u32::try_from(first - at).ok()?,
            ))
        };
        let (x, width, left) = span(at.0, image.width, screen.0)?;
        let (y, height, top) = span(at.1, image.height, screen.1)?;
        let skip = top
            .checked_mul(image.pitch)?
            .checked_add(left.checked_mul(4)?)?;
        Some(Clip {
            address: image.address.checked_add(skip)?,
            x,
            y,
            width,
            height,
        })
    }
}

/// What the interrupt said, taken and cleared.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Events {
    /// A reload happened: a flip is done.
    pub reloaded: bool,
    /// The FIFO ran dry: the bus could not keep up and the screen showed
    /// something wrong for a moment.
    pub underrun: bool,
    /// A bus error reading a buffer.
    pub transfer_error: bool,
}

/// The controller, driving its first layer and, where it has one, its
/// second as the pointer's.
#[derive(Debug)]
pub struct Ltdc<R: Registers> {
    registers: R,
    /// Bytes the controller's bus moves at once, which a line length counts
    /// past its end.
    bus_bytes: u32,
    /// How many layers it has: `LCR`, two on every STM32MP15.
    layers: u32,
    /// The mode it is running, once started.
    mode: Option<Mode>,
    /// The timing that mode came to.
    timing: Option<Timing>,
}

impl<R: Registers> Ltdc<R> {
    /// Take the controller, refusing one whose registers are not laid out as
    /// this module expects.
    ///
    /// # Errors
    ///
    /// [`LtdcError::Version`] or [`LtdcError::Layout`].
    pub fn new(registers: R) -> Result<Self, LtdcError> {
        let version = registers.read32(IDR);
        if !KNOWN_VERSIONS.contains(&version) {
            return Err(LtdcError::Version(version));
        }
        let layers = registers.read32(LCR) & 0xF;
        if layers == 0 {
            return Err(LtdcError::Layout);
        }
        let width_log2 = (registers.read32(GC2R) >> 4) & 0x7;
        if width_log2 > 4 {
            return Err(LtdcError::Layout);
        }
        Ok(Ltdc {
            registers,
            bus_bytes: 1 << width_log2,
            layers,
            mode: None,
            timing: None,
        })
    }

    /// The registers, for a test to look at.
    pub const fn registers(&self) -> &R {
        &self.registers
    }

    /// The registers, for a test to move its model of the screen on.
    #[cfg(test)]
    pub(crate) const fn registers_mut(&mut self) -> &mut R {
        &mut self.registers
    }

    /// Whether there is a second layer to show a pointer on.
    pub const fn has_cursor_layer(&self) -> bool {
        self.layers > CURSOR_LAYER
    }

    /// The mode running, once [`Ltdc::start`] has run.
    pub const fn mode(&self) -> Option<Mode> {
        self.mode
    }

    /// Run `mode` with every layer off and a black background, the reload,
    /// transfer-error and underrun interrupts enabled, and the controller on.
    ///
    /// # Errors
    ///
    /// [`LtdcError::Mode`] for a mode the counters cannot hold.
    pub fn start(&mut self, mode: &Mode) -> Result<(), LtdcError> {
        let timing = Timing::of(mode).ok_or(LtdcError::Mode)?;
        let r = &mut self.registers;
        // Off while the timing changes; the polarities live in GCR too.
        r.write32(GCR, 0);
        r.write32(SSCR, timing.sscr);
        r.write32(BPCR, timing.bpcr);
        r.write32(AWCR, timing.awcr);
        r.write32(TWCR, timing.twcr);
        r.write32(BCCR, 0);
        for layer in [FRAME_LAYER, CURSOR_LAYER] {
            r.write32(LAYER + layer * LAYER_STRIDE + L_CR, 0);
        }
        r.write32(ICR, INT_RR | INT_FUE | INT_FUW | INT_TERR);
        r.write32(IER, INT_RR | INT_FUE | INT_TERR);
        r.write32(SRCR, SRCR_IMR);
        r.write32(GCR, timing.polarity | GCR_LTDCEN);
        self.mode = Some(*mode);
        self.timing = Some(timing);
        Ok(())
    }

    /// Point the first layer at `frame`, full screen, and ask for the change
    /// at the next vertical blanking.
    ///
    /// # Errors
    ///
    /// [`LtdcError::Mode`] before [`Ltdc::start`], and [`LtdcError::Buffer`]
    /// for a frame that is not the mode's size, whose rows are shorter than
    /// its pixels, or whose pitch or line length the fields cannot hold.
    pub fn show(&mut self, frame: &Frame) -> Result<(), LtdcError> {
        let (Some(mode), Some(timing)) = (self.mode, self.timing) else {
            return Err(LtdcError::Mode);
        };
        let row = frame.width.checked_mul(4).ok_or(LtdcError::Buffer)?;
        let line = row + self.bus_bytes - 1;
        if frame.width != u32::from(mode.hdisplay)
            || frame.height != u32::from(mode.vdisplay)
            || frame.pitch < row
            || frame.pitch > 0xFFFF
            || line > 0x1FFF
            || !frame.address.is_multiple_of(4)
        {
            return Err(LtdcError::Buffer);
        }
        let hbp = timing.bpcr >> 16;
        let vbp = timing.bpcr & 0x7FF;
        let first_x = hbp + 1;
        let last_x = hbp + frame.width;
        let first_y = vbp + 1;
        let last_y = vbp + frame.height;
        let r = &mut self.registers;
        let at = |offset: u32| LAYER + offset;
        r.write32(at(L_WHPCR), (last_x << 16) | first_x);
        r.write32(at(L_WVPCR), (last_y << 16) | first_y);
        r.write32(at(L_PFCR), PF_ARGB8888);
        r.write32(at(L_CACR), 0xFF);
        r.write32(at(L_DCCR), 0);
        r.write32(at(L_BFCR), BFCR_CONSTANT);
        r.write32(at(L_CFBAR), frame.address);
        r.write32(at(L_CFBLR), (frame.pitch << 16) | line);
        r.write32(at(L_CFBLNR), frame.height);
        r.write32(at(L_CR), L_CR_LEN);
        r.write32(SRCR, SRCR_VBR);
        Ok(())
    }

    /// Take the first layer off the screen, leaving the black background, at
    /// the next vertical blanking.
    pub fn hide(&mut self) {
        self.registers.write32(LAYER + L_CR, 0);
        self.registers.write32(SRCR, SRCR_VBR);
    }

    /// Show `image` on the second layer with its top-left corner at `at` in
    /// the screen's pixels -- anywhere, including partly or wholly off the
    /// screen -- from the next vertical blanking. Whether any of it is on
    /// the screen: an image wholly off it takes the layer off.
    ///
    /// Every register of the layer is written each time, which is a dozen
    /// writes for a move: the layer then needs no state here, and a mode
    /// switch that turned it off is undone by the next call like any other.
    ///
    /// # Errors
    ///
    /// [`LtdcError::Mode`] before [`Ltdc::start`] or on a controller with
    /// one layer, and [`LtdcError::Buffer`] for an image whose rows are
    /// shorter than its pixels, whose pitch or line length the fields cannot
    /// hold, or whose address is not a pixel's; nothing is written then.
    pub fn show_cursor(&mut self, image: &CursorImage, at: (i32, i32)) -> Result<bool, LtdcError> {
        let (Some(mode), Some(timing)) = (self.mode, self.timing) else {
            return Err(LtdcError::Mode);
        };
        if !self.has_cursor_layer() {
            return Err(LtdcError::Mode);
        }
        let row = image.width.checked_mul(4).ok_or(LtdcError::Buffer)?;
        if image.pitch < row
            || image.pitch > 0xFFFF
            || row + self.bus_bytes - 1 > 0x1FFF
            || !image.address.is_multiple_of(4)
        {
            return Err(LtdcError::Buffer);
        }
        let screen = (u32::from(mode.hdisplay), u32::from(mode.vdisplay));
        let Some(clip) = Clip::of(image, at, screen) else {
            self.hide_cursor();
            return Ok(false);
        };
        let hbp = timing.bpcr >> 16;
        let vbp = timing.bpcr & 0x7FF;
        let first_x = hbp + 1 + clip.x;
        let last_x = first_x + clip.width - 1;
        let first_y = vbp + 1 + clip.y;
        let last_y = first_y + clip.height - 1;
        let line = clip.width * 4 + self.bus_bytes - 1;
        let r = &mut self.registers;
        let at = |offset: u32| LAYER + CURSOR_LAYER * LAYER_STRIDE + offset;
        r.write32(at(L_WHPCR), (last_x << 16) | first_x);
        r.write32(at(L_WVPCR), (last_y << 16) | first_y);
        r.write32(at(L_PFCR), PF_ARGB8888);
        r.write32(at(L_CACR), 0xFF);
        r.write32(at(L_DCCR), 0);
        r.write32(at(L_BFCR), BFCR_PREMULTIPLIED);
        r.write32(at(L_CFBAR), clip.address);
        r.write32(at(L_CFBLR), (image.pitch << 16) | line);
        r.write32(at(L_CFBLNR), clip.height);
        r.write32(at(L_CR), L_CR_LEN);
        r.write32(SRCR, SRCR_VBR);
        Ok(true)
    }

    /// Take the second layer off the screen at the next vertical blanking:
    /// the pointer hidden, and its buffer no longer read once the reload
    /// has happened.
    pub fn hide_cursor(&mut self) {
        if self.has_cursor_layer() {
            self.registers
                .write32(LAYER + CURSOR_LAYER * LAYER_STRIDE + L_CR, 0);
            self.registers.write32(SRCR, SRCR_VBR);
        }
    }

    /// Ask for a reload at the next vertical blanking with nothing changed:
    /// its interrupt is when a flush of the buffer on screen is done.
    pub fn request_reload(&mut self) {
        self.registers.write32(SRCR, SRCR_VBR);
    }

    /// Whether a reload asked for has not happened yet.
    pub fn reload_pending(&self) -> bool {
        self.registers.read32(SRCR) & (SRCR_VBR | SRCR_IMR) != 0
    }

    /// Take what the interrupt status says and clear it.
    pub fn take_events(&mut self) -> Events {
        let status = self.registers.read32(ISR);
        let seen = status & (INT_RR | INT_FUE | INT_FUW | INT_TERR);
        if seen != 0 {
            self.registers.write32(ICR, seen);
        }
        Events {
            reloaded: status & INT_RR != 0,
            underrun: status & (INT_FUE | INT_FUW) != 0,
            transfer_error: status & INT_TERR != 0,
        }
    }

    /// Turn the controller off: nothing more is read from memory, which is
    /// what makes it safe to give the buffers back.
    pub fn stop(&mut self) {
        let r = &mut self.registers;
        r.write32(IER, 0);
        for layer in [FRAME_LAYER, CURSOR_LAYER] {
            r.write32(LAYER + layer * LAYER_STRIDE + L_CR, 0);
        }
        r.write32(SRCR, SRCR_IMR);
        r.write32(GCR, 0);
        self.mode = None;
        self.timing = None;
    }
}
