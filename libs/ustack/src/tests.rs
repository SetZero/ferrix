//! What the builder must get right, checked by reading the bytes back.
//!
//! Almost every test here goes through [`crate::read::Walk`] rather than
//! inspecting the builder's own arithmetic, because the builder agreeing with
//! itself proves nothing. The walk starts from a stack pointer and the ABI's
//! rules and nothing else, which is all a program has.

extern crate std;

use std::vec::Vec;

use ferrix_linux_abi::types::{
    AT_BASE, AT_ENTRY, AT_EXECFN, AT_NULL, AT_PAGESZ, AT_PHDR, AT_PHNUM, AT_PLATFORM, AT_RANDOM,
    AT_SECURE,
};

use crate::read::{ReadError, Walk};
use crate::{Image, MAX_ARG_STRLEN, RANDOM_BYTES, STACK_ALIGN, Spec, StackError, Width, build};

/// A stack top high enough to be realistic and 16-byte aligned.
const TOP_64: u64 = 0x0000_7fff_ffff_f000;
/// The same for a 32-bit address space, below `USER_VIRT_END` on ARMv7-A.
const TOP_32: u64 = 0x7fff_f000;

/// The canonical spec the round-trip tests use.
fn spec(width: Width) -> Spec<'static> {
    Spec {
        args: &[b"/bin/busybox", b"sh", b"-c", b"echo hello"],
        env: &[b"PATH=/bin:/usr/bin", b"HOME=/root", b"TERM=vt100"],
        auxv: &[
            (AT_PAGESZ, 4096),
            (AT_PHDR, 0x40_0040),
            (AT_PHNUM, 9),
            (AT_ENTRY, 0x40_1234),
            (AT_BASE, 0),
            (AT_SECURE, 0),
        ],
        random: [0xa5; RANDOM_BYTES],
        exec_fn: b"/bin/busybox",
        platform: Some(b"x86_64"),
        width,
    }
}

/// Everything a walk recovers from an image.
struct RoundTrip {
    argc: u64,
    args: Vec<Vec<u8>>,
    env: Vec<Vec<u8>>,
    auxv: Vec<(u64, u64)>,
}

/// Build into `buf`, then walk the result back out.
fn round_trip(spec: &Spec<'_>, top: u64, buf: &mut [u8]) -> (Image, RoundTrip) {
    let len = u64::try_from(buf.len()).expect("a test buffer fits in u64");
    let base = top - len;
    let image = build(spec, top, buf).expect("the canonical spec fits in a page");

    let mut walk = Walk::new(buf, base, image.sp, spec.width);
    let argc = walk.argc().expect("argc is readable");
    let mut args = Vec::new();
    while let Some(s) = walk.next_arg().expect("the argument vector is well formed") {
        args.push(s.to_vec());
    }
    let mut env = Vec::new();
    while let Some(s) = walk.next_env().expect("the environment is well formed") {
        env.push(s.to_vec());
    }
    let mut auxv = Vec::new();
    while let Some(pair) = walk
        .next_aux()
        .expect("the auxiliary vector is well formed")
    {
        auxv.push(pair);
    }
    (
        image,
        RoundTrip {
            argc,
            args,
            env,
            auxv,
        },
    )
}

// ---------------------------------------------------------------------------
// The round trip
// ---------------------------------------------------------------------------

#[test]
fn a_64_bit_image_reads_back_as_it_went_in() {
    let spec = spec(Width::Bits64);
    let mut buf = [0_u8; 4096];
    let (_image, got) = round_trip(&spec, TOP_64, &mut buf);

    assert_eq!(got.argc, 4, "argc counts the arguments, not the pointers");
    assert_eq!(got.args, spec.args, "every argument survives in order");
    assert_eq!(
        got.env, spec.env,
        "every environment entry survives in order"
    );
}

#[test]
fn a_32_bit_image_reads_back_as_it_went_in() {
    // The interesting case, and the one a host test would otherwise never
    // reach: four-byte words, and a stack top that fits in 32 bits.
    let spec = spec(Width::Bits32);
    let mut buf = [0_u8; 4096];
    let (_image, got) = round_trip(&spec, TOP_32, &mut buf);

    assert_eq!(got.argc, 4);
    assert_eq!(got.args, spec.args);
    assert_eq!(got.env, spec.env);
}

