//! `verify` and the second checker agree, on programs made from a fixed seed:
//! a few hundred thousand, biased toward the opcodes seccomp allows and the
//! values that sit on the rules' edges. The fuzzer does the same without the
//! bias; this is what runs on every `cargo test`.

#![allow(
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test that fails by panicking"
)]

mod naive;

use ferrix_seccomp::{Insn, verify};

/// A xorshift generator: the same programs every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Opcodes the filter may use, and some it may not.
const CODES: [u16; 40] = [
    0x20, 0x80, 0x00, 0x60, 0x81, 0x01, 0x61, 0x02, 0x03, 0x04, 0x0c, 0x14, 0x1c, 0x24, 0x2c, 0x34,
    0x3c, 0x44, 0x4c, 0x54, 0x5c, 0x64, 0x6c, 0x74, 0x7c, 0x84, 0x05, 0x15, 0x25, 0x35, 0x45, 0x1d,
    0x06, 0x16, 0x07, 0x87, 0x94, 0x28, 0x30, 0xb1,
];

/// A value that sits on an edge of some rule, or is anything.
fn constant(rng: &mut Rng) -> u32 {
    const EDGES: [u32; 14] = [0, 1, 2, 3, 4, 15, 16, 31, 32, 60, 63, 64, 65, u32::MAX];
    if rng.below(4) == 0 {
        rng.next() as u32
    } else {
        EDGES[rng.below(EDGES.len() as u64) as usize]
    }
}

#[test]
fn the_verifier_and_the_second_checker_agree() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut accepted, mut refused) = (0, 0);
    for _ in 0..300_000 {
        let length = 1 + rng.below(12) as usize;
        let mut program: Vec<Insn> = (0..length)
            .map(|_| {
                let code = if rng.below(20) == 0 {
                    rng.next() as u16
                } else {
                    CODES[rng.below(CODES.len() as u64) as usize]
                };
                Insn::new(
                    code,
                    rng.below(4) as u8,
                    rng.below(4) as u8,
                    constant(&mut rng) % if rng.below(2) == 0 { 20 } else { u32::MAX },
                )
            })
            .collect();
        // Most programs should end well, so the later rules are reached.
        if rng.below(5) != 0 {
            program.push(Insn::new(0x06, 0, 0, 0));
        }
        let verdict = verify(&program).is_ok();
        assert_eq!(verdict, naive::naive(&program), "{program:?}");
        if verdict {
            accepted += 1;
        } else {
            refused += 1;
        }
    }
    // Both sides are really exercised, not one trivially.
    assert!(accepted > 1_000 && refused > 1_000, "{accepted} {refused}");
}
