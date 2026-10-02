use std::vec::Vec;

use super::*;
use crate::syscall::{self, nr};
use crate::time::CLOCK_MONOTONIC;

/// The address the test image's segment says it is loaded at, so that a
/// lookup that forgot to take it off is caught.
const VADDR: u64 = 0x1000;

/// Writes `value`'s little-endian bytes at `at`.
fn put(image: &mut [u8], at: usize, value: &[u8]) {
    image[at..at + value.len()].copy_from_slice(value);
}

/// A small image laid out as a vDSO is: `foo` twice, at `LINUX_2.5` (0x10)
/// and at `LINUX_2.6` (0x20), and `bar` undefined. With `versions` false the
/// dynamic section names no `DT_VERSYM`, so versions are not checked.
fn image(versions: bool) -> Vec<u8> {
    const PHDRS: usize = 64;
    const DYNAMIC: usize = 176;
    const HASH: usize = 272;
    const SYMTAB: usize = 304;
    const VERSYM: usize = 400;
    const VERDEF: usize = 408;
    const STRTAB: usize = 492;
    let strings = b"\0foo\0LINUX_2.6\0LINUX_2.5\0image\0bar\0";
    let (foo, v26, v25, base, bar) = (1u32, 5u32, 15u32, 25u32, 31u32);
    let len = STRTAB + strings.len();
    let mut image = vec![0u8; len];
    let address = |offset: usize| VADDR + offset as u64;

    put(&mut image, 0, &[0x7f, b'E', b'L', b'F', 2, 1, 1]);
    put(&mut image, 16, &3u16.to_le_bytes());
    put(&mut image, 32, &(PHDRS as u64).to_le_bytes());
    put(&mut image, 54, &56u16.to_le_bytes());
    put(&mut image, 56, &2u16.to_le_bytes());

    // PT_LOAD: the whole image, at `VADDR`.
    put(&mut image, PHDRS, &PT_LOAD.to_le_bytes());
    put(&mut image, PHDRS + 16, &VADDR.to_le_bytes());
    put(&mut image, PHDRS + 32, &(len as u64).to_le_bytes());
    put(&mut image, PHDRS + 40, &(len as u64).to_le_bytes());
    // PT_DYNAMIC.
    put(&mut image, PHDRS + 56, &PT_DYNAMIC.to_le_bytes());
    put(&mut image, PHDRS + 56 + 8, &(DYNAMIC as u64).to_le_bytes());
    put(&mut image, PHDRS + 56 + 16, &address(DYNAMIC).to_le_bytes());
    put(&mut image, PHDRS + 56 + 32, &96u64.to_le_bytes());

    let mut entries = vec![
        (DT_STRTAB, address(STRTAB)),
        (DT_SYMTAB, address(SYMTAB)),
        (DT_HASH, address(HASH)),
        (DT_VERDEF, address(VERDEF)),
    ];
    if versions {
        entries.push((DT_VERSYM, address(VERSYM)));
    }
    for (index, (tag, value)) in entries.into_iter().enumerate() {
        put(&mut image, DYNAMIC + 16 * index, &tag.to_le_bytes());
        put(&mut image, DYNAMIC + 16 * index + 8, &value.to_le_bytes());
    }

    // One bucket, four chain entries: the count is all `lookup` reads.
    for (index, word) in [1u32, 4, 1, 0, 2, 3, 0].into_iter().enumerate() {
        put(&mut image, HASH + 4 * index, &word.to_le_bytes());
    }

    // Symbols 1 to 3: global functions in section 7, but `bar` undefined.
    for (index, (name, shndx, value, version)) in [
        (foo, 7u16, 0x10u64, 2u16),
        (foo, 7, 0x20, 3),
        (bar, 0, 0x30, 3),
    ]
    .into_iter()
    .enumerate()
    {
        let sym = SYMTAB + 24 * (index + 1);
        put(&mut image, sym, &name.to_le_bytes());
        put(&mut image, sym + 4, &[0x12]);
        put(&mut image, sym + 6, &shndx.to_le_bytes());
        put(&mut image, sym + 8, &(VADDR + value).to_le_bytes());
        put(&mut image, VERSYM + 2 * (index + 1), &version.to_le_bytes());
    }

    // Three definitions, each followed by its one auxiliary entry: the base
    // one, the image's own name, then the two versions.
    for (index, (flags, name)) in [(VER_FLG_BASE, base), (0, v25), (0, v26)]
        .into_iter()
        .enumerate()
    {
        let def = VERDEF + 28 * index;
        put(&mut image, def, &1u16.to_le_bytes());
        put(&mut image, def + 2, &flags.to_le_bytes());
        put(&mut image, def + 4, &(index as u16 + 1).to_le_bytes());
        put(&mut image, def + 6, &1u16.to_le_bytes());
        put(&mut image, def + 12, &20u32.to_le_bytes());
        let next: u32 = if index == 2 { 0 } else { 28 };
        put(&mut image, def + 16, &next.to_le_bytes());
        put(&mut image, def + 20, &name.to_le_bytes());
    }
    put(&mut image, STRTAB, strings);
    image
}

#[test]
fn a_symbol_is_found_at_the_version_asked_for() {
    let image = image(true);
    assert_eq!(lookup(&image, b"foo", b"LINUX_2.6"), Some(0x20));
    assert_eq!(lookup(&image, b"foo", b"LINUX_2.5"), Some(0x10));
    // The base definition names the image, not a version.
    assert_eq!(lookup(&image, b"foo", b"image"), None);
    assert_eq!(lookup(&image, b"foo", b"LINUX_2.7"), None);
    // A prefix of a name, or of a version, is not the name.
    assert_eq!(lookup(&image, b"fo", b"LINUX_2.6"), None);
    assert_eq!(lookup(&image, b"foo", b"LINUX_2"), None);
}

