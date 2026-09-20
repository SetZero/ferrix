//! The virtio-console driver: bring the device up, walk the control
//! conversation until a named port is open, and carry that port's bytes.
//!
//! `libs/virtio`'s [`console`] module is the protocol -- the numbers, the
//! queue arithmetic, the control message. This crate is the *order*: what to
//! write when, which buffers to post, what a completion means, and what state
//! the port is in. `docs/CLIPBOARD.md` §3 is the specification.
//!
//! Nothing here allocates, maps, waits or touches a handle. The memory is the
//! caller's, behind [`QueueMemory`] and [`Area`], exactly as the other virtio
//! driver crates take theirs, which is what lets all of this be tested on the
//! host against a fake device.
//!
//! # What a driver of this does
//!
//! 1. [`Driver::init`] negotiates the features, describes the four queues and
//!    sets `DRIVER_OK`, then posts the control receive buffers and sends
//!    `DEVICE_READY`.
//! 2. On every interrupt, [`Driver::poll`] drains the control queue and
//!    answers what must be answered -- `PORT_READY` to an add, `PORT_OPEN` to
//!    the device opening its end -- and hands back what the caller has to
//!    decide about: the port's name, and whether it is open.
//! 3. Once [`Driver::port_open`], [`Driver::read`] takes what arrived and
//!    [`Driver::write`] sends. Before that both do nothing, because a port
//!    whose host end is shut carries nothing.
//!
//! # The port is chosen by name
//!
//! A device may have many ports and their numbers are its own to choose, so
//! the driver is told a name and takes the first port that answers to it
//! ([`Options::name`]). Every other port is answered politely -- `PORT_READY`
//! so the device is not left waiting -- and then ignored.
//!
//! # Trust
//!
//! The device chooses every completion length, so each is clamped to the
//! buffer that was posted before it bounds a copy. A completion naming a
//! descriptor the driver never published is [`ConsoleError::Stray`], not an
//! index into anything. A control message about a port the device did not
//! declare is refused by [`console::Control::read`] before it is acted on.
//! And a device that writes more control bytes than a control buffer holds
//! has its message refused rather than truncated into a different one.

#![no_std]

use ferrix_virtio::DeviceConfig;
use ferrix_virtio::console::{self, ConsoleError as ProtocolError, Control};
use ferrix_virtio::pci::{self, ActiveQueue, CommonConfig, QueueAddresses, TransportError};
use ferrix_virtio::{Buffer, Layout, PAGE_SIZE, QueueError, QueueMemory, SplitQueue};

#[cfg(test)]
mod tests;

/// How many buffers each queue carries.
///
/// Sixteen: the clipboard is not a disk, and a queue this size costs one page
/// of rings. The control queue never has more than a handful outstanding.
pub const QUEUE_SIZE: u16 = 16;

/// Bytes of one data buffer, and so the most this driver moves in one
/// direction per descriptor.
///
/// 1024, which is what a vdagent chunk is cut to (`docs/CLIPBOARD.md` §4.1),
/// so a chunk is never split across two completions by this driver's own
/// choice of buffer.
pub const CHUNK: usize = 1024;

/// Bytes of one control buffer: the eight-byte header and a name.
pub const CONTROL_BUFFER: usize = 64;

/// The longest port name this driver keeps.
pub const NAME_MAX: usize = CONTROL_BUFFER - console::CONTROL_BYTES;

/// Where the receive buffers start in a data area, and where the transmit
/// buffers start after them.
const TX_AT: usize = QUEUE_SIZE as usize * CHUNK;
/// Bytes a data area must hold: receive buffers then transmit buffers.
pub const AREA_BYTES: usize = TX_AT * 2;
/// Bytes a control area must hold, the same way.
pub const CONTROL_AREA_BYTES: usize = QUEUE_SIZE as usize * CONTROL_BUFFER * 2;
/// Where a control area's transmit buffers start.
const CONTROL_TX_AT: usize = QUEUE_SIZE as usize * CONTROL_BUFFER;

/// The pages of a queue's rings, which is one for [`QUEUE_SIZE`].
pub const RING_BYTES: usize = PAGE_SIZE as usize;

/// The device addresses of the pages a caller's memory is pinned at.
///
/// One address per page, in order, as a pin's address query answers. A buffer
/// never crosses a page here, so one page's address and an offset within it
/// is a device address.
pub trait DevicePages {
    /// The addresses, one per page.
    fn device_pages(&self) -> &[u64];
}

