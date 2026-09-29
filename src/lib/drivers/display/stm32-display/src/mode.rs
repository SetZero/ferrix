//! Video modes: the numbers a display controller counts pixels and lines by.

/// One video mode, in DRM's terms: each `*_start` and `*_end` counts from the
/// first active pixel or line, and `total` is the whole period.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mode {
    /// The pixel clock, in kHz.
    pub clock_khz: u32,
    /// Active pixels a line.
    pub hdisplay: u16,
    /// Where horizontal sync starts.
    pub hsync_start: u16,
    /// Where it ends.
    pub hsync_end: u16,
    /// Pixel clocks a line, blanking included.
    pub htotal: u16,
    /// Active lines a frame.
    pub vdisplay: u16,
    /// Where vertical sync starts.
    pub vsync_start: u16,
    /// Where it ends.
    pub vsync_end: u16,
    /// Lines a frame, blanking included.
    pub vtotal: u16,
    /// Whether horizontal sync is active high.
    pub hsync_high: bool,
    /// Whether vertical sync is active high.
    pub vsync_high: bool,
    /// CEA-861's video identification code, 0 for a mode it does not number.
    pub vic: u8,
}

impl Mode {
    /// CEA-861 VIC 4: 1280x720 progressive at 60 Hz, a 74.25 MHz pixel clock.
    ///
    /// The mode the DK board scanned out before it read EDIDs, and the one
    /// it falls back to: its pixel clock is exactly what the board's
    /// firmware leaves on PLL4's Q output (24 MHz / 4 x 99 / 8), which clocks
    /// the LTDC, and every HDMI sink takes it (`crate::choice`).
    pub const CEA_720P60: Mode = Mode {
        clock_khz: 74_250,
        hdisplay: 1280,
        hsync_start: 1390,
        hsync_end: 1430,
        htotal: 1650,
        vdisplay: 720,
        vsync_start: 725,
        vsync_end: 730,
        vtotal: 750,
        hsync_high: true,
        vsync_high: true,
        vic: 4,
    };

    /// Whether the numbers describe a mode at all: every span non-empty and
    /// in order.
    #[must_use]
    pub const fn is_consistent(&self) -> bool {
        self.clock_khz > 0
            && 0 < self.hdisplay
            && self.hdisplay < self.hsync_start
            && self.hsync_start < self.hsync_end
            && self.hsync_end < self.htotal
            && 0 < self.vdisplay
            && self.vdisplay < self.vsync_start
            && self.vsync_start < self.vsync_end
            && self.vsync_end < self.vtotal
    }

    /// Frames a second, rounded to the nearest.
    #[must_use]
    pub const fn refresh_hz(&self) -> u32 {
        let per_frame = self.htotal as u64 * self.vtotal as u64;
        if per_frame == 0 {
            return 0;
        }
        ((self.clock_khz as u64 * 1000 + per_frame / 2) / per_frame) as u32
    }

    /// Frames a second, in thousandths: what tells 59.94 Hz from 60, and
    /// what a log prints.
    #[must_use]
    pub const fn refresh_millihz(&self) -> u64 {
        let per_frame = self.htotal as u64 * self.vtotal as u64;
        if per_frame == 0 {
            return 0;
        }
        (self.clock_khz as u64 * 1_000_000 + per_frame / 2) / per_frame
    }

    /// The mode an EDID detailed timing descriptor describes, or `None` for a
    /// descriptor that is not a timing (a zero clock), an interlaced one, one
    /// whose sync is not digital and separate, or one whose numbers do not
    /// hang together.
    #[must_use]
    pub fn from_detailed_timing(descriptor: &[u8; 18]) -> Option<Mode> {
        let [
            c0,
            c1,
            ha,
            hb,
            hab,
            va,
            vb,
            vab,
            hso,
            hsw,
            vs,
            hi,
            _,
            _,
            _,
            _,
            _,
            flags,
        ] = *descriptor;
        let clock_10khz = u16::from_le_bytes([c0, c1]);
        if clock_10khz == 0 || flags & 0x80 != 0 || flags & 0x18 != 0x18 {
            return None;
        }
        let high = |byte: u8, shift: u8, mask: u8| u16::from((byte >> shift) & mask) << 8;
        let hdisplay = u16::from(ha) | high(hab, 4, 0xF);
        let hblank = u16::from(hb) | high(hab, 0, 0xF);
        let vdisplay = u16::from(va) | high(vab, 4, 0xF);
        let vblank = u16::from(vb) | high(vab, 0, 0xF);
        let hsync_offset = u16::from(hso) | high(hi, 6, 0x3);
        let hsync_width = u16::from(hsw) | high(hi, 4, 0x3);
        let vsync_offset = u16::from(vs >> 4) | (u16::from((hi >> 2) & 0x3) << 4);
        let vsync_width = u16::from(vs & 0xF) | (u16::from(hi & 0x3) << 4);
        let hsync_start = hdisplay.checked_add(hsync_offset)?;
        let vsync_start = vdisplay.checked_add(vsync_offset)?;
        let mode = Mode {
            clock_khz: u32::from(clock_10khz) * 10,
            hdisplay,
            hsync_start,
            hsync_end: hsync_start.checked_add(hsync_width)?,
            htotal: hdisplay.checked_add(hblank)?,
            vdisplay,
            vsync_start,
            vsync_end: vsync_start.checked_add(vsync_width)?,
            vtotal: vdisplay.checked_add(vblank)?,
            hsync_high: flags & 0x02 != 0,
            vsync_high: flags & 0x04 != 0,
            vic: 0,
        };
        mode.is_consistent().then_some(mode)
    }

    /// Whether `other` is the same timing, whatever either calls itself.
    #[must_use]
    pub const fn same_timing(&self, other: &Mode) -> bool {
        self.clock_khz == other.clock_khz
            && self.hdisplay == other.hdisplay
            && self.hsync_start == other.hsync_start
            && self.hsync_end == other.hsync_end
            && self.htotal == other.htotal
            && self.vdisplay == other.vdisplay
            && self.vsync_start == other.vsync_start
            && self.vsync_end == other.vsync_end
            && self.vtotal == other.vtotal
    }
}
