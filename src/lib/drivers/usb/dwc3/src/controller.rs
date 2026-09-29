//! The controller: bring-up, the event buffer, endpoint commands, and the
//! device events that reset, connect and disconnect it.
//!
//! # Bring-up
//!
//! First, before anything is written, what the controller must be for
//! this driver to take it over: a DWC3 (`GSNPSID`), in device mode
//! (`GSTS`), with the run bit clear and halted (`DCTL`, `DSTS`) -- which is
//! how the phone's ABL leaves it after fastboot, with its own event buffer
//! still programmed at memory that is now Ferrix's. Anything else is
//! refused untouched: another agent may be driving it.
//!
//! Then, in the order Linux's `dwc3_core_init` and `__dwc3_gadget_start`
//! take it: the core's soft reset (`DCTL.CSFTRST`, which clears itself);
//! `GSTS.CSR_TIMEOUT` cleared, which ABL leaves set; the port capability
//! set to device; the quirks the phone's device tree names for the core
//! (no free-running USB 2.0 PHY clock, frame length adjustment 0x20, LPM
//! and U1/U2 off); `DCFG` held at high speed; the event buffer's address
//! and size, replacing ABL's; `DEPSTARTCFG`, then `DEPCFG` and
//! `DEPXFERCFG` for both directions of endpoint 0, physical endpoints 0
//! and 1, and both in `DALEPENA`; a SETUP TRB started; `DEVTEN`; and last
//! `DCTL`'s run bit, which connects the pull-up, with a bounded wait for
//! `DSTS.DEVCTRLHLT` to clear.
//!
//! # Events
//!
//! The controller appends four-byte events to the event buffer, a ring of
//! [`EVENT_BYTES`], and counts the bytes in `GEVNTCOUNT`; the interrupt
//! is asserted while that count is not zero. [`Controller::on_interrupt`]
//! reads the count, invalidates that many bytes from where it last
//! stopped, handles each event, and writes the count back, which gives the
//! space back and drops the interrupt. The whole buffer is a page the
//! processor never writes, so the controller's writes cannot be undone by
//! a line the processor wrote back.
//!
//! # Endpoint commands
//!
//! Each physical endpoint has three parameter registers and `DEPCMD`.
//! A command is started by writing it with `CMDACT` set, and has ended
//! when the controller clears `CMDACT`; its status is then in bits 12-15,
//! and Start Transfer's resource index in bits 16-22, which End Transfer
//! must be given back. The wait is bounded.

use ferrix_usb_device::{Function, Speed};

use crate::endpoint::Endpoint;
use crate::ep0::Ep0;
use crate::event::{DeviceEvent, EndpointEvent, Event, LINK_U3};
use crate::layout::{AREA_BYTES, EVENT_BYTES, EVENTS, MAX_ENDPOINTS, PAGE};
use crate::regs::{
    DALEPENA, DCFG, DCFG_DEVADDR_MASK, DCFG_HIGHSPEED, DCFG_LPM_CAP, DCFG_SPEED_MASK, DCTL,
    DCTL_CSFTRST, DCTL_HIRD_THRES_MASK, DCTL_KEEP_CONNECT, DCTL_L1_HIBER_EN, DCTL_RUN_STOP,
    DCTL_U1_U2, DCTL_ULSTCHNGREQ_MASK, DEPCFG_ACTION_MODIFY, DEPCFG_EP_NUMBER_SHIFT,
    DEPCFG_EP_TYPE_SHIFT, DEPCFG_MAX_PACKET_SHIFT, DEPCFG_XFER_COMPLETE_EN,
    DEPCFG_XFER_NOT_READY_EN, DEPCMD, DEPCMD_CMDACT, DEPCMD_CMDIOC, DEPCMD_DEPSTARTCFG,
    DEPCMD_ENDTRANSFER, DEPCMD_HIPRI_FORCERM, DEPCMD_PARAM_SHIFT, DEPCMD_SETEPCONFIG,
    DEPCMD_SETTRANSFRESOURCE, DEPCMD_STARTTRANSFER, DEPCMD_STATUS_SHIFT, DEPCMDPAR0, DEPCMDPAR1,
    DEPCMDPAR2, DEVTEN, DEVTEN_CONNECTDONEEN, DEVTEN_DISCONNEVTEN, DEVTEN_ERRTICERREN,
    DEVTEN_EVNTOVERFLOWEN, DEVTEN_U3L2L1SUSPEN, DEVTEN_USBRSTEN, DEVTEN_WKUPEVTEN, DSTS,
    DSTS_CONNECTSPD, DSTS_DEVCTRLHLT, DSTS_FULLSPEED, DSTS_HIGHSPEED, EP_TYPE_CONTROL,
    FRAME_LENGTH_ADJUSTMENT, GCTL, GCTL_PRTCAP_DEVICE, GCTL_PRTCAPDIR_MASK, GEVNTADRHI, GEVNTADRLO,
    GEVNTCOUNT, GEVNTCOUNT_MASK, GEVNTSIZ, GFLADJ, GFLADJ_30MHZ_MASK, GFLADJ_30MHZ_SDBND_SEL,
    GSNPSID, GSTS, GSTS_CSR_TIMEOUT, GSTS_CURMOD_DEVICE, GSTS_CURMOD_MASK, GUSB2PHYCFG,
    GUSB2PHYCFG_ENBLSLPM, GUSB2PHYCFG_SUSPHY, GUSB2PHYCFG_U2_FREECLK_EXISTS, ID_DWC3, ID_DWC31,
    ID_DWC32, depcmd_base,
};
use crate::{Clock, Dma, Error, MICROSECOND, MILLISECOND, Parts, Registers, wait_for};

