//! A second checker for seccomp filters, written from the rules by a different
//! method than `verify`: the opcodes spelled out as a list where the verifier
//! decodes them, and a fixpoint over the scratch words where the verifier
//! makes one forward pass (`docs/SECCOMP.md` §3.1). The crate's agreement test
//! and the fuzz target `seccomp_verify_run` both hold `verify` to it.

#![allow(
    clippy::indexing_slicing,
    clippy::manual_is_multiple_of,
    reason = "a checker written to be obviously the rules"
)]

use ferrix_seccomp::{DATA_BYTES, Insn, MAX_INSNS, MEMWORDS};

/// Every opcode `seccomp_check_filter` lets through, spelled out.
pub(crate) fn allowed(code: u16) -> bool {
    const ALU_OPS: [u16; 9] = [0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0xa0];
    const JUMPS: [u16; 4] = [0x10, 0x20, 0x30, 0x40];
    match code {
        // LD: ABS, LEN, IMM, MEM. LDX: LEN, IMM, MEM. ST, STX.
        0x20 | 0x80 | 0x00 | 0x60 | 0x81 | 0x01 | 0x61 | 0x02 | 0x03 => true,
        // NEG, TAX, TXA, JA, RET K, RET A.
        0x84 | 0x07 | 0x87 | 0x05 | 0x06 | 0x16 => true,
        other => {
            let class = other & 0x07;
            let source = other & 0x08;
            let op = other & 0xf0;
            (class == 0x04 && ALU_OPS.contains(&op) && other & !0xfc == 0 && source <= 0x08)
                || (class == 0x05 && JUMPS.contains(&op) && other & !0x7d == 0)
        }
    }
}

/// Narrow the set stored at `at` to `out`; whether that changed it.
fn narrow(stored: &mut [u16], at: usize, out: u16) -> bool {
    let Some(slot) = stored.get_mut(at) else {
        return false;
    };
    let narrowed = *slot & out;
    let changed = narrowed != *slot;
    *slot = narrowed;
    changed
}

/// The rules, checked by a second method.
pub(crate) fn naive(raw: &[Insn]) -> bool {
    if raw.is_empty() || raw.len() > MAX_INSNS {
        return false;
    }
    let len = raw.len();
    for (i, insn) in raw.iter().enumerate() {
        if !allowed(insn.code) {
            return false;
        }
        match insn.code {
            0x20 if insn.k >= DATA_BYTES as u32 || insn.k % 4 != 0 => return false,
            0x34 if insn.k == 0 => return false,
            0x64 | 0x74 if insn.k >= 32 => return false,
            0x60 | 0x61 | 0x02 | 0x03 if insn.k >= MEMWORDS as u32 => return false,
            0x05 if insn.k as usize >= len - i - 1 => return false,
            _ => {}
        }
        if matches!(insn.code & 0xf7, 0x15 | 0x25 | 0x35 | 0x45)
            && (i + 1 + usize::from(insn.jt) >= len || i + 1 + usize::from(insn.jf) >= len)
        {
            return false;
        }
    }
    if !matches!(raw.last().map(|insn| insn.code), Some(0x06 | 0x16)) {
        return false;
    }
    // Scratch words definitely stored on every path: iterate to a fixpoint.
    let mut stored = vec![u16::MAX; len];
    stored[0] = 0;
    loop {
        let mut changed = false;
        for (i, insn) in raw.iter().enumerate() {
            let here = stored[i];
            let out = if matches!(insn.code, 0x02 | 0x03) {
                here | (1 << (insn.k % 16))
            } else {
                here
            };
            let successors: Vec<usize> = match insn.code {
                0x06 | 0x16 => vec![],
                0x05 => vec![i + 1 + insn.k as usize],
                c if matches!(c & 0xf7, 0x15 | 0x25 | 0x35 | 0x45) => {
                    vec![i + 1 + usize::from(insn.jt), i + 1 + usize::from(insn.jf)]
                }
                _ => vec![i + 1],
            };
            for next in successors {
                changed |= narrow(&mut stored, next, out);
            }
        }
        if !changed {
            break;
        }
    }
    for (i, insn) in raw.iter().enumerate() {
        if matches!(insn.code, 0x60 | 0x61) && stored[i] & (1 << insn.k) == 0 {
            return false;
        }
    }
    true
}