#[test]
fn the_caller_s_auxiliary_entries_come_back_unchanged_and_in_order() {
    let spec = spec(Width::Bits64);
    let mut buf = [0_u8; 4096];
    let (_image, got) = round_trip(&spec, TOP_64, &mut buf);

    let caller: Vec<(u64, u64)> = got
        .auxv
        .iter()
        .copied()
        .filter(|&(k, _)| k != AT_RANDOM && k != AT_EXECFN && k != AT_PLATFORM)
        .collect();
    assert_eq!(
        caller, spec.auxv,
        "the caller's entries keep their order and values"
    );
}

#[test]
fn the_three_supplied_entries_point_at_what_they_promise() {
    let spec = spec(Width::Bits64);
    let mut buf = [0_u8; 4096];
    let base = TOP_64 - 4096;
    let (image, got) = round_trip(&spec, TOP_64, &mut buf);
    let walk = Walk::new(&buf, base, image.sp, spec.width);

    let find = |key: u64| {
        got.auxv
            .iter()
            .find(|&&(k, _)| k == key)
            .map(|&(_, v)| v)
            .expect("the entry is present")
    };

    let random = find(AT_RANDOM);
    let offset = usize::try_from(random - base).expect("inside the buffer");
    assert_eq!(
        buf.get(offset..offset + RANDOM_BYTES),
        Some(&spec.random[..]),
        "AT_RANDOM points at the sixteen bytes it was given"
    );

    assert_eq!(
        walk.cstr_at(find(AT_EXECFN)),
        Ok(spec.exec_fn),
        "AT_EXECFN points at the path execve was given"
    );
    assert_eq!(
        walk.cstr_at(find(AT_PLATFORM)),
        Ok(&b"x86_64"[..]),
        "AT_PLATFORM points at the platform string"
    );
}

#[test]
fn no_platform_means_no_at_platform_entry() {
    let mut spec = spec(Width::Bits64);
    spec.platform = None;
    let mut buf = [0_u8; 4096];
    let (_image, got) = round_trip(&spec, TOP_64, &mut buf);

    assert!(
        !got.auxv.iter().any(|&(k, _)| k == AT_PLATFORM),
        "an absent platform must not become an entry pointing at nothing"
    );
    assert!(
        got.auxv.iter().any(|&(k, _)| k == AT_RANDOM),
        "the other two are still there"
    );
}

// ---------------------------------------------------------------------------
// The properties a program depends on
// ---------------------------------------------------------------------------

#[test]
fn the_stack_pointer_is_16_byte_aligned_whatever_the_counts() {
    // The alignment must not depend on how many arguments there are, which is
    // exactly the bug an implementation that aligns the strings instead of the
    // vectors would have.
    let all: [&[u8]; 7] = [b"a", b"bb", b"ccc", b"dddd", b"e", b"ff", b"ggg"];
    for argc in 0..all.len() {
        for envc in 0..all.len() {
            for width in [Width::Bits32, Width::Bits64] {
                let spec = Spec {
                    args: all.get(..argc).expect("in range"),
                    env: all.get(..envc).expect("in range"),
                    auxv: &[(AT_PAGESZ, 4096)],
                    random: [0; RANDOM_BYTES],
                    exec_fn: b"/x",
                    platform: None,
                    width,
                };
                let mut buf = [0_u8; 4096];
                let top = if width == Width::Bits32 {
                    TOP_32
                } else {
                    TOP_64
                };
                let image = build(&spec, top, &mut buf).expect("it fits");
                assert_eq!(
                    image.sp % STACK_ALIGN,
                    0,
                    "{argc} args, {envc} env, {width:?} must still land aligned"
                );
            }
        }
    }
}

#[test]
fn the_string_bounds_frame_exactly_the_strings() {
    let spec = spec(Width::Bits64);
    let mut buf = [0_u8; 4096];
    let (image, _got) = round_trip(&spec, TOP_64, &mut buf);

    let args_bytes: u64 = spec.args.iter().map(|s| s.len() as u64 + 1).sum();
    let env_bytes: u64 = spec.env.iter().map(|s| s.len() as u64 + 1).sum();
    assert_eq!(
        image.arg_end - image.arg_start,
        args_bytes,
        "/proc/self/cmdline is these bytes and nothing else"
    );
    assert_eq!(image.env_end - image.env_start, env_bytes);
    assert_eq!(
        image.arg_end, image.env_start,
        "the two runs are adjacent, which is what makes cmdline one read"
    );
}

