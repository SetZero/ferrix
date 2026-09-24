//! Tests against models of the hardware: the words `cmdstream.xml.h`
//! defines, a register file answering as the STM32MP157's GC400T and other
//! cores would, and a front end that runs a command buffer the way the core
//! parses one.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::format;
use std::vec;
use std::vec::Vec;

use crate::bringup::{self, Addressing, FSCALE_FULL, PULSE_EATER_BASE, RESET_PATIENCE, Stuck};
use crate::identity::{GC400T_STM32MP157, Identity};
use crate::regs::{common, fe, gl, hi, mc, mmu_v2, pm};
use crate::ring::{CommandMemory, Full, Ring};
use crate::stream::{self, OP_END, OP_LINK, OP_LOAD_STATE, OP_MASK, OP_NOP, OP_STALL, OP_WAIT};
use crate::{Clock, MICROSECOND, Registers};

// ---------------------------------------------------------------------------
// The command stream
// ---------------------------------------------------------------------------

#[test]
fn opcodes_are_the_front_ends() {
    // FE_OPCODE_* in bits 31:27.
    assert_eq!(OP_LOAD_STATE >> 27, 0x01, "FE_OPCODE_LOAD_STATE");
    assert_eq!(OP_END >> 27, 0x02, "FE_OPCODE_END");
    assert_eq!(OP_NOP >> 27, 0x03, "FE_OPCODE_NOP");
    assert_eq!(OP_WAIT >> 27, 0x07, "FE_OPCODE_WAIT");
    assert_eq!(OP_LINK >> 27, 0x08, "FE_OPCODE_LINK");
    assert_eq!(OP_STALL >> 27, 0x09, "FE_OPCODE_STALL");
    assert_eq!(OP_MASK, 0xF800_0000, "VIV_FE_*_HEADER_OP__MASK");
}

#[test]
fn load_state_counts_one_value_at_the_states_word_offset() {
    // OP_LOAD_STATE | COUNT(1) | OFFSET(0x3804 >> 2 = 0x0E01).
    assert_eq!(stream::load_state(gl::EVENT, 0x41), [0x0801_0E01, 0x41]);
    // The front end's own registers are states too, at word 0x0195.
    assert_eq!(
        stream::load_state(fe::COMMAND_ADDRESS, 0xC000_0000),
        [0x0801_0195, 0xC000_0000]
    );
}

#[test]
fn an_event_is_a_load_of_gl_event_from_the_pixel_engine() {
    // VIVS_GL_EVENT_EVENT_ID(1) | VIVS_GL_EVENT_FROM_PE.
    assert_eq!(stream::event(1), [0x0801_0E01, 0x0000_0041]);
    assert_eq!(stream::event(29), [0x0801_0E01, 0x0000_005D]);
    // An id past the field's five bits does not spill into FROM_FE.
    assert_eq!(stream::event(0x3F)[1], 0x0000_005F);
}

#[test]
fn a_pipe_select_semaphore_and_stall_are_the_ring_words() {
    assert_eq!(
        stream::pipe_select(common::PIPE_3D),
        [0x0801_0E00, 0x0000_0000]
    );
    assert_eq!(
        stream::pipe_select(common::PIPE_2D),
        [0x0801_0E00, 0x0000_0001]
    );
    let (fe, pe) = stream::FE_TO_PE;
    // SYNC_RECIPIENT_FE (1) from, SYNC_RECIPIENT_PE (7) to, in bits 12:8.
    assert_eq!(stream::semaphore(fe, pe), [0x0801_0E02, 0x0000_0701]);
    assert_eq!(stream::stall(fe, pe), [0x4800_0000, 0x0000_0701]);
}