/// How long the core's soft reset may take. `DWC_usb31` from 1.90a on
/// clears `CSFTRST` only once every clock is synchronised, which Linux
/// allows 200 ms for; this allows more.
const RESET_PATIENCE: u64 = 500 * MILLISECOND;
/// How long `DWC_usb31` up to 1.80a needs after the reset before its PHY
/// side may be touched. Which release the phone has is only known once it
/// runs, and the wait is cheap, so it is always taken.
const RESET_SETTLE: u64 = 50 * MILLISECOND;
/// How long the controller may take to start or halt.
const RUN_STOP_PATIENCE: u64 = 500 * MILLISECOND;
/// How long an endpoint command may take. Linux polls 5000 times, a few
/// hundred nanoseconds each.
const COMMAND_PATIENCE: u64 = 10 * MILLISECOND;
/// How long End Transfer without an event is given, as Linux gives it.
const END_TRANSFER_SETTLE: u64 = MILLISECOND;
/// How often a wait looks at a register.
const POLL_STEP: u64 = MILLISECOND;
/// How often a command's wait looks.
const COMMAND_STEP: u64 = MICROSECOND;
/// How many times one interrupt reads `GEVNTCOUNT` before it leaves the
/// rest for the next: events keep coming while earlier ones are handled.
const EVENT_ROUNDS: usize = 8;

/// The device events enabled: everything but start of frame and the
/// generic command's completion.
const DEVICE_EVENTS: u32 = DEVTEN_DISCONNEVTEN
    | DEVTEN_USBRSTEN
    | DEVTEN_CONNECTDONEEN
    | DEVTEN_WKUPEVTEN
    | DEVTEN_U3L2L1SUSPEN
    | DEVTEN_ERRTICERREN
    | DEVTEN_EVNTOVERFLOWEN;

/// What an interrupt found, for the program to act on.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Notice {
    /// The host reset the bus: nothing is configured any more.
    pub reset: bool,
    /// The link came up after a reset, at this speed.
    pub connected: Option<Speed>,
    /// The host went away: nothing is configured any more.
    pub disconnected: bool,
    /// The host set a configuration, or took it away.
    pub configuration: bool,
    /// An OUT endpoint has bytes to read.
    pub received: bool,
    /// An IN transfer finished: there is room to write.
    pub sent: bool,
    /// The link was suspended.
    pub suspended: bool,
}

/// The controller, running, and its memory.
#[derive(Debug)]
pub struct Controller<R, D, C> {
    pub(crate) registers: R,
    pub(crate) memory: D,
    pub(crate) clock: C,
    /// Where the next event is, in the event buffer.
    event_position: usize,
    /// The speed the link runs at, once connected.
    speed: Option<Speed>,
    pub(crate) ep0: Ep0,
    pub(crate) endpoints: [Endpoint; MAX_ENDPOINTS],
    /// The IN endpoints, by address, whose transfers of whole packets end
    /// without a zero-length packet: a bit per endpoint number.
    pub(crate) no_zero_packets: u16,
}

