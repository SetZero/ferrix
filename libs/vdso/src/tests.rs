use std::vec;
use std::vec::Vec;

use crate::{
    BuildError, CODE_AT, EM_X86_64, Function, IMAGE_BYTES, MAX_FUNCTIONS, SONAME, Spec, VERSION,
    build, elf_hash, lookup, lookup_hashed,
};

/// Both lookups, which must agree.
fn found(image: &[u8], name: &str, version: Option<&str>) -> Option<usize> {
    let walked = lookup(image, name, version);
    assert_eq!(
        walked,
        lookup_hashed(image, name, version),
        "the hash table and the symbol table disagree about {name}"
    );
    walked
}

fn image(spec: &Spec<'_>) -> Vec<u8> {
    let mut out = vec![0xa5; IMAGE_BYTES];
    build(spec, &mut out).unwrap();
    out
}

/// The hash `DT_HASH` and version definitions use. `LINUX_2.6`'s is the
/// constant glibc compares a vDSO's version against (`VDSO_HASH_LINUX_2_6`,
/// 61765110); the long name's, one that folds the high bits back, is from a
/// second implementation of the System V ABI's definition.
#[test]
fn the_elf_hash_is_the_system_v_one() {
    assert_eq!(elf_hash(b""), 0);
    assert_eq!(elf_hash(b"a"), 0x61);
    assert_eq!(elf_hash(b"LINUX_2.6"), 61_765_110);
    assert_eq!(elf_hash(b"__vdso_clock_gettime"), 0x0d35_ec75);
}

/// Every function is found by its name and its alias, at `LINUX_2.6` or
/// with no version asked for, at `CODE_AT` plus its offset; another
/// version or another name finds nothing.
#[test]
fn every_function_is_found_where_the_code_put_it() {
    let code = [0xcc_u8; 64];
    let functions = [
        Function {
            name: "__vdso_clock_gettime",
            alias: Some("clock_gettime"),
            offset: 0,
        },
        Function {
            name: "__vdso_gettimeofday",
            alias: Some("gettimeofday"),
            offset: 16,
        },
        Function {
            name: "__vdso_getcpu",
            alias: None,
            offset: 48,
        },
    ];
    let image = image(&Spec {
        machine: EM_X86_64,
        code: &code,
        functions: &functions,
    });
    for function in &functions {
        let at = Some(CODE_AT + function.offset);
        for name in [Some(function.name), function.alias].into_iter().flatten() {
            assert_eq!(found(&image, name, Some(VERSION)), at, "{name}");
            assert_eq!(found(&image, name, None), at, "{name}");
            assert_eq!(found(&image, name, Some("LINUX_2.5")), None, "{name}");
        }
    }
    assert_eq!(found(&image, "__vdso_time", None), None);
    assert_eq!(found(&image, "getcpu", None), None);
    // The image's own name and version are strings, not symbols.
    assert_eq!(found(&image, SONAME, None), None);
    assert_eq!(found(&image, VERSION, None), None);
    assert_eq!(
        image.get(CODE_AT..CODE_AT + code.len()),
        Some(code.as_slice())
    );
    assert!(
        image
            .get(CODE_AT + code.len()..)
            .unwrap()
            .iter()
            .all(|&byte| byte == 0)
    );
}

/// The headers a loader reads: a 64-bit little-endian shared object for the
/// machine asked for, with a loadable segment covering the whole page from
/// address zero, a read-only dynamic segment, and a stack that need not be
/// executable.
#[test]
fn the_headers_describe_a_shared_object_linked_at_zero() {
    let code = [0x90_u8; 16];
    let functions = [Function {
        name: "f",
        alias: None,
        offset: 0,
    }];
    let image = image(&Spec {
        machine: EM_X86_64,
        code: &code,
        functions: &functions,
    });
    let u16_at = |at: usize| u16::from_le_bytes(image[at..at + 2].try_into().unwrap());
    let u32_at = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(image[at..at + 8].try_into().unwrap());
    assert_eq!(&image[..7], &[0x7f, b'E', b'L', b'F', 2, 1, 1]);
    assert_eq!(u16_at(16), 3, "ET_DYN");
    assert_eq!(u16_at(18), EM_X86_64);
    assert_eq!(u16_at(56), 3, "three program headers");
    // PT_LOAD, read and execute, offset and address zero, a page long.
    assert_eq!(u32_at(64), 1);
    assert_eq!(u32_at(68), 5);
    assert_eq!((u64_at(72), u64_at(80)), (0, 0));
    assert_eq!((u64_at(96), u64_at(104)), (4096, 4096));
    // PT_DYNAMIC, read-only.
    assert_eq!(u32_at(120), 2);
    assert_eq!(u32_at(124), 4);
    // PT_GNU_STACK, read-write.
    assert_eq!(u32_at(176), 0x6474_e551);
    assert_eq!(u32_at(180), 6);
    // The section headers end below the code, and the name table is the last.
    let shoff = u64_at(40) as usize;
    let shnum = usize::from(u16_at(60));
    assert!(shoff + 64 * shnum <= CODE_AT);
    assert_eq!(usize::from(u16_at(62)), shnum - 1);
}