#[test]
fn wait_link_end_and_nop_are_one_slot_each() {
    // VIV_FE_WAIT_HEADER_OP_WAIT | DELAY(200): etnaviv's wait when the core
    // clock is unknown.
    assert_eq!(stream::wait(200), [0x3800_00C8, 0]);
    assert_eq!(stream::wait(0xFFFF), [0x3800_FFFF, 0]);
    // VIV_FE_LINK_HEADER_OP_LINK | PREFETCH(2), then the address.
    assert_eq!(stream::link(2, 0xC123_4008), [0x4000_0002, 0xC123_4008]);
    assert_eq!(stream::end(), [0x1000_0000, 0]);
    assert_eq!(stream::nop(), [0x1800_0000, 0]);
}

// ---------------------------------------------------------------------------
// A register file
// ---------------------------------------------------------------------------

/// Registers that read back what was written, starting from `values`, and
/// remember every access in order. A read of a register in `faults` fails
/// the test, as it would take the core down.
#[derive(Debug, Default)]
struct Chip {
    values: BTreeMap<u32, u32>,
    faults: Vec<u32>,
    reads: RefCell<Vec<u32>>,
    writes: Vec<(u32, u32)>,
}

impl Chip {
    fn with(values: &[(u32, u32)]) -> Chip {
        Chip {
            values: values.iter().copied().collect(),
            ..Chip::default()
        }
    }

    /// The STM32MP157's GC400T as etnaviv's database describes it, with a
    /// date and time no board has been seen to give.
    fn gc400t() -> Chip {
        let known = GC400T_STM32MP157;
        let mut values = vec![
            (hi::CHIP_IDENTITY, 0),
            (hi::CHIP_MODEL, known.model),
            (hi::CHIP_REV, known.revision),
            (hi::CHIP_DATE, 0x2016_1103),
            (hi::CHIP_TIME, 0x0015_5220),
            (hi::CHIP_PRODUCT_ID, known.product),
            (hi::CHIP_CUSTOMER_ID, known.customer),
            (hi::CHIP_ECO_ID, known.eco),
            (hi::CHIP_FEATURE, known.features.major),
        ];
        values.extend(
            hi::CHIP_MINOR_FEATURES
                .iter()
                .copied()
                .zip(known.features.minor),
        );
        Chip::with(&values)
    }

    fn read_offsets(&self) -> Vec<u32> {
        self.reads.borrow().clone()
    }
}

impl Registers for Chip {
    fn read32(&self, offset: u32) -> u32 {
        assert!(
            !self.faults.contains(&offset),
            "read of {offset:#x}, which faults"
        );
        self.reads.borrow_mut().push(offset);
        self.values.get(&offset).copied().unwrap_or(0)
    }

