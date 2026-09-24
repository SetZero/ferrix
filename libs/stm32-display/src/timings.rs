//! The timings an EDID names without spelling out: CTA-861's video
//! identification codes, the VESA DMT timings its standard timing codes
//! stand for, and the established timings of its bit field.
//!
//! The VIC and standard-code tables were generated from libdisplay-info
//! 0.3's own tables (`di_cta_video_format_from_vic` for VICs 1 to 127, and
//! `di_edid_standard_timing_get_dmt` for every two-byte standard timing code
//! there is), which implement CTA-861-I and VESA DMT 1.0 revision 13; the DMT
//! sync polarities, which libdisplay-info does not carry, are Linux's
//! `drm_dmt_modes` in `drivers/gpu/drm/drm_edid.c`, matched to each timing by
//! every other number, and every one matched exactly one. The established
//! timings are Linux's `edid_est_modes`, in bit order.
//!
//! Left out on purpose: interlaced VICs, and the pixel-repeated ones (1440
//! and 2880 wide), which a sink expects each pixel of twice or more and this
//! bridge is told to send once.

use crate::mode::Mode;

/// One CTA-861 video format.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Vic {
    /// Its code.
    pub(crate) vic: u8,
    /// Active pixels and lines.
    pub(crate) width: u16,
    pub(crate) height: u16,
    /// Horizontal front porch, sync and back porch, in pixels.
    pub(crate) h: [u16; 3],
    /// Vertical front porch, sync and back porch, in lines.
    pub(crate) v: [u16; 3],
    /// The pixel clock.
    pub(crate) clock_khz: u32,
    /// Whether horizontal and vertical sync are positive.
    pub(crate) positive: (bool, bool),
    /// The AVI infoframe's picture aspect code for it: 1 for 4:3, 2 for
    /// 16:9, 0 for the wider formats the VIC alone describes.
    pub(crate) aspect: u8,
}

/// One standard timing code's DMT timing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Standard {
    /// The two bytes, first byte high.
    pub(crate) code: u16,
    /// The DMT id, for the log.
    #[allow(
        dead_code,
        reason = "kept beside the numbers it names, for a reader checking them"
    )]
    pub(crate) dmt: u8,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) h: [u16; 3],
    pub(crate) v: [u16; 3],
    pub(crate) clock_khz: u32,
    pub(crate) positive: (bool, bool),
}

/// A mode from its porches.
const fn porched(
    clock_khz: u32,
    (width, height): (u16, u16),
    h: [u16; 3],
    v: [u16; 3],
    positive: (bool, bool),
    vic: u8,
) -> Mode {
    let [hf, hs, hb] = h;
    let [vf, vs, vb] = v;
    Mode {
        clock_khz,
        hdisplay: width,
        hsync_start: width + hf,
        hsync_end: width + hf + hs,
        htotal: width + hf + hs + hb,
        vdisplay: height,
        vsync_start: height + vf,
        vsync_end: height + vf + vs,
        vtotal: height + vf + vs + vb,
        hsync_high: positive.0,
        vsync_high: positive.1,
        vic,
    }
}

impl Vic {
    /// The mode, carrying its code for the AVI infoframe.
    pub(crate) const fn mode(&self) -> Mode {
        porched(
            self.clock_khz,
            (self.width, self.height),
            self.h,
            self.v,
            self.positive,
            self.vic,
        )
    }
}

impl Standard {
    /// The mode.
    pub(crate) const fn mode(&self) -> Mode {
        porched(
            self.clock_khz,
            (self.width, self.height),
            self.h,
            self.v,
            self.positive,
            0,
        )
    }
}

/// The video format `vic` names, if it is one this table keeps.
pub(crate) fn vic(vic: u8) -> Option<&'static Vic> {
    VICS.iter().find(|format| format.vic == vic)
}

/// The VIC whose timing `mode` is, for a detailed timing that spells one
/// out: the lowest such code, as the AVI infoframe of a sink that lists the
/// same timing twice under two aspects takes either.
pub(crate) fn vic_of(mode: &Mode) -> Option<&'static Vic> {
    VICS.iter().find(|format| format.mode().same_timing(mode))
}

/// The DMT timing standard timing `code` stands for.
pub(crate) fn standard(code: u16) -> Option<&'static Standard> {
    STANDARD.iter().find(|timing| timing.code == code)
}

