//! Classic BPF as `seccomp(2)` runs it: a filter is checked once, when it is
//! installed, and run against a 64-byte `struct seccomp_data` at every system
//! call, answering the action to take.
//!
//! # What a filter may do
//!
//! Linux checks a seccomp filter twice: `bpf_check_classic` for any classic
//! program (length, opcodes, jumps, scratch memory, division by a constant
//! zero), then `seccomp_check_filter` for what seccomp allows of those. Only
//! the second narrows, and [`validate`] is both: a filter may load a
//! 32-bit word of the data at an aligned offset, its length, constants and
//! scratch words, do arithmetic, jump forward, and return. It may not load a
//! byte or a half word, index, or read a "packet", because there is none. The
//! program must end in a return, and no jump leaves it.
//!
//! # Actions
//!
//! A filter returns an action in the high half of its result and data in the
//! low one. Several filters may be installed; all run, newest first, and the
//! most restrictive answer wins, by the order [`more_restrictive`] gives. An
//! action the kernel does not know is taken as [`KILL_PROCESS`] by the caller,
//! as Linux does.
//!
//! # Byte order
//!
//! `struct seccomp_data` is host-endian, and every Ferrix target is
//! little-endian; words are read so.

#![no_std]

#[cfg(test)]
mod tests;

/// Most instructions in one filter: Linux's `BPF_MAXINSNS`.
pub const MAX_INSNS: usize = 4096;

/// Most instructions across all the filters of one process: Linux's
/// `MAX_INSNS_PER_PATH`.
pub const MAX_INSNS_PER_PATH: usize = 1 << 18;

/// Words of scratch memory: `BPF_MEMWORDS`.
pub const MEMWORDS: usize = 16;

/// Bytes in `struct seccomp_data`.
pub const DATA_BYTES: usize = 64;

/// Bytes in one `struct sock_filter`.
pub const INSN_BYTES: usize = 8;

// Instruction classes, sizes, modes, operators and sources, from
// `linux/bpf_common.h`.
const LD: u16 = 0x00;
const LDX: u16 = 0x01;
const ST: u16 = 0x02;
const STX: u16 = 0x03;
const ALU: u16 = 0x04;
const JMP: u16 = 0x05;
const RET: u16 = 0x06;

const SIZE_W: u16 = 0x00;

const MODE_IMM: u16 = 0x00;
const MODE_ABS: u16 = 0x20;
const MODE_MEM: u16 = 0x60;
const MODE_LEN: u16 = 0x80;

const SRC_K: u16 = 0x00;
const SRC_X: u16 = 0x08;

const ADD: u16 = 0x00;
const SUB: u16 = 0x10;
const MUL: u16 = 0x20;
const DIV: u16 = 0x30;
const OR: u16 = 0x40;
const AND: u16 = 0x50;
const LSH: u16 = 0x60;
const RSH: u16 = 0x70;
const NEG: u16 = 0x80;
const MOD: u16 = 0x90;
const XOR: u16 = 0xa0;

const JA: u16 = 0x00;
const JEQ: u16 = 0x10;
const JGT: u16 = 0x20;
const JGE: u16 = 0x30;
const JSET: u16 = 0x40;

const RVAL_A: u16 = 0x10;
const TAX: u16 = 0x00;
const TXA: u16 = 0x80;

/// The mask of an instruction's class.
const CLASS: u16 = 0x07;
/// The mask of an ALU or jump operator.
const OP: u16 = 0xf0;

/// `SECCOMP_RET_KILL_PROCESS`: end the whole process with `SIGSYS`.
pub const KILL_PROCESS: u32 = 0x8000_0000;
/// `SECCOMP_RET_KILL_THREAD` (also `SECCOMP_RET_KILL`): end the thread.
pub const KILL_THREAD: u32 = 0x0000_0000;
/// `SECCOMP_RET_TRAP`: send `SIGSYS`.
pub const TRAP: u32 = 0x0003_0000;
/// `SECCOMP_RET_ERRNO`: fail the call with the data as its errno.
pub const ERRNO: u32 = 0x0005_0000;
/// `SECCOMP_RET_USER_NOTIF`: ask a supervisor, which Ferrix has none of.
pub const USER_NOTIF: u32 = 0x7fc0_0000;
/// `SECCOMP_RET_TRACE`: ask a tracer, which Ferrix has none of.
pub const TRACE: u32 = 0x7ff0_0000;
/// `SECCOMP_RET_LOG`: allow, and log.
pub const LOG: u32 = 0x7ffc_0000;
/// `SECCOMP_RET_ALLOW`.
pub const ALLOW: u32 = 0x7fff_0000;
/// The action half of a result.
pub const ACTION_FULL: u32 = 0xffff_0000;
/// The data half of a result.
pub const DATA: u32 = 0x0000_ffff;

/// One `struct sock_filter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insn {
    /// The opcode.
    pub code: u16,
    /// Jump this far on a true test.
    pub jt: u8,
    /// Jump this far on a false one.
    pub jf: u8,
    /// A constant, an offset or a jump distance.
    pub k: u32,
}