#[test]
fn the_argument_strings_are_one_contiguous_run() {
    let spec = spec(Width::Bits64);
    let mut buf = [0_u8; 4096];
    let base = TOP_64 - 4096;
    let (image, _got) = round_trip(&spec, TOP_64, &mut buf);

    let start = usize::try_from(image.arg_start - base).expect("inside");
    let end = usize::try_from(image.arg_end - base).expect("inside");
    let blob = buf.get(start..end).expect("inside");
    let mut expected = Vec::new();
    for a in spec.args {
        expected.extend_from_slice(a);
        expected.push(0);
    }
    assert_eq!(
        blob,
        &expected[..],
        "the arguments sit NUL-separated with nothing between them"
    );
}

#[test]
fn nothing_below_the_stack_pointer_is_written() {
    // The kernel maps a stack far larger than the image. Writing below the
    // stack pointer would be writing where the program is about to push.
    let spec = spec(Width::Bits64);
    let mut buf = [0xAA_u8; 4096];
    let base = TOP_64 - 4096;
    let image = build(&spec, TOP_64, &mut buf).expect("it fits");

    let sp_offset = usize::try_from(image.sp - base).expect("inside");
    assert!(
        buf.get(..sp_offset)
            .expect("inside")
            .iter()
            .all(|&b| b == 0xAA),
        "every byte below the stack pointer is untouched"
    );
}

#[test]
fn the_vector_ends_where_the_walk_says_it_does() {
    // A walk that ran off the end, or stopped early, would still "succeed" if
    // the terminators happened to be zero bytes. Check the cursor lands inside
    // the buffer and above the stack pointer.
    let spec = spec(Width::Bits64);
    let mut buf = [0_u8; 4096];
    let base = TOP_64 - 4096;
    let (image, _got) = round_trip(&spec, TOP_64, &mut buf);

    let mut walk = Walk::new(&buf, base, image.sp, spec.width);
    let _argc = walk.argc().expect("readable");
    while walk.next_arg().expect("well formed").is_some() {}
    while walk.next_env().expect("well formed").is_some() {}
    while walk.next_aux().expect("well formed").is_some() {}
    assert!(
        walk.position() > image.sp && walk.position() <= TOP_64,
        "the vector ends inside the image"
    );
}

