use super::*;

#[test]
fn inserts_merge_with_touching_runs() {
    let mut set = RangeSet::new();
    assert!(set.insert(10, 10));
    assert!(set.insert(30, 10));
    assert!(set.insert(20, 10));
    assert_eq!(set.iter().collect::<alloc::vec::Vec<_>>(), [(10, 30)]);
    assert!(!set.insert(15, 1), "a byte already present is refused");
    assert_eq!(set.total(), 30);
}

#[test]
fn removes_split_the_run_they_are_in() {
    let mut set = RangeSet::new();
    assert!(set.insert(0, 100));
    assert!(set.remove(40, 20));
    assert_eq!(
        set.iter().collect::<alloc::vec::Vec<_>>(),
        [(0, 40), (60, 40)]
    );
    assert!(!set.remove(30, 20), "a range across a gap is refused");
    assert!(set.remove(0, 40));
    assert!(set.contains(60, 40));
    assert!(!set.overlaps(0, 60));
}

#[test]
fn first_fit_aligns_and_avoids_the_boundary() {
    let mut set = RangeSet::new();
    assert!(set.insert(4096, 1 << 20));
    // 16 KiB blocks at 16 KiB alignment, never across 64 KiB.
    assert_eq!(set.first_fit(16384, 16384, 65536, 0), Some(16384));
    assert_eq!(set.first_fit(16384, 16384, 65536, 60000), Some(65536));
    // Wraps to the start when nothing fits after the cursor.
    assert_eq!(set.first_fit(16384, 16384, 65536, 2 << 20), Some(16384));
    assert_eq!(set.first_fit(2 << 20, 4096, 0, 0), None);
}

#[test]
fn first_prefix_takes_what_the_run_has() {
    let mut set = RangeSet::new();
    assert!(set.insert(0, 8192));
    assert!(set.insert(65536, 1 << 20));
    assert_eq!(set.first_prefix(1 << 30, 4096, 4096, 0), Some((0, 8192)));
    assert_eq!(
        set.first_prefix(1 << 30, 16384, 4096, 0),
        Some((65536, 1 << 20))
    );
}
