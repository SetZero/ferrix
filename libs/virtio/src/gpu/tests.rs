//! Tests for virtio-gpu's device protocol.
//!
//! Every length and offset below is written out by hand from QEMU 9.2.4's
//! `include/standard-headers/linux/virtio_gpu.h`, not taken from this
//! module's constants, since a constant tested against itself proves nothing.
//! The device's side is played by hand-built response bytes, most of them
//! wrong on purpose.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

fn encoded(command: &Command<'_>) -> Vec<u8> {
    let mut out = vec![0xAAu8; 512];
    let len = command.encode(&mut out).expect("the command encodes");
    assert_eq!(len, command.len());
    out.truncate(len);
    out
}

/// A response buffer holding `code` and then `body`.
fn response(code: u32, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; HEADER_LEN];
    out[..4].copy_from_slice(&code.to_le_bytes());
    out.extend_from_slice(body);
    out
}

fn written(bytes: &[u8]) -> u32 {
    u32::try_from(bytes.len()).expect("small")
}

// -- The configuration block ---------------------------------------------------------

#[test]
fn config_reads_its_three_fields_and_bounds_the_scanouts() {
    let mut block = [0u8; 16];
    block[0..4].copy_from_slice(&1u32.to_le_bytes());
    block[4..8].copy_from_slice(&0xFFu32.to_le_bytes());
    block[8..12].copy_from_slice(&1u32.to_le_bytes());
    block[12..16].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(
        Config::read(&block[..]),
        Ok(Config {
            events_read: EVENT_DISPLAY,
            num_scanouts: 1,
            num_capsets: 2,
        })
    );

    assert_eq!(
        Config::read(&block[..15]),
        Err(GpuError::ConfigTooShort(15))
    );
    for scanouts in [0u32, 17, u32::MAX] {
        block[8..12].copy_from_slice(&scanouts.to_le_bytes());
        assert_eq!(Config::read(&block[..]), Err(GpuError::Scanouts(scanouts)));
    }
    block[8..12].copy_from_slice(&16u32.to_le_bytes());
    assert!(Config::read(&block[..]).is_ok());
}

// -- Commands --------------------------------------------------------------------

#[test]
fn every_command_encodes_at_the_offsets_qemu_reads() {
    // struct virtio_gpu_ctrl_hdr: type, flags, fence_id (u64 at 8), ctx_id at
    // 16, ring_idx at 20, three bytes of padding: 24 bytes, all zero but type.
    let get = encoded(&Command::GetDisplayInfo);
    assert_eq!(get.len(), 24);
    assert_eq!(u32_at(&get, 0), 0x0100);
    assert!(get[4..24].iter().all(|&byte| byte == 0));

    // struct virtio_gpu_resource_create_2d: resource_id at 24, format at 28,
    // width at 32, height at 36.
    let create = encoded(&Command::ResourceCreate2d {
        resource_id: 7,
        format: Format::B8G8R8X8,
        width: 1280,
        height: 800,
    });
    assert_eq!(create.len(), 40);
    assert_eq!(
        [0, 24, 28, 32, 36].map(|at| u32_at(&create, at)),
        [0x0101, 7, 2, 1280, 800]
    );

    // struct virtio_gpu_resource_unref and _detach_backing: resource_id at 24,
    // padding at 28.
    let unref = encoded(&Command::ResourceUnref { resource_id: 9 });
    let detach = encoded(&Command::ResourceDetachBacking { resource_id: 9 });
    assert_eq!((unref.len(), detach.len()), (32, 32));
    assert_eq!([u32_at(&unref, 0), u32_at(&unref, 24)], [0x0102, 9]);
    assert_eq!([u32_at(&detach, 0), u32_at(&detach, 24)], [0x0107, 9]);

    // struct virtio_gpu_set_scanout: rect at 24, scanout_id at 40,
    // resource_id at 44.
    let rect = Rect {
        x: 1,
        y: 2,
        width: 3,
        height: 4,
    };
    let scanout = encoded(&Command::SetScanout {
        rect,
        scanout_id: 0,
        resource_id: 7,
    });
    assert_eq!(scanout.len(), 48);
    assert_eq!(
        [0, 24, 28, 32, 36, 40, 44].map(|at| u32_at(&scanout, at)),
        [0x0103, 1, 2, 3, 4, 0, 7]
    );

    // struct virtio_gpu_resource_flush: rect at 24, resource_id at 40,
    // padding at 44.
    let flush = encoded(&Command::ResourceFlush {
        rect,
        resource_id: 7,
    });
    assert_eq!(flush.len(), 48);
    assert_eq!([u32_at(&flush, 0), u32_at(&flush, 40)], [0x0104, 7]);

    // struct virtio_gpu_transfer_to_host_2d: rect at 24, offset (u64) at 40,
    // resource_id at 48, padding at 52.
    let transfer = encoded(&Command::TransferToHost2d {
        rect,
        offset: 0x1_0000_0000,
        resource_id: 7,
    });
    assert_eq!(transfer.len(), 56);
    assert_eq!(u32_at(&transfer, 0), 0x0105);
    assert_eq!(u64_at(&transfer, 40), 0x1_0000_0000);
    assert_eq!(u32_at(&transfer, 48), 7);

    // struct virtio_gpu_resource_attach_backing: resource_id at 24,
    // nr_entries at 28, then struct virtio_gpu_mem_entry { addr (u64),
    // length, padding } per entry from 32.
    let entries = [
        MemEntry {
            addr: 0x8000_0000,
            length: 8192,
        },
        MemEntry {
            addr: 0x1_2000_0000,
            length: 4096,
        },
    ];
    let attach = encoded(&Command::ResourceAttachBacking {
        resource_id: 7,
        entries: &entries,
    });
    assert_eq!(attach.len(), 64);
    assert_eq!([0, 24, 28].map(|at| u32_at(&attach, at)), [0x0106, 7, 2]);
    assert_eq!(
        (u64_at(&attach, 32), u32_at(&attach, 40)),
        (0x8000_0000, 8192)
    );
    assert_eq!(
        (u64_at(&attach, 48), u32_at(&attach, 56)),
        (0x1_2000_0000, 4096)
    );
    assert_eq!(u32_at(&attach, 44), 0, "the entry's padding is zero");
}

