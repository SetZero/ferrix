//! From a core fresh out of the RCC's reset to one whose front end can be
//! started: the soft reset, the initialisation, and how the core addresses
//! memory.
//!
//! The steps are etnaviv's, from `etnaviv_hw_reset`,
//! `etnaviv_gpu_update_clock` and `etnaviv_gpu_hw_init`, cut down to what a
//! GC400 needs; each function says what it leaves out and why.

use core::fmt;

use crate::identity::{Features, Identity};
use crate::regs::{fe, hi, mc, mmu_v2, pm};
use crate::{Clock, MICROSECOND, MILLISECOND, Registers};

/// The clock scaler at full speed: 64 of 64ths. etnaviv writes
/// `1 << (6 - freq_scale)`, and nothing here throttles, so its scale is
/// always 0.
pub const FSCALE_FULL: u32 = 64;

/// `PM_PULSE_EATER`'s value for a core whose register cannot be read back,
/// which etnaviv took from Vivante's own driver.
pub const PULSE_EATER_BASE: u32 = 0x0159_0880;

/// How long a reset may take before it is given up: etnaviv hopes for
/// under a second, and tries again until then.
pub const RESET_PATIENCE: u64 = 1_000 * MILLISECOND;

/// How long `SOFT_RESET` is held: etnaviv sleeps 10 to 20 µs.
pub const SOFT_RESET_HOLD: u64 = 20 * MICROSECOND;

/// The cache attribute etnaviv gives the core's reads and writes, AXI's
/// "cacheable, do not allocate".
pub const AXI_CACHE: u32 = 2;

/// Why a reset did not take: the idle state and clock control as they
/// stood at the last attempt, which say which of its checks failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResetFailed {
    /// `HI_IDLE_STATE`.
    pub idle: u32,
    /// `HI_CLOCK_CONTROL`.
    pub control: u32,
    /// Whether the MMU was still on.
    pub mmu_on: bool,
    /// How many times the reset was tried.
    pub attempts: u32,
}

impl fmt::Display for ResetFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "after {} attempts: idle state {:#010x}, clock control {:#010x}, 3D {}idle, 2D {}idle{}",
            self.attempts,
            self.idle,
            self.control,
            if self.control & hi::CLOCK_CONTROL_IDLE_3D != 0 {
                ""
            } else {
                "not "
            },
            if self.control & hi::CLOCK_CONTROL_IDLE_2D != 0 {
                ""
            } else {
                "not "
            },
            if self.mmu_on { ", MMU still on" } else { "" }
        )
    }
}

/// Write `clock` to `HI_CLOCK_CONTROL` so that its `FSCALE_VAL` takes:
/// once with `FSCALE_CMD_LOAD`, whose edge latches the scaler, and once
/// without (etnaviv's `etnaviv_gpu_load_clock`).
pub fn load_clock(registers: &mut impl Registers, clock: u32) {
    registers.write32(hi::CLOCK_CONTROL, clock | hi::CLOCK_CONTROL_FSCALE_CMD_LOAD);
    registers.write32(hi::CLOCK_CONTROL, clock);
}

/// Soft-reset the core until it comes back idle, as `etnaviv_hw_reset`
/// does, then set its clock to full speed as `etnaviv_gpu_update_clock`
/// does. The number of attempts it took, or what the core looked like
/// when [`RESET_PATIENCE`] ran out.
///
/// Each attempt:
///
/// 1. turns module clock gating off, and reads the register back so the
///    write has landed before the next: a gated module does not reset;
/// 2. turns the pulse eater off, the same way, so the core's clock is not
///    being thinned while it resets;
/// 3. loads the clock scaler at full speed: the reset runs on that clock;
/// 4. isolates the core from the bus, so nothing it was doing reaches
///    memory half done, and holds `SOFT_RESET` for [`SOFT_RESET_HOLD`];
/// 5. lets go of the reset, then of the isolation;
/// 6. checks every module is idle, both pipes say they are idle, and --
///    on a core with a version-2 MMU -- that the MMU came back off, since
///    until it is programmed it would translate through garbage. Any that
///    fails is another attempt;
/// 7. makes the debug registers readable, which `FE_DMA_*` are, for the
///    diagnosis of a front end that does not finish.
///
/// Left out: the security-mode reset through `MMUv2_AHB_CONTROL`, which is
/// for cores with the security block, and a GC400 has none.
pub fn reset<R: Registers, C: Clock>(
    registers: &mut R,
    clock: &mut C,
    identity: &Identity,
) -> Result<u32, ResetFailed> {
    let features = identity.features();
    let idle_mask = identity.idle_mask();
    let until = clock.now_nanos().saturating_add(RESET_PATIENCE);
    let mut failed = ResetFailed {
        idle: 0,
        control: 0,
        mmu_on: false,
        attempts: 0,
    };
    loop {
        failed.attempts += 1;
        match attempt(registers, clock, &features, idle_mask) {
            Ok(()) => break,
            Err((idle, control, mmu_on)) => {
                failed.idle = idle;
                failed.control = control;
                failed.mmu_on = mmu_on;
            }
        }
        if clock.now_nanos() >= until {
            return Err(failed);
        }
    }
    // Full speed. A core whose clock the clock controller scales is already
    // at the rate the kernel found; its FSCALE_VAL is not the knob.
    if !features.dynamic_frequency_scaling() {
        let control = registers.read32(hi::CLOCK_CONTROL) & !hi::CLOCK_CONTROL_FSCALE_VAL_MASK;
        load_clock(registers, control | hi::fscale(FSCALE_FULL));
    }
    Ok(failed.attempts)
}