    fn write32(&mut self, offset: u32, value: u32) {
        self.writes.push((offset, value));
        let _ = self.values.insert(offset, value);
    }
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

#[test]
fn the_board_core_reads_as_etnavivs_gc400t() {
    let chip = Chip::gc400t();
    let identity = Identity::read(&chip);
    assert_eq!(identity.model(), 0x400);
    assert_eq!(identity.revision, 0x4652);
    assert_eq!(identity.known(), Some(&GC400T_STM32MP157));
    let features = identity.features();
    assert!(features.pipe_3d(), "a GC400 draws in 3D");
    assert!(!features.pipe_2d(), "and has no 2D pipe");
    assert!(!features.mc20(), "its memory controller is 1.0");
    assert!(features.mmu_v2(), "and its MMU version 2");
    assert!(!features.dynamic_frequency_scaling());
    assert_eq!(Addressing::of(&features), Addressing::Physical);
    assert_eq!(
        format!("{identity}"),
        "model 0x400 revision 0x4652 date 0x20161103 time 0x00155220 product 0x70001 \
         customer 0x100 eco 0x0 identity 0x00000000 features 0xa0e9e004 minor 0xe1299fff \
         0xbe13b219 0xce110010 0x08000001 0x00020102 0x00120000"
    );
}

#[test]
fn the_database_overrides_what_the_registers_say_of_a_known_core() {
    let mut chip = Chip::gc400t();
    // A core that under-reports: no MMU version bit.
    let minor1 = hi::CHIP_MINOR_FEATURES[1];
    let _ = chip.values.insert(minor1, 0);
    let identity = Identity::read(&chip);
    assert_eq!(identity.features.minor[1], 0, "the register is kept");
    assert!(identity.features().mmu_v2(), "the database's word is used");
}

#[test]
fn an_unknown_core_is_driven_by_its_registers() {
    let mut chip = Chip::gc400t();
    let _ = chip.values.insert(hi::CHIP_CUSTOMER_ID, 0x200);
    let _ = chip.values.insert(hi::CHIP_MINOR_FEATURES[1], 0);
    let identity = Identity::read(&chip);
    assert_eq!(identity.known(), None);
    assert!(!identity.features().mmu_v2());
    assert_eq!(
        Addressing::of(&identity.features()),
        Addressing::Window { base: 0x8000_0000 }
    );
}

#[test]
fn a_gc400_family_model_compares_as_a_gc400_but_a_gc420_does_not() {
    let mut chip = Chip::gc400t();
    let _ = chip.values.insert(hi::CHIP_MODEL, 0x0405);
    let identity = Identity::read(&chip);
    assert_eq!(identity.model, 0x405, "printed as the register said");
    assert_eq!(identity.model(), 0x400);
    assert!(identity.known().is_some());
    let _ = chip.values.insert(hi::CHIP_MODEL, 0x0420);
    assert_eq!(Identity::read(&chip).model(), 0x420);
}

#[test]
fn minor_words_past_the_first_are_read_only_when_the_first_says_so() {
    let mut chip = Chip::gc400t();
    let _ = chip.values.insert(hi::CHIP_MINOR_FEATURES[0], 0x0000_0001);
    let identity = Identity::read(&chip);
    assert_eq!(identity.features.minor, [1, 0, 0, 0, 0, 0]);
    let reads = chip.read_offsets();
    for offset in &hi::CHIP_MINOR_FEATURES[1..] {
        assert!(!reads.contains(offset), "{offset:#x} was read");
    }
}

#[test]
fn the_oldest_family_is_identified_from_one_register() {
    let chip = Chip::with(&[
        (hi::CHIP_IDENTITY, 0x0100_1000),
        (hi::CHIP_FEATURE, 0x0000_0004),
    ]);
    let identity = Identity::read(&chip);
    assert_eq!((identity.model, identity.revision), (0x500, 1));
    assert_eq!(identity.features.minor, [0; 6], "a GC500 rev 1 has none");
    assert!(!chip.read_offsets().contains(&hi::CHIP_MODEL));
}

#[test]
fn a_gc600_of_revision_0x19_is_not_asked_its_product_or_eco() {
    let mut chip = Chip::with(&[(hi::CHIP_MODEL, 0x600), (hi::CHIP_REV, 0x19)]);
    chip.faults = vec![hi::CHIP_PRODUCT_ID, hi::CHIP_ECO_ID];
    let identity = Identity::read(&chip);
    assert_eq!(identity.model(), 0x600);
    assert_eq!(
        identity.idle_mask(),
        0xFF,
        "only the eight modules a GC600 has"
    );
}

#[test]
fn a_gc400_is_idle_when_every_module_bit_but_the_bus_low_power_one_is_set() {
    let identity = Identity::read(&Chip::gc400t());
    assert_eq!(identity.idle_mask(), 0x7FFF_FFFF);
}

// ---------------------------------------------------------------------------
// Bring-up
// ---------------------------------------------------------------------------

/// A clock that moves a microsecond each time it is read, and as far as it
/// is asked to sleep.
#[derive(Debug, Default)]
struct FakeClock {
    now: Cell<u64>,
    slept: Vec<u64>,
}

impl Clock for FakeClock {
    fn now_nanos(&self) -> u64 {
        self.now.set(self.now.get() + MICROSECOND);
        self.now.get()
    }

