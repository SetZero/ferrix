//! The endpoints a configuration enables: bulk and interrupt, IN and OUT.
//!
//! # One transfer at a time
//!
//! Each endpoint has one transfer in the controller's hands at most, of a
//! data TRB and, for a bulk IN transfer that is a whole number of packets,
//! a chained zero-length one after it; the last TRB has LST and IOC, so the
//! transfer ends in a transfer complete event and the next is started
//! with Start Transfer. That is the older of the two ways the DWC3 is
//! driven -- U-Boot's port still does it -- rather than Linux's endless
//! ring of TRBs closed by a link TRB and fed by Update Transfer. It needs
//! no transfer-in-progress bookkeeping, every hand-off is one clean before
//! one command, and a console's bytes do not need more: at high speed a
//! transfer of a page takes some tens of microseconds.
//!
//! # IN
//!
//! [`Controller::write`] copies into the endpoint's page, a ring, and
//! starts a transfer of what is waiting if none is running: up to the end
//! of the page, so one TRB covers it, and the rest after. An interrupt
//! endpoint takes a whole message of at most one packet, and only when
//! nothing is waiting: two notifications must not run into one packet.
//!
//! # OUT
//!
//! One packet at a time, as Linux's serial gadget reads: a TRB of one
//! packet's size, which ends at a short packet or a full one. Were it
//! larger, a host that writes exactly a packet and no zero-length packet
//! after, as `cdc_acm` does, would leave the bytes in the controller until
//! more came. [`Controller::read`] takes the bytes, and once all are taken
//! the next packet is asked for; until then the host's packets are
//! refused with NAK, which is the flow control.

use ferrix_usb_device::{EndpointInfo, Function, TransferKind};

use crate::controller::{Controller, Notice};
use crate::layout::{
    ENDPOINT_BYTES, ENDPOINT_TRB_BYTES, MAX_ENDPOINTS, endpoint_buffer, endpoint_trbs,
};
use crate::regs::{
    DALEPENA, DEPCFG_BINTERVAL_M1_SHIFT, DEPCFG_EP_NUMBER_SHIFT, DEPCFG_EP_TYPE_SHIFT,
    DEPCFG_FIFO_NUMBER_SHIFT, DEPCFG_MAX_PACKET_SHIFT, DEPCFG_XFER_COMPLETE_EN, DEPCMD_CLEARSTALL,
    DEPCMD_DEPSTARTCFG, DEPCMD_ENDTRANSFER, DEPCMD_PARAM_SHIFT, DEPCMD_SETEPCONFIG,
    DEPCMD_SETSTALL, DEPCMD_SETTRANSFRESOURCE, EP_TYPE_BULK, EP_TYPE_INTERRUPT,
};
use crate::trb::{CHN, IOC, ISP_IMI, LST, NORMAL, TRB_BYTES, Trb};
use crate::{Clock, Dma, Error, Registers};

/// Where an endpoint's transfer is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Transfer {
    /// None started.
    Idle,
    /// One started, of this many bytes.
    Busy(usize),
    /// Ended with End Transfer, whose command complete event is awaited.
    Ending,
}

/// An endpoint besides endpoint 0.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Endpoint {
    /// What the configuration says it is, while it is enabled.
    info: Option<EndpointInfo>,
    transfer: Transfer,
    /// The resource index of the transfer started.
    resource: u8,
    halted: bool,
    /// Clear the stall once the transfer being ended has.
    clear_after_end: bool,
    /// IN: where the waiting bytes start in the ring, and how many.
    start: usize,
    fill: usize,
    /// OUT: the bytes of the last packet, and how many have been read.
    received: usize,
    taken: usize,
}

impl Endpoint {
    pub(crate) const fn new() -> Self {
        Endpoint {
            info: None,
            transfer: Transfer::Idle,
            resource: 0,
            halted: false,
            clear_after_end: false,
            start: 0,
            fill: 0,
            received: 0,
            taken: 0,
        }
    }

