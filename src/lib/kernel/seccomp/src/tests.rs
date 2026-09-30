//! What a seccomp filter may be, and what it answers.

extern crate std;

use std::vec;

use super::*;

const LD_ABS: u16 = LD | SIZE_W | MODE_ABS;
const JEQ_K: u16 = JMP | JEQ | SRC_K;
const RET_K: u16 = RET | SRC_K;

fn allow_only(nr: u32) -> [Insn; 4] {
    // if (nr == N) return ALLOW; return ERRNO | 1;
    [
        Insn::new(LD_ABS, 0, 0, 0),
        Insn::new(JEQ_K, 0, 1, nr),
        Insn::new(RET_K, 0, 0, ALLOW),
        Insn::new(RET_K, 0, 0, ERRNO | 1),
    ]
}

fn call(nr: i32) -> [u8; DATA_BYTES] {
    data(nr, 0xc000_003e, 0x1000, [1, 2, 3, 4, 5, 6])
}

#[test]
fn a_filter_answers_by_the_call_number() {
    let program = allow_only(39);
    assert_eq!(validate(&program), Ok(()));
    assert_eq!(run(&program, &call(39)), ALLOW);
    assert_eq!(run(&program, &call(1)), ERRNO | 1);
}

#[test]
fn an_argument_is_a_word_at_its_offset() {
    // Load the low word of args[2] (offset 16 + 2 * 8) and return it.
    let program = [
        Insn::new(LD_ABS, 0, 0, 32),
        Insn::new(RET | RVAL_A, 0, 0, 0),
    ];
    assert_eq!(validate(&program), Ok(()));
    assert_eq!(run(&program, &call(0)), 3);
}

#[test]
fn what_seccomp_does_not_allow_is_refused() {
    let ret = Insn::new(RET_K, 0, 0, ALLOW);
    assert_eq!(validate(&[]), Err(Invalid::Length));
    assert_eq!(validate(&vec![ret; MAX_INSNS + 1]), Err(Invalid::Length));
    assert_eq!(validate(&vec![ret; MAX_INSNS]), Ok(()));
    // A byte, a half word, an unaligned word and one past the data.
    for (code, k) in [
        (LD | 0x10 | MODE_ABS, 0),
        (LD | 0x08 | MODE_ABS, 0),
        (LD_ABS, 2),
        (LD_ABS, 64),
    ] {
        assert!(matches!(
            validate(&[Insn::new(code, 0, 0, k), ret]),
            Err(Invalid::Opcode(0) | Invalid::Offset(0))
        ));
    }
    // Indexed and "packet" loads.
    assert_eq!(
        validate(&[Insn::new(LD | SIZE_W | 0x40, 0, 0, 0), ret]),
        Err(Invalid::Opcode(0))
    );
    assert_eq!(
        validate(&[Insn::new(LDX | 0x10 | 0xa0, 0, 0, 0), ret]),
        Err(Invalid::Opcode(0))
    );
    // Scratch memory past its words.
    assert_eq!(
        validate(&[Insn::new(ST, 0, 0, 16), ret]),
        Err(Invalid::Memory(0))
    );
    // A division by the constant zero, a shift of 32.
    assert_eq!(
        validate(&[Insn::new(ALU | DIV | SRC_K, 0, 0, 0), ret]),
        Err(Invalid::Arithmetic(0))
    );
    assert_eq!(
        validate(&[Insn::new(ALU | LSH | SRC_K, 0, 0, 32), ret]),
        Err(Invalid::Arithmetic(0))
    );
    // A jump off the end, and a program that does not end in a return.
    assert_eq!(
        validate(&[Insn::new(JEQ_K, 1, 0, 0), ret]),
        Err(Invalid::Jump(0))
    );
    assert_eq!(
        validate(&[Insn::new(LD | MODE_IMM, 0, 0, 1)]),
        Err(Invalid::NoReturn)
    );
}

#[test]
fn arithmetic_and_scratch_words_work() {
    // A = 7; A *= 6; mem[3] = A; A = 0; A = mem[3]; A -= 2; return A.
    let program = [
        Insn::new(LD | MODE_IMM, 0, 0, 7),
        Insn::new(ALU | MUL | SRC_K, 0, 0, 6),
        Insn::new(ST, 0, 0, 3),
        Insn::new(LD | MODE_IMM, 0, 0, 0),
        Insn::new(LD | MODE_MEM, 0, 0, 3),
        Insn::new(ALU | SUB | SRC_K, 0, 0, 2),
        Insn::new(RET | RVAL_A, 0, 0, 0),
    ];
    assert_eq!(validate(&program), Ok(()));
    assert_eq!(run(&program, &call(0)), 40);
}

#[test]
fn a_division_by_a_zero_index_ends_the_filter_killing() {
    // X = 0; A = 5; A /= X; return ALLOW.
    let program = [
        Insn::new(LDX | MODE_IMM, 0, 0, 0),
        Insn::new(LD | MODE_IMM, 0, 0, 5),
        Insn::new(ALU | DIV | SRC_X, 0, 0, 0),
        Insn::new(RET_K, 0, 0, ALLOW),
    ];
    assert_eq!(validate(&program), Ok(()));
    assert_eq!(run(&program, &call(0)), KILL_THREAD);
}

#[test]
fn the_most_restrictive_action_wins_and_a_tie_keeps_the_newest() {
    assert!(more_restrictive(KILL_PROCESS, KILL_THREAD));
    assert!(more_restrictive(KILL_THREAD, TRAP));
    assert!(more_restrictive(TRAP, ERRNO));
    assert!(more_restrictive(ERRNO, USER_NOTIF));
    assert!(more_restrictive(USER_NOTIF, TRACE));
    assert!(more_restrictive(TRACE, LOG));
    assert!(more_restrictive(LOG, ALLOW));
    assert!(!more_restrictive(ALLOW, ALLOW));
    assert!(!more_restrictive(ERRNO | 2, ERRNO | 1));

    let allow = [Insn::new(RET_K, 0, 0, ALLOW)];
    let eperm = [Insn::new(RET_K, 0, 0, ERRNO | 1)];
    let eacces = [Insn::new(RET_K, 0, 0, ERRNO | 13)];
    let data = call(0);
    let filters: [&[Insn]; 3] = [&allow, &eperm, &eacces];
    assert_eq!(run_all(filters, &data), ERRNO | 1);
    assert_eq!(run_all(filters.iter().rev().copied(), &data), ERRNO | 13);
    assert_eq!(run_all([&allow[..]], &data), ALLOW);
}

#[test]
fn an_instruction_is_eight_little_endian_bytes() {
    let bytes = [0x20, 0x00, 0x01, 0x02, 0x04, 0x03, 0x02, 0x01];
    assert_eq!(
        Insn::from_bytes(&bytes),
        Some(Insn::new(0x0020, 1, 2, 0x0102_0304))
    );
    assert_eq!(Insn::from_bytes(&bytes[..7]), None);
}

#[test]
fn the_data_is_laid_out_as_the_header_says() {
    let d = data(-1, 0x4000_0028, 0x1122_3344_5566_7788, [1, 2, 3, 4, 5, 6]);
    assert_eq!(&d[0..4], &(-1_i32).to_le_bytes());
    assert_eq!(&d[4..8], &0x4000_0028_u32.to_le_bytes());
    assert_eq!(&d[8..16], &0x1122_3344_5566_7788_u64.to_le_bytes());
    assert_eq!(&d[56..64], &6_u64.to_le_bytes());
}