    fn sleep_nanos(&mut self, nanos: u64) {
        self.slept.push(nanos);
        self.now.set(self.now.get() + nanos);
    }
}

/// The GC400T, reporting `idle` in `HI_IDLE_STATE` and `mmu` in
/// `MMUv2_CONTROL` whenever they are read.
fn reset_core(idle: u32, mmu: u32) -> Chip {
    let mut chip = Chip::gc400t();
    let _ = chip.values.insert(hi::IDLE_STATE, idle);
    let _ = chip.values.insert(mmu_v2::CONTROL, mmu);
    chip
}

/// A core whose `HI_CLOCK_CONTROL` reads back what was written with the
/// pipes' idle bits, `pipes`, set as the hardware sets them.
struct IdlePipes {
    chip: Chip,
    pipes: u32,
}

impl Registers for IdlePipes {
    fn read32(&self, offset: u32) -> u32 {
        let value = self.chip.read32(offset);
        if offset == hi::CLOCK_CONTROL {
            value | self.pipes
        } else {
            value
        }
    }

    fn write32(&mut self, offset: u32, value: u32) {
        self.chip.write32(offset, value);
    }
}

#[test]
fn one_reset_attempt_writes_etnavivs_sequence() {
    let identity = Identity::read(&Chip::gc400t());
    let pipes = hi::CLOCK_CONTROL_IDLE_3D | hi::CLOCK_CONTROL_IDLE_2D;
    let mut core = IdlePipes {
        chip: reset_core(0x7FFF_FFFF, 0),
        pipes,
    };
    let mut clock = FakeClock::default();
    let attempts = bringup::reset(&mut core, &mut clock, &identity).expect("a clean reset");
    assert_eq!(attempts, 1);
    let full = hi::fscale(FSCALE_FULL);
    assert_eq!(full, 0x100, "64 in bits 8:2");
    let isolated = full | hi::CLOCK_CONTROL_ISOLATE_GPU;
    let resetting = isolated | hi::CLOCK_CONTROL_SOFT_RESET;
    let after = full | pipes;
    let expected = vec![
        (pm::POWER_CONTROLS, 0),
        (pm::PULSE_EATER, PULSE_EATER_BASE | pm::PULSE_EATER_UNK17),
        (
            pm::PULSE_EATER,
            PULSE_EATER_BASE | pm::PULSE_EATER_UNK17 | pm::PULSE_EATER_DISABLE,
        ),
        (hi::CLOCK_CONTROL, full | hi::CLOCK_CONTROL_FSCALE_CMD_LOAD),
        (hi::CLOCK_CONTROL, full),
        (hi::CLOCK_CONTROL, isolated),
        (hi::CLOCK_CONTROL, resetting),
        (hi::CLOCK_CONTROL, isolated),
        (hi::CLOCK_CONTROL, full),
        // Debug registers on: the read-back less DISABLE_DEBUG_REGISTERS.
        (hi::CLOCK_CONTROL, after),
        // Full speed, loaded again as etnaviv_gpu_update_clock does.
        (hi::CLOCK_CONTROL, after | hi::CLOCK_CONTROL_FSCALE_CMD_LOAD),
        (hi::CLOCK_CONTROL, after),
    ];
    assert_eq!(core.chip.writes, expected);
    assert_eq!(clock.slept, vec![bringup::SOFT_RESET_HOLD]);
    assert!(
        core.chip.read_offsets().contains(&mmu_v2::CONTROL),
        "an MMUv2 core's MMU is checked off"
    );
}

#[test]
fn a_core_that_stays_busy_is_given_up_on_after_a_second() {
    let identity = Identity::read(&Chip::gc400t());
    let mut core = IdlePipes {
        chip: reset_core(0x7FFF_FFFE, 0),
        pipes: hi::CLOCK_CONTROL_IDLE_3D | hi::CLOCK_CONTROL_IDLE_2D,
    };
    let mut clock = FakeClock::default();
    let failed = bringup::reset(&mut core, &mut clock, &identity).expect_err("the FE is busy");
    assert!(failed.attempts > 1, "tried again");
    assert!(clock.now.get() >= RESET_PATIENCE, "for the whole second");
    assert_eq!(failed.idle, 0x7FFF_FFFE);
    assert!(!failed.mmu_on);
}

#[test]
fn a_pipe_that_stays_busy_or_an_mmu_left_on_fails_the_reset() {
    let identity = Identity::read(&Chip::gc400t());
    let mut busy_2d = IdlePipes {
        chip: reset_core(0x7FFF_FFFF, 0),
        pipes: hi::CLOCK_CONTROL_IDLE_3D,
    };
    let failed =
        bringup::reset(&mut busy_2d, &mut FakeClock::default(), &identity).expect_err("2D busy");
    assert!(format!("{failed}").contains("2D not idle"), "{failed}");

    let mut mmu_on = IdlePipes {
        chip: reset_core(0x7FFF_FFFF, mmu_v2::CONTROL_ENABLE),
        pipes: hi::CLOCK_CONTROL_IDLE_3D | hi::CLOCK_CONTROL_IDLE_2D,
    };
    let failed =
        bringup::reset(&mut mmu_on, &mut FakeClock::default(), &identity).expect_err("MMU on");
    assert!(failed.mmu_on);
    assert!(format!("{failed}").contains("MMU still on"), "{failed}");
}

#[test]
fn the_axi_bus_idling_does_not_count_as_busy() {
    let identity = Identity::read(&Chip::gc400t());
    let mut core = IdlePipes {
        chip: reset_core(0x7FFF_FFFF, 0),
        pipes: hi::CLOCK_CONTROL_IDLE_3D | hi::CLOCK_CONTROL_IDLE_2D,
    };
    let _ = core.chip.values.insert(hi::IDLE_STATE, 0xFFFF_FFFF);
    assert_eq!(
        bringup::reset(&mut core, &mut FakeClock::default(), &identity),
        Ok(1)
    );
}

#[test]
fn init_sets_the_bus_attributes_the_pulse_eater_and_every_interrupt() {
    let mut chip = Chip::default();
    bringup::init(&mut chip);
    assert_eq!(
        chip.writes,
        vec![
            (hi::AXI_CONFIG, 0x0000_2200),
            (pm::PULSE_EATER, 0x0159_0880),
            (hi::INTR_ENBL, 0xFFFF_FFFF),
        ]
    );
}

#[test]
fn a_physical_core_is_given_physical_addresses_and_no_window() {
    let mut chip = Chip::default();
    Addressing::Physical.program(&mut chip);
    assert!(chip.writes.is_empty());
    assert_eq!(
        Addressing::Physical.gpu_address(0xC012_3000),
        Some(0xC012_3000)
    );
    assert_eq!(Addressing::Physical.gpu_address(0x1_0000_0000), None);
}

#[test]
fn a_window_core_sees_the_boards_memory_through_a_window_at_two_gib() {
    let window = Addressing::Window { base: 0x8000_0000 };
    let mut chip = Chip::default();
    window.program(&mut chip);
    let expected: Vec<(u32, u32)> = mc::MEMORY_BASE_ADDRS
        .iter()
        .map(|&offset| (offset, 0x8000_0000))
        .collect();
    assert_eq!(chip.writes, expected);
    // The DK board's memory, 0xC000_0000 to 0xE000_0000.
    assert_eq!(window.gpu_address(0xC000_0000), Some(0x4000_0000));
    assert_eq!(window.gpu_address(0xDFFF_F000), Some(0x5FFF_F000));
    assert_eq!(window.gpu_address(0x7FFF_F000), None, "below the window");
    assert_eq!(window.gpu_address(0x1_0000_0000), None, "past it");
}

#[test]
fn the_front_end_is_started_address_first() {
    let mut chip = Chip::default();
    bringup::start_front_end(&mut chip, 0xC800_0000, 2);
    assert_eq!(
        chip.writes,
        vec![
            (fe::COMMAND_ADDRESS, 0xC800_0000),
            (fe::COMMAND_CONTROL, 0x0001_0002),
        ]
    );
}

#[test]
fn a_stuck_front_end_is_described_by_its_state_name() {
    let chip = Chip::with(&[
        (hi::IDLE_STATE, 0x7FFF_FFFE),
        (fe::DMA_DEBUG_STATE, 0x0000_0812),
        (fe::DMA_ADDRESS, 0xC800_0000),
    ]);
    let stuck = Stuck::read(&chip);
    let line = format!("{stuck}");
    assert!(line.contains("(FE busy)"), "{line}");
    assert!(line.contains("(WAIT)"), "{line}");
    assert!(line.contains("address 0xc8000000"), "{line}");
    assert!(chip.read_offsets().contains(&hi::INTR_ACKNOWLEDGE));
}

// ---------------------------------------------------------------------------
// The ring, run by a model of the front end
// ---------------------------------------------------------------------------

/// One access to the command page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Access {
    Write(usize, u32),
    Barrier,
}