impl<R: Registers, D: Dma, C: Clock> Controller<R, D, C> {
    /// Reset the core, lay its memory out and connect to the bus, ready for
    /// the host to reset it and enumerate.
    ///
    /// # Errors
    ///
    /// The error and the parts. The memory is the controller's to write for
    /// as long as it may be running, and it may be after any error but
    /// [`Error::Memory`], [`Error::NotDwc3`] and [`Error::Refused`], which
    /// are found before anything is written; after the others the memory
    /// must be kept.
    pub fn start(parts: Parts<R, D, C>) -> Result<Self, (Error, Parts<R, D, C>)> {
        let Parts {
            registers,
            memory,
            clock,
        } = parts;
        let mut controller = Controller {
            registers,
            memory,
            clock,
            event_position: 0,
            speed: None,
            ep0: Ep0::new(),
            endpoints: [Endpoint::new(); MAX_ENDPOINTS],
            no_zero_packets: 0,
        };
        match controller.bring_up() {
            Ok(()) => Ok(controller),
            Err(error) => Err((error, controller.into_parts())),
        }
    }

    /// The parts, whatever state the controller is in.
    #[must_use]
    pub fn into_parts(self) -> Parts<R, D, C> {
        Parts {
            registers: self.registers,
            memory: self.memory,
            clock: self.clock,
        }
    }

    /// The speed the link runs at, once connected.
    #[must_use]
    pub const fn speed(&self) -> Option<Speed> {
        self.speed
    }

    /// The clock, for the program's own waits.
    pub fn clock(&mut self) -> &mut C {
        &mut self.clock
    }

    fn bring_up(&mut self) -> Result<(), Error> {
        self.check_memory()?;
        let id = self.registers.read32(GSNPSID);
        if !matches!(id >> 16, ID_DWC3 | ID_DWC31 | ID_DWC32) {
            return Err(Error::NotDwc3(id));
        }
        self.check_state()?;
        self.soft_reset()?;
        self.store(GSTS, GSTS_CSR_TIMEOUT);
        self.configure_core();
        self.clear_memory();
        self.setup_event_buffer()?;
        let _ = self.command(0, DEPCMD_DEPSTARTCFG, [0; 3])?;
        self.configure_ep0(64, false)?;
        self.store(DALEPENA, 0b11);
        self.ep0_start_setup()?;
        self.store(DEVTEN, DEVICE_EVENTS);
        self.run()
    }

    /// Every page where the controller can reach it, and page-aligned
    /// there: nothing in the layout crosses a page, so that is all it needs.
    fn check_memory(&self) -> Result<(), Error> {
        let aligned = |page: usize| {
            self.memory
                .device_address(page)
                .is_some_and(|address| address.is_multiple_of(PAGE as u64))
        };
        if self.memory.len() < AREA_BYTES || !(0..AREA_BYTES).step_by(PAGE).all(aligned) {
            return Err(Error::Memory);
        }
        Ok(())
    }

    /// Refuse, having written nothing, a controller that is not in device
    /// mode, stopped and halted.
    fn check_state(&self) -> Result<(), Error> {
        let status = self.load(GSTS);
        if status & GSTS_CURMOD_MASK != GSTS_CURMOD_DEVICE {
            return Err(Error::Refused {
                register: GSTS,
                value: status,
            });
        }
        let control = self.load(DCTL);
        if control & DCTL_RUN_STOP != 0 {
            return Err(Error::Refused {
                register: DCTL,
                value: control,
            });
        }
        let device_status = self.load(DSTS);
        if device_status & DSTS_DEVCTRLHLT == 0 {
            return Err(Error::Refused {
                register: DSTS,
                value: device_status,
            });
        }
        Ok(())
    }

    /// The core's soft reset, with the run bit off.
    fn soft_reset(&mut self) -> Result<(), Error> {
        let control = (self.load(DCTL) | DCTL_CSFTRST) & !DCTL_RUN_STOP;
        self.write_dctl(control);
        let registers = &self.registers;
        if !wait_for(&mut self.clock, RESET_PATIENCE, POLL_STEP, || {
            registers.read32(DCTL) & DCTL_CSFTRST == 0
        }) {
            return Err(Error::Reset);
        }
        self.clock.sleep_nanos(RESET_SETTLE);
        Ok(())
    }