/// A region of memory the device reads and writes, at the caller's offsets.
pub trait Area: DevicePages {
    /// The byte at `offset`.
    fn read_u8(&self, offset: usize) -> u8;
    /// Write the byte at `offset`.
    fn write_u8(&mut self, offset: usize, value: u8);
}

/// What a driver needs of the transport beyond the configuration blocks.
pub trait Transport: CommonConfig + DeviceConfig {
    /// Tell the device that `queue` has something, at its notification
    /// offset.
    fn notify(&mut self, queue: u16, notify_off: u16);
    /// The MSI-X vector to ask for, or [`pci::NO_VECTOR`].
    fn queue_vector(&self) -> u16;
    /// Acknowledge the interrupt and say what it was.
    fn acknowledge_interrupt(&mut self) -> u8;
}

/// What went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleError {
    /// The transport refused a step of the bring-up.
    Transport(TransportError),
    /// A queue's memory is not the size its rings need, or its pages are not
    /// where they were said to be.
    Memory,
    /// A ring operation failed, which is a device that wrote nonsense into
    /// shared memory.
    Queue(QueueError),
    /// A control message the device sent is not one.
    Protocol(ProtocolError),
    /// A completion naming a descriptor this driver never published.
    Stray(u16),
    /// The device declared no port with the name asked for by the time it
    /// said it had added them all.
    NoSuchPort,
}

impl From<QueueError> for ConsoleError {
    fn from(error: QueueError) -> ConsoleError {
        ConsoleError::Queue(error)
    }
}

impl From<ProtocolError> for ConsoleError {
    fn from(error: ProtocolError) -> ConsoleError {
        ConsoleError::Protocol(error)
    }
}

/// What the caller must decide about, from one [`Driver::poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The port this driver was looking for has been found and is being
    /// opened. Its number, for a log line; the driver needs nothing more.
    Found {
        /// Which port.
        port: u32,
    },
    /// The port's host end opened or closed. Only after `open` is true does
    /// [`Driver::read`] or [`Driver::write`] carry anything, and a close
    /// means whoever was using it must forget its state.
    Open {
        /// Whether it is open.
        open: bool,
    },
    /// Bytes arrived on the port. [`Driver::read`] takes them.
    Data,
}

/// How a driver is set up.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The port to look for, by name. The first port that answers to it is
    /// taken and the rest are answered and ignored.
    pub name: &'static [u8],
    /// Polls to give the device to acknowledge a reset.
    pub reset_polls: u32,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            name: console::SPICE_PORT_NAME,
            reset_polls: 1_000_000,
        }
    }
}

/// The memory a driver is given.
#[derive(Debug)]
pub struct Parts<T, M, A> {
    /// The device.
    pub transport: T,
    /// Rings for the control receive queue.
    pub control_rx_rings: M,
    /// Rings for the control transmit queue.
    pub control_tx_rings: M,
    /// Rings for the port's receive queue.
    pub port_rx_rings: M,
    /// Rings for the port's transmit queue.
    pub port_tx_rings: M,
    /// [`CONTROL_AREA_BYTES`] of buffers for the control queues.
    pub control_area: A,
    /// [`AREA_BYTES`] of buffers for the port's queues.
    pub port_area: A,
}

/// One queue: its ring logic, where to notify it, and which buffer each
/// outstanding descriptor belongs to.
#[derive(Debug)]
struct Ring<M> {
    queue: SplitQueue<M>,
    index: u16,
    notify_off: u16,
    /// For each descriptor index, the buffer slot posted with it, or
    /// `u16::MAX` for one that was never posted.
    slot_of: [u16; QUEUE_SIZE as usize],
    /// Which slots are free to post.
    free: [bool; QUEUE_SIZE as usize],
}

impl<M: QueueMemory> Ring<M> {
    /// Note that `head` carries `slot`.
    fn posted(&mut self, head: u16, slot: u16) {
        if let Some(cell) = self.slot_of.get_mut(head as usize) {
            *cell = slot;
        }
        if let Some(cell) = self.free.get_mut(slot as usize) {
            *cell = false;
        }
    }

    /// The slot `head` carried, freeing both.
    fn completed(&mut self, head: u16) -> Result<u16, ConsoleError> {
        let slot = self
            .slot_of
            .get_mut(head as usize)
            .filter(|slot| **slot != u16::MAX)
            .ok_or(ConsoleError::Stray(head))?;
        let taken = *slot;
        *slot = u16::MAX;
        if let Some(cell) = self.free.get_mut(taken as usize) {
            *cell = true;
        }
        Ok(taken)
    }