/// The established timing at bit `bit` of the 17-bit field bytes 0x23 to
/// 0x25 make (0x23 low), or `None` for the one interlaced timing, bit 12.
pub(crate) fn established(bit: u8) -> Option<Mode> {
    ESTABLISHED
        .get(usize::from(bit))
        .copied()
        .flatten()
        .map(|(clock_khz, h, v, positive)| Mode {
            clock_khz,
            hdisplay: h[0],
            hsync_start: h[1],
            hsync_end: h[2],
            htotal: h[3],
            vdisplay: v[0],
            vsync_start: v[1],
            vsync_end: v[2],
            vtotal: v[3],
            hsync_high: positive.0,
            vsync_high: positive.1,
            vic: 0,
        })
}

/// Bits of the established timings.
pub(crate) const ESTABLISHED_BITS: u8 = 17;

/// One established timing: the clock, `[active, sync start, sync end,
/// total]` across and down, and the sync polarities.
type Established = (u32, [u16; 4], [u16; 4], (bool, bool));

/// Linux's `edid_est_modes`, by bit.
#[rustfmt::skip]
const ESTABLISHED: [Option<Established>; ESTABLISHED_BITS as usize] = [
    // 0x23 bit 0: 800x600 at 60 Hz.
    Some((40_000, [800, 840, 968, 1056], [600, 601, 605, 628], (true, true))),
    // Bit 1: 800x600 at 56 Hz.
    Some((36_000, [800, 824, 896, 1024], [600, 601, 603, 625], (true, true))),
    // Bit 2: 640x480 at 75 Hz.
    Some((31_500, [640, 656, 720, 840], [480, 481, 484, 500], (false, false))),
    // Bit 3: 640x480 at 72 Hz.
    Some((31_500, [640, 664, 704, 832], [480, 489, 492, 520], (false, false))),
    // Bit 4: 640x480 at 67 Hz, Apple's.
    Some((30_240, [640, 704, 768, 864], [480, 483, 486, 525], (false, false))),
    // Bit 5: 640x480 at 60 Hz.
    Some((25_175, [640, 656, 752, 800], [480, 490, 492, 525], (false, false))),
    // Bit 6: 720x400 at 88 Hz, IBM's.
    Some((35_500, [720, 738, 846, 900], [400, 421, 423, 449], (false, false))),
    // Bit 7: 720x400 at 70 Hz, IBM's.
    Some((28_320, [720, 738, 846, 900], [400, 412, 414, 449], (false, true))),
    // 0x24 bit 0: 1280x1024 at 75 Hz.
    Some((135_000, [1280, 1296, 1440, 1688], [1024, 1025, 1028, 1066], (true, true))),
    // Bit 1: 1024x768 at 75 Hz.
    Some((78_750, [1024, 1040, 1136, 1312], [768, 769, 772, 800], (true, true))),
    // Bit 2: 1024x768 at 70 Hz.
    Some((75_000, [1024, 1048, 1184, 1328], [768, 771, 777, 806], (false, false))),
    // Bit 3: 1024x768 at 60 Hz.
    Some((65_000, [1024, 1048, 1184, 1344], [768, 771, 777, 806], (false, false))),
    // Bit 4: 1024x768 interlaced at 87 Hz, IBM's.
    None,
    // Bit 5: 832x624 at 75 Hz, Apple's.
    Some((57_284, [832, 864, 928, 1152], [624, 625, 628, 667], (false, false))),
    // Bit 6: 800x600 at 75 Hz.
    Some((49_500, [800, 816, 896, 1056], [600, 601, 604, 625], (true, true))),
    // Bit 7: 800x600 at 72 Hz.
    Some((50_000, [800, 856, 976, 1040], [600, 637, 643, 666], (true, true))),
    // 0x25 bit 7: Apple's 1152x870 at 75 Hz, which Linux runs as 1152x864.
    Some((108_000, [1152, 1216, 1344, 1600], [864, 865, 868, 900], (true, true))),
];