    /// The bytes the ring holds: a page for bulk, a packet for interrupt.
    fn capacity(info: &EndpointInfo) -> usize {
        match info.kind {
            TransferKind::Bulk => ENDPOINT_BYTES,
            TransferKind::Interrupt => usize::from(info.max_packet).min(ENDPOINT_BYTES),
        }
    }
}

/// The physical endpoint for an endpoint address: the number times two,
/// plus one for IN.
pub(crate) const fn physical(address: u8) -> u8 {
    ((address & 0x0F) << 1) | (address >> 7)
}

impl<R: Registers, D: Dma, C: Clock> Controller<R, D, C> {
    /// The slot of the enabled endpoint with `address`.
    fn slot(&self, address: u8) -> Option<usize> {
        self.endpoints
            .iter()
            .position(|endpoint| endpoint.info.is_some_and(|info| info.address == address))
    }

    /// The slot of the enabled endpoint on `physical`.
    fn slot_of_physical(&self, number: u8) -> Option<usize> {
        self.endpoints.iter().position(|endpoint| {
            endpoint
                .info
                .is_some_and(|info| physical(info.address) == number)
        })
    }

    fn endpoint(&self, slot: usize) -> Endpoint {
        self.endpoints.get(slot).copied().unwrap_or(Endpoint::new())
    }

    fn set_endpoint(&mut self, slot: usize, endpoint: Endpoint) {
        if let Some(place) = self.endpoints.get_mut(slot) {
            *place = endpoint;
        }
    }