impl Insn {
    /// An instruction of the two parts an opcode is usually spelled in.
    #[must_use]
    pub const fn new(code: u16, jt: u8, jf: u8, k: u32) -> Insn {
        Insn { code, jt, jf, k }
    }

    /// Read one from its eight bytes, as a program's `sock_filter` array
    /// holds it: `code`, `jt`, `jf`, `k`, little-endian.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Insn> {
        let bytes: &[u8; INSN_BYTES] = bytes.get(..INSN_BYTES)?.try_into().ok()?;
        Some(Insn {
            code: u16::from_le_bytes([bytes[0], bytes[1]]),
            jt: bytes[2],
            jf: bytes[3],
            k: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }
}

/// Why a filter was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    /// No instructions, or more than [`MAX_INSNS`].
    Length,
    /// An opcode a seccomp filter may not use, at this index.
    Opcode(usize),
    /// A load outside the data, or not word-aligned, at this index.
    Offset(usize),
    /// A jump past the end of the program, at this index.
    Jump(usize),
    /// A scratch word past [`MEMWORDS`], at this index.
    Memory(usize),
    /// A division or modulus by the constant zero, or a shift by 32 or more,
    /// at this index.
    Arithmetic(usize),
    /// The program does not end in a return.
    NoReturn,
}

/// Check `program` as Linux's `bpf_check_classic` and `seccomp_check_filter`
/// do.
///
/// # Errors
///
/// [`Invalid`], naming the first instruction at fault.
pub fn validate(program: &[Insn]) -> Result<(), Invalid> {
    if program.is_empty() || program.len() > MAX_INSNS {
        return Err(Invalid::Length);
    }
    for (index, insn) in program.iter().enumerate() {
        let remaining = program.len() - index - 1;
        match insn.code & CLASS {
            LD => match insn.code & !CLASS {
                c if c == SIZE_W | MODE_ABS => {
                    if insn.k % 4 != 0 || insn.k as usize >= DATA_BYTES {
                        return Err(Invalid::Offset(index));
                    }
                }
                c if c == SIZE_W | MODE_LEN || c == MODE_IMM => {}
                c if c == MODE_MEM => memory(insn, index)?,
                _ => return Err(Invalid::Opcode(index)),
            },
            LDX => match insn.code & !CLASS {
                c if c == SIZE_W | MODE_LEN || c == MODE_IMM => {}
                c if c == MODE_MEM => memory(insn, index)?,
                _ => return Err(Invalid::Opcode(index)),
            },
            ST | STX => {
                if insn.code & !CLASS != 0 {
                    return Err(Invalid::Opcode(index));
                }
                memory(insn, index)?;
            }
            ALU => alu(insn, index)?,
            JMP => jump(insn, index, remaining)?,
            RET => match insn.code & !CLASS {
                SRC_K | RVAL_A => {}
                _ => return Err(Invalid::Opcode(index)),
            },
            // `MISC`: `TAX` and `TXA`.
            _ => match insn.code & !CLASS {
                TAX | TXA => {}
                _ => return Err(Invalid::Opcode(index)),
            },
        }
    }
    match program.last() {
        Some(last) if last.code & CLASS == RET => Ok(()),
        _ => Err(Invalid::NoReturn),
    }
}

/// A scratch word is in range.
fn memory(insn: &Insn, index: usize) -> Result<(), Invalid> {
    if insn.k as usize >= MEMWORDS {
        Err(Invalid::Memory(index))
    } else {
        Ok(())
    }
}

/// An arithmetic instruction is one Linux allows.
fn alu(insn: &Insn, index: usize) -> Result<(), Invalid> {
    let op = insn.code & OP;
    let source = insn.code & SRC_X;
    if insn.code & !(CLASS | OP | SRC_X) != 0 {
        return Err(Invalid::Opcode(index));
    }
    match op {
        ADD | SUB | MUL | OR | AND | XOR => Ok(()),
        NEG if source == SRC_K => Ok(()),
        DIV | MOD if source == SRC_K && insn.k == 0 => Err(Invalid::Arithmetic(index)),
        LSH | RSH if source == SRC_K && insn.k >= 32 => Err(Invalid::Arithmetic(index)),
        DIV | MOD | LSH | RSH => Ok(()),
        _ => Err(Invalid::Opcode(index)),
    }
}

/// A jump stays inside the program.
fn jump(insn: &Insn, index: usize, remaining: usize) -> Result<(), Invalid> {
    if insn.code & !(CLASS | OP | SRC_X) != 0 {
        return Err(Invalid::Opcode(index));
    }
    match insn.code & OP {
        JA if insn.code & SRC_X == 0 => {
            if insn.k as usize >= remaining {
                Err(Invalid::Jump(index))
            } else {
                Ok(())
            }
        }
        JEQ | JGT | JGE | JSET => {
            if usize::from(insn.jt) >= remaining || usize::from(insn.jf) >= remaining {
                Err(Invalid::Jump(index))
            } else {
                Ok(())
            }
        }
        _ => Err(Invalid::Opcode(index)),
    }
}

