//! A GICv3's Interrupt Translation Service: message-signalled interrupts.
//!
//! A `GICv2m` frame turns a device's write into an SPI named by the data
//! written. An ITS turns it into an **LPI**, by looking the pair of *which
//! device wrote* and *what it wrote* (a device ID and an event ID) up in
//! tables in memory that the kernel builds and the ITS walks:
//!
//! * the **device table**, one entry per device ID, pointing at that device's
//!   **interrupt translation table** (ITT), one entry per event ID, which names
//!   the LPI and the **collection** it goes to;
//! * the **collection table**, which says which redistributor a collection
//!   is;
//! * and beside the ITS, the redistributor's own two: the **LPI property
//!   table**, a byte per LPI with its priority and enable, and the **pending
//!   table**, a bit per LPI.
//!
//! The kernel never writes the ITS's tables itself. It writes commands into a
//! queue -- `MAPD` for a device's ITT, `MAPC` for a collection, `MAPTI` for
//! one event, `INV` to make a changed property byte be read again, `SYNC` to
//! wait for all of them -- and the ITS writes its tables.
//!
//! # What this driver chooses
//!
//! * **One collection, on the boot core.** Every LPI goes to the processor
//!   that brought the controller up, which is what the `GICv2m` path does
//!   with its SPIs: `gicv2::msi_allocate` enables each on the core asking,
//!   and every MSI vector is minted during stage 10's enumeration on the boot
//!   core. So only that core's redistributor has LPIs enabled, and a
//!   secondary's `init_this_cpu` has nothing to do for them.
//! * **The device ID is the requester ID**, bus, device and function. On
//!   QEMU's `virt` both descriptions say so: the IORT maps a root complex's
//!   requester IDs to its SMMU and the SMMU's stream IDs to the ITS group each
//!   one to one, and the device tree's `msi-map` is `<0 &its 0 0x10000>`. The
//!   kernel reads neither map; a machine whose map is not the identity would
//!   need it read here before its devices' messages were translated.
//! * **A flat device table** covering 16 bits of device ID, every requester ID
//!   there is on one PCI segment, or fewer if the ITS decodes fewer. On
//!   `virt` that is 512 KiB, taken once.
//! * **256 vectors**, event IDs 0 to 255 and LPIs 8192 to 8447, each event ID
//!   used for one device only, so an ITT of 256 entries covers any device.
//!
//! # Numbers
//!
//! An LPI's GIC identifier starts at 8192, above a gap the interrupt table
//! would otherwise have to span. So the kernel knows LPI `8192 + k` as
//! interrupt `1024 + k`: [`number`] maps one to the other when the CPU
//! interface hands an LPI over, and the acknowledgement retired is still the
//! GIC's own identifier.
//!
//! # Memory the ITS reads
//!
//! Every table and the command queue is asked for as inner-shareable,
//! write-back cacheable memory, which is what the kernel's direct map is. An
//! ITS that cannot snoop the caches says so by reading back
//! *non-shareable*; its tables are then marked non-cacheable, and every write
//! it is to read is cleaned to the point of coherency first. Tables the ITS
//! writes are cleaned and invalidated once, after they are zeroed, so no
//! dirty line of the kernel's is left to be evicted over what the ITS wrote.

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_sync::IrqSpinLock;

use super::{cpu, gicv3};
use crate::irq::Msi;
use crate::mm;
use crate::mmio::Mmio;

/// ITS control: whether it is translating, and whether it is quiet.
const GITS_CTLR: u64 = 0x0000;
/// ITS type, 64 bits: what the ITS decodes and how it names a redistributor.
const GITS_TYPER: u64 = 0x0008;
/// Command queue base, 64 bits.
const GITS_CBASER: u64 = 0x0080;
/// Command queue write offset: where the kernel has written up to.
const GITS_CWRITER: u64 = 0x0088;
/// Command queue read offset: where the ITS has consumed up to.
const GITS_CREADR: u64 = 0x0090;
/// The first of eight table registers, 64 bits each.
const GITS_BASER: u64 = 0x0100;
/// How many table registers there are.
const GITS_BASERS: u64 = 8;
/// The register a device writes its event ID to, in the translation frame
/// 64 KiB above the control frame.
const GITS_TRANSLATER: u64 = 0x1_0040;
/// The translation frame's page, which an IOMMU domain must map.
const TRANSLATION_FRAME: u64 = 0x1_0000;
/// Bytes of the control frame, which is all the kernel reads or writes.
const ITS_WINDOW: u64 = 0x1_0000;