    /// Device mode, the PHY interface, the frame length, and the speed,
    /// address and power management settings.
    fn configure_core(&mut self) {
        let control = self.load(GCTL) & !GCTL_PRTCAPDIR_MASK;
        self.store(GCTL, control | GCTL_PRTCAP_DEVICE);

        let phy = self.load(GUSB2PHYCFG);
        let off = GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM | GUSB2PHYCFG_U2_FREECLK_EXISTS;
        self.store(GUSB2PHYCFG, phy & !off);

        let adjustment = self.load(GFLADJ);
        if adjustment & GFLADJ_30MHZ_MASK != FRAME_LENGTH_ADJUSTMENT {
            let adjusted = (adjustment & !GFLADJ_30MHZ_MASK)
                | GFLADJ_30MHZ_SDBND_SEL
                | FRAME_LENGTH_ADJUSTMENT;
            self.store(GFLADJ, adjusted);
        }

        let configuration = self.load(DCFG) & !(DCFG_SPEED_MASK | DCFG_DEVADDR_MASK | DCFG_LPM_CAP);
        self.store(DCFG, configuration | DCFG_HIGHSPEED);

        let control = self.load(DCTL)
            & !(DCTL_U1_U2 | DCTL_HIRD_THRES_MASK | DCTL_L1_HIBER_EN | DCTL_KEEP_CONNECT);
        self.write_dctl(control & !DCTL_RUN_STOP);
    }

    /// Zero the area and hand all of it over: nothing the controller will
    /// write may be in a line the processor has dirty.
    fn clear_memory(&mut self) {
        for offset in (0..AREA_BYTES).step_by(4) {
            self.memory.write32(offset, 0);
        }
        self.memory.clean(0, AREA_BYTES);
    }

    fn setup_event_buffer(&mut self) -> Result<(), Error> {
        let address = self.address(EVENTS)?;
        self.store(GEVNTADRLO, address as u32);
        self.store(GEVNTADRHI, (address >> 32) as u32);
        self.store(GEVNTSIZ, EVENT_BYTES as u32);
        let stale = self.load(GEVNTCOUNT) & GEVNTCOUNT_MASK;
        self.store(GEVNTCOUNT, stale);
        self.event_position = 0;
        Ok(())
    }

    /// `DEPCFG` for both directions of endpoint 0, with `max_packet`; and,
    /// the first time, one transfer resource each.
    pub(crate) fn configure_ep0(&mut self, max_packet: u16, modify: bool) -> Result<(), Error> {
        for physical in 0..2 {
            let mut parameter0 = (EP_TYPE_CONTROL << DEPCFG_EP_TYPE_SHIFT)
                | (u32::from(max_packet) << DEPCFG_MAX_PACKET_SHIFT);
            if modify {
                parameter0 |= DEPCFG_ACTION_MODIFY;
            }
            let parameter1 = DEPCFG_XFER_COMPLETE_EN
                | DEPCFG_XFER_NOT_READY_EN
                | (u32::from(physical) << DEPCFG_EP_NUMBER_SHIFT);
            let _ = self.command(physical, DEPCMD_SETEPCONFIG, [parameter0, parameter1, 0])?;
            if !modify {
                let _ = self.command(physical, DEPCMD_SETTRANSFRESOURCE, [1, 0, 0])?;
            }
        }
        self.ep0.max_packet = usize::from(max_packet);
        Ok(())
    }

    /// Set the run bit and wait for the controller to leave its halt.
    fn run(&mut self) -> Result<(), Error> {
        let control = (self.load(DCTL) | DCTL_RUN_STOP) & !DCTL_KEEP_CONNECT;
        self.write_dctl(control);
        let registers = &self.registers;
        if wait_for(&mut self.clock, RUN_STOP_PATIENCE, POLL_STEP, || {
            registers.read32(DSTS) & DSTS_DEVCTRLHLT == 0
        }) {
            Ok(())
        } else {
            Err(Error::RunStop)
        }
    }

    /// End every transfer, disconnect from the bus and wait for the
    /// controller to halt, taking the events it writes meanwhile: it does
    /// not halt with events counted.
    ///
    /// # Errors
    ///
    /// [`Error::RunStop`] if it would not halt. It may then still write
    /// the memory, which must be kept.
    pub fn stop(&mut self) -> Result<(), Error> {
        self.end_all_transfers();
        let control = self.load(DCTL) & !DCTL_RUN_STOP;
        self.write_dctl(control);
        let registers = &mut self.registers;
        let mut position = self.event_position;
        let halted = wait_for(&mut self.clock, RUN_STOP_PATIENCE, POLL_STEP, || {
            let count = registers.read32(GEVNTCOUNT) & GEVNTCOUNT_MASK;
            if count != 0 {
                registers.write32(GEVNTCOUNT, count);
                position = (position + count as usize) % EVENT_BYTES;
            }
            registers.read32(DSTS) & DSTS_DEVCTRLHLT != 0
        });
        self.event_position = position;
        self.store(DEVTEN, 0);
        self.speed = None;
        if halted { Ok(()) } else { Err(Error::RunStop) }
    }