/// A command page that logs every write and barrier.
#[derive(Debug)]
struct Page {
    words: Vec<u32>,
    log: RefCell<Vec<Access>>,
}

impl Page {
    fn new(words: usize) -> Page {
        Page {
            words: vec![0; words],
            log: RefCell::new(Vec::new()),
        }
    }
}

impl CommandMemory for Page {
    fn words(&self) -> usize {
        self.words.len()
    }

    fn write32(&mut self, index: usize, value: u32) {
        self.words[index] = value;
        self.log.borrow_mut().push(Access::Write(index, value));
    }

    fn read32(&self, index: usize) -> u32 {
        self.words[index]
    }

    fn barrier(&self) {
        self.log.borrow_mut().push(Access::Barrier);
    }
}

/// The front end, as far as these commands go: it parses a slot at a time
/// from `at`, loads states, spins on WAIT, follows LINK and stops at END.
#[derive(Debug)]
struct FrontEnd {
    base: u32,
    at: u32,
    ended: bool,
    events: Vec<u32>,
    states: Vec<(u32, u32)>,
    prefetches: Vec<u16>,
}

impl FrontEnd {
    fn started(base: u32, (address, prefetch): (u32, u16)) -> FrontEnd {
        FrontEnd {
            base,
            at: address,
            ended: false,
            events: Vec::new(),
            states: Vec::new(),
            prefetches: vec![prefetch],
        }
    }