    /// The lowest free slot.
    fn free_slot(&self) -> Option<u16> {
        self.free
            .iter()
            .position(|free| *free)
            .and_then(|at| u16::try_from(at).ok())
    }
}

/// The driver.
#[derive(Debug)]
pub struct Driver<T, M, A> {
    transport: T,
    control_rx: Ring<M>,
    control_tx: Ring<M>,
    port_rx: Ring<M>,
    port_tx: Ring<M>,
    control_area: A,
    port_area: A,
    options: Options,
    /// How many ports the configuration block declared.
    ports: u32,
    /// The port taken, once one has answered to the name.
    port: Option<u32>,
    /// Whether its host end is open.
    open: bool,
}

/// A device address for the byte at `offset` of a pinned area.
fn address_of(pages: &[u64], offset: usize) -> Option<u64> {
    let page = pages.get(offset / PAGE_SIZE as usize)?;
    Some(page + (offset % PAGE_SIZE as usize) as u64)
}

/// Where a split queue's three parts are, given the pages its rings live in.
fn ring_addresses(layout: &Layout, pages: &[u64]) -> Option<QueueAddresses> {
    Some(QueueAddresses {
        descriptors: address_of(pages, layout.descriptor_table)?,
        driver: address_of(pages, layout.available_ring)?,
        device: address_of(pages, layout.used_ring)?,
    })
}

impl<T: Transport, M: QueueMemory + DevicePages, A: Area> Driver<T, M, A> {
    /// Bring the device up, describe the queues, and post the control
    /// buffers.
    ///
    /// # Errors
    ///
    /// [`ConsoleError::Transport`] for a device that will not negotiate or
    /// take a queue, [`ConsoleError::Memory`] for memory that cannot hold the
    /// rings or whose pages are missing, and [`ConsoleError::Protocol`] for a
    /// configuration block that is not a console's.
    pub fn init(parts: Parts<T, M, A>, options: Options) -> Result<Driver<T, M, A>, ConsoleError> {
        let Parts {
            mut transport,
            control_rx_rings,
            control_tx_rings,
            port_rx_rings,
            port_tx_rings,
            control_area,
            port_area,
        } = parts;

        // The features agreed are not kept: this driver asks for nothing
        // optional, so what came back is what it asked for or the call
        // failed.
        let _agreed = pci::negotiate(
            &mut transport,
            console::DRIVER_FEATURES,
            console::REQUIRED_FEATURES,
            options.reset_polls,
        )
        .map_err(ConsoleError::Transport)?;

        let config = console::Config::read(&transport)?;
        // The port this driver wants is port 1 at the numbering of §3.2, and
        // a device that declares fewer ports than that has nowhere to put it.
        let queues = [
            console::CONTROL_RECEIVE_QUEUE,
            console::CONTROL_TRANSMIT_QUEUE,
            console::receive_queue(1),
            console::transmit_queue(1),
        ];
        let mut rings = [
            Ring::new(control_rx_rings, queues[0]),
            Ring::new(control_tx_rings, queues[1]),
            Ring::new(port_rx_rings, queues[2]),
            Ring::new(port_tx_rings, queues[3]),
        ];
        for ring in &mut rings {
            ring.activate(&mut transport)?;
        }
        let [control_rx, control_tx, port_rx, port_tx] = rings;

        pci::driver_ok(&mut transport).map_err(ConsoleError::Transport)?;

        let mut driver = Driver {
            transport,
            control_rx,
            control_tx,
            port_rx,
            port_tx,
            control_area,
            port_area,
            options,
            ports: config.ports,
            port: None,
            open: false,
        };
        // The control buffers before DEVICE_READY: the device may answer it
        // immediately, and an answer with nowhere to land is a lost port.
        driver.post_control_buffers()?;
        driver.post_receive_buffers()?;
        driver.send_control(Control::DeviceReady { ready: true })?;
        Ok(driver)
    }

    /// Whether the port has been found and its host end is open.
    #[must_use]
    pub fn port_open(&self) -> bool {
        self.port.is_some() && self.open
    }

    /// The port's number, once one has answered to the name.
    #[must_use]
    pub fn port(&self) -> Option<u32> {
        self.port
    }

    /// The transport, for a caller that must reset the device.
    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Acknowledge an interrupt and drain what it was about, handing back at
    /// most one event; call it until it gives `None`.
    ///
    /// # Errors
    ///
    /// As [`ConsoleError`] describes: a device that writes nonsense into the
    /// rings or sends a control message that is not one.
    pub fn poll(&mut self) -> Result<Option<Event>, ConsoleError> {
        if let Some(event) = self.drain_control()? {
            return Ok(Some(event));
        }
        // A transmit completion frees its buffer and is not news.
        while let Some(done) = self.port_tx.queue.take_used()? {
            let _ = self.port_tx.completed(done.head)?;
        }
        while let Some(done) = self.control_tx.queue.take_used()? {
            let _ = self.control_tx.completed(done.head)?;
        }
        if self.port_rx.queue.has_used() {
            return Ok(Some(Event::Data));
        }
        Ok(None)
    }

