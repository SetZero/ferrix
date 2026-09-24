//! The registers and bitfields this crate uses, by the etnaviv database's
//! names less their `VIVS_` prefix, grouped by the block that has them.
//!
//! Offsets are bytes into the core's register window. Each group says which
//! header of the database it is taken from (see the crate's documentation);
//! nothing here is a value that was not read there.

/// The host interface: clocks, reset, idle state, interrupts and the chip's
/// identification (`state_hi.xml.h`, `VIVS_HI_*`).
pub mod hi {
    /// `HI_CLOCK_CONTROL`: the core's clock, its frequency scaler and its
    /// soft reset, and whether the 3D and 2D pipes are idle.
    pub const CLOCK_CONTROL: u32 = 0x0000;
    /// Stop the 3D pipe's clock.
    pub const CLOCK_CONTROL_CLK3D_DIS: u32 = 0x0000_0001;
    /// Stop the 2D pipe's clock.
    pub const CLOCK_CONTROL_CLK2D_DIS: u32 = 0x0000_0002;
    /// `FSCALE_VAL`: the clock scaler, in 64ths of the core clock.
    pub const CLOCK_CONTROL_FSCALE_VAL_MASK: u32 = 0x0000_01FC;
    /// Where `FSCALE_VAL` starts.
    pub const CLOCK_CONTROL_FSCALE_VAL_SHIFT: u32 = 2;
    /// Latch `FSCALE_VAL`: the scaler takes a new value on this bit's edge.
    pub const CLOCK_CONTROL_FSCALE_CMD_LOAD: u32 = 0x0000_0200;
    /// Keep the RAMs' clocks running.
    pub const CLOCK_CONTROL_DISABLE_RAM_CLK_GATING: u32 = 0x0000_0400;
    /// Hide the debug registers.
    pub const CLOCK_CONTROL_DISABLE_DEBUG_REGISTERS: u32 = 0x0000_0800;
    /// Hold the core in reset.
    pub const CLOCK_CONTROL_SOFT_RESET: u32 = 0x0000_1000;
    /// The 3D pipe is idle (read only).
    pub const CLOCK_CONTROL_IDLE_3D: u32 = 0x0001_0000;
    /// The 2D pipe is idle (read only).
    pub const CLOCK_CONTROL_IDLE_2D: u32 = 0x0002_0000;
    /// The vector graphics pipe is idle (read only).
    pub const CLOCK_CONTROL_IDLE_VG: u32 = 0x0004_0000;
    /// Cut the core off the bus while it resets.
    pub const CLOCK_CONTROL_ISOLATE_GPU: u32 = 0x0008_0000;

    /// `FSCALE_VAL` for `scale` 64ths, placed.
    #[must_use]
    pub const fn fscale(scale: u32) -> u32 {
        (scale << CLOCK_CONTROL_FSCALE_VAL_SHIFT) & CLOCK_CONTROL_FSCALE_VAL_MASK
    }

    /// `HI_IDLE_STATE`: a bit per module, set when it is idle.
    pub const IDLE_STATE: u32 = 0x0004;
    /// The front end.
    pub const IDLE_STATE_FE: u32 = 0x0000_0001;
    /// The drawing engine (2D).
    pub const IDLE_STATE_DE: u32 = 0x0000_0002;
    /// The pixel engine.
    pub const IDLE_STATE_PE: u32 = 0x0000_0004;
    /// The shader.
    pub const IDLE_STATE_SH: u32 = 0x0000_0008;
    /// The primitive assembler.
    pub const IDLE_STATE_PA: u32 = 0x0000_0010;
    /// The setup engine.
    pub const IDLE_STATE_SE: u32 = 0x0000_0020;
    /// The rasteriser.
    pub const IDLE_STATE_RA: u32 = 0x0000_0040;
    /// The texture unit.
    pub const IDLE_STATE_TX: u32 = 0x0000_0080;
    /// The AXI bus is in low power: not a module, and set whenever it likes.
    pub const IDLE_STATE_AXI_LP: u32 = 0x8000_0000;

    /// `HI_AXI_CONFIG`: the bus master's IDs and cache attributes.
    pub const AXI_CONFIG: u32 = 0x0008;
    /// Where the write cache attribute (`AWCACHE`) starts.
    pub const AXI_CONFIG_AWCACHE_SHIFT: u32 = 8;
    /// Its field.
    pub const AXI_CONFIG_AWCACHE_MASK: u32 = 0x0000_0F00;
    /// Where the read cache attribute (`ARCACHE`) starts.
    pub const AXI_CONFIG_ARCACHE_SHIFT: u32 = 12;
    /// Its field.
    pub const AXI_CONFIG_ARCACHE_MASK: u32 = 0x0000_F000;

    /// `HI_AXI_STATUS`: bus errors the master has seen.
    pub const AXI_STATUS: u32 = 0x000C;
    /// A write the bus refused.
    pub const AXI_STATUS_DET_WR_ERR: u32 = 0x0000_0100;
    /// A read the bus refused.
    pub const AXI_STATUS_DET_RD_ERR: u32 = 0x0000_0200;

