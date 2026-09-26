//! A model of a DWC3 in device mode, the processor's cache in front of its
//! memory, and a host on the other end of the cable.
//!
//! The controller keeps the registers the driver touches, executes endpoint
//! commands the moment `DEPCMD` is written, and appends events to the
//! event buffer as a real one does, counting them in `GEVNTCOUNT`. It moves
//! data only when the host side is stepped ([`Model::advance`]): a SETUP,
//! a control data or status stage, bulk packets either way. Each step
//! walks the TRBs the driver started, reads and writes their buffers and
//! writes the TRBs back with HWO clear, then writes the event.
//!
//! # The cache
//!
//! The processor reaches the area through [`Memory`], a write-back cache of
//! [`CACHE_LINE`] lines; the controller reaches it directly. What a real
//! non-coherent controller would get silently wrong is recorded in
//! [`Model::violations`] instead:
//!
//! * the controller reads a byte the processor wrote and did not clean;
//! * the controller writes a line the processor holds dirty, which a later
//!   write-back would undo;
//! * the processor reads a line it had cached before the controller wrote
//!   it, without invalidating it since;
//! * an invalidate throws away processor writes;
//! * a TRB, an IN buffer or the event buffer is handed over -- by the Start
//!   Transfer, `GEVNTADRLO` or `GEVNTCOUNT` write that gives it to the
//!   controller -- with a dirty line in it. On uncached memory that is the
//!   missing barrier between a Normal write and the Device write after it.
//!
//! It also records what a real controller would do wrong or never do: a
//! register set against the phone's tree at run time, a command out of
//! order, a TRB of the wrong type for its stage, an OUT control TRB that is
//! not whole packets, an IN data stage that stops on a full packet short of
//! `wLength` without a zero-length packet, the address not in `DCFG` by the
//! status stage of `SET_ADDRESS`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::format;
use std::rc::Rc;
use std::string::String;
use std::vec;
use std::vec::Vec;

use ferrix_usb_device::Setup;

use crate::layout::{AREA_BYTES, CACHE_LINE, EVENT_BYTES, PAGE};
use crate::regs::{
    DALEPENA, DCFG, DCFG_DEVADDR_MASK, DCFG_DEVADDR_SHIFT, DCFG_LPM_CAP, DCFG_SPEED_MASK, DCTL,
    DCTL_CSFTRST, DCTL_RUN_STOP, DCTL_U1_U2, DCTL_ULSTCHNGREQ_MASK, DEPCMD_CLEARSTALL,
    DEPCMD_CMDACT, DEPCMD_CMDIOC, DEPCMD_DEPSTARTCFG, DEPCMD_ENDTRANSFER, DEPCMD_SETEPCONFIG,
    DEPCMD_SETSTALL, DEPCMD_SETTRANSFRESOURCE, DEPCMD_STARTTRANSFER, DEVTEN, DSTS, DSTS_DEVCTRLHLT,
    GCTL, GCTL_PRTCAP_DEVICE, GCTL_PRTCAPDIR_MASK, GEVNTADRHI, GEVNTADRLO, GEVNTCOUNT, GEVNTSIZ,
    GFLADJ, GFLADJ_30MHZ_MASK, GFLADJ_30MHZ_SDBND_SEL, GSNPSID, GUSB2PHYCFG, GUSB2PHYCFG_SUSPHY,
    GUSB2PHYCFG_U2_FREECLK_EXISTS,
};
use crate::trb::{
    CHN, CONTROL_DATA, CONTROL_SETUP, CONTROL_STATUS2, CONTROL_STATUS3, HWO, SIZE_MASK, TRB_BYTES,
    TRBCTL_MASK,
};
use crate::{Clock, Dma, MILLISECOND, Registers};

/// Where each page of the area is, as the controller sees it: scattered,
/// out of order, and some above 4 GiB, so both halves of every pointer
/// matter.
pub(super) const PAGES: [u64; 7] = [
    0x9700_2000,
    0x1_0004_0000,
    0x9700_0000,
    0x2_8123_4000,
    0x9700_5000,
    0x1_0000_1000,
    0x9700_7000,
];

/// The core's ID: `DWC_usb31`, release 1.90a as `GSNPSID` gives it.
pub(super) const DWC31: u32 = 0x3331_0000;

/// The event codes the model writes.
const XFER_COMPLETE: u32 = 1;
const XFER_NOT_READY: u32 = 3;
const COMMAND_COMPLETE: u32 = 7;
const DISCONNECT: u32 = 0;
const RESET: u32 = 1;
const CONNECT_DONE: u32 = 2;
/// A transfer-not-ready's status for the data and the status stage.
const NOT_READY_DATA: u32 = 1;
const NOT_READY_STATUS: u32 = 2;
/// An endpoint event's status for a short packet.
const SHORT: u32 = 2;

/// How long the core's soft reset takes.
const RESET_TIME: u64 = 20 * MILLISECOND;

/// A cache line, as the processor holds it.
#[derive(Clone, Copy, Debug)]
struct Line {
    cached: bool,
    /// A bit per byte the controller wrote since the processor cached the
    /// line.
    stale: u64,
    /// A bit per byte the processor wrote and has not cleaned.
    dirty: u64,
    data: [u8; CACHE_LINE],
}

impl Line {
    const EMPTY: Line = Line {
        cached: false,
        stale: 0,
        dirty: 0,
        data: [0; CACHE_LINE],
    };
}

