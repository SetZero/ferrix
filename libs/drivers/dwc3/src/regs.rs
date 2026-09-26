//! The registers this driver touches, their bits, and the endpoint command
//! codes: the facts of Linux's `drivers/usb/dwc3/core.h` and `gadget.h`.
//! Offsets are from the core's base, where `GSNPSID` reads at `0xC120`.

/// `GCTL`: global configuration.
pub(crate) const GCTL: u32 = 0xC110;
/// `GSNPSID`: which core, and which release of it.
pub(crate) const GSNPSID: u32 = 0xC120;
/// `GUSB2PHYCFG(0)`: the USB 2.0 PHY's interface.
pub(crate) const GUSB2PHYCFG: u32 = 0xC200;
/// `GEVNTADRLO(0)`: the event buffer's address, low word.
pub(crate) const GEVNTADRLO: u32 = 0xC400;
/// `GEVNTADRHI(0)`: its high word.
pub(crate) const GEVNTADRHI: u32 = 0xC404;
/// `GEVNTSIZ(0)`: its size, and the interrupt mask.
pub(crate) const GEVNTSIZ: u32 = 0xC408;
/// `GEVNTCOUNT(0)`: the bytes of events written and not yet taken.
pub(crate) const GEVNTCOUNT: u32 = 0xC40C;
/// `GFLADJ`: frame length adjustment.
pub(crate) const GFLADJ: u32 = 0xC630;
/// `DCFG`: device configuration.
pub(crate) const DCFG: u32 = 0xC700;
/// `DCTL`: device control.
pub(crate) const DCTL: u32 = 0xC704;
/// `DEVTEN`: which device events are written.
pub(crate) const DEVTEN: u32 = 0xC708;
/// `DSTS`: device status.
pub(crate) const DSTS: u32 = 0xC70C;
/// `DALEPENA`: a bit per physical endpoint that is active.
pub(crate) const DALEPENA: u32 = 0xC720;

/// A physical endpoint's command registers: `DEPCMDPAR2`, `PAR1`, `PAR0`
/// and `DEPCMD`, sixteen bytes each from `0xC800`.
pub(crate) const fn depcmd_base(physical: u8) -> u32 {
    0xC800 + 0x10 * physical as u32
}
/// `DEPCMDPAR2`, from an endpoint's base.
pub(crate) const DEPCMDPAR2: u32 = 0x0;
/// `DEPCMDPAR1`.
pub(crate) const DEPCMDPAR1: u32 = 0x4;
/// `DEPCMDPAR0`.
pub(crate) const DEPCMDPAR0: u32 = 0x8;
/// `DEPCMD`.
pub(crate) const DEPCMD: u32 = 0xC;

// GSNPSID's high half, the core.
/// `DWC_usb3`.
pub(crate) const ID_DWC3: u32 = 0x5533;
/// `DWC_usb31`, the gs201's.
pub(crate) const ID_DWC31: u32 = 0x3331;
/// `DWC_usb32`.
pub(crate) const ID_DWC32: u32 = 0x3332;

// GCTL.
/// The port capability direction field.
pub(crate) const GCTL_PRTCAPDIR_MASK: u32 = 3 << 12;
/// Port capability: device.
pub(crate) const GCTL_PRTCAP_DEVICE: u32 = 2 << 12;

// GUSB2PHYCFG.
/// Suspend the USB 2.0 PHY when the link is suspended. Kept off: at high
/// speed an endpoint command waits on the PHY's clock, and Linux clears
/// this around every one; off, nothing need be saved and restored.
pub(crate) const GUSB2PHYCFG_SUSPHY: u32 = 1 << 6;
/// Let the core drive the PHY's sleep and L1 suspend lines, which only
/// link power management uses; off with it.
pub(crate) const GUSB2PHYCFG_ENBLSLPM: u32 = 1 << 8;
/// The PHY has a free-running clock. The tree's
/// `snps,dis-u2-freeclk-exists-quirk` says it does not.
pub(crate) const GUSB2PHYCFG_U2_FREECLK_EXISTS: u32 = 1 << 30;

// GEVNTCOUNT.
/// `GEVNTCOUNT`'s count, in bytes.
pub(crate) const GEVNTCOUNT_MASK: u32 = 0xFFFC;

// GFLADJ.
/// Use the 30 MHz adjustment field below.
pub(crate) const GFLADJ_30MHZ_SDBND_SEL: u32 = 1 << 7;
/// The frame length adjustment for the 30 MHz clock.
pub(crate) const GFLADJ_30MHZ_MASK: u32 = 0x3F;
/// The tree's `snps,quirk-frame-length-adjustment`.
pub(crate) const FRAME_LENGTH_ADJUSTMENT: u32 = 0x20;

// DCFG.
/// The fastest speed the device connects at.
pub(crate) const DCFG_SPEED_MASK: u32 = 7;
/// High speed.
pub(crate) const DCFG_HIGHSPEED: u32 = 0;
/// The device address field.
pub(crate) const DCFG_DEVADDR_MASK: u32 = 0x7F << 3;
/// The device address's shift.
pub(crate) const DCFG_DEVADDR_SHIFT: u32 = 3;
/// Answer link power management tokens. The tree's
/// `snps,usb2-gadget-lpm-disable` keeps it off.
pub(crate) const DCFG_LPM_CAP: u32 = 1 << 22;