#[test]
fn a_32_bit_image_is_smaller_than_the_same_64_bit_one() {
    let mut buf = [0_u8; 4096];
    let wide = build(&spec(Width::Bits64), TOP_64, &mut buf).expect("it fits");
    let wide_words = TOP_64 - wide.sp;
    let mut buf = [0_u8; 4096];
    let narrow = build(&spec(Width::Bits32), TOP_32, &mut buf).expect("it fits");
    let narrow_words = TOP_32 - narrow.sp;
    assert!(
        narrow_words < wide_words,
        "four-byte pointers make a shorter image: {narrow_words} against {wide_words}"
    );
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[test]
fn the_keys_this_crate_supplies_are_refused_from_the_caller() {
    for key in [AT_RANDOM, AT_EXECFN, AT_PLATFORM, AT_NULL] {
        let mut spec = spec(Width::Bits64);
        let auxv = [(key, 1)];
        spec.auxv = &auxv;
        let mut buf = [0_u8; 4096];
        assert_eq!(
            build(&spec, TOP_64, &mut buf),
            Err(StackError::ReservedAuxKey(key)),
            "a duplicate key is resolved differently by every C library, so it is refused"
        );
    }
}

#[test]
fn a_value_too_wide_for_a_32_bit_pointer_is_refused_not_truncated() {
    let mut spec = spec(Width::Bits32);
    let auxv = [(AT_ENTRY, 0x1_0000_0000)];
    spec.auxv = &auxv;
    let mut buf = [0_u8; 4096];
    assert_eq!(
        build(&spec, TOP_32, &mut buf),
        Err(StackError::ValueTooWide(0x1_0000_0000)),
        "a truncated AT_ENTRY is a pointer into the wrong page, and musl would follow it"
    );
}

#[test]
fn an_unaligned_stack_top_is_refused() {
    let spec = spec(Width::Bits64);
    let mut buf = [0_u8; 4096];
    assert_eq!(
        build(&spec, TOP_64 + 8, &mut buf),
        Err(StackError::UnalignedTop)
    );
}

#[test]
fn a_buffer_too_small_is_refused_rather_than_truncated() {
    let spec = spec(Width::Bits64);
    let mut buf = [0_u8; 64];
    assert!(
        matches!(
            build(&spec, TOP_64, &mut buf),
            Err(StackError::TooSmall | StackError::Overflow)
        ),
        "a half-written image is worse than no image"
    );
}

#[test]
fn a_string_longer_than_the_limit_is_refused() {
    let long = std::vec![b'x'; MAX_ARG_STRLEN + 1];
    let args: [&[u8]; 1] = [&long];
    let mut spec = spec(Width::Bits64);
    spec.args = &args;
    let mut buf = [0_u8; 4096];
    assert_eq!(
        build(&spec, TOP_64, &mut buf),
        Err(StackError::StringTooLong),
        "Linux calls this E2BIG, and so should we"
    );
}

#[test]
fn an_empty_argument_and_environment_vector_still_builds() {
    // `execve` permits both to be empty. musl handles it; so must this.
    let spec = Spec {
        args: &[],
        env: &[],
        auxv: &[],
        random: [0; RANDOM_BYTES],
        exec_fn: b"",
        platform: None,
        width: Width::Bits64,
    };
    let mut buf = [0_u8; 256];
    let (image, got) = round_trip(&spec, TOP_64, &mut buf);
    assert_eq!(got.argc, 0);
    assert!(got.args.is_empty());
    assert!(got.env.is_empty());
    assert_eq!(got.auxv.len(), 2, "AT_RANDOM and AT_EXECFN remain");
    assert_eq!(image.arg_start, image.arg_end);
}

#[test]
fn an_empty_string_is_an_argument_not_a_terminator() {
    // argv entries may be empty. If the builder or the walk treated an empty
    // string as the end of the vector, this would come back short.
    let args: [&[u8]; 3] = [b"a", b"", b"c"];
    let mut spec = spec(Width::Bits64);
    spec.args = &args;
    let mut buf = [0_u8; 4096];
    let (_image, got) = round_trip(&spec, TOP_64, &mut buf);
    assert_eq!(got.argc, 3);
    assert_eq!(got.args, args);
}

// ---------------------------------------------------------------------------
// The reader's own refusals
// ---------------------------------------------------------------------------

#[test]
fn the_walk_refuses_an_address_outside_the_buffer() {
    let buf = [0_u8; 64];
    let walk = Walk::new(&buf, 0x1000, 0x1000, Width::Bits64);
    assert_eq!(walk.cstr_at(0x0fff), Err(ReadError::OutOfBounds(0x0fff)));
    assert_eq!(walk.cstr_at(0x2000), Err(ReadError::OutOfBounds(0x2000)));
}

#[test]
fn the_walk_refuses_a_string_with_no_terminator() {
    let buf = [b'x'; 16];
    let walk = Walk::new(&buf, 0x1000, 0x1000, Width::Bits64);
    assert_eq!(walk.cstr_at(0x1000), Err(ReadError::Unterminated(0x1000)));
}

#[test]
fn a_string_with_a_nul_inside_it_is_refused() {
    // Found by the fuzzer, and it is the sharpest kind of bug this crate can
    // have: the image builds, the walk succeeds, and the program is handed a
    // *different, shorter* argument than the one that was asked for, because
    // everything on this stack is read back by scanning for a NUL.
    for spoiled in [&b"\0"[..], &b"a\0b"[..], &b"trailing\0"[..]] {
        let args: [&[u8]; 2] = [b"ok", spoiled];
        let mut spec = spec(Width::Bits64);
        spec.args = &args;
        let mut buf = [0_u8; 4096];
        assert_eq!(
            build(&spec, TOP_64, &mut buf),
            Err(StackError::EmbeddedNul),
            "an argument that would be truncated on read is refused instead"
        );
    }
}

#[test]
fn every_string_the_image_holds_gets_the_same_check() {
    // The vectors are the obvious ones; AT_EXECFN and AT_PLATFORM are read
    // back by the same NUL scan and are just as easy to forget.
    let mut by_execfn = spec(Width::Bits64);
    by_execfn.exec_fn = b"/bin/\0sh";
    let mut buf = [0_u8; 4096];
    assert_eq!(
        build(&by_execfn, TOP_64, &mut buf),
        Err(StackError::EmbeddedNul),
        "AT_EXECFN is a C string too"
    );

    let mut by_platform = spec(Width::Bits64);
    by_platform.platform = Some(b"x86\0_64");
    assert_eq!(
        build(&by_platform, TOP_64, &mut buf),
        Err(StackError::EmbeddedNul),
        "AT_PLATFORM is a C string too"
    );

    let mut by_env = spec(Width::Bits64);
    let env: [&[u8]; 1] = [b"PATH=\0/bin"];
    by_env.env = &env;
    assert_eq!(
        build(&by_env, TOP_64, &mut buf),
        Err(StackError::EmbeddedNul),
        "the environment is checked as well as the arguments"
    );
}