/// A physical endpoint's state in the controller.
#[derive(Clone, Copy, Default, Debug)]
struct Physical {
    /// `DEPCFG`'s parameters 0 and 1.
    config: Option<(u32, u32)>,
    resource_ready: bool,
    /// The first TRB of the transfer started, and its resource index.
    transfer: Option<u64>,
    resource: u8,
    stalled: bool,
    /// `DEPCMDPAR0`, `PAR1`, `PAR2`.
    parameters: [u32; 3],
    command: u32,
}

impl Physical {
    fn max_packet(&self) -> usize {
        self.config
            .map_or(0, |(p0, _)| ((p0 >> 3) & 0x7FF) as usize)
    }
}

/// How a host's control transfer ended badly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum HostError {
    /// The device stalled it.
    Stall,
    /// The device did not answer at the host's address.
    NoResponse,
}

/// Where a host's control transfer is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Setup,
    DataIn,
    DataOut,
    StatusIn,
    StatusOut,
}

#[derive(Clone, Debug)]
struct Control {
    setup: Setup,
    out: Vec<u8>,
    stage: Stage,
    received: Vec<u8>,
    /// Transfer-not-ready written for this stage.
    notified: bool,
}

/// The controller, the cache, and the host.
#[derive(Debug)]
pub(super) struct Model {
    ram: Vec<u8>,
    lines: Vec<Line>,
    pub(super) violations: Vec<String>,
    pub(super) now: u64,

    pub(super) gsnpsid: u32,
    gctl: u32,
    gusb2phycfg: u32,
    gfladj: u32,
    event_address: u64,
    gevntsiz: u32,
    event_count: u32,
    event_write: usize,
    dcfg: u32,
    dctl: u32,
    devten: u32,
    dalepena: u32,
    reset_until: Option<u64>,
    endpoints: [Physical; 8],
    started_config: bool,
    /// The core's soft reset never finishes.
    pub(super) wedged_reset: bool,
    /// Endpoint commands never finish.
    pub(super) wedged_command: bool,
    /// The processor's clean and invalidate do nothing, as a driver that
    /// forgot them.
    pub(super) skip_clean: bool,
    pub(super) skip_invalidate: bool,
    /// Cleans starting in this range do nothing.
    pub(super) skip_clean_in: Option<core::ops::Range<usize>>,
    /// Every endpoint command run: the physical endpoint and `DEPCMD`.
    pub(super) commands: Vec<(u8, u32)>,

    /// The host is on the bus, at high speed rather than full.
    attached: bool,
    high_speed: bool,
    pub(super) host_address: u8,
    control: Option<Control>,
    result: Option<Result<Vec<u8>, HostError>>,
    ep0_stalled: bool,
    /// Packets the host has for each OUT endpoint.
    out_packets: [VecDeque<Vec<u8>>; 8],
    /// Each IN endpoint's read in progress on the host, and those done.
    in_partial: [Vec<u8>; 8],
    pub(super) in_reads: [Vec<Vec<u8>>; 8],
}

pub(super) type Shared = Rc<RefCell<Model>>;

impl Model {
    pub(super) fn new() -> Shared {
        Rc::new(RefCell::new(Model {
            ram: vec![0xA5; AREA_BYTES],
            lines: vec![Line::EMPTY; AREA_BYTES / CACHE_LINE],
            violations: Vec::new(),
            now: 0,
            gsnpsid: DWC31 | 0x190A,
            // What a boot loader running fastboot may leave: an OTG port,
            // the PHY's reset defaults, SuperSpeedPlus with LPM, running.
            gctl: 3 << 12,
            gusb2phycfg: GUSB2PHYCFG_U2_FREECLK_EXISTS | (1 << 8) | GUSB2PHYCFG_SUSPHY,
            gfladj: 0x0C80_0000,
            event_address: 0,
            gevntsiz: 0,
            event_count: 0,
            event_write: 0,
            dcfg: DCFG_LPM_CAP | 5 | (9 << DCFG_DEVADDR_SHIFT),
            dctl: DCTL_RUN_STOP | DCTL_U1_U2,
            devten: 0,
            dalepena: 0,
            reset_until: None,
            endpoints: [Physical::default(); 8],
            started_config: false,
            wedged_reset: false,
            wedged_command: false,
            skip_clean: false,
            skip_invalidate: false,
            skip_clean_in: None,
            commands: Vec::new(),
            attached: false,
            high_speed: true,
            host_address: 0,
            control: None,
            result: None,
            ep0_stalled: false,
            out_packets: Default::default(),
            in_partial: Default::default(),
            in_reads: Default::default(),
        }))
    }

    fn violation(&mut self, text: String) {
        self.violations.push(text);
    }

    // -----------------------------------------------------------------------
    // Memory: the processor's side, through the cache
    // -----------------------------------------------------------------------

    fn load_line(&mut self, index: usize) {
        let line = &mut self.lines[index];
        if !line.cached {
            line.data
                .copy_from_slice(&self.ram[index * CACHE_LINE..(index + 1) * CACHE_LINE]);
            line.cached = true;
            line.stale = 0;
            line.dirty = 0;
        }
    }

    fn cpu_read8(&mut self, offset: usize) -> u8 {
        let index = offset / CACHE_LINE;
        self.load_line(index);
        let bit = 1 << (offset % CACHE_LINE);
        if self.lines[index].stale & bit != 0 {
            self.violation(format!(
                "the processor read {offset:#x}, which the controller wrote, without invalidating"
            ));
            self.lines[index].stale &= !bit;
        }
        self.lines[index].data[offset % CACHE_LINE]
    }