/// `GITS_CTLR`: translating.
const CTLR_ENABLED: u32 = 1;
/// `GITS_CTLR`: disabled, and finished with everything it was doing.
const CTLR_QUIESCENT: u32 = 1 << 31;

/// `GITS_TYPER`: physical LPIs are supported.
const TYPER_PHYSICAL: u64 = 1;
/// `GITS_TYPER`: bytes of one ITT entry, less one, in bits 7:4.
const TYPER_ITT_ENTRY_SHIFT: u32 = 4;
/// `GITS_TYPER`: event ID bits, less one, in bits 12:8.
const TYPER_EVENT_BITS_SHIFT: u32 = 8;
/// `GITS_TYPER`: device ID bits, less one, in bits 17:13.
const TYPER_DEVICE_BITS_SHIFT: u32 = 13;
/// `GITS_TYPER`: a collection names its redistributor by physical address
/// rather than by processor number.
const TYPER_PTA: u64 = 1 << 19;

/// Table and queue registers: valid.
const BASER_VALID: u64 = 1 << 63;
/// `GITS_BASER`: what the table holds, bits 58:56.
const BASER_TYPE_SHIFT: u32 = 56;
/// `GITS_BASER` type: the device table.
const BASER_TYPE_DEVICE: u64 = 1;
/// `GITS_BASER` type: the collection table.
const BASER_TYPE_COLLECTION: u64 = 4;
/// `GITS_BASER`: bytes of one entry, less one, bits 52:48.
const BASER_ENTRY_SHIFT: u32 = 48;
/// `GITS_BASER`: the page size, bits 9:8, as 4, 16 or 64 KiB.
const BASER_PAGE_SIZE_SHIFT: u32 = 8;
/// Largest page count a table register's eight-bit size field holds.
const BASER_MAX_PAGES: u64 = 256;

/// The ITS's registers' memory attributes: inner cacheability in bits 61:59,
/// outer in 55:53 (zero: as inner), shareability in 11:10.
const ITS_INNER_CACHE_SHIFT: u32 = 59;
/// The redistributor's: inner cacheability in bits 9:7, outer in 58:56 (zero:
/// as inner), shareability in 11:10.
const GICR_INNER_CACHE_SHIFT: u32 = 7;
/// Shareability, bits 11:10, in every one of these registers.
const SHAREABILITY_SHIFT: u32 = 10;
/// Normal memory, inner write-back, read- and write-allocate.
const CACHE_WRITE_BACK: u64 = 0b111;
/// Normal memory, not cacheable.
const CACHE_NONE: u64 = 0b001;
/// Inner shareable.
const SHARE_INNER: u64 = 0b01;

/// Redistributor control: bit 0 enables LPIs, and cannot be relied on to
/// clear again once set.
const GICR_CTLR: u64 = 0x0000;
/// Redistributor type, 64 bits.
const GICR_TYPER: u64 = 0x0008;
/// LPI property table base, 64 bits.
const GICR_PROPBASER: u64 = 0x0070;
/// LPI pending table base, 64 bits.
const GICR_PENDBASER: u64 = 0x0078;
/// `GICR_CTLR`: LPIs enabled.
const GICR_CTLR_ENABLE_LPIS: u32 = 1;
/// `GICR_TYPER`: the redistributor handles physical LPIs.
const GICR_TYPER_PLPIS: u32 = 1;
/// `GICR_PENDBASER`: the pending table is zero, so the redistributor need
/// not read it.
const PENDBASER_PTZ: u64 = 1 << 62;

/// `GICD_TYPER`: the distributor supports LPIs.
const GICD_TYPER_LPIS: u32 = 1 << 17;
/// `GICD_TYPER`: interrupt identifier bits, less one, in bits 23:19.
const GICD_TYPER_ID_BITS_SHIFT: u32 = 19;

