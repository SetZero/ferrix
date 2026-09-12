//! Fuzz the initial process stack image, by building one and reading it back.
//!
//! From stage 7 this runs on every `execve`, over bytes an attacker chose: the
//! argument and environment strings are whatever the calling program passed,
//! and their count and length are bounded only by what the ABI allows. The
//! builder does address arithmetic on all of it — subtracting string lengths
//! from a stack top, rounding to an alignment, multiplying a count by a
//! pointer width — and it does that arithmetic in ring 0, with
//! `overflow-checks` on, which makes a wrapped subtraction a kernel panic
//! rather than a wrong number.
//!
//! # The property
//!
//! Not "it does not crash" — that is the floor. The real property is a round
//! trip: whatever the builder accepts, a walk that knows nothing but the stack
//! pointer and the ABI's own rules must recover *exactly* what went in. That
//! is the thing a program depends on, and it is the thing an off-by-one word
//! breaks while leaving the image superficially well formed.
//!
//! The second property is that nothing below the returned stack pointer is
//! written. The buffer is pre-filled with a sentinel to check it, because the
//! kernel hands this function a whole stack mapping and the program is about
//! to push onto the part below.

#![no_main]

use ferrix_ustack::read::Walk;
use ferrix_ustack::{RANDOM_BYTES, Spec, Width, build};
use libfuzzer_sys::fuzz_target;

/// A byte the builder must never write.
const SENTINEL: u8 = 0xCD;

/// The largest buffer a case may ask for; a page and a half, so that the
/// "it did not fit" path is reached often rather than never.
const MAX_BUF: usize = 6144;

/// Hands out the fuzzer's bytes as the shapes this target needs.
struct Input<'a> {
    rest: &'a [u8],
}

impl<'a> Input<'a> {
    fn byte(&mut self) -> u8 {
        match self.rest.split_first() {
            Some((first, tail)) => {
                self.rest = tail;
                *first
            }
            None => 0,
        }
    }