#[test]
fn a_command_is_refused_rather_than_cut_short() {
    let command = Command::ResourceCreate2d {
        resource_id: 1,
        format: Format::B8G8R8X8,
        width: 1,
        height: 1,
    };
    let mut short = [0u8; 39];
    assert_eq!(
        command.encode(&mut short),
        Err(GpuError::BufferTooShort {
            needed: 40,
            got: 39
        })
    );
    let none: [MemEntry; 0] = [];
    assert_eq!(
        Command::ResourceAttachBacking {
            resource_id: 1,
            entries: &none
        }
        .encode(&mut [0u8; 64]),
        Err(GpuError::BackingEntries(0))
    );
    let many = vec![MemEntry::default(); MAX_BACKING_ENTRIES + 1];
    assert_eq!(
        Command::ResourceAttachBacking {
            resource_id: 1,
            entries: &many
        }
        .encode(&mut vec![0u8; 32 + many.len() * 16]),
        Err(GpuError::BackingEntries(MAX_BACKING_ENTRIES + 1))
    );
}

#[test]
fn response_buffers_are_the_size_qemu_needs() {
    // 24 + 16 × (rect 16 + enabled 4 + flags 4).
    assert_eq!(Command::GetDisplayInfo.response_len(), 408);
    assert_eq!(Command::ResourceUnref { resource_id: 1 }.response_len(), 24);
}

// -- Responses -------------------------------------------------------------------

fn display_one(rect: Rect, enabled: u32) -> Vec<u8> {
    let mut one = Vec::new();
    for field in [rect.x, rect.y, rect.width, rect.height, enabled, 0] {
        one.extend_from_slice(&field.to_le_bytes());
    }
    one
}

fn display_info(first: Rect, enabled: u32) -> Vec<u8> {
    let mut body = display_one(first, enabled);
    for _ in 1..MAX_SCANOUTS {
        body.extend(display_one(Rect::default(), 0));
    }
    response(0x1101, &body)
}

#[test]
fn display_info_reads_every_scanout() {
    let bytes = display_info(Rect::sized(1280, 800), 1);
    assert_eq!(bytes.len(), 408);
    let Ok(Response::DisplayInfo(scanouts)) =
        Response::parse(&Command::GetDisplayInfo, &bytes, written(&bytes))
    else {
        panic!("display info parses");
    };
    assert_eq!(
        scanouts[0],
        Scanout {
            rect: Rect::sized(1280, 800),
            enabled: true,
            flags: 0
        }
    );
    assert!(scanouts[1..].iter().all(|scanout| !scanout.enabled));
}

#[test]
fn nodata_answers_every_other_command() {
    let bytes = response(0x1100, &[]);
    for command in [
        Command::ResourceUnref { resource_id: 1 },
        Command::ResourceFlush {
            rect: Rect::sized(1, 1),
            resource_id: 1,
        },
    ] {
        assert_eq!(Response::parse(&command, &bytes, 24), Ok(Response::NoData));
    }
}

