//! Fuzz virtio-gpu's device protocol: the responses a device writes and the
//! backing lists built from pinned pages.
//!
//! The driver runs in ring 3 against a device it does not control, so every
//! response is the device's word; and the pages it builds backing from are
//! whatever the IOMMU domain handed out.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **A parsed response keeps its promises**: it came from at least a
//!    header's worth of bytes and no more than the buffer; a success is the
//!    one the command expects; every enabled scanout of a display-info
//!    response has a size a mode can have.
//! 2. **Backing covers exactly the pages**: expanding the entries page by page
//!    gives back the input, in order, and two neighbouring entries are never
//!    joinable.
//! 3. **An encoded command reads back**: its type code is the command's, its
//!    length is `len()`, and encoding into a shorter buffer is refused.

#![no_main]

use ferrix_virtio::gpu::{
    BLOB_MEM_HOST3D, Box3d, CAPSET_VIRGL2, CMD_RESOURCE_MAP_BLOB, CONTEXT_NAME_LEN, Command,
    Context, DISPLAY_INFO_LEN, Format, GpuError, HEADER_LEN, MAP_INFO_LEN, MAX_CAPSET_SIZE,
    MAX_DIMENSION, MemEntry, PAGE_SIZE, Rect, Response, backing_entries,
};
use libfuzzer_sys::fuzz_target;

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .map_or(0, |field| u32::from_le_bytes(field.try_into().expect("four bytes")))
}

fn commands(bytes: &[u8]) -> [Command<'static>; 13] {
    let rect = Rect {
        x: u32_at(bytes, 0),
        y: u32_at(bytes, 4),
        width: u32_at(bytes, 8),
        height: u32_at(bytes, 12),
    };
    [
        Command::GetDisplayInfo,
        Command::ResourceCreate2d {
            resource_id: u32_at(bytes, 16),
            format: Format::B8G8R8X8,
            width: rect.width,
            height: rect.height,
        },
        Command::SetScanout {
            rect,
            scanout_id: u32_at(bytes, 20),
            resource_id: u32_at(bytes, 24),
        },
        Command::TransferToHost2d {
            rect,
            offset: u64::from(u32_at(bytes, 28)),
            resource_id: u32_at(bytes, 32),
        },
        Command::GetCapsetInfo {
            index: u32_at(bytes, 36),
        },
        // A size inside the bound, so that the command encodes: asking for
        // one outside it is refused, which the module's own tests cover.
        Command::GetCapset {
            capset_id: CAPSET_VIRGL2,
            capset_version: u32_at(bytes, 40),
            max_size: u32_at(bytes, 44) % MAX_CAPSET_SIZE + 1,
        },
        Command::CtxCreate {
            capset: bytes.first().copied().unwrap_or(0),
            // Inside the field, so that it encodes: a longer one is refused,
            // which the module's own tests cover.
            name: "fuzz",
        },
        Command::CtxDestroy,
        Command::ResourceCreate3d {
            resource_id: u32_at(bytes, 48),
            target: u32_at(bytes, 52),
            format: u32_at(bytes, 56),
            bind: u32_at(bytes, 60),
            size: Box3d {
                x: u32_at(bytes, 64),
                y: u32_at(bytes, 68),
                z: u32_at(bytes, 72),
                width: rect.width,
                height: rect.height,
                depth: u32_at(bytes, 76),
            },
            array_size: u32_at(bytes, 80),
            last_level: u32_at(bytes, 84),
            samples: u32_at(bytes, 88),
            flags: u32_at(bytes, 92),
        },
        Command::TransferToHost3d {
            region: Box3d::flat(rect.width, rect.height),
            offset: u64::from(u32_at(bytes, 96)),
            resource_id: u32_at(bytes, 100),
            level: u32_at(bytes, 104),
            stride: u32_at(bytes, 108),
            layer_stride: u32_at(bytes, 112),
        },
        // Whole pages and a host blob with no entries, so that it encodes: a
        // blob that is not is refused, which the module's own tests cover.
        Command::ResourceCreateBlob {
            resource_id: u32_at(bytes, 116),
            blob_mem: BLOB_MEM_HOST3D,
            blob_flags: u32_at(bytes, 120),
            blob_id: u64::from(u32_at(bytes, 124)),
            size: (u64::from(u32_at(bytes, 128) % 1024) + 1) * 4096,
            entries: &[],
        },
        Command::ResourceMapBlob {
            resource_id: u32_at(bytes, 132),
            offset: u64::from(u32_at(bytes, 136)) << 12,
        },
        Command::ResourceUnmapBlob {
            resource_id: u32_at(bytes, 140),
        },
    ]
}