/// CTA-861's progressive, unrepeated video formats with one-byte codes.
#[rustfmt::skip]
const VICS: [Vic; VIC_COUNT] = [
    Vic { vic: 1, width: 640, height: 480, h: [16, 96, 48], v: [10, 2, 33], clock_khz: 25175, positive: (false, false), aspect: 1 },
    Vic { vic: 2, width: 720, height: 480, h: [16, 62, 60], v: [9, 6, 30], clock_khz: 27000, positive: (false, false), aspect: 1 },
    Vic { vic: 3, width: 720, height: 480, h: [16, 62, 60], v: [9, 6, 30], clock_khz: 27000, positive: (false, false), aspect: 2 },
    Vic { vic: 4, width: 1280, height: 720, h: [110, 40, 220], v: [5, 5, 20], clock_khz: 74250, positive: (true, true), aspect: 2 },
    Vic { vic: 16, width: 1920, height: 1080, h: [88, 44, 148], v: [4, 5, 36], clock_khz: 148500, positive: (true, true), aspect: 2 },
    Vic { vic: 17, width: 720, height: 576, h: [12, 64, 68], v: [5, 5, 39], clock_khz: 27000, positive: (false, false), aspect: 1 },
    Vic { vic: 18, width: 720, height: 576, h: [12, 64, 68], v: [5, 5, 39], clock_khz: 27000, positive: (false, false), aspect: 2 },
    Vic { vic: 19, width: 1280, height: 720, h: [440, 40, 220], v: [5, 5, 20], clock_khz: 74250, positive: (true, true), aspect: 2 },
    Vic { vic: 31, width: 1920, height: 1080, h: [528, 44, 148], v: [4, 5, 36], clock_khz: 148500, positive: (true, true), aspect: 2 },
    Vic { vic: 32, width: 1920, height: 1080, h: [638, 44, 148], v: [4, 5, 36], clock_khz: 74250, positive: (true, true), aspect: 2 },
    Vic { vic: 33, width: 1920, height: 1080, h: [528, 44, 148], v: [4, 5, 36], clock_khz: 74250, positive: (true, true), aspect: 2 },
    Vic { vic: 34, width: 1920, height: 1080, h: [88, 44, 148], v: [4, 5, 36], clock_khz: 74250, positive: (true, true), aspect: 2 },
    Vic { vic: 41, width: 1280, height: 720, h: [440, 40, 220], v: [5, 5, 20], clock_khz: 148500, positive: (true, true), aspect: 2 },
    Vic { vic: 42, width: 720, height: 576, h: [12, 64, 68], v: [5, 5, 39], clock_khz: 54000, positive: (false, false), aspect: 1 },
    Vic { vic: 43, width: 720, height: 576, h: [12, 64, 68], v: [5, 5, 39], clock_khz: 54000, positive: (false, false), aspect: 2 },
    Vic { vic: 47, width: 1280, height: 720, h: [110, 40, 220], v: [5, 5, 20], clock_khz: 148500, positive: (true, true), aspect: 2 },
    Vic { vic: 48, width: 720, height: 480, h: [16, 62, 60], v: [9, 6, 30], clock_khz: 54000, positive: (false, false), aspect: 1 },
    Vic { vic: 49, width: 720, height: 480, h: [16, 62, 60], v: [9, 6, 30], clock_khz: 54000, positive: (false, false), aspect: 2 },
    Vic { vic: 52, width: 720, height: 576, h: [12, 64, 68], v: [5, 5, 39], clock_khz: 108000, positive: (false, false), aspect: 1 },
    Vic { vic: 53, width: 720, height: 576, h: [12, 64, 68], v: [5, 5, 39], clock_khz: 108000, positive: (false, false), aspect: 2 },
    Vic { vic: 56, width: 720, height: 480, h: [16, 62, 60], v: [9, 6, 30], clock_khz: 108000, positive: (false, false), aspect: 1 },
    Vic { vic: 57, width: 720, height: 480, h: [16, 62, 60], v: [9, 6, 30], clock_khz: 108000, positive: (false, false), aspect: 2 },
    Vic { vic: 60, width: 1280, height: 720, h: [1760, 40, 220], v: [5, 5, 20], clock_khz: 59400, positive: (true, true), aspect: 2 },
    Vic { vic: 61, width: 1280, height: 720, h: [2420, 40, 220], v: [5, 5, 20], clock_khz: 74250, positive: (true, true), aspect: 2 },
    Vic { vic: 62, width: 1280, height: 720, h: [1760, 40, 220], v: [5, 5, 20], clock_khz: 74250, positive: (true, true), aspect: 2 },
    Vic { vic: 63, width: 1920, height: 1080, h: [88, 44, 148], v: [4, 5, 36], clock_khz: 297000, positive: (true, true), aspect: 2 },
    Vic { vic: 64, width: 1920, height: 1080, h: [528, 44, 148], v: [4, 5, 36], clock_khz: 297000, positive: (true, true), aspect: 2 },
    Vic { vic: 65, width: 1280, height: 720, h: [1760, 40, 220], v: [5, 5, 20], clock_khz: 59400, positive: (true, true), aspect: 0 },
    Vic { vic: 66, width: 1280, height: 720, h: [2420, 40, 220], v: [5, 5, 20], clock_khz: 74250, positive: (true, true), aspect: 0 },
    Vic { vic: 67, width: 1280, height: 720, h: [1760, 40, 220], v: [5, 5, 20], clock_khz: 74250, positive: (true, true), aspect: 0 },
    Vic { vic: 68, width: 1280, height: 720, h: [440, 40, 220], v: [5, 5, 20], clock_khz: 74250, positive: (true, true), aspect: 0 },
    Vic { vic: 69, width: 1280, height: 720, h: [110, 40, 220], v: [5, 5, 20], clock_khz: 74250, positive: (true, true), aspect: 0 },
    Vic { vic: 70, width: 1280, height: 720, h: [440, 40, 220], v: [5, 5, 20], clock_khz: 148500, positive: (true, true), aspect: 0 },
    Vic { vic: 71, width: 1280, height: 720, h: [110, 40, 220], v: [5, 5, 20], clock_khz: 148500, positive: (true, true), aspect: 0 },
    Vic { vic: 72, width: 1920, height: 1080, h: [638, 44, 148], v: [4, 5, 36], clock_khz: 74250, positive: (true, true), aspect: 0 },
    Vic { vic: 73, width: 1920, height: 1080, h: [528, 44, 148], v: [4, 5, 36], clock_khz: 74250, positive: (true, true), aspect: 0 },
    Vic { vic: 74, width: 1920, height: 1080, h: [88, 44, 148], v: [4, 5, 36], clock_khz: 74250, positive: (true, true), aspect: 0 },
    Vic { vic: 75, width: 1920, height: 1080, h: [528, 44, 148], v: [4, 5, 36], clock_khz: 148500, positive: (true, true), aspect: 0 },
    Vic { vic: 76, width: 1920, height: 1080, h: [88, 44, 148], v: [4, 5, 36], clock_khz: 148500, positive: (true, true), aspect: 0 },
    Vic { vic: 77, width: 1920, height: 1080, h: [528, 44, 148], v: [4, 5, 36], clock_khz: 297000, positive: (true, true), aspect: 0 },
    Vic { vic: 78, width: 1920, height: 1080, h: [88, 44, 148], v: [4, 5, 36], clock_khz: 297000, positive: (true, true), aspect: 0 },
    Vic { vic: 79, width: 1680, height: 720, h: [1360, 40, 220], v: [5, 5, 20], clock_khz: 59400, positive: (true, true), aspect: 0 },
    Vic { vic: 80, width: 1680, height: 720, h: [1228, 40, 220], v: [5, 5, 20], clock_khz: 59400, positive: (true, true), aspect: 0 },
    Vic { vic: 81, width: 1680, height: 720, h: [700, 40, 220], v: [5, 5, 20], clock_khz: 59400, positive: (true, true), aspect: 0 },
    Vic { vic: 82, width: 1680, height: 720, h: [260, 40, 220], v: [5, 5, 20], clock_khz: 82500, positive: (true, true), aspect: 0 },
    Vic { vic: 83, width: 1680, height: 720, h: [260, 40, 220], v: [5, 5, 20], clock_khz: 99000, positive: (true, true), aspect: 0 },
    Vic { vic: 84, width: 1680, height: 720, h: [60, 40, 220], v: [5, 5, 95], clock_khz: 165000, positive: (true, true), aspect: 0 },
    Vic { vic: 85, width: 1680, height: 720, h: [60, 40, 220], v: [5, 5, 95], clock_khz: 198000, positive: (true, true), aspect: 0 },
    Vic { vic: 86, width: 2560, height: 1080, h: [998, 44, 148], v: [4, 5, 11], clock_khz: 99000, positive: (true, true), aspect: 0 },
    Vic { vic: 87, width: 2560, height: 1080, h: [448, 44, 148], v: [4, 5, 36], clock_khz: 90000, positive: (true, true), aspect: 0 },
    Vic { vic: 88, width: 2560, height: 1080, h: [768, 44, 148], v: [4, 5, 36], clock_khz: 118800, positive: (true, true), aspect: 0 },
    Vic { vic: 89, width: 2560, height: 1080, h: [548, 44, 148], v: [4, 5, 36], clock_khz: 185625, positive: (true, true), aspect: 0 },
    Vic { vic: 90, width: 2560, height: 1080, h: [248, 44, 148], v: [4, 5, 11], clock_khz: 198000, positive: (true, true), aspect: 0 },
    Vic { vic: 91, width: 2560, height: 1080, h: [218, 44, 148], v: [4, 5, 161], clock_khz: 371250, positive: (true, true), aspect: 0 },
    Vic { vic: 92, width: 2560, height: 1080, h: [548, 44, 148], v: [4, 5, 161], clock_khz: 495000, positive: (true, true), aspect: 0 },
    Vic { vic: 93, width: 3840, height: 2160, h: [1276, 88, 296], v: [8, 10, 72], clock_khz: 297000, positive: (true, true), aspect: 2 },
    Vic { vic: 94, width: 3840, height: 2160, h: [1056, 88, 296], v: [8, 10, 72], clock_khz: 297000, positive: (true, true), aspect: 2 },
    Vic { vic: 95, width: 3840, height: 2160, h: [176, 88, 296], v: [8, 10, 72], clock_khz: 297000, positive: (true, true), aspect: 2 },
    Vic { vic: 96, width: 3840, height: 2160, h: [1056, 88, 296], v: [8, 10, 72], clock_khz: 594000, positive: (true, true), aspect: 2 },
    Vic { vic: 97, width: 3840, height: 2160, h: [176, 88, 296], v: [8, 10, 72], clock_khz: 594000, positive: (true, true), aspect: 2 },
    Vic { vic: 98, width: 4096, height: 2160, h: [1020, 88, 296], v: [8, 10, 72], clock_khz: 297000, positive: (true, true), aspect: 0 },
    Vic { vic: 99, width: 4096, height: 2160, h: [968, 88, 128], v: [8, 10, 72], clock_khz: 297000, positive: (true, true), aspect: 0 },
    Vic { vic: 100, width: 4096, height: 2160, h: [88, 88, 128], v: [8, 10, 72], clock_khz: 297000, positive: (true, true), aspect: 0 },
    Vic { vic: 101, width: 4096, height: 2160, h: [968, 88, 128], v: [8, 10, 72], clock_khz: 594000, positive: (true, true), aspect: 0 },
    Vic { vic: 102, width: 4096, height: 2160, h: [88, 88, 128], v: [8, 10, 72], clock_khz: 594000, positive: (true, true), aspect: 0 },
    Vic { vic: 103, width: 3840, height: 2160, h: [1276, 88, 296], v: [8, 10, 72], clock_khz: 297000, positive: (true, true), aspect: 0 },
    Vic { vic: 104, width: 3840, height: 2160, h: [1056, 88, 296], v: [8, 10, 72], clock_khz: 297000, positive: (true, true), aspect: 0 },
    Vic { vic: 105, width: 3840, height: 2160, h: [176, 88, 296], v: [8, 10, 72], clock_khz: 297000, positive: (true, true), aspect: 0 },
    Vic { vic: 106, width: 3840, height: 2160, h: [1056, 88, 296], v: [8, 10, 72], clock_khz: 594000, positive: (true, true), aspect: 0 },
    Vic { vic: 107, width: 3840, height: 2160, h: [176, 88, 296], v: [8, 10, 72], clock_khz: 594000, positive: (true, true), aspect: 0 },
    Vic { vic: 108, width: 1280, height: 720, h: [960, 40, 220], v: [5, 5, 20], clock_khz: 90000, positive: (true, true), aspect: 2 },
    Vic { vic: 109, width: 1280, height: 720, h: [960, 40, 220], v: [5, 5, 20], clock_khz: 90000, positive: (true, true), aspect: 0 },
    Vic { vic: 110, width: 1680, height: 720, h: [810, 40, 220], v: [5, 5, 20], clock_khz: 99000, positive: (true, true), aspect: 0 },
    Vic { vic: 111, width: 1920, height: 1080, h: [638, 44, 148], v: [4, 5, 36], clock_khz: 148500, positive: (true, true), aspect: 2 },
    Vic { vic: 112, width: 1920, height: 1080, h: [638, 44, 148], v: [4, 5, 36], clock_khz: 148500, positive: (true, true), aspect: 0 },
    Vic { vic: 113, width: 2560, height: 1080, h: [998, 44, 148], v: [4, 5, 11], clock_khz: 198000, positive: (true, true), aspect: 0 },
    Vic { vic: 114, width: 3840, height: 2160, h: [1276, 88, 296], v: [8, 10, 72], clock_khz: 594000, positive: (true, true), aspect: 2 },
    Vic { vic: 115, width: 4096, height: 2160, h: [1020, 88, 296], v: [8, 10, 72], clock_khz: 594000, positive: (true, true), aspect: 0 },
    Vic { vic: 116, width: 3840, height: 2160, h: [1276, 88, 296], v: [8, 10, 72], clock_khz: 594000, positive: (true, true), aspect: 0 },
    Vic { vic: 117, width: 3840, height: 2160, h: [1056, 88, 296], v: [8, 10, 72], clock_khz: 1188000, positive: (true, true), aspect: 2 },
    Vic { vic: 118, width: 3840, height: 2160, h: [176, 88, 296], v: [8, 10, 72], clock_khz: 1188000, positive: (true, true), aspect: 2 },
    Vic { vic: 119, width: 3840, height: 2160, h: [1056, 88, 296], v: [8, 10, 72], clock_khz: 1188000, positive: (true, true), aspect: 0 },
    Vic { vic: 120, width: 3840, height: 2160, h: [176, 88, 296], v: [8, 10, 72], clock_khz: 1188000, positive: (true, true), aspect: 0 },
    Vic { vic: 121, width: 5120, height: 2160, h: [1996, 88, 296], v: [8, 10, 22], clock_khz: 396000, positive: (true, true), aspect: 0 },
    Vic { vic: 122, width: 5120, height: 2160, h: [1696, 88, 296], v: [8, 10, 22], clock_khz: 396000, positive: (true, true), aspect: 0 },
    Vic { vic: 123, width: 5120, height: 2160, h: [664, 88, 128], v: [8, 10, 22], clock_khz: 396000, positive: (true, true), aspect: 0 },
    Vic { vic: 124, width: 5120, height: 2160, h: [746, 88, 296], v: [8, 10, 297], clock_khz: 742500, positive: (true, true), aspect: 0 },
    Vic { vic: 125, width: 5120, height: 2160, h: [1096, 88, 296], v: [8, 10, 72], clock_khz: 742500, positive: (true, true), aspect: 0 },
    Vic { vic: 126, width: 5120, height: 2160, h: [164, 88, 128], v: [8, 10, 72], clock_khz: 742500, positive: (true, true), aspect: 0 },
    Vic { vic: 127, width: 5120, height: 2160, h: [1096, 88, 296], v: [8, 10, 72], clock_khz: 1485000, positive: (true, true), aspect: 0 },
];