    /// End endpoint 0's transfers and every other endpoint's, without
    /// waiting for their events: the controller is being reset or stopped.
    fn end_all_transfers(&mut self) {
        for physical in 0..2 {
            if let Some(resource) = self.ep0.active_resource(physical) {
                self.end_transfer_now(physical, resource);
            }
        }
        self.ep0.forget();
        self.deactivate_endpoints();
    }

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    /// Handle every event the controller has written, answering the host's
    /// requests through `function`.
    ///
    /// # Errors
    ///
    /// The first error an event's handling met; the events after it are
    /// still handled and taken. [`Error::Events`] when the count is past
    /// the buffer, which only a controller gone wrong writes.
    pub fn on_interrupt<F: Function>(&mut self, function: &mut F) -> Result<Notice, Error> {
        let mut notice = Notice::default();
        let mut first_error = None;
        for _ in 0..EVENT_ROUNDS {
            let count = self.load(GEVNTCOUNT) & GEVNTCOUNT_MASK;
            if count == 0 {
                break;
            }
            if count as usize > EVENT_BYTES {
                return Err(Error::Events(count));
            }
            self.invalidate_events(count as usize);
            for _ in 0..count / 4 {
                let raw = self.memory.read32(EVENTS + self.event_position);
                self.event_position = (self.event_position + 4) % EVENT_BYTES;
                if let Err(error) = self.dispatch(Event::decode(raw), function, &mut notice) {
                    first_error = first_error.or(Some(error));
                }
            }
            self.store(GEVNTCOUNT, count);
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(notice),
        }
    }

    /// Invalidate `count` bytes of events from where the last read stopped,
    /// in two pieces if they wrap.
    fn invalidate_events(&mut self, count: usize) {
        let first = count.min(EVENT_BYTES - self.event_position);
        self.memory.invalidate(EVENTS + self.event_position, first);
        if first < count {
            self.memory.invalidate(EVENTS, count - first);
        }
    }

    fn dispatch<F: Function>(
        &mut self,
        event: Event,
        function: &mut F,
        notice: &mut Notice,
    ) -> Result<(), Error> {
        match event {
            Event::Endpoint {
                physical: physical @ 0..=1,
                kind,
                status,
                ..
            } => self.ep0_event(physical, kind, status, function, notice),
            Event::Endpoint {
                physical,
                kind: EndpointEvent::XferComplete,
                ..
            } => self.endpoint_complete(physical, notice),
            Event::Endpoint {
                physical,
                kind: EndpointEvent::CommandComplete,
                parameter,
                ..
            } => self.endpoint_command_complete(physical, parameter),
            Event::Device(device) => self.device_event(device, function, notice),
            Event::Endpoint { .. } | Event::Other(_) => Ok(()),
        }
    }

