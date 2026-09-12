//! Stage 10's device self-check: play driver for a virtio entropy device,
//! once, before any real driver exists.
//!
//! The kernel does not drive devices (`docs/ARCHITECTURE.md` §7). What it
//! does own is everything underneath a driver: the BAR mappings, the
//! capability locations, and the physical memory a device reads and writes.
//! None of that is proven by enumeration, which only reads configuration
//! space. So once, at boot, the kernel takes the part of a driver for the
//! simplest device there is — virtio-rng, one queue, no configuration — and
//! requires that a device given addresses in memory the kernel allocated
//! writes into exactly that memory. Then it resets the device and gives
//! everything back.
//!
//! This is also the harness the next two pieces of stage 10 need: MSI-X is
//! proven when this completion arrives as an interrupt rather than by
//! polling, and an IOMMU domain when a descriptor pointing outside it faults.

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_pci::bar::Region;
use ferrix_pci::header::{COMMAND, COMMAND_BUS_MASTER, COMMAND_MEMORY_SPACE};
use ferrix_pci::virtio::{Location, Transport};
use ferrix_pci::{Address, ConfigSpace};
use ferrix_virtio::pci::{
    self as transport, COMMON_CONFIG_LEN, CommonConfig, NO_VECTOR, QueueAddresses,
};
use ferrix_virtio::{Buffer, Layout, QueueMemory, SplitQueue};

use super::{Failure, Space};
use crate::mm;
use crate::mmio::Mmio;
use crate::timer;
use crate::vmap;

/// Entries in the queue. Eight is ample for one request, and small enough
/// that the rings fit in one page.
const QUEUE_SIZE: u16 = 8;

/// Bytes of entropy asked for.
const REQUEST: u32 = 64;

/// Reads of `device_status` a reset may take.
const RESET_POLLS: u32 = 100_000;

/// How long the device has to complete the request.
const DEADLINE_NANOS: u64 = 2_000_000_000;

/// One register block from a BAR, mapped for as long as this lives.
#[derive(Debug)]
struct Block {
    /// The registers.
    registers: Mmio,
    /// Where `vmap` put them, to give back.
    virt: u64,
    /// Bytes long.
    len: u64,
}

impl Block {
    /// Map the block `location` names, in the BAR `regions` holds for it.
    ///
    /// `Transport::verify` has already required the block to lie inside its
    /// BAR, so this only turns BAR-relative into physical.
    fn map(location: Location, regions: &[Region]) -> Result<Self, Failure> {
        let region = regions
            .iter()
            .find(|region| region.index == location.bar)
            .ok_or(Failure::Entropy(
                "a virtio block names a BAR that was not sized",
            ))?;
        let phys = region
            .bar
            .address()
            .checked_add(u64::from(location.offset))
            .ok_or(Failure::Entropy("a virtio block's address overflows"))?;
        let len = u64::from(location.length);
        let virt = vmap::map_device(phys, len)
            .map_err(|_| Failure::Entropy("a virtio block could not be mapped"))?;
        Ok(Block {
            registers: Mmio::at(virt),
            virt,
            len,
        })
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        let _ = vmap::unmap_device(self.virt);
    }
}

/// The common configuration, through its mapped block.
#[derive(Debug)]
struct Common<'b>(&'b Block);

impl CommonConfig for Common<'_> {
    fn read8(&self, offset: u32) -> u8 {
        self.0.registers.read8(u64::from(offset))
    }
    fn read16(&self, offset: u32) -> u16 {
        self.0.registers.read16(u64::from(offset))
    }
    fn read32(&self, offset: u32) -> u32 {
        self.0.registers.read32(u64::from(offset))
    }
    fn write8(&mut self, offset: u32, value: u8) {
        self.0.registers.write8(u64::from(offset), value);
    }
    fn write16(&mut self, offset: u32, value: u16) {
        self.0.registers.write16(u64::from(offset), value);
    }
    fn write32(&mut self, offset: u32, value: u32) {
        self.0.registers.write32(u64::from(offset), value);
    }
}

/// One zeroed page a device is given the physical address of.
#[derive(Debug)]
struct DmaPage {
    /// The frame number.
    frame: u64,
}

impl DmaPage {
    /// Take and zero a frame.
    fn new() -> Result<Self, Failure> {
        let frame = mm::allocate_frames(0).ok_or(Failure::Entropy("no frame for DMA"))?;
        mm::zero_frame(frame);
        Ok(DmaPage { frame })
    }

    /// The physical address the device is given.
    const fn phys(&self) -> u64 {
        self.frame * PAGE_SIZE
    }

    /// Where the kernel reads and writes the same page.
    fn virt(&self) -> u64 {
        mm::direct_map(self.phys())
    }
}

impl Drop for DmaPage {
    fn drop(&mut self) {
        mm::deallocate_frames(self.frame, 0);
    }
}

/// The rings, in a [`DmaPage`] that outlives the queue built over them.
#[derive(Debug)]
struct Rings {
    /// The page's direct-map address.
    virt: u64,
}