// DCTL.
/// Run: connect to the bus.
pub(crate) const DCTL_RUN_STOP: u32 = 1 << 31;
/// The core's soft reset, which clears itself when done.
pub(crate) const DCTL_CSFTRST: u32 = 1 << 30;
/// L1 hibernation, for LPM.
pub(crate) const DCTL_L1_HIBER_EN: u32 = 1 << 18;
/// Keep the connection across a run/stop.
pub(crate) const DCTL_KEEP_CONNECT: u32 = 1 << 19;
/// The four U1 and U2 enables. The tree's `snps,dis-u1-entry-quirk` and
/// `dis-u2-entry-quirk` keep them off, which at high speed they are anyway.
pub(crate) const DCTL_U1_U2: u32 = (1 << 9) | (1 << 10) | (1 << 11) | (1 << 12);
/// The HIRD threshold, for LPM.
pub(crate) const DCTL_HIRD_THRES_MASK: u32 = 0x1F << 24;
/// The link state change request field, which a read-modify-write must
/// write as zero so as not to ask for a change.
pub(crate) const DCTL_ULSTCHNGREQ_MASK: u32 = 0xF << 5;

// DEVTEN.
/// Disconnect.
pub(crate) const DEVTEN_DISCONNEVTEN: u32 = 1 << 0;
/// USB reset.
pub(crate) const DEVTEN_USBRSTEN: u32 = 1 << 1;
/// Connect done.
pub(crate) const DEVTEN_CONNECTDONEEN: u32 = 1 << 2;
/// Wakeup.
pub(crate) const DEVTEN_WKUPEVTEN: u32 = 1 << 4;
/// Suspend (U3, L2 or L1), from release 2.30a on.
pub(crate) const DEVTEN_U3L2L1SUSPEN: u32 = 1 << 6;
/// Erratic error.
pub(crate) const DEVTEN_ERRTICERREN: u32 = 1 << 9;
/// Event buffer overflow.
pub(crate) const DEVTEN_EVNTOVERFLOWEN: u32 = 1 << 11;

// DSTS.
/// The speed the link connected at.
pub(crate) const DSTS_CONNECTSPD: u32 = 7;
/// Connected at high speed.
pub(crate) const DSTS_HIGHSPEED: u32 = 0;
/// Connected at full speed.
pub(crate) const DSTS_FULLSPEED: u32 = 1;
/// The controller has halted.
pub(crate) const DSTS_DEVCTRLHLT: u32 = 1 << 22;

// DEPCMD.
/// The command is running; set to start one.
pub(crate) const DEPCMD_CMDACT: u32 = 1 << 10;
/// Write an endpoint command complete event when it ends.
pub(crate) const DEPCMD_CMDIOC: u32 = 1 << 8;
/// End Transfer: force the transfer's resources off.
pub(crate) const DEPCMD_HIPRI_FORCERM: u32 = 1 << 11;
/// The command's parameter field's shift: a resource index.
pub(crate) const DEPCMD_PARAM_SHIFT: u32 = 16;
/// The status field's shift.
pub(crate) const DEPCMD_STATUS_SHIFT: u32 = 12;
/// Set Endpoint Configuration.
pub(crate) const DEPCMD_SETEPCONFIG: u32 = 0x01;
/// Set Endpoint Transfer Resource Configuration.
pub(crate) const DEPCMD_SETTRANSFRESOURCE: u32 = 0x02;
/// Set Stall.
pub(crate) const DEPCMD_SETSTALL: u32 = 0x04;
/// Clear Stall.
pub(crate) const DEPCMD_CLEARSTALL: u32 = 0x05;
/// Start Transfer.
pub(crate) const DEPCMD_STARTTRANSFER: u32 = 0x06;
/// End Transfer.
pub(crate) const DEPCMD_ENDTRANSFER: u32 = 0x08;
/// Start New Configuration.
pub(crate) const DEPCMD_DEPSTARTCFG: u32 = 0x09;

// DEPCFG's parameter 0.
/// The endpoint's transfer type's shift.
pub(crate) const DEPCFG_EP_TYPE_SHIFT: u32 = 1;
/// Control.
pub(crate) const EP_TYPE_CONTROL: u32 = 0;
/// Bulk.
pub(crate) const EP_TYPE_BULK: u32 = 2;
/// Interrupt.
pub(crate) const EP_TYPE_INTERRUPT: u32 = 3;
/// The largest packet's shift.
pub(crate) const DEPCFG_MAX_PACKET_SHIFT: u32 = 3;
/// The transmit FIFO's shift, for IN endpoints.
pub(crate) const DEPCFG_FIFO_NUMBER_SHIFT: u32 = 17;
/// Modify an endpoint's configuration rather than initialise it.
pub(crate) const DEPCFG_ACTION_MODIFY: u32 = 2 << 30;

// DEPCFG's parameter 1.
/// Write transfer complete events.
pub(crate) const DEPCFG_XFER_COMPLETE_EN: u32 = 1 << 8;
/// Write transfer not ready events.
pub(crate) const DEPCFG_XFER_NOT_READY_EN: u32 = 1 << 10;
/// `bInterval` minus one's shift.
pub(crate) const DEPCFG_BINTERVAL_M1_SHIFT: u32 = 16;
/// The physical endpoint number's shift.
pub(crate) const DEPCFG_EP_NUMBER_SHIFT: u32 = 25;