/// The first LPI's GIC identifier.
const FIRST_LPI: u32 = 8192;
/// Interrupt identifier bits the property table covers, less one: fourteen
/// bits reach LPIs 8192 to 16383, the fewest there can be, and the 8 KiB
/// table this makes is two frames.
const LPI_ID_BITS: u32 = 13;
/// Frames of the property table, as an order: one byte per LPI.
const PROPERTY_ORDER: u8 = 1;
/// Frames the pending table is taken as, as an order. It needs 2 KiB but
/// must be 64 KiB aligned, so it is taken as a 64 KiB block and all but its
/// first frame given back.
const PENDING_ORDER: u8 = 4;
/// A property byte: the default priority in bits 7:2, bit 1 which the
/// architecture reserves as one, and bit 0, enabled.
const PROPERTY_ENABLED: u8 = 0xA0 | 0b10 | 1;

/// Event ID bits each device's ITT covers, and so the vectors there are.
const EVENT_BITS: u32 = 8;
/// Device ID bits the device table covers at most: a whole PCI segment.
const DEVICE_BITS: u32 = 16;
/// Devices the ITS can be told about.
const DEVICES: usize = 64;

/// What the kernel calls the first LPI: where the interrupt table's slots for
/// messages start.
const FIRST_NUMBER: u32 = 1024;
/// Vectors handed out, at most.
const VECTORS: u32 = 1 << EVENT_BITS;

const _: () = assert!(
    FIRST_NUMBER as usize + VECTORS as usize <= crate::irq::SLOTS,
    "the interrupt table has a slot for every vector"
);

/// Bytes of one command.
const COMMAND_BYTES: u64 = 32;
/// Command: map a device to its ITT.
const CMD_MAPD: u64 = 0x08;
/// Command: map a collection to a redistributor.
const CMD_MAPC: u64 = 0x09;
/// Command: map one of a device's events to an LPI and a collection.
const CMD_MAPTI: u64 = 0x0A;
/// Command: make the redistributor read an LPI's property byte again.
const CMD_INV: u64 = 0x0C;
/// Command: wait until every command before it has taken effect.
const CMD_SYNC: u64 = 0x05;
/// The one collection's identifier.
const COLLECTION: u64 = 0;

/// Register polls give up after this many reads, as in `gicv3`.
const POLL_LIMIT: u32 = 1_000_000;

/// One command: four doublewords.
type Command = [u64; 4];

/// The ITS, once [`init`] has brought it up, or why there is none.
static ITS: IrqSpinLock<Result<Its, &'static str>, crate::arch::Irq> =
    IrqSpinLock::new(Err("the machine describes no GICv3 ITS"));

/// The ITS and everything the kernel keeps to drive it.
#[derive(Debug)]
struct Its {
    /// The control frame, mapped.
    registers: Mmio,
    /// Physical address of the control frame.
    phys: u64,
    /// Physical address of the one-page command queue.
    commands: u64,
    /// Where the next command goes, in bytes from the queue's start.
    written: u64,
    /// Whether a command must be cleaned to the point of coherency before the
    /// ITS is told of it.
    clean_commands: bool,
    /// Physical address of the LPI property table.
    properties: u64,
    /// Whether a property byte must be cleaned before the redistributor is
    /// told to read it.
    clean_properties: bool,
    /// The collection's redistributor, as `MAPC` and `SYNC` name it.
    target: u64,
    /// The frames one ITT is taken as, as an order.
    itt_order: u8,
    /// Device IDs from here up are past the device table.
    device_limit: u64,
    /// Devices mapped so far, with their ITTs' physical addresses.
    devices: [(u32, u64); DEVICES],
    /// How many of `devices` are in use.
    mapped: usize,
    /// Which vectors are taken, one bit each.
    taken: [u64; VECTORS as usize / 64],
    /// Whether a command stalled or never finished, after which nothing more
    /// is queued.
    stopped: bool,
}