    fn word(&self, page: &Page, address: u32) -> u32 {
        let index = (address - self.base) as usize / 4;
        page.read32(index)
    }

    /// Parse up to `slots` commands.
    fn run(&mut self, page: &Page, slots: usize) {
        for _ in 0..slots {
            if self.ended {
                return;
            }
            let header = self.word(page, self.at);
            let argument = self.word(page, self.at + 4);
            match header & OP_MASK {
                OP_LOAD_STATE => {
                    assert_eq!((header >> 16) & 0x3FF, 1, "one value");
                    let state = (header & 0xFFFF) << 2;
                    self.states.push((state, argument));
                    if state == gl::EVENT {
                        assert_ne!(argument & gl::EVENT_FROM_PE, 0, "from the PE");
                        self.events.push(argument & gl::EVENT_EVENT_ID_MASK);
                    }
                    self.at += 8;
                }
                OP_WAIT | OP_NOP => self.at += 8,
                OP_LINK => {
                    self.prefetches.push((header & 0xFFFF) as u16);
                    self.at = argument;
                }
                OP_END => self.ended = true,
                other => panic!("opcode {other:#x} at {:#x}", self.at),
            }
        }
    }
}

const BASE: u32 = 0xC800_0000;
const PAGE_WORDS: usize = 1024;

#[test]
fn the_idle_loop_spins_on_its_wait_and_raises_nothing() {
    let ring = Ring::new(Page::new(PAGE_WORDS), BASE, 200).expect("room");
    assert_eq!(ring.start(), (BASE, 2), "the WAIT and its LINK");
    let mut front = FrontEnd::started(BASE, ring.start());
    front.run(ring.memory(), 101);
    assert!(!front.ended && front.events.is_empty());
    assert_eq!(front.at, BASE + 8, "on the LINK after 101 slots");
    assert_eq!(ring.waiting_at(), BASE);
}

