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

#![no_main]

use ferrix_frame::{Frame, Frames, MAX_ORDER, PageEntry, State};
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
    let mut live: Vec<(Frame, u8)> = Vec::new();
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
                assert_eq!(
                    frames.state(block + offset),
                    Some(State::Allocated),
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
            live.push((block, order));
        } else if !live.is_empty() {
            let victim = usize::from(command & 0x7F) % live.len();
            let (block, order) = live.swap_remove(victim);
            for offset in 0..(1u64 << order) {
                let index = usize::try_from(block + offset).expect("frame inside the arena");
                owner[index] = 0;
            }
            frames.deallocate(block, order).expect("a live block frees");
        }

        // The books balance after every single command: frames out with
        // callers, plus frames on the free lists, is the whole arena. Checking
        // it here rather than at the end is what turns "the count is wrong" into
        // "the count went wrong at *this* command".
        let held: u64 = live.iter().map(|&(_, order)| 1u64 << order).sum();
        assert_eq!(held + frames.free_frames(), arena, "frame accounting drifted");
    }

    // Give everything back; the arena must be whole again, and no frame outside
    // what was inserted may have appeared on a free list along the way.
    for (block, order) in live {
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