/// The kernel's number for GIC identifier `id`: its own below the LPIs, and
/// the slot [`allocate`] gave it for one of this driver's LPIs.
///
/// An LPI this driver did not hand out, which only a misprogrammed ITS could
/// raise, becomes a number past the interrupt table, and so arrives as
/// unclaimed rather than as some other device's interrupt.
pub(super) fn number(id: u32) -> u32 {
    match id.checked_sub(FIRST_LPI) {
        None => id,
        Some(event) if event < VECTORS => FIRST_NUMBER + event,
        Some(_) => u32::MAX,
    }
}

/// Bring up the ITS whose control frame is at `phys`, and LPIs on this core's
/// redistributor.
///
/// Called once, on the boot core, after `gicv3::init`. A failure leaves the
/// machine without MSI vectors and nothing else, and [`allocate`] says why to
/// whoever asks for one.
pub(super) fn init(phys: u64) -> Result<(), &'static str> {
    let brought_up = bring_up(phys);
    let result = brought_up.as_ref().map(|_| ()).map_err(|why| *why);
    *ITS.lock() = brought_up;
    result
}

/// Everything [`init`] does, returning the driver's state.
fn bring_up(phys: u64) -> Result<Its, &'static str> {
    let (properties, clean_properties, target) = enable_lpis()?;
    let window =
        crate::vmap::map_device(phys, ITS_WINDOW).map_err(|_| "could not map the GICv3 ITS")?;
    let registers = Mmio::at(window);
    quiesce(registers)?;

    let typer = read64(registers, GITS_TYPER);
    if typer & TYPER_PHYSICAL == 0 {
        return Err("the GICv3 ITS does not handle physical LPIs");
    }
    if (typer >> TYPER_EVENT_BITS_SHIFT & 0x1F) + 1 < u64::from(EVENT_BITS) {
        return Err("the GICv3 ITS decodes fewer event ID bits than the driver hands out");
    }
    let device_bits = ((typer >> TYPER_DEVICE_BITS_SHIFT & 0x1F) + 1).min(u64::from(DEVICE_BITS));
    let itt_entry = (typer >> TYPER_ITT_ENTRY_SHIFT & 0xF) + 1;

    let (commands, clean_commands) = command_queue(registers)?;
    let device_limit = program_tables(registers, device_bits)?;
    registers.write32(GITS_CTLR, CTLR_ENABLED);

    let mut its = Its {
        registers,
        phys,
        commands,
        written: 0,
        clean_commands,
        properties,
        clean_properties,
        target: if typer & TYPER_PTA != 0 {
            target.0 & !0xFFFF
        } else {
            target.1 << 16
        },
        itt_order: order_for(itt_entry << EVENT_BITS),
        device_limit,
        devices: [(0, 0); DEVICES],
        mapped: 0,
        taken: [0; VECTORS as usize / 64],
        stopped: false,
    };
    let mapc = [CMD_MAPC, 0, BASER_VALID | its.target | COLLECTION, 0];
    its.submit(&[mapc])?;
    Ok(its)
}

/// Give this core's redistributor its LPI property and pending tables and
/// turn LPIs on. Returns the property table's address, whether its bytes
/// must be cleaned before the redistributor reads them, and the
/// redistributor as a `MAPC` names it: its physical address and its
/// processor number.
fn enable_lpis() -> Result<(u64, bool, (u64, u64)), &'static str> {
    let typer = gicv3::distributor_typer();
    if typer & GICD_TYPER_LPIS == 0 {
        return Err("this GICv3's distributor does not support LPIs");
    }
    if typer >> GICD_TYPER_ID_BITS_SHIFT & 0x1F < LPI_ID_BITS {
        return Err("this GICv3 has too few interrupt identifier bits for LPIs");
    }
    let (rd_base, rd_phys) =
        gicv3::this_redistributor_both().ok_or("no GICv3 redistributor names this core")?;
    let rd = Mmio::at(rd_base);
    let rd_typer = read64(rd, GICR_TYPER);
    if rd_typer as u32 & GICR_TYPER_PLPIS == 0 {
        return Err("this core's GICv3 redistributor does not handle LPIs");
    }
    if rd.read32(GICR_CTLR) & GICR_CTLR_ENABLE_LPIS != 0 {
        return Err("firmware left LPIs enabled, so their tables cannot be replaced");
    }

    let properties = zeroed(PROPERTY_ORDER).ok_or("no frames for the LPI property table")?;
    let pending = pending_table().ok_or("no frames for the LPI pending table")?;
    cpu::clean_invalidate_to_poc(mm::direct_map(properties), PAGE_SIZE << PROPERTY_ORDER);
    cpu::clean_invalidate_to_poc(mm::direct_map(pending), PAGE_SIZE);

    let clean_properties = !set_attributes(
        rd,
        GICR_PROPBASER,
        properties | u64::from(LPI_ID_BITS),
        GICR_INNER_CACHE_SHIFT,
    );
    let _ = set_attributes(
        rd,
        GICR_PENDBASER,
        pending | PENDBASER_PTZ,
        GICR_INNER_CACHE_SHIFT,
    );
    cpu::dsb_ishst();
    rd.write32(GICR_CTLR, rd.read32(GICR_CTLR) | GICR_CTLR_ENABLE_LPIS);
    Ok((
        properties,
        clean_properties,
        (rd_phys, rd_typer >> 8 & 0xFFFF),
    ))
}

