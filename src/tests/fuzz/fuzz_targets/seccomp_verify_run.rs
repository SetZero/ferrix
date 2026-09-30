//! Fuzz seccomp's verifier and interpreter (`docs/SECCOMP.md` §3.1).
//!
//! A filter is a program a process writes and the kernel runs at every system
//! call it makes, in ring 0. The verifier stands between what a process wrote
//! and the machine, so the bytes it is given are anything at all.
//!
//! # The input
//!
//! The first two bytes are a count; the next `8 * count` are
//! `struct sock_filter`s (`code u16, jt u8, jf u8, k u32`); the rest is
//! a `struct seccomp_data` to run a verified program on, padded with zeros.
//!
//! # The properties
//!
//! 1. `verify` never panics, whatever it is given.
//! 2. A verified program's `run` never panics, and takes at most as many
//!    steps as it has instructions.
//! 3. A verified program never loads outside the data: every `LD|W|ABS` in it
//!    is aligned and below 64.
//! 4. `verify` agrees with a second checker written from the rules, by a
//!    different method: a fixpoint over the scratch words where the verifier
//!    makes one forward pass, and the opcodes spelled out as a list where it
//!    decodes them.

#![no_main]

use ferrix_seccomp::{DATA_BYTES, Insn, MAX_INSNS, SeccompData, run_counted, verify};
use libfuzzer_sys::fuzz_target;

/// The second checker, shared with the crate's own agreement test.
#[path = "../../../lib/kernel/seccomp/tests/naive/mod.rs"]
mod naive;

fuzz_target!(|input: &[u8]| {
    let Some((&[lo, hi], rest)) = input.split_first_chunk::<2>() else {
        return;
    };
    let count = usize::from(u16::from_le_bytes([lo, hi])) % (MAX_INSNS + 8);
    let body = rest.len().min(count * 8);
    let (raw_bytes, tail) = rest.split_at(body);
    let raw: Vec<Insn> = raw_bytes.chunks_exact(8).filter_map(Insn::from_bytes).collect();

    let verified = verify(&raw);
    // 4. Agreement with the second checker.
    assert_eq!(verified.is_ok(), naive::naive(&raw), "verify and the naive checker differ");
    let Ok(program) = verified else {
        return;
    };

    // 3. No load outside the data.
    for insn in program.insns() {
        if insn.code == 0x20 {
            assert!(insn.k < DATA_BYTES as u32 && insn.k % 4 == 0);
        }
    }

    // 2. A run ends within the program's length.
    let mut padded = [0_u8; 64];
    let copy = tail.len().min(64);
    padded[..copy].copy_from_slice(&tail[..copy]);
    let word = |at: usize| u32::from_le_bytes([padded[at], padded[at + 1], padded[at + 2], padded[at + 3]]);
    let quad = |at: usize| u64::from(word(at)) | u64::from(word(at + 4)) << 32;
    let data = SeccompData {
        nr: word(0) as i32,
        arch: word(4),
        instruction_pointer: quad(8),
        args: [quad(16), quad(24), quad(32), quad(40), quad(48), quad(56)],
    };
    let (_, steps) = run_counted(&program, &data);
    assert!(steps <= program.len());
});