/// One attempt of [`reset`]: nothing, or the idle state, the clock control
/// and whether the MMU was on, as the check that failed saw them.
fn attempt<R: Registers, C: Clock>(
    registers: &mut R,
    clock: &mut C,
    features: &Features,
    idle_mask: u32,
) -> Result<(), (u32, u32, bool)> {
    registers.write32(pm::POWER_CONTROLS, 0);
    let _ = registers.read32(pm::POWER_CONTROLS);
    let mut pulse_eater = PULSE_EATER_BASE | pm::PULSE_EATER_UNK17;
    registers.write32(pm::PULSE_EATER, pulse_eater);
    pulse_eater |= pm::PULSE_EATER_DISABLE;
    registers.write32(pm::PULSE_EATER, pulse_eater);
    let _ = registers.read32(pm::PULSE_EATER);

    let mut control = hi::fscale(FSCALE_FULL);
    load_clock(registers, control);
    control |= hi::CLOCK_CONTROL_ISOLATE_GPU;
    registers.write32(hi::CLOCK_CONTROL, control);
    control |= hi::CLOCK_CONTROL_SOFT_RESET;
    registers.write32(hi::CLOCK_CONTROL, control);
    clock.sleep_nanos(SOFT_RESET_HOLD);
    control &= !hi::CLOCK_CONTROL_SOFT_RESET;
    registers.write32(hi::CLOCK_CONTROL, control);
    control &= !hi::CLOCK_CONTROL_ISOLATE_GPU;
    registers.write32(hi::CLOCK_CONTROL, control);

    let idle = registers.read32(hi::IDLE_STATE);
    let control = registers.read32(hi::CLOCK_CONTROL);
    let mmu_on =
        features.mmu_v2() && registers.read32(mmu_v2::CONTROL) & mmu_v2::CONTROL_ENABLE != 0;
    let pipes = hi::CLOCK_CONTROL_IDLE_3D | hi::CLOCK_CONTROL_IDLE_2D;
    if idle & idle_mask != idle_mask || control & pipes != pipes || mmu_on {
        return Err((idle, control, mmu_on));
    }
    registers.write32(
        hi::CLOCK_CONTROL,
        control & !hi::CLOCK_CONTROL_DISABLE_DEBUG_REGISTERS,
    );
    Ok(())
}

/// Program what a reset core needs before its front end runs, as
/// `etnaviv_gpu_hw_init` does for a GC400:
///
/// 1. the bus master's cache attributes to [`AXI_CACHE`] for reads and
///    writes, which etnaviv sets on every core because the reset value
///    locked up an i.MX6's interconnect;
/// 2. the pulse eater to [`PULSE_EATER_BASE`], undoing the reset's
///    disabling of it: a GC400 of this revision is none of the cores
///    etnaviv gives another value;
/// 3. every interrupt enabled: each event raised gets its bit in
///    `HI_INTR_ACKNOWLEDGE` and the line, as do bus errors and MMU faults.
///
/// Left out: module-level clock gating, which etnaviv turns on here with
/// per-revision exceptions (the primitive assembler, the pixel engine, an
/// unnamed bit 15) that are fixes for hangs. It saves power and nothing
/// else, the reset left it off, and it stays off until the core has been
/// seen running on the board without it. The GC320's memory debug patch,
/// the GC2000's bus configuration and the security block's AHB access are
/// other cores'.
pub fn init(registers: &mut impl Registers) {
    registers.write32(
        hi::AXI_CONFIG,
        ((AXI_CACHE << hi::AXI_CONFIG_AWCACHE_SHIFT) & hi::AXI_CONFIG_AWCACHE_MASK)
            | ((AXI_CACHE << hi::AXI_CONFIG_ARCACHE_SHIFT) & hi::AXI_CONFIG_ARCACHE_MASK),
    );
    registers.write32(pm::PULSE_EATER, PULSE_EATER_BASE);
    let _ = registers.read32(pm::PULSE_EATER);
    registers.write32(hi::INTR_ENBL, !0);
}

