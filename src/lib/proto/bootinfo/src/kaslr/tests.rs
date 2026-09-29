//! Tests for where a loader may put what it moves.

use crate::*;

#[test]
fn slots_count_pick_and_skip() {
    let slots = Slots::new(0x1000, 0x100, 10);
    assert_eq!(slots.count(), 10);
    assert_eq!(slots.bits(), 3, "ten places are three bits, rounded down");
    assert_eq!(slots.nth(0), Some(0x1000));
    assert_eq!(slots.nth(9), Some(0x1900));
    assert_eq!(slots.nth(10), None);
    assert_eq!(slots.pick(23), Some(0x1300));

    // A 0x180-byte region at each slot, avoiding 0x1350..0x1500: slots 2 to 4
    // overlap it (0x1200+0x180 > 0x1350, and 0x1400 < 0x1500).
    let avoiding = slots.avoiding(0x180, 0x1350, 0x1500);
    assert_eq!(avoiding.count(), 7);
    assert_eq!(avoiding.nth(1), Some(0x1100));
    assert_eq!(avoiding.nth(2), Some(0x1500), "the first one clear of it");
    assert_eq!(avoiding.nth(6), Some(0x1900));
    for index in 0..avoiding.count() {
        let base = avoiding.nth(index).unwrap();
        assert!(base + 0x180 <= 0x1350 || base >= 0x1500, "{base:#x}");
    }

    // Something wholly outside the candidates takes nothing out.
    assert_eq!(slots.avoiding(0x100, 0x10, 0x20).count(), 10);
    assert_eq!(slots.avoiding(0x100, 0x9000, 0xA000).count(), 10);
    assert_eq!(Slots::NONE.bits(), 0);
    assert_eq!(Slots::NONE.pick(7), None);
    assert_eq!(Slots::new(0, 1, 1).bits(), 0, "one place is no bits");
}

#[test]
fn a_kernel_image_moves_inside_its_region_and_never_stays() {
    for layout in [LAYOUT_64, LAYOUT_32] {
        let len = 24 * 1024 * 1024;
        for granule in [PAGE_SIZE, 0x1_0000, 2 * 1024 * 1024] {
            let slots = layout.kernel_slots(len, granule);
            assert!(slots.count() > 0);
            let first = slots.nth(0).unwrap();
            let last = slots.nth(slots.count() - 1).unwrap();
            assert_eq!(
                first,
                layout.kernel_base + granule,
                "the link address is not a slot"
            );
            assert!(last + len <= layout.kernel_ceiling());
            assert!(
                (last + len)
                    .checked_add(granule)
                    .is_none_or(|end| end > layout.kernel_ceiling()),
                "the last slot is the highest"
            );
            assert!(first.is_multiple_of(granule));
        }
        assert_eq!(layout.kernel_slots(len, 3 * PAGE_SIZE), Slots::NONE);
        assert_eq!(layout.kernel_slots(len, 0x800), Slots::NONE);
        assert_eq!(
            layout.kernel_slots(1 << 31, PAGE_SIZE),
            Slots::NONE,
            "too large to move"
        );
    }
    // What SPECULATION.md argues: some 19 bits on the 64-bit pair at a page,
    // and 11 on ARMv7-A at 64 KiB.
    assert_eq!(LAYOUT_64.kernel_slots(24 << 20, PAGE_SIZE).bits(), 18);
    assert_eq!(LAYOUT_64.kernel_slots(4 << 20, PAGE_SIZE).bits(), 18);
    assert_eq!(LAYOUT_32.kernel_slots(8 << 20, 0x1_0000).bits(), 11);
}

#[test]
fn a_direct_map_moves_in_whole_granules_inside_its_region() {
    for layout in [LAYOUT_64, LAYOUT_32] {
        let len = 512 * 1024 * 1024;
        let slots = layout.physmap_slots(len, None);
        assert_eq!(
            slots.nth(0),
            Some(layout.physmap_base),
            "the fixed base is one"
        );
        let last = slots.nth(slots.count() - 1).unwrap();
        assert!(last + len <= layout.physmap_end);
        assert!(last + len + layout.physmap_granule() > layout.physmap_end);
        let full = layout.physmap_slots(layout.physmap_size(), None);
        assert_eq!(
            full.count(),
            1,
            "a direct map that fills its region cannot move"
        );
        assert_eq!(
            layout.physmap_slots(layout.physmap_size() + 1, None),
            Slots::NONE
        );
    }
    assert_eq!(LAYOUT_64.physmap_slots(512 << 20, None).bits(), 16);
    assert_eq!(LAYOUT_32.physmap_slots(512 << 20, None).bits(), 8);

    // The DK1: 512 MiB at 3 GiB, and the loader's image near its top, which
    // the loader maps at its own address in the kernel's tree.
    let loader = (0xDDF0_0000, 0xDDF4_0000);
    let slots = LAYOUT_32.physmap_slots(512 << 20, Some(loader));
    assert!(slots.count() > 0);
    for index in 0..slots.count() {
        let base = slots.nth(index).unwrap();
        assert!(
            base + (512 << 20) <= loader.0 || base >= loader.1,
            "{base:#x}"
        );
        let plan = LAYOUT_32.plan_identity_map_in(
            Placement {
                physmap_base: base,
                kernel_base: LAYOUT_32.kernel_base,
            },
            0xC000_0000,
            512 << 20,
            loader.0,
            loader.1 - loader.0,
            0x50_0000,
        );
        assert_eq!(
            plan.map(|plan| plan.tree),
            Ok(IdentityTree::Kernel),
            "{base:#x}"
        );
    }
}

#[test]
fn the_vmap_arena_top_moves_by_whole_granules() {
    for layout in [LAYOUT_64, LAYOUT_32] {
        let ends = layout.vmap_ends();
        assert_eq!(ends.count(), layout.vmap_slots());
        assert_eq!(ends.nth(ends.count() - 1), Some(layout.vmap_end()));
        for index in [0, 1, ends.count() - 1] {
            assert!(layout.is_vmap_end(ends.nth(index).unwrap()));
        }
        assert!(!layout.is_vmap_end(layout.vmap_end() - PAGE_SIZE));
        assert!(!layout.is_vmap_end(ends.nth(0).unwrap() - layout.vmap_granule()));
        assert!(ends.nth(0).unwrap() > layout.vmap_base + layout.vmap_reserved);
    }
    assert_eq!(LAYOUT_64.vmap_ends().bits(), 17);
    assert_eq!(LAYOUT_32.vmap_ends().bits(), 9);
}

#[test]
fn a_loader_that_moved_nothing_says_why() {
    let fixed = Kaslr::fixed(&LAYOUT_64, KASLR_NO_ENTROPY);
    assert_eq!(fixed.link, LAYOUT_64.kernel_base);
    assert_eq!(fixed.vmap_end, LAYOUT_64.vmap_end());
    assert!(!fixed.is_random());
    assert!(fixed.state_text().starts_with("NOT randomised"));
    let moved = Kaslr {
        state: KASLR_MOVED,
        source: SOURCE_FIRMWARE_RNG,
        ..fixed
    };
    assert!(moved.is_random());
    assert_eq!(moved.source_text(), "EFI_RNG");
    let guessed = Kaslr {
        source: SOURCE_COUNTER,
        ..moved
    };
    assert!(!guessed.is_random(), "a counter is not a secret");
    let trng = Kaslr {
        source: SOURCE_SMCCC_TRNG,
        ..moved
    };
    assert!(trng.is_random(), "firmware's TRNG is a secret source");
    assert_eq!(trng.source_text(), "SMCCC TRNG");
}