#[test]
fn an_undefined_symbol_is_not_found() {
    assert_eq!(lookup(&image(true), b"bar", b"LINUX_2.6"), None);
}

#[test]
fn without_symbol_versions_the_first_definition_is_taken() {
    assert_eq!(lookup(&image(false), b"foo", b"LINUX_2.6"), Some(0x10));
}

#[test]
fn a_truncated_or_damaged_image_is_refused_without_a_panic() {
    let whole = image(true);
    for len in 0..whole.len() {
        // The string table is last, and `foo` is found before `bar`'s name.
        let found = lookup(&whole[..len], b"foo", b"LINUX_2.6");
        assert!(found.is_none() || found == Some(0x20), "{len} bytes");
    }
    for at in 0..whole.len() {
        let mut damaged = whole.clone();
        damaged[at] ^= 0xff;
        let _ = lookup(&damaged, b"foo", b"LINUX_2.6");
    }
    let mut not_elf = whole.clone();
    not_elf[0] = 0;
    // SAFETY: the image is a live allocation longer than an ELF header.
    assert_eq!(unsafe { extent(not_elf.as_ptr()) }, None);
}

#[test]
fn extent_covers_the_segments() {
    let image = image(true);
    // SAFETY: the image is a live allocation as long as its headers say.
    assert_eq!(unsafe { extent(image.as_ptr()) }, Some(image.len()));
}

/// The host's own vDSO, from this test process's auxiliary vector, when the
/// host maps one.
fn host_vdso() -> Option<usize> {
    let vector = std::fs::read("/proc/self/auxv").ok()?;
    vector
        .chunks_exact(16)
        .map(|pair| {
            let key = usize::from_le_bytes(pair[..8].try_into().unwrap_or_default());
            let value = usize::from_le_bytes(pair[8..].try_into().unwrap_or_default());
            (key, value)
        })
        .find(|&(key, _)| key == AT_SYSINFO_EHDR)
        .map(|(_, value)| value)
        .filter(|&value| value != 0)
}

#[test]
fn the_hosts_vdso_reads_the_clock_the_system_call_reads() {
    let Some(ehdr) = host_vdso() else {
        std::eprintln!("vdso: the host maps no vDSO; skipped");
        return;
    };
    let slot = Slot::new(b"__vdso_clock_gettime");
    // SAFETY: `ehdr` is this process's vDSO, mapped for its life.
    let address = unsafe { slot.settle(Some(Some(ehdr))) };
    let Some(address) = address else {
        panic!("the host's vDSO has no __vdso_clock_gettime at LINUX_2.6");
    };
    assert_eq!(slot.get(), Some(address));
    // SAFETY: the address is the host vDSO's `__vdso_clock_gettime`.
    let function = unsafe { core::mem::transmute::<usize, ClockGettime>(address) };

    let mut before = Timespec::default();
    let mut through = Timespec::default();
    let mut after = Timespec::default();
    // SAFETY: each pointer is a live local.
    let ret = unsafe {
        syscall::syscall2(
            nr::CLOCK_GETTIME,
            CLOCK_MONOTONIC as usize,
            (&raw mut before).addr(),
        )
    };
    assert_eq!(ret, 0);
    // SAFETY: as above.
    assert_eq!(unsafe { function(CLOCK_MONOTONIC, &raw mut through) }, 0);
    // SAFETY: as above.
    let ret = unsafe {
        syscall::syscall2(
            nr::CLOCK_GETTIME,
            CLOCK_MONOTONIC as usize,
            (&raw mut after).addr(),
        )
    };
    assert_eq!(ret, 0);
    let nanos = |ts: Timespec| i128::from(ts.tv_sec) * 1_000_000_000 + i128::from(ts.tv_nsec);
    assert!(nanos(before) <= nanos(through) && nanos(through) <= nanos(after));

    for name in [&b"__vdso_gettimeofday"[..], b"__vdso_time"] {
        // SAFETY: as above.
        assert!(unsafe { find(ehdr, name) }.is_some());
    }
    // SAFETY: as above.
    assert_eq!(unsafe { find(ehdr, b"__vdso_no_such_function") }, None);
}

#[test]
fn a_slot_decides_nothing_until_the_vector_is_known() {
    let slot = Slot::new(b"__vdso_clock_gettime");
    // SAFETY: no address is given.
    assert_eq!(unsafe { slot.settle(None) }, None);
    assert_eq!(slot.address.load(Ordering::Relaxed), UNRESOLVED);
    // SAFETY: no address is given.
    assert_eq!(unsafe { slot.settle(Some(None)) }, None);
    assert_eq!(slot.address.load(Ordering::Relaxed), ABSENT);
    assert_eq!(slot.get(), None);
}

#[test]
fn threads_resolving_at_once_agree() {
    let Some(ehdr) = host_vdso() else {
        return;
    };
    let slot = Slot::new(b"__vdso_time");
    let found: Vec<Option<usize>> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                // SAFETY: `ehdr` is this process's vDSO, mapped for its life.
                scope.spawn(|| unsafe { slot.settle(Some(Some(ehdr))) })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap_or_default())
            .collect()
    });
    assert!(found[0].is_some());
    assert!(found.iter().all(|&address| address == found[0]));
    assert_eq!(slot.get(), found[0]);
}