// SAFETY: `virt` is the direct-map alias of a whole page taken for this queue
// alone and held until after the device has been reset, which is the memory
// `QueueMemory` asks for: at least `Layout::total_size` bytes (`drive` checks
// the layout fits a page), aligned to a page and so to 16 bytes, and shared
// with nothing but the device. The barrier is every access itself: each is a
// volatile load or store, which the compiler may neither defer nor reorder,
// so every access issued before any point is visible to the device before
// every access issued after it.
unsafe impl QueueMemory for Rings {
    fn read_u8(&self, offset: usize) -> u8 {
        // SAFETY: `offset` is below `Layout::total_size`, which fits in the
        // page, so the address is inside memory this queue owns.
        unsafe { core::ptr::read_volatile((self.virt + offset as u64) as *const u8) }
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        // SAFETY: as `read_u8`.
        unsafe { core::ptr::write_volatile((self.virt + offset as u64) as *mut u8, value) };
    }

    fn barrier(&self) {}
}

/// Read entropy from the virtio-rng device at `address`, returning how many
/// bytes it wrote.
///
/// Memory decoding and bus mastering are on only for the duration, and the
/// device is reset — so it holds no address into memory this gives back —
/// before the pages are freed or the command register restored, whatever
/// happened in between.
pub(super) fn entropy(
    space: &mut Space,
    address: Address,
    transport: &Transport,
    regions: &[Region],
) -> Result<u32, Failure> {
    transport.verify(regions)?;
    if transport.common.length < COMMON_CONFIG_LEN {
        return Err(Failure::Entropy(
            "the common configuration block is shorter than virtio 1.x defines",
        ));
    }
    let common = Block::map(transport.common, regions)?;
    let notify = Block::map(transport.notify, regions)?;
    let rings = DmaPage::new()?;
    let buffer = DmaPage::new()?;

    let command = space.read16(address, COMMAND);
    space.write16(
        address,
        COMMAND,
        command | COMMAND_MEMORY_SPACE | COMMAND_BUS_MASTER,
    );

    let result = drive(
        &common,
        &notify,
        transport.notify_multiplier,
        &rings,
        &buffer,
    );

    let reset = transport::reset(&mut Common(&common), RESET_POLLS);
    space.write16(address, COMMAND, command);
    let written = result?;
    reset?;
    Ok(written)
}

/// Bring the device up, make one request and wait for it.
fn drive(
    common: &Block,
    notify: &Block,
    multiplier: u32,
    rings: &DmaPage,
    buffer: &DmaPage,
) -> Result<u32, Failure> {
    let mut config = Common(common);
    let _ = transport::negotiate(&mut config, 0, 0, RESET_POLLS)?;

    let max = transport::queue_max_size(&mut config, 0)?;
    // The largest power of two no bigger than either.
    let size = QUEUE_SIZE.min(1 << (15 - max.leading_zeros()));
    let layout = Layout::for_size(size)?;
    if layout.total_size as u64 > PAGE_SIZE {
        return Err(Failure::Entropy("the rings do not fit in a page"));
    }
    let base = rings.phys();
    let active = transport::activate_queue(
        &mut config,
        0,
        size,
        QueueAddresses {
            descriptors: base + layout.descriptor_table as u64,
            driver: base + layout.available_ring as u64,
            device: base + layout.used_ring as u64,
        },
        NO_VECTOR,
    )?;
    transport::driver_ok(&mut config)?;

    let mut queue = SplitQueue::new(layout, Rings { virt: rings.virt() });
    let head = queue.add_chain(&[Buffer::writable(buffer.phys(), REQUEST)])?;

    let doorbell = transport::notify_offset(active.notify_off, multiplier);
    if doorbell.checked_add(2).is_none_or(|end| end > notify.len) {
        return Err(Failure::Entropy(
            "the queue's doorbell is outside its block",
        ));
    }
    notify.registers.write16(doorbell, 0);

    let deadline = timer::now_nanos().saturating_add(DEADLINE_NANOS);
    let completion = loop {
        if let Some(completion) = queue.take_used()? {
            break completion;
        }
        if timer::now_nanos() > deadline {
            return Err(Failure::Entropy("the device never completed the request"));
        }
        core::hint::spin_loop();
    };

    if completion.head != head {
        return Err(Failure::Entropy(
            "the device completed a request nobody made",
        ));
    }
    if completion.written == 0 || completion.written > REQUEST {
        return Err(Failure::Entropy(
            "the device wrote a length it was not given",
        ));
    }
    // SAFETY: the buffer page is the direct-map alias of a frame this check
    // owns, `written` is at most `REQUEST`, which is less than a page, and the
    // device has finished writing it.
    let bytes = unsafe {
        core::slice::from_raw_parts(buffer.virt() as *const u8, completion.written as usize)
    };
    // Sixty-four zero bytes from an entropy source is a one in 2^512 event;
    // a buffer the device never wrote is not.
    if bytes.iter().all(|byte| *byte == 0) {
        return Err(Failure::Entropy(
            "the device wrote nothing into the buffer it was given",
        ));
    }
    Ok(completion.written)
}