    fn cpu_write8(&mut self, offset: usize, value: u8) {
        let index = offset / CACHE_LINE;
        self.load_line(index);
        let line = &mut self.lines[index];
        line.data[offset % CACHE_LINE] = value;
        line.dirty |= 1 << (offset % CACHE_LINE);
    }

    fn lines_of(offset: usize, len: usize) -> core::ops::Range<usize> {
        if len == 0 {
            return 0..0;
        }
        offset / CACHE_LINE..(offset + len).div_ceil(CACHE_LINE)
    }

    fn clean(&mut self, offset: usize, len: usize) {
        if self.skip_clean
            || self
                .skip_clean_in
                .as_ref()
                .is_some_and(|range| range.contains(&offset))
        {
            return;
        }
        for index in Self::lines_of(offset, len) {
            let line = self.lines[index];
            if line.dirty == 0 {
                continue;
            }
            if line.stale != 0 {
                // A cache writes the whole line back, the controller's
                // bytes in it too.
                self.violation(format!(
                    "a clean at {:#x} wrote back a line the controller had written",
                    index * CACHE_LINE
                ));
            }
            for byte in 0..CACHE_LINE {
                if line.dirty & (1 << byte) != 0 {
                    self.ram[index * CACHE_LINE + byte] = line.data[byte];
                }
            }
            self.lines[index].dirty = 0;
        }
    }

    fn invalidate(&mut self, offset: usize, len: usize) {
        if self.skip_invalidate {
            return;
        }
        for index in Self::lines_of(offset, len) {
            if self.lines[index].dirty != 0 {
                self.violation(format!(
                    "an invalidate at {:#x} threw away the processor's writes",
                    index * CACHE_LINE
                ));
            }
            self.lines[index] = Line::EMPTY;
        }
    }

    // -----------------------------------------------------------------------
    // Memory: the controller's side
    // -----------------------------------------------------------------------

    /// The area offset of a device address.
    fn translate(&mut self, address: u64) -> Option<usize> {
        let found = PAGES.iter().enumerate().find_map(|(page, &base)| {
            (base..base + PAGE as u64)
                .contains(&address)
                .then(|| page * PAGE + (address - base) as usize)
        });
        if found.is_none() {
            self.violation(format!(
                "the controller reached {address:#x}, outside the area"
            ));
        }
        found
    }

    fn device_read(&mut self, address: u64, len: usize) -> Vec<u8> {
        let Some(offset) = self.translate(address) else {
            return vec![0; len];
        };
        for at in offset..offset + len {
            if self.lines[at / CACHE_LINE].dirty & (1 << (at % CACHE_LINE)) != 0 {
                self.violation(format!(
                    "the controller read {at:#x}, which the processor wrote and did not clean"
                ));
                break;
            }
        }
        self.ram[offset..offset + len].to_vec()
    }