    /// `HI_INTR_ACKNOWLEDGE`: the interrupts pending, one bit per event;
    /// reading it acknowledges every one it returns.
    pub const INTR_ACKNOWLEDGE: u32 = 0x0010;
    /// The events, bits 29:0.
    pub const INTR_ACKNOWLEDGE_INTR_VEC_MASK: u32 = 0x3FFF_FFFF;
    /// The MMU faulted.
    pub const INTR_ACKNOWLEDGE_MMU_EXCEPTION: u32 = 0x4000_0000;
    /// The bus returned an error.
    pub const INTR_ACKNOWLEDGE_AXI_BUS_ERROR: u32 = 0x8000_0000;

    /// `HI_INTR_ENBL`: which of those raise the interrupt line.
    pub const INTR_ENBL: u32 = 0x0014;

    /// `HI_CHIP_IDENTITY`: the oldest cores' whole identification.
    pub const CHIP_IDENTITY: u32 = 0x0018;
    /// Its family, bits 31:24; `0x01` is a core too old for the registers
    /// below.
    pub const CHIP_IDENTITY_FAMILY_SHIFT: u32 = 24;
    /// Its revision, bits 15:12, on such a core.
    pub const CHIP_IDENTITY_REVISION_MASK: u32 = 0x0000_F000;
    /// Where that starts.
    pub const CHIP_IDENTITY_REVISION_SHIFT: u32 = 12;

    /// `HI_CHIP_FEATURE`: the major feature word.
    pub const CHIP_FEATURE: u32 = 0x001C;
    /// `HI_CHIP_MODEL`: `0x400` for a GC400.
    pub const CHIP_MODEL: u32 = 0x0020;
    /// `HI_CHIP_REV`.
    pub const CHIP_REV: u32 = 0x0024;
    /// `HI_CHIP_DATE`, as BCD `yyyymmdd`.
    pub const CHIP_DATE: u32 = 0x0028;
    /// `HI_CHIP_TIME`.
    pub const CHIP_TIME: u32 = 0x002C;
    /// `HI_CHIP_CUSTOMER_ID`.
    pub const CHIP_CUSTOMER_ID: u32 = 0x0030;
    /// `HI_CHIP_MINOR_FEATURE_0` to `_5`, in order.
    pub const CHIP_MINOR_FEATURES: [u32; 6] = [0x0034, 0x0074, 0x0084, 0x0088, 0x0094, 0x00A0];
    /// `HI_CHIP_PRODUCT_ID`.
    pub const CHIP_PRODUCT_ID: u32 = 0x00A8;
    /// `HI_CHIP_ECO_ID`.
    pub const CHIP_ECO_ID: u32 = 0x00E8;
}

/// Power management (`state_hi.xml.h`, `VIVS_PM_*`).
pub mod pm {
    /// `PM_POWER_CONTROLS`: module-level clock gating.
    pub const POWER_CONTROLS: u32 = 0x0100;
    /// Gate the clocks of idle modules.
    pub const POWER_CONTROLS_ENABLE_MODULE_CLOCK_GATING: u32 = 0x0000_0001;
    /// `PM_PULSE_EATER`: the clock's dynamic frequency scaler.
    pub const PULSE_EATER: u32 = 0x010C;
    /// Turn the pulse eater off.
    pub const PULSE_EATER_DISABLE: u32 = 0x0000_0001;
    /// A bit the database knows only by position, which a reset sets.
    pub const PULSE_EATER_UNK17: u32 = 0x0002_0000;
}

/// The MMU's version-2 control register (`state_hi.xml.h`, `VIVS_MMUv2_*`),
/// read only to check that a reset left the MMU off.
pub mod mmu_v2 {
    /// `MMUv2_CONTROL`.
    pub const CONTROL: u32 = 0x018C;
    /// The MMU translates.
    pub const CONTROL_ENABLE: u32 = 0x0000_0001;
}

/// The memory controller (`state_hi.xml.h`, `VIVS_MC_*`).
pub mod mc {
    /// `MC_MEMORY_BASE_ADDR_RA`, `_FE`, `_TX`, `_PEZ` and `_PE`: where each
    /// client's linear window starts in physical memory, on a core whose MMU
    /// is version 1.
    pub const MEMORY_BASE_ADDRS: [u32; 5] = [0x0418, 0x041C, 0x0420, 0x0424, 0x0428];
}