/// The pending table: one zeroed frame, 64 KiB aligned. Taken as a 64 KiB
/// block for the alignment, and the rest of the block given back.
fn pending_table() -> Option<u64> {
    let first = mm::allocate_frames(PENDING_ORDER)?;
    mm::zero_frame(first);
    if mm::split_frames(first, PENDING_ORDER) {
        for frame in first + 1..first + (1 << PENDING_ORDER) {
            let _ = mm::release_frame(frame);
        }
    }
    Some(first * PAGE_SIZE)
}

/// Write `value` to the 64-bit register at `register` as write-back,
/// inner-shareable memory, with the cacheability field at `cache_shift`, and
/// read back whether the device kept it shareable. If it did not, it cannot
/// snoop the caches: rewrite it as non-cacheable, and return false, so the
/// caller cleans what it writes there.
fn set_attributes(registers: Mmio, register: u64, value: u64, cache_shift: u32) -> bool {
    let wanted = value | CACHE_WRITE_BACK << cache_shift | SHARE_INNER << SHAREABILITY_SHIFT;
    write64(registers, register, wanted);
    let shareable = read64(registers, register) >> SHAREABILITY_SHIFT & 0b11 != 0;
    if !shareable {
        write64(registers, register, value | CACHE_NONE << cache_shift);
    }
    shareable
}

/// Turn the ITS off if firmware left it on, and wait until it is quiet: its
/// tables and queue may be replaced only then.
fn quiesce(registers: Mmio) -> Result<(), &'static str> {
    let control = registers.read32(GITS_CTLR);
    if control & CTLR_ENABLED != 0 {
        registers.write32(GITS_CTLR, control & !CTLR_ENABLED);
    }
    for _ in 0..POLL_LIMIT {
        if registers.read32(GITS_CTLR) & CTLR_QUIESCENT != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err("the GICv3 ITS never became quiescent")
}

/// Give the ITS a one-page command queue. Returns its address and whether a
/// command must be cleaned before the ITS reads it.
fn command_queue(registers: Mmio) -> Result<(u64, bool), &'static str> {
    let queue = zeroed(0).ok_or("no frame for the GICv3 ITS command queue")?;
    // A size field of zero is one page.
    let coherent = set_attributes(
        registers,
        GITS_CBASER,
        BASER_VALID | queue,
        ITS_INNER_CACHE_SHIFT,
    );
    registers.write32(GITS_CWRITER, 0);
    Ok((queue, !coherent))
}

/// Give the ITS its device table and, where it keeps collections in memory,
/// its collection table. Returns how many device IDs the device table covers.
fn program_tables(registers: Mmio, device_bits: u64) -> Result<u64, &'static str> {
    let mut device_limit = None;
    for index in 0..GITS_BASERS {
        let register = GITS_BASER + index * 8;
        match read64(registers, register) >> BASER_TYPE_SHIFT & 0b111 {
            BASER_TYPE_DEVICE => {
                device_limit = Some(program_table(registers, register, device_bits)?);
            }
            BASER_TYPE_COLLECTION => {
                let _ = program_table(registers, register, 0)?;
            }
            _ => {}
        }
    }
    device_limit.ok_or("the GICv3 ITS has no device table register")
}