    /// Take one buffer's worth of what arrived on the port into `out`, and
    /// say how many bytes that was; `None` when nothing is waiting.
    ///
    /// The buffer is posted back before this returns, so a caller that reads
    /// in a loop never starves the device of somewhere to write.
    ///
    /// # Errors
    ///
    /// As [`ConsoleError`] describes.
    pub fn read(&mut self, out: &mut [u8]) -> Result<Option<usize>, ConsoleError> {
        let Some(done) = self.port_rx.queue.take_used()? else {
            return Ok(None);
        };
        let slot = self.port_rx.completed(done.head)?;
        let at = slot as usize * CHUNK;
        // The device chooses `written`; it is clamped to the buffer it was
        // given and to what the caller can take.
        let len = (done.written as usize).min(CHUNK).min(out.len());
        for (index, byte) in out.iter_mut().enumerate().take(len) {
            *byte = self.port_area.read_u8(at + index);
        }
        self.post_receive_slot(slot)?;
        Ok(Some(len))
    }

    /// Send `bytes` on the port, and say how many were taken: a short answer
    /// means the queue is full and the rest is the caller's to send again.
    ///
    /// # Errors
    ///
    /// As [`ConsoleError`] describes.
    pub fn write(&mut self, bytes: &[u8]) -> Result<usize, ConsoleError> {
        if !self.port_open() {
            return Ok(0);
        }
        let Some(slot) = self.port_tx.free_slot() else {
            return Ok(0);
        };
        let take = bytes.len().min(CHUNK);
        let at = TX_AT + slot as usize * CHUNK;
        for (index, byte) in bytes.iter().take(take).enumerate() {
            self.port_area.write_u8(at + index, *byte);
        }
        let address = address_of(self.port_area.device_pages(), at).ok_or(ConsoleError::Memory)?;
        let head = self
            .port_tx
            .queue
            .add_chain(&[Buffer::readable(address, take as u32)])?;
        self.port_tx.posted(head, slot);
        let (index, notify) = (self.port_tx.index, self.port_tx.notify_off);
        self.transport.notify(index, notify);
        Ok(take)
    }

    // -- the control conversation -----------------------------------------

    /// Drain one control message and answer what must be answered.
    fn drain_control(&mut self) -> Result<Option<Event>, ConsoleError> {
        let Some(done) = self.control_rx.queue.take_used()? else {
            return Ok(None);
        };
        let slot = self.control_rx.completed(done.head)?;
        let at = slot as usize * CONTROL_BUFFER;
        let len = (done.written as usize).min(CONTROL_BUFFER);
        let mut bytes = [0_u8; CONTROL_BUFFER];
        for (index, byte) in bytes.iter_mut().enumerate().take(len) {
            *byte = self.control_area.read_u8(at + index);
        }
        // The buffer goes back before the message is acted on: answering may
        // send, and a send that fails should not also have lost a buffer.
        self.post_control_slot(slot)?;

        let message = Control::read(bytes.get(..len).unwrap_or_default(), self.ports)?;
        self.on_control(message)
    }

    /// What one control message means, and what it is answered with.
    fn on_control(&mut self, message: Control<'_>) -> Result<Option<Event>, ConsoleError> {
        match message {
            // Every port is answered, whether or not it is the one wanted:
            // a device left waiting for PORT_READY adds nothing more, and
            // the port being looked for may be the next one.
            Control::Add { port } => {
                self.send_control(Control::Ready { port, ready: true })?;
                Ok(None)
            }
            Control::Name { port, name } => {
                if self.port.is_none() && name == self.options.name {
                    self.port = Some(port);
                    // The device is told this end is open as soon as the port
                    // is recognised; its own PORT_OPEN may come before or
                    // after, and §3.3 has both ends say it.
                    self.send_control(Control::Open { port, open: true })?;
                    return Ok(Some(Event::Found { port }));
                }
                Ok(None)
            }
            Control::Open { port, open } if self.port == Some(port) => {
                self.open = open;
                Ok(Some(Event::Open { open }))
            }
            Control::Remove { port } if self.port == Some(port) => {
                self.open = false;
                self.port = None;
                Ok(Some(Event::Open { open: false }))
            }
            // A console port, a resize, an event this crate does not know,
            // and anything about a port that is not this one: nothing to do.
            _ => Ok(None),
        }
    }