#[test]
fn a_queued_block_raises_its_event_once_and_the_front_end_waits_after_it() {
    let mut ring = Ring::new(Page::new(PAGE_WORDS), BASE, 200).expect("room");
    let mut front = FrontEnd::started(BASE, ring.start());
    front.run(ring.memory(), 10);
    ring.queue_event(&[stream::pipe_select(common::PIPE_3D)], 1)
        .expect("room");
    front.run(ring.memory(), 50);
    assert_eq!(front.events, vec![1]);
    assert_eq!(
        front.states.first(),
        Some(&(gl::PIPE_SELECT, common::PIPE_3D)),
        "the pipe is selected before the event"
    );
    // The block is the pipe select, the event, and a WAIT/LINK: four slots,
    // all prefetched by the LINK that jumped to it.
    assert!(front.prefetches.contains(&4), "{:?}", front.prefetches);
    assert_eq!(ring.waiting_at(), BASE + 16 + 16, "the block's WAIT");
    assert!(
        front.at == ring.waiting_at() || front.at == ring.waiting_at() + 8,
        "spinning in the new loop at {:#x}",
        front.at
    );
}

#[test]
fn two_blocks_run_in_order_and_stop_ends_the_front_end() {
    let mut ring = Ring::new(Page::new(PAGE_WORDS), BASE, 200).expect("room");
    let mut front = FrontEnd::started(BASE, ring.start());
    ring.queue_event(&[stream::pipe_select(common::PIPE_3D)], 1)
        .expect("room");
    front.run(ring.memory(), 50);
    ring.queue_event(&[], 2).expect("room");
    front.run(ring.memory(), 50);
    assert_eq!(front.events, vec![1, 2]);
    assert!(front.prefetches.contains(&3), "event, WAIT, LINK");
    assert!(!front.ended);
    ring.stop();
    front.run(ring.memory(), 50);
    assert!(front.ended, "at the END that replaced the last WAIT");
    assert_eq!(front.at, ring.waiting_at());
    assert_eq!(front.events, vec![1, 2], "nothing twice");
}

#[test]
fn a_block_queued_before_the_front_end_looks_is_still_run() {
    let mut ring = Ring::new(Page::new(PAGE_WORDS), BASE, 200).expect("room");
    ring.queue_event(&[], 5).expect("room");
    ring.queue_event(&[], 6).expect("room");
    let mut front = FrontEnd::started(BASE, ring.start());
    front.run(ring.memory(), 50);
    assert_eq!(front.events, vec![5, 6]);
}

#[test]
fn a_splice_writes_the_argument_then_the_header_between_barriers() {
    let mut ring = Ring::new(Page::new(PAGE_WORDS), BASE, 200).expect("room");
    ring.memory().log.borrow_mut().clear();
    ring.queue_event(&[], 3).expect("room");
    let log = ring.memory().log.borrow().clone();
    // The block, slots 2 to 4, is written first; then the WAIT in slot 0.
    let tail = &log[log.len() - 5..];
    assert_eq!(
        tail,
        &[
            Access::Barrier,
            Access::Write(1, BASE + 16),
            Access::Barrier,
            Access::Write(0, 0x4000_0003),
            Access::Barrier,
        ]
    );
    assert!(
        log[..log.len() - 5]
            .iter()
            .all(|access| matches!(access, Access::Write(index, _) if *index >= 4)),
        "nothing of slot 0 or 1 before the splice: {log:?}"
    );
}

#[test]
fn a_ring_with_no_room_says_so_and_changes_nothing() {
    // Room for the idle loop and one slot more.
    let mut ring = Ring::new(Page::new(6), BASE, 200).expect("room for the loop");
    let before = ring.memory().words.clone();
    assert_eq!(ring.queue_event(&[], 1), Err(Full));
    assert_eq!(ring.memory().words, before);
    assert_eq!(ring.waiting_at(), BASE);
    assert!(Ring::new(Page::new(2), BASE, 200).is_err());
}