/// With as many functions and aliases as the tables hold, every hash bucket
/// that two names share still chains to both.
#[test]
fn a_full_table_chains_every_symbol() {
    let names: Vec<std::string::String> = (0..MAX_FUNCTIONS)
        .map(|index| std::format!("__vdso_f{index}"))
        .collect();
    let aliases: Vec<std::string::String> = (0..MAX_FUNCTIONS)
        .map(|index| std::format!("f{index}"))
        .collect();
    let functions: Vec<Function<'_>> = names
        .iter()
        .zip(&aliases)
        .enumerate()
        .map(|(index, (name, alias))| Function {
            name,
            alias: Some(alias),
            offset: index * 4,
        })
        .collect();
    let code = [0_u8; 4 * MAX_FUNCTIONS];
    let image = image(&Spec {
        machine: EM_X86_64,
        code: &code,
        functions: &functions,
    });
    for function in &functions {
        let at = Some(CODE_AT + function.offset);
        assert_eq!(found(&image, function.name, Some(VERSION)), at);
        assert_eq!(found(&image, function.alias.unwrap(), Some(VERSION)), at);
    }
}

/// What a build refuses.
#[test]
fn what_a_build_refuses() {
    let one = [Function {
        name: "f",
        alias: None,
        offset: 0,
    }];
    let spec = |code: &'static [u8], functions| Spec {
        machine: EM_X86_64,
        code,
        functions,
    };
    let mut page = vec![0; IMAGE_BYTES];
    let mut short = vec![0; IMAGE_BYTES - 1];
    assert_eq!(
        build(&spec(&[0; 4], &one), &mut short),
        Err(BuildError::WrongSize)
    );
    static TOO_LONG: [u8; IMAGE_BYTES - CODE_AT + 1] = [0; IMAGE_BYTES - CODE_AT + 1];
    assert_eq!(
        build(&spec(&TOO_LONG, &one), &mut page),
        Err(BuildError::CodeTooLarge)
    );
    let past = [Function {
        name: "f",
        alias: None,
        offset: 4,
    }];
    assert_eq!(
        build(&spec(&[0; 4], &past), &mut page),
        Err(BuildError::BadOffset)
    );
    let many = [Function {
        name: "f",
        alias: None,
        offset: 0,
    }; MAX_FUNCTIONS + 1];
    assert_eq!(
        build(&spec(&[0; 4], &many), &mut page),
        Err(BuildError::TooManyFunctions)
    );
    let long_name = "x".repeat(600);
    let long = [Function {
        name: &long_name,
        alias: None,
        offset: 0,
    }];
    assert_eq!(
        build(&spec(&[0; 4], &long), &mut page),
        Err(BuildError::TablesTooLarge)
    );
}

/// A lookup in something that is not an image finds nothing, and does not
/// panic, however it is cut short.
#[test]
fn a_lookup_in_a_broken_image_finds_nothing() {
    let code = [0_u8; 8];
    let functions = [Function {
        name: "__vdso_time",
        alias: None,
        offset: 0,
    }];
    let image = image(&Spec {
        machine: EM_X86_64,
        code: &code,
        functions: &functions,
    });
    for len in 0..CODE_AT {
        let _ = lookup(&image[..len], "__vdso_time", Some(VERSION));
        let _ = lookup_hashed(&image[..len], "__vdso_time", Some(VERSION));
    }
    let mut bad = image.clone();
    bad[0] = 0;
    assert_eq!(lookup(&bad, "__vdso_time", None), None);
    // A chain that loops back on itself ends.
    let mut looped = image;
    // The hash table follows the three program headers.
    let hash = 64 + 3 * 56;
    let count = u32::from_le_bytes(looped[hash + 4..hash + 8].try_into().unwrap()) as usize;
    let chains = hash + 8 + 4 * count;
    for index in 0..count {
        looped[chains + 4 * index..chains + 4 * index + 4].copy_from_slice(&1_u32.to_le_bytes());
    }
    assert_eq!(lookup_hashed(&looped, "nothing", None), None);
}