/// Give the table register at `register` a zeroed table of `2^bits` entries,
/// or fewer if that many do not fit the register's size field. Returns how
/// many entries it covers.
fn program_table(registers: Mmio, register: u64, bits: u64) -> Result<u64, &'static str> {
    // The page size is the one field the kernel chooses that the ITS may
    // refuse: ask for 4 KiB, and take whatever it keeps.
    write64(registers, register, 0);
    let current = read64(registers, register);
    let page_field = current >> BASER_PAGE_SIZE_SHIFT & 0b11;
    let page = match page_field {
        0 => 0x1000,
        1 => 0x4000,
        _ => 0x1_0000,
    };
    let entry = (current >> BASER_ENTRY_SHIFT & 0x1F) + 1;
    let mut bits = bits;
    while bits > 0 && ((entry << bits).div_ceil(page)) > BASER_MAX_PAGES {
        bits -= 1;
    }
    let pages = (entry << bits).div_ceil(page);
    let bytes = pages * page;
    let order = order_for(bytes);
    let table = zeroed(order).ok_or("no frames for a GICv3 ITS table")?;
    if table >> 48 != 0 {
        return Err("a GICv3 ITS table landed above 48 bits of address");
    }
    let value = BASER_VALID | table | page_field << BASER_PAGE_SIZE_SHIFT | (pages - 1);
    let _ = set_attributes(registers, register, value, ITS_INNER_CACHE_SHIFT);
    cpu::clean_invalidate_to_poc(mm::direct_map(table), bytes);
    Ok((bytes / entry).min(1 << bits))
}

/// Take a vector for the device whose writes carry device ID `device`: map the
/// device if it is new, then one of its events to a free LPI on the boot
/// core, enabled, and say what the device writes to raise it.
///
/// # Errors
///
/// No ITS, a device ID past the device table, every vector or device slot
/// taken, or an ITS that stopped taking commands.
pub(super) fn allocate(device: u32) -> Result<Msi, &'static str> {
    let mut guard = ITS.lock();
    let its = guard.as_mut().map_err(|why| *why)?;
    if its.stopped {
        return Err("the GICv3 ITS stopped taking commands");
    }
    if u64::from(device) >= its.device_limit {
        return Err("the device's ID is past the GICv3 ITS's device table");
    }
    its.map_device(device)?;
    let event = its
        .free_event()
        .ok_or("every GICv3 ITS vector is allocated")?;
    // Taken before the commands: a vector whose commands failed is in an
    // unknown state, and is not handed out again.
    its.take(event);
    its.enable_property(event);
    let device_word = u64::from(device) << 32;
    let lpi = u64::from(FIRST_LPI + event);
    its.submit(&[
        [
            CMD_MAPTI | device_word,
            u64::from(event) | lpi << 32,
            COLLECTION,
            0,
        ],
        [CMD_INV | device_word, u64::from(event), 0, 0],
        [CMD_SYNC, 0, its.target, 0],
    ])?;
    Ok(Msi {
        number: FIRST_NUMBER + event,
        address: its.phys + GITS_TRANSLATER,
        data: event,
    })
}

/// The page a device's MSI writes land in, if there is an ITS.
pub(super) fn doorbell() -> Option<u64> {
    ITS.lock()
        .as_ref()
        .ok()
        .map(|its| its.phys + TRANSLATION_FRAME)
}

impl Its {
    /// The lowest vector nobody has.
    fn free_event(&self) -> Option<u32> {
        self.taken.iter().enumerate().find_map(|(word, &bits)| {
            let free = bits.trailing_ones();
            (free < u64::BITS).then(|| word as u32 * u64::BITS + free)
        })
    }

    /// Mark vector `event` taken.
    fn take(&mut self, event: u32) {
        if let Some(word) = self.taken.get_mut((event / u64::BITS) as usize) {
            *word |= 1 << (event % u64::BITS);
        }
    }