/// How the core turns an address it is given into a physical one, before
/// any MMU of its own is set up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Addressing {
    /// A version-2 MMU comes out of reset disabled, and until it is turned
    /// on every address is physical. etnaviv relies on exactly this: the
    /// first stream it runs on such a core, the one that sets the MMU up,
    /// is fetched from its physical address.
    Physical,
    /// A version-1 MMU translates only addresses from 2 GiB up; below that
    /// is a linear window whose start in physical memory each client's
    /// `MC_MEMORY_BASE_ADDR_*` sets, and a command buffer has to be in it.
    /// etnaviv puts the window at 2 GiB when the command buffer is above
    /// 2 GiB, as all of a DK board's memory (`0xC000_0000` up) is, so the
    /// window covers `0x8000_0000` to `0xFFFF_FFFF` and a GPU address is the
    /// physical one less `base`.
    Window {
        /// The window's physical start.
        base: u32,
    },
}

/// Where [`Addressing::Window`] is put: 2 GiB.
pub const WINDOW_BASE: u32 = 0x8000_0000;
/// How much the window covers: the addresses below the MMU's 2 GiB.
const WINDOW_BYTES: u64 = 0x8000_0000;

impl Addressing {
    /// The addressing a core with `features` has out of reset.
    #[must_use]
    pub const fn of(features: &Features) -> Addressing {
        if features.mmu_v2() {
            Addressing::Physical
        } else {
            Addressing::Window { base: WINDOW_BASE }
        }
    }

    /// Program it: nothing for [`Addressing::Physical`], every client's
    /// window base for [`Addressing::Window`], as `etnaviv_iommuv1_restore`
    /// does. (That also points each client at an MMU page table; nothing
    /// here uses an address the MMU would translate, so there is none.)
    pub fn program(&self, registers: &mut impl Registers) {
        if let Addressing::Window { base } = *self {
            for offset in mc::MEMORY_BASE_ADDRS {
                registers.write32(offset, base);
            }
        }
    }

    /// The address the core reaches `physical` by, if it can without an MMU.
    #[must_use]
    pub fn gpu_address(&self, physical: u64) -> Option<u32> {
        match *self {
            Addressing::Physical => u32::try_from(physical).ok(),
            Addressing::Window { base } => {
                let offset = physical.checked_sub(u64::from(base))?;
                if offset >= WINDOW_BYTES {
                    return None;
                }
                u32::try_from(offset).ok()
            }
        }
    }
}

impl fmt::Display for Addressing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Addressing::Physical => f.write_str("physical, MMUv2 off"),
            Addressing::Window { base } => write!(f, "MMUv1 linear window at {base:#010x}"),
        }
    }
}

/// Start the front end fetching `prefetch` 64-bit slots at `address`, as
/// `etnaviv_gpu_start_fe` does: the address first, then the control
/// register, whose `ENABLE` starts it.
pub fn start_front_end(registers: &mut impl Registers, address: u32, prefetch: u16) {
    registers.write32(fe::COMMAND_ADDRESS, address);
    registers.write32(
        fe::COMMAND_CONTROL,
        fe::COMMAND_CONTROL_ENABLE | (u32::from(prefetch) & fe::COMMAND_CONTROL_PREFETCH_MASK),
    );
}

/// What a person debugging a front end that did not finish wants from its
/// registers, read without acknowledging anything but what
/// `HI_INTR_ACKNOWLEDGE` held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stuck {
    /// `HI_IDLE_STATE`.
    pub idle: u32,
    /// `HI_INTR_ACKNOWLEDGE`, which reading acknowledged: set bits are
    /// events the core raised that no interrupt delivered.
    pub pending: u32,
    /// `HI_AXI_STATUS`.
    pub axi: u32,
    /// `FE_DMA_STATUS`.
    pub dma_status: u32,
    /// `FE_DMA_DEBUG_STATE`.
    pub dma_debug: u32,
    /// `FE_DMA_ADDRESS`.
    pub dma_address: u32,
    /// `FE_DMA_LOW` and `FE_DMA_HIGH`: the command last fetched.
    pub dma_command: (u32, u32),
}

impl Stuck {
    /// Read them.
    pub fn read(registers: &impl Registers) -> Stuck {
        Stuck {
            idle: registers.read32(hi::IDLE_STATE),
            pending: registers.read32(hi::INTR_ACKNOWLEDGE),
            axi: registers.read32(hi::AXI_STATUS),
            dma_status: registers.read32(fe::DMA_STATUS),
            dma_debug: registers.read32(fe::DMA_DEBUG_STATE),
            dma_address: registers.read32(fe::DMA_ADDRESS),
            dma_command: (
                registers.read32(fe::DMA_LOW),
                registers.read32(fe::DMA_HIGH),
            ),
        }
    }
}

impl fmt::Display for Stuck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "idle {:#010x} (FE {}) pending {:#010x} axi {:#010x} dma status {:#010x} debug {:#010x} ({}) address {:#010x} command {:#010x} {:#010x}",
            self.idle,
            if self.idle & hi::IDLE_STATE_FE != 0 {
                "idle"
            } else {
                "busy"
            },
            self.pending,
            self.axi,
            self.dma_status,
            self.dma_debug,
            fe::command_state(self.dma_debug),
            self.dma_address,
            self.dma_command.0,
            self.dma_command.1
        )
    }
}