    /// Whether the host has configured the device, so there are endpoints
    /// to write and read.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.endpoints
            .iter()
            .any(|endpoint| endpoint.info.is_some())
    }

    /// Queue `bytes` on the IN endpoint with `address`: how many were
    /// taken, which is fewer when the ring is full and none when the
    /// endpoint is not enabled.
    ///
    /// # Errors
    ///
    /// A Start Transfer that failed; the bytes taken stay queued.
    pub fn write(&mut self, address: u8, bytes: &[u8]) -> Result<usize, Error> {
        let Some(slot) = self.slot(address) else {
            return Ok(0);
        };
        let mut endpoint = self.endpoint(slot);
        let Some(info) = endpoint.info.filter(EndpointInfo::is_in) else {
            return Ok(0);
        };
        let capacity = Endpoint::capacity(&info);
        let message = info.kind == TransferKind::Interrupt;
        if message && (endpoint.fill != 0 || bytes.len() > capacity || bytes.is_empty()) {
            return Ok(0);
        }
        // An empty ring starts again at its beginning. Nothing is in flight
        // when it is empty (`fill` counts what the controller has not
        // finished), and it keeps a write that fits the ring in one
        // transfer: one that ran past the ring's end would go as two, and a
        // host reading a message a part at a time, as adb's does, would take
        // the first as all of it.
        if endpoint.fill == 0 {
            endpoint.start = 0;
        }
        let taken = bytes.len().min(capacity - endpoint.fill);
        let (bytes, _) = bytes.split_at(taken);
        let at = (endpoint.start + endpoint.fill) % capacity;
        let first = taken.min(capacity - at);
        let (head, tail) = bytes.split_at(first);
        let buffer = endpoint_buffer(slot);
        self.memory.write_bytes(buffer + at, head);
        self.memory.write_bytes(buffer, tail);
        endpoint.fill += taken;
        self.set_endpoint(slot, endpoint);
        self.kick(slot)?;
        Ok(taken)
    }

    /// Take what the OUT endpoint with `address` has received into
    /// `bytes`: how many.
    ///
    /// # Errors
    ///
    /// A Start Transfer for the next packet that failed.
    pub fn read(&mut self, address: u8, bytes: &mut [u8]) -> Result<usize, Error> {
        let Some(slot) = self.slot(address) else {
            return Ok(0);
        };
        let mut endpoint = self.endpoint(slot);
        let count = bytes.len().min(endpoint.received - endpoint.taken);
        let (bytes, _) = bytes.split_at_mut(count);
        self.memory
            .read_bytes(endpoint_buffer(slot) + endpoint.taken, bytes);
        endpoint.taken += count;
        self.set_endpoint(slot, endpoint);
        self.kick(slot)?;
        Ok(count)
    }

    /// Whether a bulk transfer on the IN endpoint with `address` that is a
    /// whole number of packets ends with a zero-length packet, as it does
    /// unless told otherwise. A class whose host reads exactly the length it
    /// was told, as adb's does, a header and then a payload, wants none: the
    /// empty packet would arrive where the host reads the next header. A
    /// serial port's host reads what comes, and needs the empty packet to
    /// know a transfer ended. Kept across configurations.
    pub fn set_zero_length_packets(&mut self, address: u8, on: bool) {
        let bit = 1 << (address & 0x0F);
        if on {
            self.no_zero_packets &= !bit;
        } else {
            self.no_zero_packets |= bit;
        }
    }

    /// How many bytes wait to go on the IN endpoint with `address`.
    #[must_use]
    pub fn pending(&self, address: u8) -> usize {
        self.slot(address)
            .map_or(0, |slot| self.endpoint(slot).fill)
    }

    /// Drop what waits to go on the IN endpoint with `address` and has not
    /// been handed to the controller, as a program does when the host has
    /// closed the port.
    pub fn discard(&mut self, address: u8) {
        let Some(slot) = self.slot(address) else {
            return;
        };
        let mut endpoint = self.endpoint(slot);
        let in_flight = match endpoint.transfer {
            Transfer::Busy(length) => length,
            Transfer::Idle | Transfer::Ending => 0,
        };
        endpoint.fill = in_flight;
        self.set_endpoint(slot, endpoint);
    }

    /// Start a transfer on `slot` if it is enabled, not halted, idle, and
    /// has something to do.
    fn kick(&mut self, slot: usize) -> Result<(), Error> {
        let endpoint = self.endpoint(slot);
        let Some(info) = endpoint.info else {
            return Ok(());
        };
        if endpoint.halted || endpoint.transfer != Transfer::Idle {
            return Ok(());
        }
        if info.is_in() {
            if endpoint.fill == 0 {
                return Ok(());
            }
            self.start_in(slot, endpoint, info)
        } else {
            if endpoint.taken < endpoint.received {
                return Ok(());
            }
            self.start_out(slot, endpoint, info)
        }
    }

    /// Hand the waiting bytes up to the end of the ring to the controller,
    /// with a zero-length packet after a bulk transfer of whole packets.
    fn start_in(
        &mut self,
        slot: usize,
        mut endpoint: Endpoint,
        info: EndpointInfo,
    ) -> Result<(), Error> {
        let capacity = Endpoint::capacity(&info);
        let length = endpoint.fill.min(capacity - endpoint.start);
        let data = endpoint_buffer(slot) + endpoint.start;
        let buffer = self.address(data)?;
        let packet = usize::from(info.max_packet).max(1);
        let zero_packet = info.kind == TransferKind::Bulk
            && length.is_multiple_of(packet)
            && self.no_zero_packets & (1 << (info.address & 0x0F)) == 0;
        let trbs = endpoint_trbs(slot);
        // Taken back from the controller, which wrote the last ones.
        self.memory.invalidate(trbs, ENDPOINT_TRB_BYTES);
        if zero_packet {
            Trb::new(buffer, length, NORMAL, CHN).write(&mut self.memory, trbs);
            Trb::new(buffer, 0, NORMAL, LST | IOC).write(&mut self.memory, trbs + TRB_BYTES);
        } else {
            Trb::new(buffer, length, NORMAL, LST | IOC).write(&mut self.memory, trbs);
        }
        self.memory.clean(data, length);
        self.memory.clean(trbs, ENDPOINT_TRB_BYTES);
        endpoint.resource = self.start_transfer(physical(info.address), trbs)?;
        endpoint.transfer = Transfer::Busy(length);
        self.set_endpoint(slot, endpoint);
        Ok(())
    }

    /// Ask the controller for the next packet.
    fn start_out(
        &mut self,
        slot: usize,
        mut endpoint: Endpoint,
        info: EndpointInfo,
    ) -> Result<(), Error> {
        let length = usize::from(info.max_packet).min(ENDPOINT_BYTES);
        let buffer = self.address(endpoint_buffer(slot))?;
        let trbs = endpoint_trbs(slot);
        self.memory.invalidate(trbs, ENDPOINT_TRB_BYTES);
        Trb::new(buffer, length, NORMAL, LST | IOC | ISP_IMI).write(&mut self.memory, trbs);
        self.memory.clean(trbs, ENDPOINT_TRB_BYTES);
        endpoint.resource = self.start_transfer(physical(info.address), trbs)?;
        endpoint.transfer = Transfer::Busy(length);
        endpoint.received = 0;
        endpoint.taken = 0;
        self.set_endpoint(slot, endpoint);
        Ok(())
    }

    /// A transfer complete event on `number`: account for what moved and
    /// start the next.
    pub(crate) fn endpoint_complete(
        &mut self,
        number: u8,
        notice: &mut Notice,
    ) -> Result<(), Error> {
        let Some(slot) = self.slot_of_physical(number) else {
            return Ok(());
        };
        let mut endpoint = self.endpoint(slot);
        let (Some(info), Transfer::Busy(length)) = (endpoint.info, endpoint.transfer) else {
            return Ok(());
        };
        let trbs = endpoint_trbs(slot);
        self.memory.invalidate(trbs, ENDPOINT_TRB_BYTES);
        let moved = length.saturating_sub(Trb::read(&self.memory, trbs).remaining());
        if info.is_in() {
            let capacity = Endpoint::capacity(&info);
            endpoint.start = (endpoint.start + moved) % capacity;
            endpoint.fill -= moved.min(endpoint.fill);
            notice.sent = true;
        } else {
            self.memory.invalidate(endpoint_buffer(slot), moved);
            endpoint.received = moved;
            endpoint.taken = 0;
            notice.received |= moved > 0;
        }
        endpoint.transfer = Transfer::Idle;
        self.set_endpoint(slot, endpoint);
        self.kick(slot)
    }

    /// An endpoint command complete event on `number`: an End Transfer
    /// has finished, so the endpoint may start again.
    pub(crate) fn endpoint_command_complete(
        &mut self,
        number: u8,
        parameter: u16,
    ) -> Result<(), Error> {
        let command = u32::from(parameter >> 8) & 0xF;
        let Some(slot) = self.slot_of_physical(number) else {
            return Ok(());
        };
        let mut endpoint = self.endpoint(slot);
        if command != DEPCMD_ENDTRANSFER || endpoint.transfer != Transfer::Ending {
            return Ok(());
        }
        endpoint.transfer = Transfer::Idle;
        let clear = endpoint.clear_after_end;
        endpoint.clear_after_end = false;
        self.set_endpoint(slot, endpoint);
        if clear {
            let _ = self.command(number, DEPCMD_CLEARSTALL, [0; 3])?;
        }
        self.kick(slot)
    }

    /// Halt the endpoint with `address`, or clear its halt and its data
    /// toggle: a started transfer is ended first.
    pub(crate) fn halt(&mut self, address: u8, halted: bool) -> Result<(), Error> {
        let Some(slot) = self.slot(address) else {
            return Ok(());
        };
        let mut endpoint = self.endpoint(slot);
        let number = physical(address);
        if let Transfer::Busy(_) = endpoint.transfer {
            self.end_transfer(number, endpoint.resource)?;
            endpoint.transfer = Transfer::Ending;
        }
        endpoint.halted = halted;
        if halted {
            endpoint.clear_after_end = false;
            self.set_endpoint(slot, endpoint);
            return self.command(number, DEPCMD_SETSTALL, [0; 3]).map(|_| ());
        }
        if endpoint.transfer == Transfer::Ending {
            endpoint.clear_after_end = true;
            self.set_endpoint(slot, endpoint);
            return Ok(());
        }
        self.set_endpoint(slot, endpoint);
        let _ = self.command(number, DEPCMD_CLEARSTALL, [0; 3])?;
        self.kick(slot)
    }

    /// Apply `SET_CONFIGURATION`: take every endpoint down, and for a
    /// configuration other than 0 bring up the function's with a new
    /// resource allocation. What waited to go is dropped.
    pub(crate) fn configure<F: Function>(&mut self, value: u8, function: &F) -> Result<(), Error> {
        self.deactivate_endpoints();
        if value == 0 {
            return Ok(());
        }
        let list = function.endpoints();
        if list.len() > MAX_ENDPOINTS {
            return Err(Error::Endpoints);
        }
        let _ = self.command(0, DEPCMD_DEPSTARTCFG | (2 << DEPCMD_PARAM_SHIFT), [0; 3])?;
        for (slot, info) in list.iter().enumerate() {
            self.enable(slot, *info)?;
        }
        for slot in 0..list.len() {
            self.kick(slot)?;
        }
        Ok(())
    }

    /// `DEPCFG`, a transfer resource and `DALEPENA` for one endpoint.
    fn enable(&mut self, slot: usize, info: EndpointInfo) -> Result<(), Error> {
        let number = physical(info.address);
        let kind = match info.kind {
            TransferKind::Bulk => EP_TYPE_BULK,
            TransferKind::Interrupt => EP_TYPE_INTERRUPT,
        };
        let mut parameter0 = (kind << DEPCFG_EP_TYPE_SHIFT)
            | (u32::from(info.max_packet) << DEPCFG_MAX_PACKET_SHIFT);
        if info.is_in() {
            parameter0 |= u32::from(number >> 1) << DEPCFG_FIFO_NUMBER_SHIFT;
        }
        let mut parameter1 =
            DEPCFG_XFER_COMPLETE_EN | (u32::from(number) << DEPCFG_EP_NUMBER_SHIFT);
        if info.interval > 0 {
            let interval = u32::from(info.interval - 1).min(13);
            parameter1 |= interval << DEPCFG_BINTERVAL_M1_SHIFT;
        }
        let _ = self.command(number, DEPCMD_SETEPCONFIG, [parameter0, parameter1, 0])?;
        let _ = self.command(number, DEPCMD_SETTRANSFRESOURCE, [1, 0, 0])?;
        let active = self.load(DALEPENA) | (1 << number);
        self.store(DALEPENA, active);
        self.set_endpoint(
            slot,
            Endpoint {
                info: Some(info),
                ..Endpoint::new()
            },
        );
        Ok(())
    }

    /// End every endpoint's transfer, clear its stall, and disable it.
    pub(crate) fn deactivate_endpoints(&mut self) {
        for slot in 0..MAX_ENDPOINTS {
            let endpoint = self.endpoint(slot);
            let Some(info) = endpoint.info else {
                continue;
            };
            let number = physical(info.address);
            if let Transfer::Busy(_) = endpoint.transfer {
                self.end_transfer_now(number, endpoint.resource);
            }
            if endpoint.halted {
                // A stall the host set outlives the configuration in the
                // controller; Linux clears them all at a reset.
                let _ = self.command(number, DEPCMD_CLEARSTALL, [0; 3]);
            }
            let active = self.load(DALEPENA) & !(1 << number);
            self.store(DALEPENA, active);
            self.set_endpoint(slot, Endpoint::new());
        }
    }
}
