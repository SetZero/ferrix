//! The Silicon Image `SiI9022` HDMI transmitter, through its TPI registers.
//!
//! What it is told and in what order is Linux's
//! `drivers/gpu/drm/bridge/sii902x.c`, which is what drives it on the DK
//! boards: TPI mode on, the chip id checked, the video mode and input format
//! written, the output mode chosen, the AVI infoframe for an HDMI sink, and
//! TMDS turned on last. The monitor's EDID is read through the bridge's DDC
//! pass-through, which the bridge hands over on request and takes back
//! after.

use crate::Budget;
use crate::mode::Mode;

/// The bridge's I2C address on a DK board (`reg = <0x39>`).
pub const ADDRESS: u8 = 0x39;
/// The monitor's EDID EEPROM, reached through the pass-through.
pub const EDID_ADDRESS: u8 = 0x50;

/// Video mode data: pixel clock, refresh, size (10 bytes with the next two).
pub const VIDEO_DATA: u8 = 0x00;
/// Pixel repetition and input bus.
pub const PIXEL_REPETITION: u8 = 0x08;
/// Input format.
pub const INPUT_FORMAT: u8 = 0x09;
/// AVI infoframe, from its checksum byte.
pub const AVI_INFOFRAME: u8 = 0x0C;
/// System control.
pub const SYS_CTRL: u8 = 0x1A;
/// The chip id, four bytes.
pub const CHIP_ID: u8 = 0x1B;
/// Power state.
pub const POWER_STATE: u8 = 0x1E;
/// Interrupt status: hotplug.
pub const INT_STATUS: u8 = 0x3D;
/// Writing 0 here turns TPI mode on.
pub const TPI_REQUEST: u8 = 0xC7;

/// `SYS_CTRL`: TMDS output off.
pub const SYS_POWER_DOWN: u8 = 1 << 4;
/// `SYS_CTRL`: the host asks for the DDC bus.
pub const SYS_DDC_REQUEST: u8 = 1 << 2;
/// `SYS_CTRL`: the bridge granted it.
pub const SYS_DDC_GRANTED: u8 = 1 << 1;
/// `SYS_CTRL`: HDMI rather than DVI.
pub const SYS_OUTPUT_HDMI: u8 = 1 << 0;
/// `INT_STATUS`: a sink is plugged in.
pub const PLUGGED: u8 = 1 << 2;
/// The chip id's first byte.
pub const CHIP_ID_SII902X: u8 = 0xB0;
/// `PIXEL_REPETITION` for a 24-bit bus, one pixel a clock, latched on the
/// clock's rising edge ratio 1x: `CLK_RATIO_1X | BUS_24BIT`.
pub const PIXEL_24BIT_1X: u8 = (1 << 6) | (1 << 5);
/// `INPUT_FORMAT`: RGB, range chosen by the bridge.
pub const INPUT_RGB: u8 = 0;

/// How many reads of `SYS_CTRL` the DDC hand-over may take: Linux waits
/// 500 ms, and a read over I2C is some 300 µs.
pub const DDC_BUDGET: Budget = Budget(2000);

/// Why a transfer on the bus failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BusError {
    /// Nobody acknowledged: no device at the address, or it is unpowered or
    /// held in reset.
    Nack,
    /// A flag never came.
    Timeout,
    /// A bus error or lost arbitration.
    Bus,
    /// The bus stayed busy.
    Busy,
    /// No bytes, or more than one transfer carries.
    Length,
}

/// A bus with 7-bit targets on it.
pub trait Bus {
    /// Write `bytes` to `address`.
    ///
    /// # Errors
    ///
    /// [`BusError`].
    fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), BusError>;

    /// Write `out` to `address`, then read `into` from it after a repeated
    /// START.
    ///
    /// # Errors
    ///
    /// [`BusError`].
    fn write_read(&mut self, address: u8, out: &[u8], into: &mut [u8]) -> Result<(), BusError>;
}

/// Why the bridge cannot be used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BridgeError {
    /// The bus failed.
    Bus(BusError),
    /// The chip answered with an id that is not a `SiI902x`'s.
    Chip(u8),
    /// The bridge did not hand over, or take back, the DDC bus.
    Ddc,
    /// The mode is outside what the bridge carries (25 to 165 MHz).
    Mode,
}