    /// Tell the ITS about `device` and give it an ITT, unless it already has
    /// one.
    fn map_device(&mut self, device: u32) -> Result<(), &'static str> {
        let known = self.devices.get(..self.mapped).unwrap_or(&[]);
        if known.iter().any(|&(mapped, _)| mapped == device) {
            return Ok(());
        }
        let slot = self
            .devices
            .get_mut(self.mapped)
            .ok_or("the GICv3 ITS has been told about as many devices as the driver keeps")?;
        let itt = zeroed(self.itt_order).ok_or("no frames for a device's GICv3 ITS table")?;
        cpu::clean_invalidate_to_poc(mm::direct_map(itt), PAGE_SIZE << self.itt_order);
        *slot = (device, itt);
        self.mapped += 1;
        let mapd = [
            CMD_MAPD | u64::from(device) << 32,
            u64::from(EVENT_BITS - 1),
            BASER_VALID | itt,
            0,
        ];
        self.submit(&[mapd])
    }

    /// Enable vector `event`'s LPI at the default priority, in the property
    /// table the redistributor reads.
    fn enable_property(&self, event: u32) {
        let at = mm::direct_map(self.properties + u64::from(event));
        // SAFETY: the property table is two frames this driver took for itself
        // and never gives back, and `event` is below `VECTORS`, well inside
        // it. Volatile because the redistributor reads the byte.
        unsafe { core::ptr::write_volatile(at as *mut u8, PROPERTY_ENABLED) };
        if self.clean_properties {
            cpu::clean_to_poc(at, 1);
        }
    }

    /// Queue `commands` and wait until the ITS has consumed them.
    ///
    /// The queue is empty whenever this starts -- every call waits for the
    /// last -- so a handful of commands always fit. An ITS that stalls or
    /// never catches up is taken out of service: the queue is then in a state
    /// nothing here could reason about.
    fn submit(&mut self, commands: &[Command]) -> Result<(), &'static str> {
        let queue = mm::direct_map(self.commands);
        for command in commands {
            for (word, &value) in (0_u64..).zip(command) {
                // SAFETY: `written` is a multiple of 32 below a page, so these
                // eight bytes are inside the queue's frame, which this driver
                // took for itself and never gives back. Volatile because the
                // ITS reads them.
                unsafe {
                    core::ptr::write_volatile((queue + self.written + word * 8) as *mut u64, value);
                }
            }
            self.written = (self.written + COMMAND_BYTES) % PAGE_SIZE;
        }
        if self.clean_commands {
            cpu::clean_to_poc(queue, PAGE_SIZE);
        } else {
            cpu::dsb_ishst();
        }
        self.registers.write32(GITS_CWRITER, self.written as u32);
        let why = 'wait: {
            for _ in 0..POLL_LIMIT {
                let read = self.registers.read32(GITS_CREADR);
                if read & 1 != 0 {
                    break 'wait "the GICv3 ITS stalled on a command";
                }
                if u64::from(read) == self.written {
                    return Ok(());
                }
                core::hint::spin_loop();
            }
            "the GICv3 ITS never finished its commands"
        };
        self.stopped = true;
        Err(why)
    }
}

/// `2^order` zeroed frames, by physical address.
fn zeroed(order: u8) -> Option<u64> {
    let first = mm::allocate_frames(order)?;
    for frame in first..first + (1 << order) {
        mm::zero_frame(frame);
    }
    Some(first * PAGE_SIZE)
}

/// The smallest order of frames holding `bytes`.
fn order_for(bytes: u64) -> u8 {
    bytes
        .div_ceil(PAGE_SIZE)
        .next_power_of_two()
        .trailing_zeros() as u8
}

/// Read a 64-bit register as two 32-bit halves, low first, which the
/// architecture allows for every 64-bit GIC register.
fn read64(registers: Mmio, at: u64) -> u64 {
    u64::from(registers.read32(at)) | u64::from(registers.read32(at + 4)) << 32
}

/// Write a 64-bit register as two 32-bit halves, low first.
fn write64(registers: Mmio, at: u64, value: u64) {
    registers.write32(at, value as u32);
    registers.write32(at + 4, (value >> 32) as u32);
}
