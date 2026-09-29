//! Endpoint 0: control transfers, a stage at a time (the DWC3 programming
//! model Linux's `ep0.c` follows).
//!
//! Physical endpoint 0 is the control endpoint's OUT direction and 1 its
//! IN. They share the two TRBs at [`EP0_TRBS`], because only one stage is
//! ever in the controller's hands:
//!
//! 1. **Setup.** A `CONTROL_SETUP` TRB for eight bytes on physical 0. Its
//!    transfer complete event brings the packet, which the function
//!    answers.
//! 2. **Data**, if `wLength` is not zero: started at once, on physical 1 for
//!    IN and 0 for OUT, rather than on the transfer-not-ready event for the
//!    data stage, which the controller does not write if the host sends
//!    another SETUP instead. An IN stage shorter than `wLength` that ends
//!    on a full packet gets a chained zero-length TRB, so the host sees it
//!    end. An OUT stage's TRB is a whole number of packets, as the
//!    controller requires, and the function is given what arrived.
//! 3. **Status**: started on the transfer-not-ready event for the status
//!    stage, on the endpoint the event names, as `CONTROL_STATUS2` with no
//!    data stage and `CONTROL_STATUS3` after one. Its transfer complete
//!    ends the request, and a SETUP TRB is started for the next.
//!
//! A request the function refuses is answered with Set Stall on physical 0
//! and a SETUP TRB started straight after; the controller clears the stall
//! when the next SETUP arrives. What a request does to the controller --
//! the address in `DCFG`, which the databook has written before the status
//! stage; endpoints configured; a halt -- is done before its status stage
//! is started, so the host does not see the request done before it is.

use ferrix_usb_device::{Effect, Function, Reply, Setup, Status};

use crate::controller::{Controller, Notice};
use crate::event::{CONTROL_DATA, CONTROL_STATUS, EndpointEvent};
use crate::layout::{EP0_BYTES, EP0_DATA, EP0_TRBS, SETUP};
use crate::regs::{DCFG, DCFG_DEVADDR_MASK, DCFG_DEVADDR_SHIFT, DEPCMD_SETSTALL};
use crate::trb::{
    CHN, CONTROL_DATA as TRB_DATA, CONTROL_SETUP, CONTROL_STATUS2, CONTROL_STATUS3, IOC, ISP_IMI,
    LST, STATUS_SETUP_PENDING, TRB_BYTES, Trb,
};
use crate::{Clock, Dma, Error, Registers};

/// Where a control transfer is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Phase {
    /// A SETUP TRB is started, waiting for a packet.
    Setup,
    /// A data stage's TRB is started.
    Data,
    /// Waiting for the host to reach the status stage.
    AwaitStatus,
    /// A status stage's TRB is started.
    Status,
}

/// What the function's answer comes to, once any IN data is in memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Plan {
    In(usize),
    Out(usize),
    Ack(Effect),
    Stall,
}

/// Endpoint 0's state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ep0 {
    phase: Phase,
    /// Whether the request has a data stage.
    three_stage: bool,
    /// Whether that stage is IN.
    data_in: bool,
    /// An OUT stage: the bytes the function asked for, and the TRB's size.
    out_length: usize,
    trb_length: usize,
    /// A SETUP arrived before the stage could finish.
    setup_pending: bool,
    /// The resource index of the transfer started on each direction.
    active: [Option<u8>; 2],
    /// The control packet size, from connect done.
    pub(crate) max_packet: usize,
}

impl Ep0 {
    pub(crate) const fn new() -> Self {
        Ep0 {
            phase: Phase::Setup,
            three_stage: false,
            data_in: false,
            out_length: 0,
            trb_length: 0,
            setup_pending: false,
            active: [None, None],
            max_packet: 64,
        }
    }

    /// The resource index of the transfer on `physical`, 0 or 1.
    pub(crate) fn active_resource(&self, physical: u8) -> Option<u8> {
        self.active.get(usize::from(physical)).copied().flatten()
    }

    /// Forget the transfers, which have been ended.
    pub(crate) fn forget(&mut self) {
        self.active = [None, None];
        self.phase = Phase::Setup;
    }

    fn set_active(&mut self, physical: u8, resource: Option<u8>) {
        if let Some(slot) = self.active.get_mut(usize::from(physical)) {
            *slot = resource;
        }
    }
}