    fn device_write(&mut self, address: u64, bytes: &[u8]) {
        let Some(offset) = self.translate(address) else {
            return;
        };
        for index in Self::lines_of(offset, bytes.len()) {
            if self.lines[index].dirty != 0 {
                self.violation(format!(
                    "the controller wrote {:#x}, a line the processor holds dirty",
                    index * CACHE_LINE
                ));
            }
        }
        for at in offset..offset + bytes.len() {
            let line = &mut self.lines[at / CACHE_LINE];
            if line.cached {
                line.stale |= 1 << (at % CACHE_LINE);
            }
        }
        self.ram[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn device_read32(&mut self, address: u64) -> u32 {
        let bytes = self.device_read(address, 4);
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// Record a violation for any dirty byte in `len` bytes at the area
    /// `offset`, handed over as `what`; `whole_lines` for memory the
    /// controller will write.
    fn check_clean(&mut self, offset: usize, len: usize, whole_lines: bool, what: &str) {
        let dirty = if whole_lines {
            Self::lines_of(offset, len).any(|index| self.lines[index].dirty != 0)
        } else {
            (offset..offset + len)
                .any(|at| self.lines[at / CACHE_LINE].dirty & (1 << (at % CACHE_LINE)) != 0)
        };
        if dirty {
            self.violation(format!("{what} at {offset:#x} handed over not cleaned"));
        }
    }

    // -----------------------------------------------------------------------
    // Registers
    // -----------------------------------------------------------------------

    fn settle(&mut self) {
        if let Some(until) = self.reset_until
            && self.now >= until
            && !self.wedged_reset
        {
            self.dctl &= !DCTL_CSFTRST;
            self.reset_until = None;
        }
    }

    fn halted(&self) -> bool {
        self.dctl & DCTL_RUN_STOP == 0 && self.event_count == 0
    }

    fn read(&mut self, offset: u32) -> u32 {
        self.settle();
        match offset {
            GSNPSID => self.gsnpsid,
            GCTL => self.gctl,
            GUSB2PHYCFG => self.gusb2phycfg,
            GEVNTADRLO => self.event_address as u32,
            GEVNTADRHI => (self.event_address >> 32) as u32,
            GEVNTSIZ => self.gevntsiz,
            GEVNTCOUNT => self.event_count,
            GFLADJ => self.gfladj,
            DCFG => self.dcfg,
            DCTL => self.dctl,
            DEVTEN => self.devten,
            DSTS => {
                let speed = u32::from(!self.high_speed);
                speed | if self.halted() { DSTS_DEVCTRLHLT } else { 0 }
            }
            DALEPENA => self.dalepena,
            0xC800..=0xC87F => {
                let endpoint = &self.endpoints[((offset - 0xC800) / 16) as usize];
                match offset % 16 {
                    0x0 => endpoint.parameters[2],
                    0x4 => endpoint.parameters[1],
                    0x8 => endpoint.parameters[0],
                    _ => endpoint.command,
                }
            }
            _ => 0,
        }
    }

    fn write(&mut self, offset: u32, value: u32) {
        self.settle();
        match offset {
            GCTL => self.gctl = value,
            GUSB2PHYCFG => self.gusb2phycfg = value,
            GEVNTADRLO => {
                self.event_address = (self.event_address & !0xFFFF_FFFF) | u64::from(value);
            }
            GEVNTADRHI => {
                self.event_address = (self.event_address & 0xFFFF_FFFF) | (u64::from(value) << 32);
            }
            GEVNTSIZ => self.gevntsiz = value,
            GEVNTCOUNT => self.take_events(value),
            GFLADJ => self.gfladj = value,
            DCFG => self.dcfg = value,
            DCTL => self.write_dctl(value),
            DEVTEN => self.devten = value,
            DALEPENA => self.dalepena = value,
            0xC800..=0xC87F => {
                let physical = ((offset - 0xC800) / 16) as u8;
                let endpoint = &mut self.endpoints[usize::from(physical)];
                match offset % 16 {
                    0x0 => endpoint.parameters[2] = value,
                    0x4 => endpoint.parameters[1] = value,
                    0x8 => endpoint.parameters[0] = value,
                    _ => self.write_command(physical, value),
                }
            }
            _ => self.violation(format!(
                "a write to {offset:#x}, which the driver has no need of"
            )),
        }
    }

    fn take_events(&mut self, value: u32) {
        self.check_event_buffer("the event buffer");
        if value > self.event_count {
            self.violation(format!(
                "GEVNTCOUNT written {value}, more than {}",
                self.event_count
            ));
        }
        self.event_count = self.event_count.saturating_sub(value);
    }

    fn check_event_buffer(&mut self, what: &str) {
        if let Some(offset) = self.translate_quiet(self.event_address) {
            self.check_clean(offset, EVENT_BYTES, true, what);
        }
    }

    fn translate_quiet(&self, address: u64) -> Option<usize> {
        PAGES.iter().enumerate().find_map(|(page, &base)| {
            (base..base + PAGE as u64)
                .contains(&address)
                .then(|| page * PAGE + (address - base) as usize)
        })
    }

    fn write_dctl(&mut self, value: u32) {
        if value & DCTL_ULSTCHNGREQ_MASK != 0 {
            self.violation(format!(
                "DCTL written {value:#x}, asking for a link state change"
            ));
        }
        let was_running = self.dctl & DCTL_RUN_STOP != 0;
        self.dctl = value;
        if value & DCTL_CSFTRST != 0 {
            self.soft_reset();
            return;
        }
        if value & DCTL_RUN_STOP != 0 && !was_running {
            self.check_run();
        }
    }

    fn soft_reset(&mut self) {
        self.dctl &= !DCTL_RUN_STOP;
        self.reset_until = Some(self.now + RESET_TIME);
        self.dcfg = DCFG_LPM_CAP | 5;
        self.devten = 0;
        self.dalepena = 0;
        self.endpoints = [Physical::default(); 8];
        self.started_config = false;
        self.event_count = 0;
        self.event_write = 0;
    }

    /// What the phone's tree and the brief require by the time the
    /// controller runs.
    fn check_run(&mut self) {
        let checks = [
            (self.dctl & DCTL_CSFTRST == 0, "run during the soft reset"),
            (
                self.gctl & GCTL_PRTCAPDIR_MASK == GCTL_PRTCAP_DEVICE,
                "the port is not a device",
            ),
            (
                self.dcfg & DCFG_SPEED_MASK == 0,
                "DCFG is not held at high speed",
            ),
            (self.dcfg & DCFG_LPM_CAP == 0, "LPM is offered"),
            (
                self.gusb2phycfg & (GUSB2PHYCFG_U2_FREECLK_EXISTS | GUSB2PHYCFG_SUSPHY) == 0,
                "GUSB2PHYCFG keeps the free clock or PHY suspend",
            ),
            (
                self.gfladj & (GFLADJ_30MHZ_MASK | GFLADJ_30MHZ_SDBND_SEL)
                    == 0x20 | GFLADJ_30MHZ_SDBND_SEL,
                "the frame length adjustment is not 0x20",
            ),
            (self.dctl & DCTL_U1_U2 == 0, "U1 or U2 entry is enabled"),
            (
                self.gevntsiz == EVENT_BYTES as u32,
                "the event buffer is not a page, unmasked",
            ),
            (
                self.devten & 0b111 == 0b111,
                "reset, connect done or disconnect is not enabled",
            ),
            (
                self.endpoints[0].transfer.is_some(),
                "no SETUP TRB is started",
            ),
            (self.dalepena & 0b11 == 0b11, "endpoint 0 is not active"),
        ];
        for (good, what) in checks {
            if !good {
                self.violation(format!("at run: {what}"));
            }
        }
        if self.translate_quiet(self.event_address) != Some(0) {
            self.violation(String::from(
                "at run: the event buffer is not the area's page 0",
            ));
        }
    }

    // -----------------------------------------------------------------------
    // Endpoint commands
    // -----------------------------------------------------------------------

    fn write_command(&mut self, physical: u8, value: u32) {
        self.commands.push((physical, value));
        let index = usize::from(physical);
        self.endpoints[index].command = value;
        if value & DEPCMD_CMDACT == 0 {
            self.violation(format!("DEPCMD {value:#x} written without CMDACT"));
            return;
        }
        if self.wedged_command {
            return;
        }
        let parameters = self.endpoints[index].parameters;
        let resource = match value & 0xF {
            DEPCMD_DEPSTARTCFG => self.start_config(physical, value),
            DEPCMD_SETEPCONFIG => self.set_config(physical, parameters),
            DEPCMD_SETTRANSFRESOURCE => {
                if parameters[0] != 1 {
                    self.violation(format!("DEPXFERCFG on {physical} for {}", parameters[0]));
                }
                self.endpoints[index].resource_ready = true;
                0
            }
            DEPCMD_STARTTRANSFER => self.start_transfer(physical, parameters),
            DEPCMD_ENDTRANSFER => self.end_transfer(physical, value),
            DEPCMD_SETSTALL => {
                self.endpoints[index].stalled = true;
                self.ep0_stalled |= physical < 2;
                0
            }
            DEPCMD_CLEARSTALL => {
                self.endpoints[index].stalled = false;
                0
            }
            other => {
                self.violation(format!("unknown endpoint command {other:#x}"));
                0
            }
        };
        self.endpoints[index].command =
            (value & !DEPCMD_CMDACT & 0xFFFF) | (u32::from(resource) << 16);
    }

    fn start_config(&mut self, physical: u8, value: u32) -> u8 {
        if physical != 0 {
            self.violation(format!("DEPSTARTCFG on {physical}"));
        }
        match (value >> 16) & 0x7F {
            0 => self.started_config = true,
            2 => {
                for endpoint in &mut self.endpoints[2..] {
                    endpoint.resource_ready = false;
                }
            }
            other => self.violation(format!("DEPSTARTCFG with resource index {other}")),
        }
        0
    }

    fn set_config(&mut self, physical: u8, [p0, p1, _]: [u32; 3]) -> u8 {
        let modify = p0 >> 30 == 2;
        let checks = [
            (self.started_config, "DEPCFG before DEPSTARTCFG"),
            (
                (p1 >> 25) & 0x1F == u32::from(physical),
                "DEPCFG naming another endpoint",
            ),
            (
                physical.is_multiple_of(2) || (p0 >> 17) & 0x1F == u32::from(physical >> 1),
                "an IN endpoint's FIFO is not its number",
            ),
            (p1 & (1 << 8) != 0, "no transfer complete events"),
            (
                physical >= 2 || p1 & (1 << 10) != 0,
                "endpoint 0 without transfer-not-ready events",
            ),
            (
                !modify || self.endpoints[usize::from(physical)].config.is_some(),
                "a modify first",
            ),
        ];
        for (good, what) in checks {
            if !good {
                self.violation(format!("physical {physical}: {what}"));
            }
        }
        self.endpoints[usize::from(physical)].config = Some((p0, p1));
        0
    }

    fn start_transfer(&mut self, physical: u8, [p0, p1, _]: [u32; 3]) -> u8 {
        let index = usize::from(physical);
        let endpoint = self.endpoints[index];
        let checks = [
            (endpoint.config.is_some(), "Start Transfer before DEPCFG"),
            (
                endpoint.resource_ready,
                "Start Transfer without a transfer resource",
            ),
            (
                self.dalepena & (1 << physical) != 0,
                "Start Transfer on an inactive endpoint",
            ),
            (
                endpoint.transfer.is_none(),
                "Start Transfer with one started",
            ),
        ];
        for (good, what) in checks {
            if !good {
                self.violation(format!("physical {physical}: {what}"));
            }
        }
        let address = (u64::from(p0) << 32) | u64::from(p1);
        self.check_chain(physical, address);
        self.endpoints[index].transfer = Some(address);
        self.endpoints[index].resource = 0x20 + physical;
        0x20 + physical
    }

    /// The hand-off: every TRB of the chain, and each IN buffer, cleaned;
    /// each OUT buffer's lines not dirty; HWO set.
    fn check_chain(&mut self, physical: u8, mut address: u64) {
        for _ in 0..4 {
            let Some(offset) = self.translate(address) else {
                return;
            };
            self.check_clean(offset, TRB_BYTES, true, "a TRB");
            let trb = self.read_trb(address);
            if trb.2 & HWO == 0 {
                self.violation(format!("physical {physical}: a TRB started without HWO"));
            }
            if let Some(buffer) = self.translate_quiet(trb.0) {
                let length = (trb.1 & SIZE_MASK) as usize;
                if physical % 2 == 1 {
                    self.check_clean(buffer, length, false, "IN data");
                } else {
                    self.check_clean(buffer, length, true, "an OUT buffer");
                }
            }
            if trb.2 & CHN == 0 {
                return;
            }
            address += TRB_BYTES as u64;
        }
    }

    fn end_transfer(&mut self, physical: u8, value: u32) -> u8 {
        let index = usize::from(physical);
        let resource = ((value >> 16) & 0x7F) as u8;
        if self.endpoints[index].transfer.is_some() && resource != self.endpoints[index].resource {
            self.violation(format!(
                "End Transfer on {physical} with resource {resource}"
            ));
        }
        self.endpoints[index].transfer = None;
        if value & DEPCMD_CMDIOC != 0 {
            self.post_endpoint(physical, COMMAND_COMPLETE, 0, DEPCMD_ENDTRANSFER << 8);
        }
        0
    }

    // -----------------------------------------------------------------------
    // TRBs and events
    // -----------------------------------------------------------------------

    /// A TRB as the controller reads it: buffer, size word, control word.
    fn read_trb(&mut self, address: u64) -> (u64, u32, u32) {
        let low = self.device_read32(address);
        let high = self.device_read32(address + 4);
        let size = self.device_read32(address + 8);
        let control = self.device_read32(address + 12);
        (u64::from(low) | (u64::from(high) << 32), size, control)
    }

    /// Write a TRB back as done: what was left, HWO clear.
    fn finish_trb(&mut self, address: u64, left: usize, status: u32) {
        let control = self.device_read32(address + 12) & !HWO;
        let size = (left as u32 & SIZE_MASK) | (status << 28);
        self.device_write(address + 8, &size.to_le_bytes());
        self.device_write(address + 12, &control.to_le_bytes());
    }

    fn post(&mut self, raw: u32) {
        let size = self.gevntsiz & 0xFFFF;
        if size == 0 {
            self.violation(String::from("an event with no event buffer"));
            return;
        }
        if self.event_count + 4 > size {
            self.violation(String::from("the event buffer overflowed"));
            return;
        }
        let address = self.event_address + self.event_write as u64;
        self.device_write(address, &raw.to_le_bytes());
        self.event_write = (self.event_write + 4) % size as usize;
        self.event_count += 4;
    }

    fn post_endpoint(&mut self, physical: u8, kind: u32, status: u32, parameter: u32) {
        let enables = self.endpoints[usize::from(physical)]
            .config
            .map_or(0, |(_, p1)| p1);
        let enabled = match kind {
            XFER_COMPLETE => enables & (1 << 8) != 0,
            XFER_NOT_READY => enables & (1 << 10) != 0,
            _ => true,
        };
        if enabled {
            self.post(
                (u32::from(physical) << 1) | (kind << 6) | (status << 12) | (parameter << 16),
            );
        }
    }

    fn post_device(&mut self, kind: u32) {
        if self.devten & (1 << kind) != 0 {
            self.post(1 | (kind << 8));
        }
    }

    /// Whether the interrupt line is asserted.
    pub(super) fn interrupt(&self) -> bool {
        self.event_count > 0 && self.gevntsiz & (1 << 31) == 0
    }

    /// Whether the controller has halted.
    pub(super) fn is_halted(&self) -> bool {
        self.halted()
    }

    /// Whether the physical endpoint is stalled.
    pub(super) fn stalled(&self, physical: usize) -> bool {
        self.endpoints[physical].stalled
    }

    /// `DCFG`'s device address.
    pub(super) fn dcfg_address(&self) -> u8 {
        ((self.dcfg & DCFG_DEVADDR_MASK) >> DCFG_DEVADDR_SHIFT) as u8
    }

    // -----------------------------------------------------------------------
    // The host: the link
    // -----------------------------------------------------------------------

    /// Plug the cable in: a reset, then the link up at the host's speed.
    pub(super) fn attach(&mut self, high_speed: bool) {
        self.attached = true;
        self.high_speed = high_speed;
        self.bus_reset();
    }

    /// Reset the bus, as a hub port reset does.
    pub(super) fn bus_reset(&mut self) {
        if !self.attached || self.dctl & DCTL_RUN_STOP == 0 {
            return;
        }
        self.host_address = 0;
        self.control = None;
        self.ep0_stalled = false;
        self.post_device(RESET);
        self.post_device(CONNECT_DONE);
    }

    /// Pull the cable.
    pub(super) fn detach(&mut self) {
        self.attached = false;
        self.control = None;
        self.host_address = 0;
        self.post_device(DISCONNECT);
    }

    // -----------------------------------------------------------------------
    // The host: control transfers
    // -----------------------------------------------------------------------

    pub(super) fn begin_control(&mut self, setup: Setup, out: &[u8]) {
        self.result = None;
        self.control = Some(Control {
            setup,
            out: out.to_vec(),
            stage: Stage::Setup,
            received: Vec::new(),
            notified: false,
        });
    }

    pub(super) fn take_result(&mut self) -> Option<Result<Vec<u8>, HostError>> {
        self.result.take()
    }

    /// Move the bus on as far as the device lets it: whether anything
    /// happened.
    pub(super) fn advance(&mut self) -> bool {
        let control = self.advance_control();
        let mut bulk = false;
        for physical in 2..8 {
            bulk |= self.advance_endpoint(physical);
        }
        control || bulk
    }

    fn advance_control(&mut self) -> bool {
        let Some(control) = self.control.clone() else {
            return false;
        };
        if control.stage != Stage::Setup && self.ep0_stalled {
            return self.finish(Err(HostError::Stall));
        }
        match control.stage {
            Stage::Setup => self.send_setup(control.setup),
            Stage::DataIn => self.stage_step(1, NOT_READY_DATA, Self::data_in),
            Stage::DataOut => self.stage_step(0, NOT_READY_DATA, Self::data_out),
            Stage::StatusIn => self.stage_step(1, NOT_READY_STATUS, Self::status),
            Stage::StatusOut => self.stage_step(0, NOT_READY_STATUS, Self::status),
        }
    }

    fn finish(&mut self, result: Result<Vec<u8>, HostError>) -> bool {
        self.control = None;
        self.result = Some(result);
        true
    }

    fn set_stage(&mut self, stage: Stage) {
        if let Some(control) = self.control.as_mut() {
            control.stage = stage;
            control.notified = false;
        }
    }

    /// Run a stage on `physical` if a transfer is started there, or say
    /// the host is waiting, once.
    fn stage_step(&mut self, physical: u8, waiting: u32, run: fn(&mut Self, u64) -> bool) -> bool {
        if let Some(address) = self.endpoints[usize::from(physical)].transfer {
            return run(self, address);
        }
        let control = self.control.as_mut().expect("a control transfer");
        if control.notified {
            return false;
        }
        control.notified = true;
        self.post_endpoint(physical, XFER_NOT_READY, waiting, 0);
        true
    }

    fn send_setup(&mut self, setup: Setup) -> bool {
        if self.dcfg_address() != self.host_address {
            return self.finish(Err(HostError::NoResponse));
        }
        let Some(address) = self.endpoints[0].transfer else {
            return false;
        };
        let trb = self.read_trb(address);
        if trb.2 & TRBCTL_MASK != CONTROL_SETUP || trb.1 & SIZE_MASK != 8 {
            self.violation(format!("a SETUP went to a TRB {:#x} of {}", trb.2, trb.1));
            return false;
        }
        self.device_write(trb.0, &setup.bytes());
        self.finish_trb(address, 0, 0);
        self.endpoints[0].transfer = None;
        // A SETUP clears a control endpoint's stall.
        self.ep0_stalled = false;
        self.endpoints[0].stalled = false;
        self.endpoints[1].stalled = false;
        self.post_endpoint(0, XFER_COMPLETE, 0, 0);
        let stage = match (setup.length, setup.is_in()) {
            (0, _) => Stage::StatusIn,
            (_, true) => Stage::DataIn,
            (_, false) => Stage::DataOut,
        };
        self.set_stage(stage);
        true
    }

    fn data_in(&mut self, first: u64) -> bool {
        let control = self.control.clone().expect("a control transfer");
        let asked = usize::from(control.setup.length);
        let (data, sizes) = self.take_chain(1, first, CONTROL_DATA);
        let zero_packet = sizes.len() > 1 && sizes.last() == Some(&0);
        if data.len() > asked {
            self.violation(format!("{} bytes for a wLength of {asked}", data.len()));
        }
        let ended = data.len() >= asked || data.len() % 64 != 0 || zero_packet || data.is_empty();
        if !ended {
            self.violation(format!(
                "a data stage of {} bytes, short of {asked}, ended with no zero-length packet",
                data.len()
            ));
        }
        self.post_endpoint(1, XFER_COMPLETE, 0, 0);
        if let Some(control) = self.control.as_mut() {
            control.received = data;
        }
        self.set_stage(Stage::StatusOut);
        true
    }

    fn data_out(&mut self, address: u64) -> bool {
        let control = self.control.clone().expect("a control transfer");
        let trb = self.read_trb(address);
        let size = (trb.1 & SIZE_MASK) as usize;
        if trb.2 & TRBCTL_MASK != CONTROL_DATA {
            self.violation(format!("an OUT data stage to a TRB {:#x}", trb.2));
        }
        if !size.is_multiple_of(64) {
            self.violation(format!("an OUT control TRB of {size}, not whole packets"));
        }
        let sent = control.out.len().min(size);
        self.device_write(trb.0, &control.out[..sent]);
        self.finish_trb(address, size - sent, 0);
        self.endpoints[0].transfer = None;
        let status = if sent < size { SHORT } else { 0 };
        self.post_endpoint(0, XFER_COMPLETE, status, 0);
        self.set_stage(Stage::StatusIn);
        true
    }

    fn status(&mut self, address: u64) -> bool {
        let control = self.control.clone().expect("a control transfer");
        let trb = self.read_trb(address);
        let expected = if control.setup.length == 0 {
            CONTROL_STATUS2
        } else {
            CONTROL_STATUS3
        };
        if trb.2 & TRBCTL_MASK != expected {
            self.violation(format!("a status stage to a TRB {:#x}", trb.2));
        }
        let physical = if control.stage == Stage::StatusIn {
            1
        } else {
            0
        };
        self.finish_trb(address, 0, 0);
        self.endpoints[physical].transfer = None;
        self.post_endpoint(physical as u8, XFER_COMPLETE, 0, 0);
        if control.setup.request_type == 0 && control.setup.request == 5 {
            let address = control.setup.value as u8;
            if self.dcfg_address() != address {
                self.violation(format!(
                    "SET_ADDRESS {address}: DCFG has {}",
                    self.dcfg_address()
                ));
            }
            self.host_address = address;
        }
        self.finish(Ok(control.received))
    }

    /// Walk a transfer's TRBs on `physical`, each of `kind` (or any, for
    /// 0), taking what they hold: the bytes, and each TRB's size.
    fn take_chain(&mut self, physical: u8, mut address: u64, kind: u32) -> (Vec<u8>, Vec<usize>) {
        let mut data = Vec::new();
        let mut sizes = Vec::new();
        for _ in 0..4 {
            let trb = self.read_trb(address);
            if trb.2 & HWO == 0 {
                self.violation(format!(
                    "physical {physical}: the controller met a TRB without HWO"
                ));
                break;
            }
            if kind != 0 && trb.2 & TRBCTL_MASK != kind {
                self.violation(format!("physical {physical}: a TRB {:#x}", trb.2));
            }
            let size = (trb.1 & SIZE_MASK) as usize;
            data.extend(self.device_read(trb.0, size));
            sizes.push(size);
            self.finish_trb(address, 0, 0);
            if trb.2 & CHN == 0 {
                break;
            }
            address += TRB_BYTES as u64;
        }
        self.endpoints[usize::from(physical)].transfer = None;
        (data, sizes)
    }

    // -----------------------------------------------------------------------
    // The host: bulk and interrupt
    // -----------------------------------------------------------------------

    /// Queue `bytes` for the OUT endpoint `physical`, in packets.
    pub(super) fn host_write(&mut self, physical: usize, bytes: &[u8], packet: usize) {
        for chunk in bytes.chunks(packet) {
            self.out_packets[physical].push_back(chunk.to_vec());
        }
        if bytes.is_empty() {
            self.out_packets[physical].push_back(Vec::new());
        }
    }

    fn advance_endpoint(&mut self, physical: usize) -> bool {
        let endpoint = self.endpoints[physical];
        let Some(address) = endpoint.transfer else {
            return false;
        };
        if endpoint.stalled || self.dalepena & (1 << physical) == 0 || !self.attached {
            return false;
        }
        if physical % 2 == 1 {
            self.bulk_in(physical, address);
            true
        } else if self.out_packets[physical].is_empty() {
            false
        } else {
            self.bulk_out(physical, address);
            true
        }
    }

    fn bulk_in(&mut self, physical: usize, address: u64) {
        let packet = self.endpoints[physical].max_packet().max(1);
        let (data, sizes) = self.take_chain(physical as u8, address, 0);
        let mut packets: Vec<&[u8]> = data.chunks(packet).collect();
        if sizes.last() == Some(&0) || data.is_empty() {
            packets.push(&[]);
        }
        for chunk in packets {
            self.in_partial[physical].extend_from_slice(chunk);
            if chunk.len() < packet {
                let read = std::mem::take(&mut self.in_partial[physical]);
                self.in_reads[physical].push(read);
            }
        }
        self.post_endpoint(physical as u8, XFER_COMPLETE, 0, 0);
    }

    fn bulk_out(&mut self, physical: usize, address: u64) {
        let packet = self.endpoints[physical].max_packet();
        let trb = self.read_trb(address);
        let size = (trb.1 & SIZE_MASK) as usize;
        let mut written = 0;
        while let Some(bytes) = self.out_packets[physical].pop_front() {
            if written + bytes.len() > size {
                self.violation(format!("a packet of {} past a TRB of {size}", bytes.len()));
                break;
            }
            self.device_write(trb.0 + written as u64, &bytes);
            written += bytes.len();
            if bytes.len() < packet || written == size {
                break;
            }
        }
        self.finish_trb(address, size - written, 0);
        self.endpoints[physical].transfer = None;
        let status = if written < size { SHORT } else { 0 };
        self.post_endpoint(physical as u8, XFER_COMPLETE, status, 0);
    }
}

/// The controller's registers, as the driver sees them.
#[derive(Debug)]
pub(super) struct Regs(pub(super) Shared);

impl Registers for Regs {
    fn read32(&self, offset: u32) -> u32 {
        self.0.borrow_mut().read(offset)
    }

    fn write32(&mut self, offset: u32, value: u32) {
        self.0.borrow_mut().write(offset, value);
    }
}

/// The area, through the processor's cache.
#[derive(Debug)]
pub(super) struct Memory(pub(super) Shared, pub(super) usize);

impl Dma for Memory {
    fn len(&self) -> usize {
        self.1
    }

    fn read32(&self, offset: usize) -> u32 {
        let mut model = self.0.borrow_mut();
        u32::from_le_bytes([
            model.cpu_read8(offset),
            model.cpu_read8(offset + 1),
            model.cpu_read8(offset + 2),
            model.cpu_read8(offset + 3),
        ])
    }

    fn write32(&mut self, offset: usize, value: u32) {
        let mut model = self.0.borrow_mut();
        for (at, byte) in (offset..).zip(value.to_le_bytes()) {
            model.cpu_write8(at, byte);
        }
    }

    fn read8(&self, offset: usize) -> u8 {
        self.0.borrow_mut().cpu_read8(offset)
    }

    fn write8(&mut self, offset: usize, value: u8) {
        self.0.borrow_mut().cpu_write8(offset, value);
    }

    fn clean(&mut self, offset: usize, len: usize) {
        self.0.borrow_mut().clean(offset, len);
    }

    fn invalidate(&mut self, offset: usize, len: usize) {
        self.0.borrow_mut().invalidate(offset, len);
    }

    fn device_address(&self, offset: usize) -> Option<u64> {
        let page = PAGES.get(offset / PAGE)?;
        (offset < self.1).then(|| page + (offset % PAGE) as u64)
    }
}

/// The time, which only sleeping moves.
#[derive(Debug)]
pub(super) struct Time(pub(super) Shared);

impl Clock for Time {
    fn now_nanos(&self) -> u64 {
        self.0.borrow().now
    }

    fn sleep_nanos(&mut self, nanos: u64) {
        self.0.borrow_mut().now += nanos.max(1);
    }
}