#[test]
fn error_responses_are_the_devices_refusal() {
    let command = Command::ResourceUnref { resource_id: 1 };
    for (code, error) in [
        (0x1200, DeviceError::Unspecified),
        (0x1201, DeviceError::OutOfMemory),
        (0x1202, DeviceError::InvalidScanoutId),
        (0x1203, DeviceError::InvalidResourceId),
        (0x1204, DeviceError::InvalidContextId),
        (0x1205, DeviceError::InvalidParameter),
    ] {
        let bytes = response(code, &[]);
        assert_eq!(
            Response::parse(&command, &bytes, 24),
            Err(GpuError::Device(error))
        );
    }
}

#[test]
fn a_hostile_response_is_refused() {
    let command = Command::GetDisplayInfo;
    let good = display_info(Rect::sized(1280, 800), 1);

    // Claims more than the buffer holds, or less than a header.
    assert_eq!(
        Response::parse(&command, &good, 409),
        Err(GpuError::WroteTooMuch {
            written: 409,
            buffer: 408
        })
    );
    assert_eq!(
        Response::parse(&command, &good, 23),
        Err(GpuError::ResponseTooShort(23))
    );
    assert_eq!(
        Response::parse(&command, &good, 0),
        Err(GpuError::ResponseTooShort(0))
    );
    // A display-info header with the body cut short.
    assert_eq!(
        Response::parse(&command, &good, 407),
        Err(GpuError::ResponseTooShort(407))
    );
    // A type nobody defined, and the wrong success for the command.
    assert_eq!(
        Response::parse(&command, &response(0x1107, &[]), 24),
        Err(GpuError::UnknownResponse(0x1107))
    );
    assert_eq!(
        Response::parse(&command, &response(0x1100, &[]), 24),
        Err(GpuError::UnexpectedResponse {
            command: 0x0100,
            response: 0x1100
        })
    );
    assert_eq!(
        Response::parse(&Command::ResourceUnref { resource_id: 1 }, &good, 408),
        Err(GpuError::UnexpectedResponse {
            command: 0x0102,
            response: 0x1101
        })
    );

    // Enabled scanouts no mode can have.
    for rect in [
        Rect::sized(0, 800),
        Rect::sized(1280, 0),
        Rect::sized(16385, 800),
        Rect {
            x: u32::MAX,
            y: 0,
            width: 1,
            height: 1,
        },
    ] {
        let bytes = display_info(rect, 1);
        assert_eq!(
            Response::parse(&command, &bytes, 408),
            Err(GpuError::ScanoutSize { index: 0, rect })
        );
    }
    // The same rectangles on a disabled scanout are nobody's business.
    assert!(Response::parse(&command, &display_info(Rect::sized(0, 0), 0), 408).is_ok());
}

// -- Backing ---------------------------------------------------------------------

#[test]
fn backing_joins_only_consecutive_device_pages() {
    let mut out = [MemEntry::default(); 8];
    let pages = [0x1000, 0x2000, 0x3000, 0x9000, 0xA000, 0x5000];
    assert_eq!(backing_entries(&pages, &mut out), Ok(3));
    assert_eq!(
        out[..3],
        [
            MemEntry {
                addr: 0x1000,
                length: 12288
            },
            MemEntry {
                addr: 0x9000,
                length: 8192
            },
            MemEntry {
                addr: 0x5000,
                length: 4096
            },
        ]
    );

    // Pages out of order stay apart even when they are neighbours backwards.
    assert_eq!(backing_entries(&[0x2000, 0x1000], &mut out), Ok(2));

    assert_eq!(
        backing_entries(&[], &mut out),
        Err(GpuError::BackingEntries(0))
    );
    assert_eq!(
        backing_entries(&[0x1001], &mut out),
        Err(GpuError::MisalignedPage(0x1001))
    );
    let apart: Vec<u64> = (0..9).map(|page| page * 0x2000).collect();
    assert_eq!(
        backing_entries(&apart, &mut out),
        Err(GpuError::BackingEntries(9))
    );
}

#[test]
fn a_run_stops_before_its_length_would_wrap() {
    // 2^20 consecutive pages is 4 GiB, one page more than a u32 length holds.
    let pages: Vec<u64> = (0..(1u64 << 20)).map(|page| page * PAGE_SIZE).collect();
    let mut out = [MemEntry::default(); 2];
    assert_eq!(backing_entries(&pages, &mut out), Ok(2));
    assert_eq!(out[0].length, u32::MAX - 4095);
    assert_eq!(out[1].addr, u64::from(u32::MAX - 4095));
    assert_eq!(out[1].length, 4096);
}

#[test]
fn rect_fits_inside_a_resource() {
    assert!(Rect::sized(1280, 800).fits(1280, 800));
    assert!(
        !Rect {
            x: 1,
            ..Rect::sized(1280, 800)
        }
        .fits(1280, 800)
    );
    assert!(
        !Rect {
            x: u32::MAX,
            y: 0,
            width: 2,
            height: 1
        }
        .fits(u32::MAX, 1)
    );
}