impl From<BusError> for BridgeError {
    fn from(error: BusError) -> Self {
        BridgeError::Bus(error)
    }
}

/// The bridge on `bus`.
#[derive(Debug)]
pub struct Bridge<B: Bus> {
    bus: B,
    address: u8,
    /// The chip id's four bytes.
    id: [u8; 4],
}

impl<B: Bus> Bridge<B> {
    /// Turn TPI mode on, check the chip, and clear its pending interrupts.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Bus`] when nothing answers, [`BridgeError::Chip`] when
    /// something else does.
    pub fn probe(mut bus: B, address: u8) -> Result<Self, BridgeError> {
        bus.write(address, &[TPI_REQUEST, 0])?;
        let mut id = [0_u8; 4];
        bus.write_read(address, &[CHIP_ID], &mut id)?;
        let [first, ..] = id;
        if first != CHIP_ID_SII902X {
            return Err(BridgeError::Chip(first));
        }
        let mut bridge = Bridge { bus, address, id };
        let status = bridge.read(INT_STATUS)?;
        bridge.write(INT_STATUS, status)?;
        Ok(bridge)
    }

    /// The chip id, as it answered.
    pub const fn id(&self) -> [u8; 4] {
        self.id
    }

    /// The bus, for a test to look at.
    pub const fn bus(&self) -> &B {
        &self.bus
    }

    /// Read one register.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Bus`].
    pub fn read(&mut self, register: u8) -> Result<u8, BridgeError> {
        let mut value = [0_u8; 1];
        self.bus.write_read(self.address, &[register], &mut value)?;
        let [value] = value;
        Ok(value)
    }

    /// Write one register.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Bus`].
    pub fn write(&mut self, register: u8, value: u8) -> Result<(), BridgeError> {
        self.bus.write(self.address, &[register, value])?;
        Ok(())
    }

    /// Whether a sink is plugged in, by the hotplug line.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Bus`].
    pub fn plugged(&mut self) -> Result<bool, BridgeError> {
        Ok(self.read(INT_STATUS)? & PLUGGED != 0)
    }

    /// Read EDID block `index` (128 bytes) through the DDC pass-through.
    ///
    /// The bus is always asked back, even when the read failed, so a failed
    /// read does not leave the bridge unreachable.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Ddc`] when the bridge will not hand the bus over or
    /// take it back, [`BridgeError::Bus`] when the read itself fails.
    pub fn read_edid(&mut self, index: u8, block: &mut [u8; 128]) -> Result<(), BridgeError> {
        let control = self.read(SYS_CTRL)?;
        self.write(SYS_CTRL, control | SYS_DDC_REQUEST)?;
        let mut status = 0;
        let granted = DDC_BUDGET.wait(|| {
            status = self.read(SYS_CTRL).unwrap_or(0);
            status & SYS_DDC_GRANTED != 0
        });
        if !granted {
            let _ = self.write(SYS_CTRL, control & !(SYS_DDC_REQUEST | SYS_DDC_GRANTED));
            return Err(BridgeError::Ddc);
        }
        // Writing both bits back closes the switch onto the monitor's bus.
        self.write(SYS_CTRL, status)?;
        let offset = index.wrapping_mul(128);
        let read = self.bus.write_read(EDID_ADDRESS, &[offset], block);
        let released = self.release_ddc(control);
        read?;
        released
    }

    /// Take the DDC bus back from the monitor.
    fn release_ddc(&mut self, control: u8) -> Result<(), BridgeError> {
        // The bridge sometimes misses the first reads after the switch
        // opens; Linux retries them.
        let mut current = None;
        for _ in 0..5 {
            if let Ok(value) = self.read(SYS_CTRL) {
                current = Some(value);
                break;
            }
        }
        let current = current.unwrap_or(control);
        self.write(SYS_CTRL, current & !(SYS_DDC_REQUEST | SYS_DDC_GRANTED))?;
        let released = DDC_BUDGET.wait(|| {
            self.read(SYS_CTRL)
                .is_ok_and(|value| value & (SYS_DDC_REQUEST | SYS_DDC_GRANTED) == 0)
        });
        if released {
            Ok(())
        } else {
            Err(BridgeError::Ddc)
        }
    }