    fn device_event<F: Function>(
        &mut self,
        event: DeviceEvent,
        function: &mut F,
        notice: &mut Notice,
    ) -> Result<(), Error> {
        match event {
            DeviceEvent::Reset => {
                notice.reset = true;
                self.bus_reset(function)
            }
            DeviceEvent::Disconnect => {
                notice.disconnected = true;
                self.speed = None;
                self.bus_reset(function)
            }
            DeviceEvent::ConnectDone => self.connect_done(function, notice),
            DeviceEvent::Suspend(state) | DeviceEvent::LinkStateChange(state) => {
                notice.suspended |= state == LINK_U3;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// A reset or a disconnect: endpoint 0 back to waiting for a SETUP,
    /// every other endpoint ended and disabled, address 0, and the
    /// function back to its default state.
    fn bus_reset<F: Function>(&mut self, function: &mut F) -> Result<(), Error> {
        let result = self.ep0_reset_state();
        self.deactivate_endpoints();
        let configuration = self.load(DCFG) & !DCFG_DEVADDR_MASK;
        self.store(DCFG, configuration);
        function.reset();
        result
    }

    /// The link is up: at the speed `DSTS` says, with endpoint 0's packet
    /// size to match, and LPM left off.
    fn connect_done<F: Function>(
        &mut self,
        function: &mut F,
        notice: &mut Notice,
    ) -> Result<(), Error> {
        let (speed, max_packet) = match self.load(DSTS) & DSTS_CONNECTSPD {
            DSTS_HIGHSPEED => (Speed::High, 64),
            DSTS_FULLSPEED => (Speed::Full, 64),
            // SuperSpeed, which DCFG does not allow: say high speed, with
            // SuperSpeed's control packets.
            _ => (Speed::High, 512),
        };
        let configuration = self.load(DCFG) & !DCFG_LPM_CAP;
        self.store(DCFG, configuration);
        let control = self.load(DCTL) & !DCTL_HIRD_THRES_MASK;
        self.write_dctl(control);
        self.configure_ep0(max_packet, true)?;
        self.speed = Some(speed);
        function.set_speed(speed);
        notice.connected = Some(speed);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    /// Run an endpoint command and wait for it: `DEPCMD` as it ended.
    pub(crate) fn command(
        &mut self,
        physical: u8,
        command: u32,
        parameters: [u32; 3],
    ) -> Result<u32, Error> {
        self.issue(physical, command | DEPCMD_CMDACT, parameters);
        let base = depcmd_base(physical);
        let registers = &self.registers;
        let mut ended = 0;
        let done = wait_for(&mut self.clock, COMMAND_PATIENCE, COMMAND_STEP, || {
            ended = registers.read32(base + DEPCMD);
            ended & DEPCMD_CMDACT == 0
        });
        let code = (command & 0xF) as u8;
        if !done {
            return Err(Error::Command {
                physical,
                command: code,
                status: None,
            });
        }
        let status = ((ended >> DEPCMD_STATUS_SHIFT) & 0xF) as u8;
        if status != 0 {
            return Err(Error::Command {
                physical,
                command: code,
                status: Some(status),
            });
        }
        Ok(ended)
    }

    /// Write a command's parameters, then the command.
    fn issue(
        &mut self,
        physical: u8,
        command: u32,
        [parameter0, parameter1, parameter2]: [u32; 3],
    ) {
        let base = depcmd_base(physical);
        self.store(base + DEPCMDPAR0, parameter0);
        self.store(base + DEPCMDPAR1, parameter1);
        self.store(base + DEPCMDPAR2, parameter2);
        self.store(base + DEPCMD, command);
    }

    /// Start a transfer at the TRB at `trb`, which the caller has written
    /// and cleaned: the resource index it got.
    pub(crate) fn start_transfer(&mut self, physical: u8, trb: usize) -> Result<u8, Error> {
        let address = self.address(trb)?;
        let parameters = [(address >> 32) as u32, address as u32, 0];
        let ended = self.command(physical, DEPCMD_STARTTRANSFER, parameters)?;
        Ok(((ended >> DEPCMD_PARAM_SHIFT) & 0x7F) as u8)
    }

    /// End the transfer with `resource`, with an endpoint command complete
    /// event to say when it has.
    pub(crate) fn end_transfer(&mut self, physical: u8, resource: u8) -> Result<(), Error> {
        let command = DEPCMD_ENDTRANSFER
            | DEPCMD_CMDIOC
            | DEPCMD_HIPRI_FORCERM
            | (u32::from(resource) << DEPCMD_PARAM_SHIFT);
        self.command(physical, command, [0; 3]).map(|_| ())
    }

    /// End the transfer with `resource` and give it the millisecond Linux
    /// gives it, with no event and no wait on `CMDACT`: for a reset,
    /// where nothing will be started on the endpoint until it is configured
    /// again.
    pub(crate) fn end_transfer_now(&mut self, physical: u8, resource: u8) {
        let command = DEPCMD_ENDTRANSFER
            | DEPCMD_HIPRI_FORCERM
            | DEPCMD_CMDACT
            | (u32::from(resource) << DEPCMD_PARAM_SHIFT);
        self.issue(physical, command, [0; 3]);
        self.clock.sleep_nanos(END_TRANSFER_SETTLE);
    }

    // -----------------------------------------------------------------------
    // Registers and memory
    // -----------------------------------------------------------------------

    pub(crate) fn load(&self, offset: u32) -> u32 {
        self.registers.read32(offset)
    }

    pub(crate) fn store(&mut self, offset: u32, value: u32) {
        self.registers.write32(offset, value);
    }

    /// Write `DCTL` without asking for a link state change.
    fn write_dctl(&mut self, value: u32) {
        self.store(DCTL, value & !DCTL_ULSTCHNGREQ_MASK);
    }

    /// The address the controller reaches `offset` of the area by.
    pub(crate) fn address(&self, offset: usize) -> Result<u64, Error> {
        self.memory.device_address(offset).ok_or(Error::Memory)
    }
}