/// Run a validated `program` against `data`, and answer what it returned.
///
/// A program that [`validate`] passed cannot run off its end, loop or read
/// outside the data; one that was not validated is answered `0`
/// ([`KILL_THREAD`]) rather than trusted, as is a division by a zero `X`.
#[must_use]
pub fn run(program: &[Insn], data: &[u8; DATA_BYTES]) -> u32 {
    let (mut a, mut x) = (0_u32, 0_u32);
    let mut scratch = [0_u32; MEMWORDS];
    let mut pc = 0_usize;
    // Forward jumps only: no more steps than instructions.
    for _ in 0..=program.len() {
        let Some(insn) = program.get(pc) else {
            return KILL_THREAD;
        };
        pc += 1;
        let source = if insn.code & SRC_X == 0 { insn.k } else { x };
        match insn.code & CLASS {
            LD => {
                a = match insn.code & !CLASS {
                    c if c == SIZE_W | MODE_ABS => word(data, insn.k),
                    c if c == SIZE_W | MODE_LEN => DATA_BYTES as u32,
                    c if c == MODE_IMM => insn.k,
                    _ => scratch.get(insn.k as usize).copied().unwrap_or(0),
                };
            }
            LDX => {
                x = match insn.code & !CLASS {
                    c if c == SIZE_W | MODE_LEN => DATA_BYTES as u32,
                    c if c == MODE_IMM => insn.k,
                    _ => scratch.get(insn.k as usize).copied().unwrap_or(0),
                };
            }
            ST => {
                if let Some(slot) = scratch.get_mut(insn.k as usize) {
                    *slot = a;
                }
            }
            STX => {
                if let Some(slot) = scratch.get_mut(insn.k as usize) {
                    *slot = x;
                }
            }
            ALU => match arithmetic(insn.code & OP, a, source) {
                Some(result) => a = result,
                None => return KILL_THREAD,
            },
            JMP => {
                let taken = match insn.code & OP {
                    JA => {
                        pc = pc.saturating_add(insn.k as usize);
                        continue;
                    }
                    JEQ => a == source,
                    JGT => a > source,
                    JGE => a >= source,
                    _ => a & source != 0,
                };
                pc += usize::from(if taken { insn.jt } else { insn.jf });
            }
            RET => return if insn.code & RVAL_A != 0 { a } else { insn.k },
            _ => {
                if insn.code & TXA != 0 {
                    a = x;
                } else {
                    x = a;
                }
            }
        }
    }
    KILL_THREAD
}

/// The word of `data` at the aligned offset `at`, or zero past it.
fn word(data: &[u8; DATA_BYTES], at: u32) -> u32 {
    let at = at as usize;
    data.get(at..at + 4)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map_or(0, u32::from_le_bytes)
}

/// One ALU operation on the 32-bit accumulator; `None` for a division by
/// zero, which ends the filter as Linux's interpreter does.
fn arithmetic(op: u16, a: u32, b: u32) -> Option<u32> {
    Some(match op {
        ADD => a.wrapping_add(b),
        SUB => a.wrapping_sub(b),
        MUL => a.wrapping_mul(b),
        DIV => a.checked_div(b)?,
        MOD => a.checked_rem(b)?,
        OR => a | b,
        AND => a & b,
        LSH => a << (b & 31),
        RSH => a >> (b & 31),
        NEG => a.wrapping_neg(),
        _ => a ^ b,
    })
}

/// `struct seccomp_data` for a call: `nr`, `arch`, the instruction pointer
/// and six arguments, in the order the filter sees them.
#[must_use]
pub fn data(nr: i32, arch: u32, instruction_pointer: u64, args: [u64; 6]) -> [u8; DATA_BYTES] {
    let mut out = [0_u8; DATA_BYTES];
    out[0..4].copy_from_slice(&nr.to_le_bytes());
    out[4..8].copy_from_slice(&arch.to_le_bytes());
    out[8..16].copy_from_slice(&instruction_pointer.to_le_bytes());
    for (slot, arg) in out[16..].chunks_exact_mut(8).zip(args) {
        slot.copy_from_slice(&arg.to_le_bytes());
    }
    out
}

/// Whether result `a` is more restrictive than `b`: Linux compares the action
/// halves as signed numbers, so `KILL_PROCESS` (negative) is the most
/// restrictive, then `KILL_THREAD`, `TRAP`, `ERRNO`, `USER_NOTIF`, `TRACE`,
/// `LOG` and `ALLOW`; an action nobody defined sorts among them by its value.
#[must_use]
pub const fn more_restrictive(a: u32, b: u32) -> bool {
    ((a & ACTION_FULL) as i32) < ((b & ACTION_FULL) as i32)
}

/// Run every filter, newest first, and answer the most restrictive result.
/// On a tie the newer one's data stands, as Linux's loop keeps the first it
/// found.
#[must_use]
pub fn run_all<'a>(
    filters: impl IntoIterator<Item = &'a [Insn]>,
    data: &[u8; DATA_BYTES],
) -> u32 {
    let mut answer = ALLOW;
    for program in filters {
        let result = run(program, data);
        if more_restrictive(result, answer) {
            answer = result;
        }
    }
    answer
}