    /// Tell the bridge the mode the LTDC is sending, with TMDS off, and
    /// whether the sink wants HDMI (with an AVI infoframe) or DVI.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Mode`] for a pixel clock the bridge cannot carry, and
    /// [`BridgeError::Bus`].
    pub fn set_mode(&mut self, mode: &Mode, hdmi: bool) -> Result<(), BridgeError> {
        if !(25_000..=165_000).contains(&mode.clock_khz) || !mode.is_consistent() {
            return Err(BridgeError::Mode);
        }
        let output = if hdmi { SYS_OUTPUT_HDMI } else { 0 };
        self.write(SYS_CTRL, SYS_POWER_DOWN | output)?;
        let clock = u16::try_from(mode.clock_khz / 10).map_err(|_| BridgeError::Mode)?;
        let refresh = u16::try_from(mode.refresh_hz()).map_err(|_| BridgeError::Mode)?;
        let [c0, c1] = clock.to_le_bytes();
        let [r0, r1] = refresh.to_le_bytes();
        let [h0, h1] = mode.hdisplay.to_le_bytes();
        let [v0, v1] = mode.vdisplay.to_le_bytes();
        self.bus.write(
            self.address,
            &[
                VIDEO_DATA,
                c0,
                c1,
                r0,
                r1,
                h0,
                h1,
                v0,
                v1,
                PIXEL_24BIT_1X,
                INPUT_RGB,
            ],
        )?;
        if hdmi {
            let frame = avi_infoframe(mode.vic, mode.hdisplay, mode.vdisplay);
            let mut bytes = [AVI_INFOFRAME; 15];
            for (slot, byte) in bytes.iter_mut().skip(1).zip(frame) {
                *slot = byte;
            }
            self.bus.write(self.address, &bytes)?;
        }
        Ok(())
    }

    /// Power state D0 and TMDS on: the monitor starts receiving.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Bus`].
    pub fn enable(&mut self) -> Result<(), BridgeError> {
        let power = self.read(POWER_STATE)?;
        self.write(POWER_STATE, power & !0x3)?;
        let control = self.read(SYS_CTRL)?;
        self.write(SYS_CTRL, control & !SYS_POWER_DOWN)
    }

    /// TMDS off: the monitor sees no signal.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Bus`].
    pub fn disable(&mut self) -> Result<(), BridgeError> {
        let control = self.read(SYS_CTRL)?;
        self.write(SYS_CTRL, control | SYS_POWER_DOWN)
    }
}

/// The AVI infoframe for an RGB mode, as the bridge takes it at
/// [`AVI_INFOFRAME`]: the checksum, then the thirteen data bytes, without
/// the three header bytes (type 0x82, version 2, length 13), which the
/// checksum still covers.
///
/// RGB, no overscan information, the picture aspect from the size (16:9 for
/// a wide one, 4:3 otherwise), the active format the same as the picture,
/// and `vic`, which is 0 for a mode CEA-861 does not number.
#[must_use]
pub fn avi_infoframe(vic: u8, width: u16, height: u16) -> [u8; 14] {
    const HEADER: [u8; 3] = [0x82, 0x02, 0x0D];
    let wide = u32::from(width) * 9 >= u32::from(height) * 16;
    let aspect: u8 = if wide { 0b10 } else { 0b01 };
    // PB1: Y = RGB, and A0 set: the active format in PB2 is valid.
    let pb1 = 0x10;
    // PB2: no colorimetry, the picture aspect, active format = picture.
    let pb2 = (aspect << 4) | 0x8;
    let pb4 = vic & 0x7F;
    let data = [pb1, pb2, 0, pb4, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let sum = HEADER
        .iter()
        .chain(data.iter())
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    let mut frame = [0_u8.wrapping_sub(sum); 14];
    for (slot, byte) in frame.iter_mut().skip(1).zip(data) {
        *slot = byte;
    }
    frame
}