/// How many VICs [`VICS`] keeps.
const VIC_COUNT: usize = 90;

/// Every standard timing code with a DMT timing, and the timing.
#[rustfmt::skip]
const STANDARD: [Standard; 49] = [
    Standard { code: 0x3119, dmt: 0x02, width: 640, height: 400, h: [32, 64, 96], v: [1, 3, 41], clock_khz: 31500, positive: (false, true) },
    Standard { code: 0x3140, dmt: 0x04, width: 640, height: 480, h: [16, 96, 48], v: [10, 2, 33], clock_khz: 25175, positive: (false, false) },
    Standard { code: 0x314c, dmt: 0x05, width: 640, height: 480, h: [24, 40, 128], v: [9, 3, 28], clock_khz: 31500, positive: (false, false) },
    Standard { code: 0x314f, dmt: 0x06, width: 640, height: 480, h: [16, 64, 120], v: [1, 3, 16], clock_khz: 31500, positive: (false, false) },
    Standard { code: 0x3159, dmt: 0x07, width: 640, height: 480, h: [56, 56, 80], v: [1, 3, 25], clock_khz: 36000, positive: (false, false) },
    Standard { code: 0x4540, dmt: 0x09, width: 800, height: 600, h: [40, 128, 88], v: [1, 4, 23], clock_khz: 40000, positive: (true, true) },
    Standard { code: 0x454c, dmt: 0x0a, width: 800, height: 600, h: [56, 120, 64], v: [37, 6, 23], clock_khz: 50000, positive: (true, true) },
    Standard { code: 0x454f, dmt: 0x0b, width: 800, height: 600, h: [16, 80, 160], v: [1, 3, 21], clock_khz: 49500, positive: (true, true) },
    Standard { code: 0x4559, dmt: 0x0c, width: 800, height: 600, h: [32, 64, 152], v: [1, 3, 27], clock_khz: 56250, positive: (true, true) },
    Standard { code: 0x6140, dmt: 0x10, width: 1024, height: 768, h: [24, 136, 160], v: [3, 6, 29], clock_khz: 65000, positive: (false, false) },
    Standard { code: 0x614a, dmt: 0x11, width: 1024, height: 768, h: [24, 136, 144], v: [3, 6, 29], clock_khz: 75000, positive: (false, false) },
    Standard { code: 0x614f, dmt: 0x12, width: 1024, height: 768, h: [16, 96, 176], v: [1, 3, 28], clock_khz: 78750, positive: (true, true) },
    Standard { code: 0x6159, dmt: 0x13, width: 1024, height: 768, h: [48, 96, 208], v: [1, 3, 36], clock_khz: 94500, positive: (true, true) },
    Standard { code: 0x714f, dmt: 0x15, width: 1152, height: 864, h: [64, 128, 256], v: [1, 3, 32], clock_khz: 108000, positive: (true, true) },
    Standard { code: 0x8100, dmt: 0x1c, width: 1280, height: 800, h: [72, 128, 200], v: [3, 6, 22], clock_khz: 83500, positive: (false, true) },
    Standard { code: 0x810f, dmt: 0x1d, width: 1280, height: 800, h: [80, 128, 208], v: [3, 6, 29], clock_khz: 106500, positive: (false, true) },
    Standard { code: 0x8119, dmt: 0x1e, width: 1280, height: 800, h: [80, 136, 216], v: [3, 6, 34], clock_khz: 122500, positive: (false, true) },
    Standard { code: 0x8140, dmt: 0x20, width: 1280, height: 960, h: [96, 112, 312], v: [1, 3, 36], clock_khz: 108000, positive: (true, true) },
    Standard { code: 0x8159, dmt: 0x21, width: 1280, height: 960, h: [64, 160, 224], v: [1, 3, 47], clock_khz: 148500, positive: (true, true) },
    Standard { code: 0x8180, dmt: 0x23, width: 1280, height: 1024, h: [48, 112, 248], v: [1, 3, 38], clock_khz: 108000, positive: (true, true) },
    Standard { code: 0x818f, dmt: 0x24, width: 1280, height: 1024, h: [16, 144, 248], v: [1, 3, 38], clock_khz: 135000, positive: (true, true) },
    Standard { code: 0x8199, dmt: 0x25, width: 1280, height: 1024, h: [64, 160, 224], v: [1, 3, 44], clock_khz: 157500, positive: (true, true) },
    Standard { code: 0x81c0, dmt: 0x55, width: 1280, height: 720, h: [110, 40, 220], v: [5, 5, 20], clock_khz: 74250, positive: (true, true) },
    Standard { code: 0x9040, dmt: 0x2a, width: 1400, height: 1050, h: [88, 144, 232], v: [3, 4, 32], clock_khz: 121750, positive: (false, true) },
    Standard { code: 0x904f, dmt: 0x2b, width: 1400, height: 1050, h: [104, 144, 248], v: [3, 4, 42], clock_khz: 156000, positive: (false, true) },
    Standard { code: 0x9059, dmt: 0x2c, width: 1400, height: 1050, h: [104, 152, 256], v: [3, 4, 48], clock_khz: 179500, positive: (false, true) },
    Standard { code: 0x9500, dmt: 0x2f, width: 1440, height: 900, h: [80, 152, 232], v: [3, 6, 25], clock_khz: 106500, positive: (false, true) },
    Standard { code: 0x950f, dmt: 0x30, width: 1440, height: 900, h: [96, 152, 248], v: [3, 6, 33], clock_khz: 136750, positive: (false, true) },
    Standard { code: 0x9519, dmt: 0x31, width: 1440, height: 900, h: [104, 152, 256], v: [3, 6, 39], clock_khz: 157000, positive: (false, true) },
    Standard { code: 0xa940, dmt: 0x33, width: 1600, height: 1200, h: [64, 192, 304], v: [1, 3, 46], clock_khz: 162000, positive: (true, true) },
    Standard { code: 0xa945, dmt: 0x34, width: 1600, height: 1200, h: [64, 192, 304], v: [1, 3, 46], clock_khz: 175500, positive: (true, true) },
    Standard { code: 0xa94a, dmt: 0x35, width: 1600, height: 1200, h: [64, 192, 304], v: [1, 3, 46], clock_khz: 189000, positive: (true, true) },
    Standard { code: 0xa94f, dmt: 0x36, width: 1600, height: 1200, h: [64, 192, 304], v: [1, 3, 46], clock_khz: 202500, positive: (true, true) },
    Standard { code: 0xa959, dmt: 0x37, width: 1600, height: 1200, h: [64, 192, 304], v: [1, 3, 46], clock_khz: 229500, positive: (true, true) },
    Standard { code: 0xa9c0, dmt: 0x53, width: 1600, height: 900, h: [24, 80, 96], v: [1, 3, 96], clock_khz: 108000, positive: (true, true) },
    Standard { code: 0xb300, dmt: 0x3a, width: 1680, height: 1050, h: [104, 176, 280], v: [3, 6, 30], clock_khz: 146250, positive: (false, true) },
    Standard { code: 0xb30f, dmt: 0x3b, width: 1680, height: 1050, h: [120, 176, 296], v: [3, 6, 40], clock_khz: 187000, positive: (false, true) },
    Standard { code: 0xb319, dmt: 0x3c, width: 1680, height: 1050, h: [128, 176, 304], v: [3, 6, 46], clock_khz: 214750, positive: (false, true) },
    Standard { code: 0xc140, dmt: 0x3e, width: 1792, height: 1344, h: [128, 200, 328], v: [1, 3, 46], clock_khz: 204750, positive: (false, true) },
    Standard { code: 0xc14f, dmt: 0x3f, width: 1792, height: 1344, h: [96, 216, 352], v: [1, 3, 69], clock_khz: 261000, positive: (false, true) },
    Standard { code: 0xc940, dmt: 0x41, width: 1856, height: 1392, h: [96, 224, 352], v: [1, 3, 43], clock_khz: 218250, positive: (false, true) },
    Standard { code: 0xc94f, dmt: 0x42, width: 1856, height: 1392, h: [128, 224, 352], v: [1, 3, 104], clock_khz: 288000, positive: (false, true) },
    Standard { code: 0xd100, dmt: 0x45, width: 1920, height: 1200, h: [136, 200, 336], v: [3, 6, 36], clock_khz: 193250, positive: (false, true) },
    Standard { code: 0xd10f, dmt: 0x46, width: 1920, height: 1200, h: [136, 208, 344], v: [3, 6, 46], clock_khz: 245250, positive: (false, true) },
    Standard { code: 0xd119, dmt: 0x47, width: 1920, height: 1200, h: [144, 208, 352], v: [3, 6, 53], clock_khz: 281250, positive: (false, true) },
    Standard { code: 0xd140, dmt: 0x49, width: 1920, height: 1440, h: [128, 208, 344], v: [1, 3, 56], clock_khz: 234000, positive: (false, true) },
    Standard { code: 0xd14f, dmt: 0x4a, width: 1920, height: 1440, h: [144, 224, 352], v: [1, 3, 56], clock_khz: 297000, positive: (false, true) },
    Standard { code: 0xd1c0, dmt: 0x52, width: 1920, height: 1080, h: [88, 44, 148], v: [4, 5, 36], clock_khz: 148500, positive: (false, false) },
    Standard { code: 0xe1c0, dmt: 0x54, width: 2048, height: 1152, h: [26, 80, 96], v: [1, 3, 44], clock_khz: 162000, positive: (true, true) },
];
