//! The front end's commands, as `cmdstream.xml.h` encodes them.
//!
//! The front end reads its command buffer in 64-bit slots. Every command
//! starts a slot with a header word whose bits 31:27 are the opcode; the
//! commands here are each exactly one slot, the header and one argument
//! word, which is zero where a command takes none. (A `LOAD_STATE` of more
//! than one value runs over several slots; nothing here needs one.)
//!
//! Each function returns its slot as the two words in memory order, so a
//! caller writes `slot[0]` at an 8-byte-aligned address and `slot[1]` after
//! it.

use crate::regs::{common, gl};

/// A slot: the header word, then the argument word.
pub type Slot = [u32; 2];

/// `VIV_FE_LOAD_STATE_HEADER_OP_LOAD_STATE`: opcode 1.
pub const OP_LOAD_STATE: u32 = 0x0800_0000;
/// `VIV_FE_END_HEADER_OP_END`: opcode 2.
pub const OP_END: u32 = 0x1000_0000;
/// `VIV_FE_NOP_HEADER_OP_NOP`: opcode 3.
pub const OP_NOP: u32 = 0x1800_0000;
/// `VIV_FE_WAIT_HEADER_OP_WAIT`: opcode 7.
pub const OP_WAIT: u32 = 0x3800_0000;
/// `VIV_FE_LINK_HEADER_OP_LINK`: opcode 8.
pub const OP_LINK: u32 = 0x4000_0000;
/// `VIV_FE_STALL_HEADER_OP_STALL`: opcode 9.
pub const OP_STALL: u32 = 0x4800_0000;
/// Bits 31:27 of a header: the opcode.
pub const OP_MASK: u32 = 0xF800_0000;

/// `LOAD_STATE`'s value count, bits 25:16.
const LOAD_STATE_COUNT_SHIFT: u32 = 16;
const LOAD_STATE_COUNT_MASK: u32 = 0x03FF_0000;
/// `LOAD_STATE`'s first state, in words, bits 15:0: a register's byte
/// offset shifted right by two (`VIV_FE_LOAD_STATE_HEADER_OFFSET__SHR`).
const LOAD_STATE_OFFSET_MASK: u32 = 0x0000_FFFF;
const LOAD_STATE_OFFSET_SHR: u32 = 2;
/// `STALL`'s and the semaphore's `TO`, bits 12:8; `FROM` is bits 4:0.
const TOKEN_TO_SHIFT: u32 = 8;
const TOKEN_FROM_MASK: u32 = 0x0000_001F;
const TOKEN_TO_MASK: u32 = 0x0000_1F00;

/// `LOAD_STATE` of one value: write `value` to the state at byte offset
/// `state`, in the stream's order with everything around it.
#[must_use]
pub const fn load_state(state: u32, value: u32) -> Slot {
    let header = OP_LOAD_STATE
        | ((1 << LOAD_STATE_COUNT_SHIFT) & LOAD_STATE_COUNT_MASK)
        | ((state >> LOAD_STATE_OFFSET_SHR) & LOAD_STATE_OFFSET_MASK);
    [header, value]
}

/// `END`: stop fetching. The front end goes idle, and fetches again only
/// when `FE_COMMAND_CONTROL` is written.
#[must_use]
pub const fn end() -> Slot {
    [OP_END, 0]
}

/// `NOP`.
#[must_use]
pub const fn nop() -> Slot {
    [OP_NOP, 0]
}

/// `WAIT`: do nothing for `cycles` core clock cycles, then go on.
#[must_use]
pub const fn wait(cycles: u16) -> Slot {
    // The delay is bits 15:0, all of a `u16`.
    [OP_WAIT | cycles as u32, 0]
}

/// `LINK`: fetch from `address` on, `prefetch` slots at once.
#[must_use]
pub const fn link(prefetch: u16, address: u32) -> Slot {
    // The prefetch is bits 15:0, likewise.
    [OP_LINK | prefetch as u32, address]
}

/// `STALL`: the front end waits for `to`'s token from `from`, which a
/// [`semaphore`] earlier in the stream asked for.
#[must_use]
pub const fn stall(from: u32, to: u32) -> Slot {
    [OP_STALL, token(from, to)]
}

/// The semaphore a [`stall`] waits on: `LOAD_STATE` of `GL_SEMAPHORE_TOKEN`.
#[must_use]
pub const fn semaphore(from: u32, to: u32) -> Slot {
    load_state(gl::SEMAPHORE_TOKEN, token(from, to))
}

/// Select the pipe the following states and draws are for: `LOAD_STATE` of
/// `GL_PIPE_SELECT`, [`common::PIPE_3D`] or [`common::PIPE_2D`].
#[must_use]
pub const fn pipe_select(pipe: u32) -> Slot {
    load_state(gl::PIPE_SELECT, pipe & 1)
}

/// Raise event `id` (0 to 29) once the pixel engine has finished everything
/// before it: `LOAD_STATE` of `GL_EVENT` with `FROM_PE`. It arrives as bit
/// `id` of `HI_INTR_ACKNOWLEDGE`, and as the interrupt if that bit is
/// enabled.
#[must_use]
pub const fn event(id: u32) -> Slot {
    load_state(
        gl::EVENT,
        (id & gl::EVENT_EVENT_ID_MASK) | gl::EVENT_FROM_PE,
    )
}

/// A semaphore's or a stall's token: `from` signals `to`.
const fn token(from: u32, to: u32) -> u32 {
    (from & TOKEN_FROM_MASK) | ((to << TOKEN_TO_SHIFT) & TOKEN_TO_MASK)
}

/// The front end and the pixel engine, as a semaphore's and a stall's ends:
/// the pair every stream in etnaviv's ring synchronises on.
pub const FE_TO_PE: (u32, u32) = (common::SYNC_FE, common::SYNC_PE);