/// The front end: where it fetches commands from (`state.xml.h`,
/// `VIVS_FE_*`).
pub mod fe {
    /// `FE_COMMAND_ADDRESS`: the address of the first command.
    pub const COMMAND_ADDRESS: u32 = 0x0654;
    /// `FE_COMMAND_CONTROL`: start fetching.
    pub const COMMAND_CONTROL: u32 = 0x0658;
    /// How many 64-bit slots to fetch first, bits 15:0.
    pub const COMMAND_CONTROL_PREFETCH_MASK: u32 = 0x0000_FFFF;
    /// Fetch.
    pub const COMMAND_CONTROL_ENABLE: u32 = 0x0001_0000;
    /// `FE_DMA_STATUS`.
    pub const DMA_STATUS: u32 = 0x065C;
    /// `FE_DMA_DEBUG_STATE`: what the command parser is doing, bits 4:0,
    /// and its fetcher's state above.
    pub const DMA_DEBUG_STATE: u32 = 0x0660;
    /// The parser's state.
    pub const DMA_DEBUG_STATE_CMD_STATE_MASK: u32 = 0x0000_001F;
    /// `FE_DMA_ADDRESS`: the address the fetcher is at.
    pub const DMA_ADDRESS: u32 = 0x0664;
    /// `FE_DMA_LOW`: the low word of the command last fetched.
    pub const DMA_LOW: u32 = 0x0668;
    /// `FE_DMA_HIGH`: its high word.
    pub const DMA_HIGH: u32 = 0x066C;

    /// The parser's state in `DMA_DEBUG_STATE` by the database's name for
    /// it, for a person reading a stuck front end's registers.
    #[must_use]
    pub const fn command_state(debug_state: u32) -> &'static str {
        match debug_state & DMA_DEBUG_STATE_CMD_STATE_MASK {
            0x00 => "IDLE",
            0x01 => "DEC",
            0x02 => "ADR0",
            0x03 => "LOAD0",
            0x04 => "ADR1",
            0x05 => "LOAD1",
            0x06 => "3DADR",
            0x07 => "3DCMD",
            0x08 => "3DCNTL",
            0x09 => "3DIDXCNTL",
            0x0A => "INITREQDMA",
            0x0B => "DRAWIDX",
            0x0C => "DRAW",
            0x0D => "2DRECT0",
            0x0E => "2DRECT1",
            0x0F => "2DDATA0",
            0x10 => "2DDATA1",
            0x11 => "WAITFIFO",
            0x12 => "WAIT",
            0x13 => "LINK",
            0x14 => "END",
            0x15 => "STALL",
            _ => "unnamed",
        }
    }
}

/// The `GL` block: states a command stream loads to synchronise the pipes
/// (`state.xml.h`, `VIVS_GL_*`).
pub mod gl {
    /// `GL_PIPE_SELECT`: which pipe the following commands are for.
    pub const PIPE_SELECT: u32 = 0x3800;
    /// `GL_EVENT`: raise an event when a module gets here.
    pub const EVENT: u32 = 0x3804;
    /// The event's number, bits 4:0, which is its bit in
    /// `HI_INTR_ACKNOWLEDGE`.
    pub const EVENT_EVENT_ID_MASK: u32 = 0x0000_001F;
    /// Raised by the front end, as it parses the state.
    pub const EVENT_FROM_FE: u32 = 0x0000_0020;
    /// Raised by the pixel engine, once everything before it has been drawn.
    pub const EVENT_FROM_PE: u32 = 0x0000_0040;
    /// `GL_SEMAPHORE_TOKEN`: one module signals another, which a `STALL`
    /// waits for.
    pub const SEMAPHORE_TOKEN: u32 = 0x3808;
}

/// Values the database defines once for several registers
/// (`common.xml.h`).
pub mod common {
    /// `PIPE_ID_PIPE_3D`.
    pub const PIPE_3D: u32 = 0;
    /// `PIPE_ID_PIPE_2D`.
    pub const PIPE_2D: u32 = 1;
    /// `SYNC_RECIPIENT_FE`: the front end, as a semaphore's end.
    pub const SYNC_FE: u32 = 0x01;
    /// `SYNC_RECIPIENT_PE`: the pixel engine.
    pub const SYNC_PE: u32 = 0x07;
}

/// The feature bits this crate reads (`common.xml.h`, `chipFeatures_*` and
/// `chipMinorFeatures*_*`).
pub mod features {
    /// `chipFeatures_PIPE_3D`, in the major feature word.
    pub const PIPE_3D: u32 = 0x0000_0004;
    /// `chipFeatures_PIPE_2D`, likewise.
    pub const PIPE_2D: u32 = 0x0000_0200;
    /// `chipMinorFeatures0_MORE_MINOR_FEATURES`: minor feature words 1 to 5
    /// exist.
    pub const MINOR0_MORE_MINOR_FEATURES: u32 = 0x0020_0000;
    /// `chipMinorFeatures0_MC20`: the version 2.0 memory controller.
    pub const MINOR0_MC20: u32 = 0x0040_0000;
    /// `chipMinorFeatures1_MMU_VERSION`: the MMU is version 2.
    pub const MINOR1_MMU_VERSION: u32 = 0x1000_0000;
    /// `chipMinorFeatures2_DYNAMIC_FREQUENCY_SCALING`: the core's clock is
    /// scaled by the clock controller rather than by `FSCALE_VAL`.
    pub const MINOR2_DYNAMIC_FREQUENCY_SCALING: u32 = 0x0000_4000;
}
