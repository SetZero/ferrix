//! Fuzz the buddy allocator by driving it with an arbitrary command stream.
//!
//! Unlike the ELF target, the input here is not data the kernel will one day
//! parse — it is a *schedule*. Each byte is an allocate or a free, and what is
//! being hunted is an ordering of them that breaks an invariant: a frame handed
//! to two callers at once, a free-frame count that stops matching reality, or a
//! block that merges into memory the allocator was never given.
//!
//! Those are the bugs that do not reproduce. A page handed out twice does not
//! fail where it happens; it fails later, in whichever subsystem writes to the
//! page second, and the report names that subsystem.
//!
//! The schedule also shares and releases frames, which is what copy-on-write
//! does to this allocator: a `fork` raises a count, a write fault lowers it,
//! and the frame must come back exactly when the last reference goes and not a
//! command sooner. A model count is carried beside each live block and checked
//! against the allocator's after every operation, so a count that drifts is
//! caught at the command that drifted it rather than at the eventual
//! double-free.

#![no_main]

use ferrix_frame::{Frame, FrameError, Frames, MAX_ORDER, PageEntry, Released, State};
use libfuzzer_sys::fuzz_target;

/// Frames in the arena. Small enough that exhaustion is reached often, which is
/// where the interesting orderings live.
const FRAMES: usize = 512;

/// Highest order the driver will ask for. Above this every request fails on an
/// arena this size and the schedule stops exploring anything.
const MAX_REQUEST: u8 = 6;

fuzz_target!(|data: &[u8]| {
    let mut entries = [PageEntry::RESERVED; FRAMES];
    let mut frames = Frames::new(&mut entries, 0);

    // Let the first byte carve a hole, so schedules run against a fragmented
    // arena as well as a whole one -- which is what a real memory map is.
    let mut input = data.iter().copied();
    let hole = usize::from(input.next().unwrap_or(0)) % 4;
    match hole {
        0 => frames.insert_free(0, FRAMES as u64),
        1 => {
            frames.insert_free(0, 100);
            frames.insert_free(200, 312);
        }
        2 => frames.insert_free(1, FRAMES as u64 - 2),
        _ => frames.insert_free(0, 300),
    }

    let arena = frames.free_frames();

    // Which live allocation owns each frame, or 0 for nobody.
    let mut owner = [0u32; FRAMES];
    // Block, order, and how many references this model believes it has.
    let mut live: Vec<(Frame, u8, u32)> = Vec::new();
    let mut next_id: u32 = 1;

    for command in input {
        if command & 0x80 == 0 {
            let order = (command % (MAX_REQUEST + 1)) as u8;
            let Some(block) = frames.allocate(order) else {
                continue;
            };

            assert!(
                block.is_multiple_of(1u64 << order),
                "order {order} block at {block} is not aligned to its own order",
            );

            let id = next_id;
            next_id = next_id.wrapping_add(1).max(1);

            for offset in 0..(1u64 << order) {
                let index = usize::try_from(block + offset).expect("frame inside the arena");
                let expected = if offset == 0 {
                    State::Allocated
                } else {
                    State::AllocatedTail
                };
                assert_eq!(
                    frames.state(block + offset),
                    Some(expected),
                    "frame {} of an allocated block is not marked allocated",
                    block + offset,
                );
                assert_eq!(
                    owner[index], 0,
                    "frame {} was handed out twice",
                    block + offset,
                );
                owner[index] = id;
            }
            assert_eq!(
                frames.entry(block).map(PageEntry::refcount),
                Some(1),
                "a fresh allocation starts with exactly one reference",
            );
            live.push((block, order, 1));
        } else if !live.is_empty() {
            let victim = usize::from(command & 0x1F) % live.len();
            let (block, order, count) = live[victim];

            if command & 0x40 != 0 && order == 0 {
                // Another address space maps the page.
                let raised = frames.share(block).expect("a live frame shares");
                assert_eq!(raised, count + 1, "share did not report the new count");
                assert_eq!(
                    frames.entry(block).map(PageEntry::refcount),
                    Some(raised),
                    "the allocator's count disagrees with what share returned",
                );
                live[victim].2 = raised;

                // While it is shared, the allocator must refuse to take it back
                // from one holder alone. This is the check that would have
                // caught the whole class of bug the guard exists for.
                assert_eq!(
                    frames.deallocate(block, 0),
                    Err(FrameError::StillShared(block)),
                    "a shared frame was freed out from under a holder",
                );
                assert_eq!(frames.state(block), Some(State::Allocated));
            } else if count > 1 {
                // One holder goes away; the frame belongs to the others.
                match frames.release(block).expect("a live frame releases") {
                    Released::Shared(left) => {
                        assert_eq!(left, count - 1, "release did not report the new count");
                        live[victim].2 = left;
                    }
                    Released::Freed => {
                        panic!("frame {block} was freed with {} references left", count - 1)
                    }
                }
                assert_eq!(
                    frames.state(block),
                    Some(State::Allocated),
                    "a frame with references left was returned to the allocator",
                );
            } else {
                // The last reference. Half the time through `release` and half
                // through `deallocate`, because both are live paths in the
                // kernel and only one of them is on the copy-on-write side.
                let (block, order, _) = live.swap_remove(victim);
                for offset in 0..(1u64 << order) {
                    let index = usize::try_from(block + offset).expect("frame inside the arena");
                    owner[index] = 0;
                }

                if command & 0x20 != 0 && order == 0 {
                    assert_eq!(
                        frames.release(block),
                        Ok(Released::Freed),
                        "the last reference did not free the frame",
                    );
                } else {
                    frames.deallocate(block, order).expect("a live block frees");
                }

                // Free, but not necessarily the *head* of a free block: if it
                // coalesced upwards with a lower buddy, the merged block's head
                // is the buddy and this frame is now `FreeTail`. Both mean
                // free; only `Allocated` would be the bug.
                assert!(
                    matches!(
                        frames.state(block),
                        Some(State::Free | State::FreeTail)
                    ),
                    "frame {block} is {:?} after being freed",
                    frames.state(block),
                );
            }
        }

        // The books balance after every single command: frames out with
        // callers, plus frames on the free lists, is the whole arena. Checking
        // it here rather than at the end is what turns "the count is wrong" into
        // "the count went wrong at *this* command".
        let held: u64 = live.iter().map(|&(_, order, _)| 1u64 << order).sum();
        assert_eq!(held + frames.free_frames(), arena, "frame accounting drifted");
    }

    // Give everything back; the arena must be whole again, and no frame outside
    // what was inserted may have appeared on a free list along the way.
    for (block, order, count) in live {
        // Shared frames need every reference dropped; the arena is not whole
        // again until the last one goes, which is the property being asserted
        // immediately below.
        for _ in 1..count {
            let left = frames.release(block).expect("a shared frame releases");
            assert_ne!(left, Released::Freed, "freed before the last reference");
        }
        frames.deallocate(block, order).expect("a live block frees");
    }
    assert_eq!(
        frames.free_frames(),
        arena,
        "freeing everything did not restore the arena",
    );
    assert_eq!(
        frames.free_frames(),
        frames.managed_frames(),
        "free frames should equal managed frames once nothing is held",
    );

    let _ = MAX_ORDER;
});