    /// A short byte string, length taken from the input.
    fn string(&mut self) -> &'a [u8] {
        let want = usize::from(self.byte()) % 24;
        let take = want.min(self.rest.len());
        let (head, tail) = self.rest.split_at(take);
        self.rest = tail;
        head
    }

    /// A value wide enough to exercise the 32-bit refusal.
    fn value(&mut self) -> u64 {
        let mut v = 0_u64;
        for _ in 0..4 {
            v = (v << 8) | u64::from(self.byte());
        }
        // Half the time, push it past what a 32-bit pointer can hold.
        if self.byte() & 1 == 0 { v } else { v << 24 }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input { rest: data };

    let width = if input.byte() & 1 == 0 {
        Width::Bits64
    } else {
        Width::Bits32
    };
    // Both tops are 16-byte aligned, which `build` requires; the unaligned
    // refusal is a unit test, not a thing worth spending fuzzer time on.
    let top: u64 = match width {
        Width::Bits64 => 0x0000_7fff_ffff_f000,
        Width::Bits32 => 0x7fff_f000,
    };

    let argc = usize::from(input.byte()) % 12;
    let envc = usize::from(input.byte()) % 12;
    let auxc = usize::from(input.byte()) % 8;
    let buf_len = (usize::from(input.byte()) * 24 + 64).min(MAX_BUF);

    let args: Vec<&[u8]> = (0..argc).map(|_| input.string()).collect();
    let env: Vec<&[u8]> = (0..envc).map(|_| input.string()).collect();
    // Keys are kept away from the three this crate supplies and from AT_NULL,
    // since refusing those is a unit test; here they would only stop the run
    // before it reached the arithmetic.
    let auxv: Vec<(u64, u64)> = (0..auxc)
        .map(|_| (u64::from(input.byte()) + 64, input.value()))
        .collect();

    let exec_fn = input.string();
    let platform = if input.byte() & 1 == 0 {
        None
    } else {
        Some(input.string())
    };
    let mut random = [0_u8; RANDOM_BYTES];
    for slot in &mut random {
        *slot = input.byte();
    }

    let spec = Spec {
        args: &args,
        env: &env,
        auxv: &auxv,
        random,
        exec_fn,
        platform,
        width,
    };

    let mut buf = vec![SENTINEL; buf_len];
    let base = top - buf_len as u64;
    let Ok(image) = build(&spec, top, &mut buf) else {
        // A refusal is a correct answer. The builder may have written part of
        // an image before deciding it would not fit; the kernel discards the
        // mapping, so there is nothing to check here.
        return;
    };

    assert_eq!(image.sp % 16, 0, "the ABI's alignment is not negotiable");
    assert!(
        image.sp >= base && image.sp < top,
        "the stack pointer is inside the buffer it was built in"
    );

    // Nothing below the stack pointer was touched.
    let below = (image.sp - base) as usize;
    assert!(
        buf[..below].iter().all(|&b| b == SENTINEL),
        "the builder wrote below the stack pointer, where the program will push"
    );

    // The string bounds frame exactly the strings they claim to.
    let args_bytes: u64 = args.iter().map(|s| s.len() as u64 + 1).sum();
    let env_bytes: u64 = env.iter().map(|s| s.len() as u64 + 1).sum();
    assert_eq!(image.arg_end - image.arg_start, args_bytes);
    assert_eq!(image.env_end - image.env_start, env_bytes);
    assert_eq!(
        image.arg_end, image.env_start,
        "cmdline and environ must stay adjacent"
    );

    // The round trip, which is the point.
    let mut walk = Walk::new(&buf, base, image.sp, width);
    assert_eq!(
        walk.argc().expect("argc is readable"),
        args.len() as u64,
        "argc counts the arguments"
    );
    for (index, expected) in args.iter().enumerate() {
        let got = walk
            .next_arg()
            .expect("the argument vector is well formed")
            .expect("the vector is not short");
        assert_eq!(&got, expected, "argument {index} came back changed");
    }
    assert!(
        walk.next_arg().expect("well formed").is_none(),
        "the argument vector ends where argc says it does"
    );
    for (index, expected) in env.iter().enumerate() {
        let got = walk
            .next_env()
            .expect("the environment is well formed")
            .expect("the vector is not short");
        assert_eq!(&got, expected, "environment entry {index} came back changed");
    }
    assert!(
        walk.next_env().expect("well formed").is_none(),
        "the environment ends with a null"
    );

    // The caller's auxiliary entries, in order, followed by the supplied ones.
    let mut seen = Vec::new();
    while let Some(pair) = walk.next_aux().expect("the auxiliary vector is well formed") {
        seen.push(pair);
    }
    assert_eq!(
        &seen[..auxv.len()],
        &auxv[..],
        "the caller's entries keep their order and values"
    );

    let supplied = if platform.is_some() { 3 } else { 2 };
    assert_eq!(
        seen.len(),
        auxv.len() + supplied,
        "exactly the entries this crate promises to add"
    );

    // AT_EXECFN must resolve to the string it was given.
    let execfn_addr = seen
        .iter()
        .find(|&&(k, _)| k == ferrix_linux_abi::types::AT_EXECFN)
        .map(|&(_, v)| v)
        .expect("AT_EXECFN is always supplied");
    assert_eq!(
        walk.cstr_at(execfn_addr).expect("it points at a string"),
        exec_fn,
        "AT_EXECFN points at the path execve was given"
    );

    // AT_RANDOM must point at the sixteen bytes the caller chose.
    let random_addr = seen
        .iter()
        .find(|&&(k, _)| k == ferrix_linux_abi::types::AT_RANDOM)
        .map(|&(_, v)| v)
        .expect("AT_RANDOM is always supplied");
    let offset = (random_addr - base) as usize;
    assert_eq!(
        &buf[offset..offset + RANDOM_BYTES],
        &random[..],
        "musl's stack guard comes from these bytes"
    );
});