impl<R: Registers, D: Dma, C: Clock> Controller<R, D, C> {
    /// An event on physical endpoint 0 or 1.
    pub(crate) fn ep0_event<F: Function>(
        &mut self,
        physical: u8,
        kind: EndpointEvent,
        status: u8,
        function: &mut F,
        notice: &mut Notice,
    ) -> Result<(), Error> {
        match kind {
            EndpointEvent::XferComplete => {
                self.ep0.set_active(physical, None);
                match self.ep0.phase {
                    Phase::Setup => self.ep0_setup(function, notice),
                    Phase::Data => self.ep0_data_done(function),
                    Phase::Status => self.ep0_start_setup(),
                    Phase::AwaitStatus => Ok(()),
                }
            }
            EndpointEvent::XferNotReady => match status & 3 {
                CONTROL_DATA => self.ep0_data_not_ready(physical),
                CONTROL_STATUS => self.ep0_status(physical),
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    }

    /// Give the controller a TRB for the next SETUP packet.
    pub(crate) fn ep0_start_setup(&mut self) -> Result<(), Error> {
        let buffer = self.address(SETUP)?;
        self.ep0_take_trbs();
        Trb::new(buffer, 8, CONTROL_SETUP, LST | IOC | ISP_IMI).write(&mut self.memory, EP0_TRBS);
        self.memory.clean(EP0_TRBS, TRB_BYTES);
        let resource = self.start_transfer(0, EP0_TRBS)?;
        self.ep0 = Ep0 {
            active: [Some(resource), None],
            ..Ep0::new()
        }
        .with_packet(self.ep0.max_packet);
        Ok(())
    }

    /// Take endpoint 0's TRBs back before rewriting them: the controller
    /// wrote the last one back when it finished it, and a line the
    /// processor still held from before would, cleaned, write its old
    /// bytes over the controller's.
    fn ep0_take_trbs(&mut self) {
        self.memory.invalidate(EP0_TRBS, 2 * TRB_BYTES);
    }

    /// A SETUP arrived: ask the function, and start what it answered.
    fn ep0_setup<F: Function>(
        &mut self,
        function: &mut F,
        notice: &mut Notice,
    ) -> Result<(), Error> {
        self.memory.invalidate(SETUP, 8);
        let mut bytes = [0; 8];
        self.memory.read_bytes(SETUP, &mut bytes);
        let setup = Setup::parse(bytes);
        let length = usize::from(setup.length);
        let plan = match function.setup(&setup) {
            Reply::In(data) => Plan::In(self.ep0_fill(data, length)),
            Reply::Out(wanted) => Plan::Out(wanted.min(length).min(EP0_BYTES)),
            Reply::Ack(effect) => Plan::Ack(effect),
            Reply::Stall => Plan::Stall,
        };
        match plan {
            _ if length == 0 && plan != Plan::Stall => {
                let effect = match plan {
                    Plan::Ack(effect) => effect,
                    _ => Effect::None,
                };
                self.ep0_effect(effect, function, notice)
            }
            Plan::In(filled) if setup.is_in() => self.ep0_data_in(filled, length),
            Plan::Out(wanted) if !setup.is_in() && wanted > 0 => self.ep0_data_out(wanted),
            _ => self.ep0_stall_and_restart(),
        }
    }

    /// Copy an IN stage's bytes into the data buffer, and clean them: how
    /// many.
    fn ep0_fill(&mut self, data: &[u8], length: usize) -> usize {
        let filled = data.len().min(length).min(EP0_BYTES);
        let (data, _) = data.split_at(filled);
        // The controller may have written here in an OUT stage since.
        self.memory.invalidate(EP0_DATA, filled);
        self.memory.write_bytes(EP0_DATA, data);
        self.memory.clean(EP0_DATA, filled);
        filled
    }

    /// Do what a request with no data stage asked, then wait for the status
    /// stage; stall it if that failed.
    fn ep0_effect<F: Function>(
        &mut self,
        effect: Effect,
        function: &mut F,
        notice: &mut Notice,
    ) -> Result<(), Error> {
        let done = match effect {
            Effect::None => Ok(()),
            Effect::Address(address) => {
                let configuration = self.load(DCFG) & !DCFG_DEVADDR_MASK;
                self.store(
                    DCFG,
                    configuration | (u32::from(address) << DCFG_DEVADDR_SHIFT),
                );
                Ok(())
            }
            Effect::Configure(value) => {
                notice.configuration = true;
                self.configure(value, function)
            }
            Effect::Halt { endpoint, halted } => self.halt(endpoint, halted),
        };
        if let Err(error) = done {
            self.ep0_stall_and_restart()?;
            return Err(error);
        }
        self.ep0.phase = Phase::AwaitStatus;
        self.ep0.three_stage = false;
        Ok(())
    }

    /// Start an IN data stage of `filled` bytes, for a request that asked
    /// for `length`.
    fn ep0_data_in(&mut self, filled: usize, length: usize) -> Result<(), Error> {
        let buffer = self.address(EP0_DATA)?;
        let short_of_asked = filled < length;
        let zero_packet =
            filled > 0 && short_of_asked && filled.is_multiple_of(self.ep0.max_packet);
        self.ep0_take_trbs();
        if zero_packet {
            Trb::new(buffer, filled, TRB_DATA, CHN | ISP_IMI).write(&mut self.memory, EP0_TRBS);
            Trb::new(buffer, 0, TRB_DATA, LST | IOC | ISP_IMI)
                .write(&mut self.memory, EP0_TRBS + TRB_BYTES);
            self.memory.clean(EP0_TRBS, 2 * TRB_BYTES);
        } else {
            Trb::new(buffer, filled, TRB_DATA, LST | IOC | ISP_IMI)
                .write(&mut self.memory, EP0_TRBS);
            self.memory.clean(EP0_TRBS, TRB_BYTES);
        }
        let resource = self.start_transfer(1, EP0_TRBS)?;
        self.ep0.set_active(1, Some(resource));
        self.ep0.phase = Phase::Data;
        self.ep0.three_stage = true;
        self.ep0.data_in = true;
        Ok(())
    }

    /// Start an OUT data stage for `wanted` bytes, in a TRB of whole
    /// packets.
    fn ep0_data_out(&mut self, wanted: usize) -> Result<(), Error> {
        let buffer = self.address(EP0_DATA)?;
        let packet = self.ep0.max_packet.max(1);
        let trb_length = wanted
            .div_ceil(packet)
            .saturating_mul(packet)
            .min(EP0_BYTES);
        self.ep0_take_trbs();
        Trb::new(buffer, trb_length, TRB_DATA, LST | IOC | ISP_IMI)
            .write(&mut self.memory, EP0_TRBS);
        self.memory.clean(EP0_TRBS, TRB_BYTES);
        let resource = self.start_transfer(0, EP0_TRBS)?;
        self.ep0.set_active(0, Some(resource));
        self.ep0.phase = Phase::Data;
        self.ep0.three_stage = true;
        self.ep0.data_in = false;
        self.ep0.out_length = wanted;
        self.ep0.trb_length = trb_length;
        Ok(())
    }

    /// The data stage is over: hand an OUT stage's bytes to the function,
    /// and wait for the status stage.
    fn ep0_data_done<F: Function>(&mut self, function: &mut F) -> Result<(), Error> {
        self.memory.invalidate(EP0_TRBS, 2 * TRB_BYTES);
        let trb = Trb::read(&self.memory, EP0_TRBS);
        self.ep0.setup_pending = trb.status() == STATUS_SETUP_PENDING;
        self.ep0.phase = Phase::AwaitStatus;
        if self.ep0.data_in || self.ep0.setup_pending {
            return Ok(());
        }
        let received = self
            .ep0
            .trb_length
            .saturating_sub(trb.remaining())
            .min(self.ep0.out_length);
        self.memory.invalidate(EP0_DATA, received);
        let mut bytes = [0; EP0_BYTES];
        let (data, _) = bytes.split_at_mut(received);
        self.memory.read_bytes(EP0_DATA, data);
        match function.out_data(data) {
            Status::Ack => Ok(()),
            Status::Stall => self.ep0_stall_and_restart(),
        }
    }

    /// The host wants a data stage: on the wrong endpoint, it is not the
    /// one the request said, and the request is stalled.
    fn ep0_data_not_ready(&mut self, physical: u8) -> Result<(), Error> {
        if self.ep0.phase == Phase::Data && (physical == 1) != self.ep0.data_in {
            return self.ep0_stall_and_restart();
        }
        Ok(())
    }

    /// The host is in the status stage: start it on `physical`, unless a
    /// SETUP overtook the data stage, when the request is stalled.
    fn ep0_status(&mut self, physical: u8) -> Result<(), Error> {
        if self.ep0.phase != Phase::AwaitStatus {
            return Ok(());
        }
        if self.ep0.setup_pending {
            return self.ep0_stall_and_restart();
        }
        let buffer = self.address(SETUP)?;
        let kind = if self.ep0.three_stage {
            CONTROL_STATUS3
        } else {
            CONTROL_STATUS2
        };
        self.ep0_take_trbs();
        Trb::new(buffer, 0, kind, LST | IOC | ISP_IMI).write(&mut self.memory, EP0_TRBS);
        self.memory.clean(EP0_TRBS, TRB_BYTES);
        let resource = self.start_transfer(physical, EP0_TRBS)?;
        self.ep0.set_active(physical, Some(resource));
        self.ep0.phase = Phase::Status;
        Ok(())
    }

    /// Stall the request: end whatever stage is started, Set Stall, and
    /// wait for the next SETUP.
    pub(crate) fn ep0_stall_and_restart(&mut self) -> Result<(), Error> {
        for physical in 0..2 {
            if let Some(resource) = self.ep0.active_resource(physical) {
                self.ep0.set_active(physical, None);
                self.end_transfer(physical, resource)?;
            }
        }
        let _ = self.command(0, DEPCMD_SETSTALL, [0; 3])?;
        self.ep0_start_setup()
    }

    /// After a reset or disconnect: a request cut off mid-way is stalled
    /// and endpoint 0 made ready for a SETUP again; one waiting for its
    /// SETUP is left.
    pub(crate) fn ep0_reset_state(&mut self) -> Result<(), Error> {
        if self.ep0.phase == Phase::Setup {
            return Ok(());
        }
        self.ep0_stall_and_restart()
    }
}

impl Ep0 {
    const fn with_packet(mut self, max_packet: usize) -> Self {
        self.max_packet = max_packet;
        self
    }
}