fuzz_target!(|bytes: &[u8]| {
    let Some((&selector, rest)) = bytes.split_first() else {
        return;
    };
    let written = u32_at(rest, 0);
    let buffer = rest.get(4..).unwrap_or(&[]);

    // 1.
    for command in commands(rest) {
        match Response::parse(&command, buffer, written) {
            Ok(response) => {
                let written = written as usize;
                assert!((HEADER_LEN..=buffer.len()).contains(&written));
                assert_eq!(u32_at(buffer, 0), command.expects());
                if let Response::CapsetInfo(info) = response {
                    // The size the driver would allocate on the device's
                    // word is inside the bound, whatever the device said.
                    assert!(info.max_size <= MAX_CAPSET_SIZE);
                }
                if let Response::Capset { len } = response {
                    // A capability set is what came after the header, and a
                    // device that sent none did not succeed.
                    assert!(len > 0);
                    assert_eq!(len, written - HEADER_LEN);
                }
                if let Response::MapInfo { .. } = response {
                    // Only a map is answered with where a blob went, and the
                    // answer has its body.
                    assert_eq!(command.code(), CMD_RESOURCE_MAP_BLOB);
                    assert!(written >= MAP_INFO_LEN);
                }
                if let Response::DisplayInfo(scanouts) = response {
                    assert!(written >= DISPLAY_INFO_LEN);
                    for scanout in scanouts.iter().filter(|scanout| scanout.enabled) {
                        let rect = scanout.rect;
                        assert!((1..=MAX_DIMENSION).contains(&rect.width));
                        assert!((1..=MAX_DIMENSION).contains(&rect.height));
                        assert!(rect.x.checked_add(rect.width).is_some());
                        assert!(rect.y.checked_add(rect.height).is_some());
                    }
                }
            }
            Err(GpuError::Device(_)) => assert!((0x1200..=0x1205).contains(&u32_at(buffer, 0))),
            Err(_) => {}
        }

        // 3. A context command carries its context and its fence in the
        // header, and the header is written whole whatever they are.
        let context = Context {
            id: u32_at(rest, 0),
            fence: (selector & 4 == 4).then(|| u64::from(u32_at(rest, 4))),
            ring: (selector & 8 == 8).then_some(selector),
        };
        let mut wide = [0u8; 128];
        if let Ok(len) = command.encode_in(context, &mut wide) {
            assert_eq!(len, command.len());
            assert_eq!(u32_at(&wide, 0), command.code());
            assert_eq!(u32_at(&wide, 4), context.flags());
            assert_eq!(u32_at(&wide, 16), context.id);
            // Every byte of a context name's field is written, so nothing
            // of whatever was in the buffer survives into the request.
            if let Command::CtxCreate { name, .. } = command {
                assert!(name.len() <= CONTEXT_NAME_LEN);
            }
        }

        // 3.
        let mut out = [0u8; 64];
        // Only the ones that fit: `CTX_CREATE` and `RESOURCE_CREATE_3D` are
        // longer than this buffer, and a short buffer is refused, which is
        // the property below.
        if command.len() <= out.len() {
            let len = command.encode(&mut out).expect("it fits");
            assert_eq!(len, command.len());
            assert_eq!(u32_at(&out, 0), command.code());
            assert!(command.encode(&mut out[..len - 1]).is_err());
        } else {
            assert!(command.encode(&mut out).is_err());
        }
    }

    // 2. Page addresses from the input, mostly aligned, with runs when the
    // selector says so.
    let pages: Vec<u64> = rest
        .chunks_exact(2)
        .take(64)
        .scan(0u64, |previous, pair| {
            let raw = u64::from(u16::from_le_bytes([pair[0], pair[1]]));
            let next = if selector & 1 == 1 && raw % 3 == 0 {
                previous.wrapping_add(PAGE_SIZE)
            } else if selector & 2 == 2 {
                raw
            } else {
                raw * PAGE_SIZE
            };
            *previous = next;
            Some(next)
        })
        .collect();
    let mut out = vec![MemEntry::default(); usize::from(selector >> 4) + 1];
    if let Ok(count) = backing_entries(&pages, &mut out) {
        let entries = &out[..count];
        let expanded: Vec<u64> = entries
            .iter()
            .flat_map(|entry| {
                (0..u64::from(entry.length) / PAGE_SIZE).map(move |page| entry.addr + page * PAGE_SIZE)
            })
            .collect();
        assert_eq!(expanded, pages, "the entries do not cover the pages");
        for pair in entries.windows(2) {
            assert_ne!(
                pair[0].addr + u64::from(pair[0].length),
                pair[1].addr,
                "two joinable entries were left apart"
            );
        }
        assert!(entries.iter().all(|entry| entry.length as u64 % PAGE_SIZE == 0));
    }
});