    /// Send one control message.
    fn send_control(&mut self, message: Control<'_>) -> Result<(), ConsoleError> {
        let Some(slot) = self.control_tx.free_slot() else {
            // Sixteen outstanding control messages means a device that has
            // completed none of them; there is nothing useful to do but say
            // so.
            return Err(ConsoleError::Memory);
        };
        let mut bytes = [0_u8; console::CONTROL_BYTES];
        let len = message.write(&mut bytes)?;
        let at = CONTROL_TX_AT + slot as usize * CONTROL_BUFFER;
        for (index, byte) in bytes.iter().take(len).enumerate() {
            self.control_area.write_u8(at + index, *byte);
        }
        let address =
            address_of(self.control_area.device_pages(), at).ok_or(ConsoleError::Memory)?;
        let head = self
            .control_tx
            .queue
            .add_chain(&[Buffer::readable(address, len as u32)])?;
        self.control_tx.posted(head, slot);
        let (index, notify) = (self.control_tx.index, self.control_tx.notify_off);
        self.transport.notify(index, notify);
        Ok(())
    }

    /// Post every control receive buffer.
    fn post_control_buffers(&mut self) -> Result<(), ConsoleError> {
        for slot in 0..QUEUE_SIZE {
            self.post_control_slot(slot)?;
        }
        Ok(())
    }

    /// Post one control receive buffer.
    fn post_control_slot(&mut self, slot: u16) -> Result<(), ConsoleError> {
        let at = slot as usize * CONTROL_BUFFER;
        let address =
            address_of(self.control_area.device_pages(), at).ok_or(ConsoleError::Memory)?;
        let head = self
            .control_rx
            .queue
            .add_chain(&[Buffer::writable(address, CONTROL_BUFFER as u32)])?;
        self.control_rx.posted(head, slot);
        let (index, notify) = (self.control_rx.index, self.control_rx.notify_off);
        self.transport.notify(index, notify);
        Ok(())
    }

    /// Post every receive buffer of the port.
    fn post_receive_buffers(&mut self) -> Result<(), ConsoleError> {
        for slot in 0..QUEUE_SIZE {
            self.post_receive_slot(slot)?;
        }
        Ok(())
    }

    /// Post one receive buffer of the port.
    fn post_receive_slot(&mut self, slot: u16) -> Result<(), ConsoleError> {
        let at = slot as usize * CHUNK;
        let address = address_of(self.port_area.device_pages(), at).ok_or(ConsoleError::Memory)?;
        let head = self
            .port_rx
            .queue
            .add_chain(&[Buffer::writable(address, CHUNK as u32)])?;
        self.port_rx.posted(head, slot);
        let (index, notify) = (self.port_rx.index, self.port_rx.notify_off);
        self.transport.notify(index, notify);
        Ok(())
    }
}

impl<M: QueueMemory + DevicePages> Ring<M> {
    /// A ring over `memory` for queue `index`, not yet described to the
    /// device.
    fn new(memory: M, index: u16) -> Ring<M> {
        // `Layout::for_size` refuses only a size that is not a power of two
        // or is too large, and `QUEUE_SIZE` is neither.
        let layout = Layout::for_size(QUEUE_SIZE).unwrap_or(Layout {
            queue_size: QUEUE_SIZE,
            descriptor_table: 0,
            available_ring: 0,
            used_ring: 0,
            total_size: 0,
        });
        Ring {
            queue: SplitQueue::new(layout, memory),
            index,
            notify_off: 0,
            slot_of: [u16::MAX; QUEUE_SIZE as usize],
            free: [true; QUEUE_SIZE as usize],
        }
    }

    /// Describe the queue to the device and enable it.
    fn activate<T: Transport>(&mut self, transport: &mut T) -> Result<(), ConsoleError> {
        let layout = Layout::for_size(QUEUE_SIZE).map_err(ConsoleError::Queue)?;
        if layout.total_size > RING_BYTES {
            return Err(ConsoleError::Memory);
        }
        let addresses = ring_addresses(&layout, self.queue.memory().device_pages())
            .ok_or(ConsoleError::Memory)?;
        let vector = transport.queue_vector();
        let ActiveQueue { notify_off, .. } =
            pci::activate_queue(transport, self.index, QUEUE_SIZE, addresses, vector)
                .map_err(ConsoleError::Transport)?;
        self.notify_off = notify_off;
        Ok(())
    }
}
