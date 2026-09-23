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

#[test]
fn write_with_puts_every_byte_once_and_agrees_with_encode() {
    let entries = [
        MemEntry {
            addr: 0x1000,
            length: 4096,
        },
        MemEntry {
            addr: 0x9000,
            length: 8192,
        },
    ];
    let rect = Rect {
        x: 1,
        y: 2,
        width: 3,
        height: 4,
    };
    for command in [
        Command::GetDisplayInfo,
        Command::ResourceCreate2d {
            resource_id: 1,
            format: Format::B8G8R8X8,
            width: 2,
            height: 3,
        },
        Command::ResourceUnref { resource_id: 1 },
        Command::SetScanout {
            rect,
            scanout_id: 0,
            resource_id: 1,
        },
        Command::ResourceFlush {
            rect,
            resource_id: 1,
        },
        Command::TransferToHost2d {
            rect,
            offset: 9,
            resource_id: 1,
        },
        Command::ResourceAttachBacking {
            resource_id: 1,
            entries: &entries,
        },
        Command::ResourceDetachBacking { resource_id: 1 },
    ] {
        let mut out = vec![0xEEu8; command.len()];
        let mut times = vec![0u32; command.len()];
        let len = command
            .write_with(|at, bytes| {
                out[at..at + bytes.len()].copy_from_slice(bytes);
                for count in &mut times[at..at + bytes.len()] {
                    *count += 1;
                }
            })
            .expect("it writes");
        assert_eq!(len, command.len());
        assert!(
            times.iter().all(|&count| count == 1),
            "{command:?}: {times:?}"
        );
        assert_eq!(out, encoded(&command), "{command:?}");
    }
}

/// `GET_CAPSET_INFO` is a header and a capset *index*, and its response says
/// which set that index is and how big.
///
/// Offsets by hand from `struct virtio_gpu_get_capset_info` and
/// `struct virtio_gpu_resp_capset_info`: an index at 24 with four bytes of
/// padding after it, and a response of an id, a maximum version and a
/// maximum size at 24, 28 and 32.
#[test]
fn get_capset_info_asks_by_index_and_is_answered_by_id() {
    let bytes = encoded(&Command::GetCapsetInfo { index: 0 });
    assert_eq!(bytes.len(), 24 + 8);
    assert_eq!(u32_at(&bytes, 0), 0x0108, "VIRTIO_GPU_CMD_GET_CAPSET_INFO");
    assert_eq!(u32_at(&bytes, 24), 0, "the index");
    assert_eq!(u32_at(&bytes, 28), 0, "the padding is written, not left");

    let mut body = Vec::new();
    body.extend_from_slice(&2u32.to_le_bytes()); // VIRTIO_GPU_CAPSET_VIRGL2
    body.extend_from_slice(&3u32.to_le_bytes()); // max version
    body.extend_from_slice(&1432u32.to_le_bytes()); // max size
    body.extend_from_slice(&0u32.to_le_bytes()); // padding
    let buffer = response(0x1102, &body);
    let parsed = Response::parse_for(0x0108, &buffer, written(&buffer)).expect("a capset info");
    assert_eq!(
        parsed,
        Response::CapsetInfo(CapsetInfo {
            id: CAPSET_VIRGL2,
            max_version: 3,
            max_size: 1432,
        })
    );
}

/// The size in that response is what the driver allocates next, on the
/// device's word alone, so it is held to what a capability set can be.
#[test]
fn a_capset_larger_than_the_bound_is_refused() {
    let mut body = Vec::new();
    body.extend_from_slice(&CAPSET_VIRGL2.to_le_bytes());
    body.extend_from_slice(&1u32.to_le_bytes());
    body.extend_from_slice(&(MAX_CAPSET_SIZE + 1).to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    let buffer = response(0x1102, &body);
    assert_eq!(
        Response::parse_for(0x0108, &buffer, written(&buffer)),
        Err(GpuError::CapsetSize(MAX_CAPSET_SIZE + 1))
    );

    // And a command that would ask for one is refused before it is sent.
    let mut out = vec![0u8; 64];
    assert_eq!(
        Command::GetCapset {
            capset_id: CAPSET_VIRGL2,
            capset_version: 1,
            max_size: MAX_CAPSET_SIZE + 1,
        }
        .encode(&mut out),
        Err(GpuError::CapsetSize(MAX_CAPSET_SIZE + 1))
    );
    assert_eq!(
        Command::GetCapset {
            capset_id: CAPSET_VIRGL2,
            capset_version: 1,
            max_size: 0,
        }
        .encode(&mut out),
        Err(GpuError::CapsetSize(0)),
        "a set of no size is nothing to ask for"
    );
}

/// `GET_CAPSET` asks by id and version, and needs a response buffer as long
/// as the header plus the size the device named.
#[test]
fn get_capset_asks_by_id_and_sizes_its_own_response() {
    let command = Command::GetCapset {
        capset_id: CAPSET_VIRGL2,
        capset_version: 1,
        max_size: 1432,
    };
    let bytes = encoded(&command);
    assert_eq!(bytes.len(), 24 + 8);
    assert_eq!(u32_at(&bytes, 0), 0x0109, "VIRTIO_GPU_CMD_GET_CAPSET");
    assert_eq!(u32_at(&bytes, 24), 2, "the capset id");
    assert_eq!(u32_at(&bytes, 28), 1, "the version");
    assert_eq!(
        command.response_len(),
        24 + 1432,
        "the header and the set the device said it would write"
    );

    // The set itself is left where it is; the response says how much of it
    // there is.
    let buffer = response(0x1103, &[0x5Au8; 1432]);
    assert_eq!(
        Response::parse_for(0x0109, &buffer, written(&buffer)),
        Ok(Response::Capset { len: 1432 })
    );
}

/// A device that answers `GET_CAPSET` with a header and nothing after it
/// sent no capability set, which is not a success.
#[test]
fn an_empty_capset_response_is_refused() {
    let buffer = response(0x1103, &[]);
    assert_eq!(
        Response::parse_for(0x0109, &buffer, written(&buffer)),
        Err(GpuError::ResponseTooShort(HEADER_LEN))
    );
}

/// Each of the two is answered with its own response type and no other:
/// `RESP_OK_NODATA` for a `GET_CAPSET` is a device saying something the
/// command cannot have produced.
#[test]
fn a_capset_command_takes_only_its_own_response() {
    let nodata = response(0x1100, &[]);
    assert_eq!(
        Response::parse_for(0x0109, &nodata, written(&nodata)),
        Err(GpuError::UnexpectedResponse {
            command: 0x0109,
            response: 0x1100,
        })
    );
    assert_eq!(expects(CMD_GET_CAPSET_INFO), 0x1102);
    assert_eq!(expects(CMD_GET_CAPSET), 0x1103);
}

/// A context command carries its id in the *header*, not in its body, and
/// `CTX_CREATE` carries a capset and a name in a fixed 64-byte field.
///
/// Offsets by hand from `struct virtio_gpu_ctx_create`: `nlen` at 24,
/// `context_init` at 28, and 64 bytes of `debug_name` from 32. The header's
/// `ctx_id` is at 16, from `struct virtio_gpu_ctrl_hdr`.
#[test]
fn a_context_is_created_by_a_command_that_names_it_in_its_header() {
    let command = Command::CtxCreate {
        capset: 2,
        name: "ferrix",
    };
    let mut out = vec![0xAAu8; 256];
    let len = command
        .encode_in(
            Context {
                id: 7,
                ..Context::NONE
            },
            &mut out,
        )
        .expect("the command encodes");
    assert_eq!(len, 24 + 8 + 64);
    assert_eq!(u32_at(&out, 0), 0x0200, "VIRTIO_GPU_CMD_CTX_CREATE");
    assert_eq!(u32_at(&out, 16), 7, "the header's ctx_id");
    assert_eq!(u32_at(&out, 24), 6, "nlen, the name's length");
    assert_eq!(u32_at(&out, 28), 2, "context_init's capset");
    assert_eq!(&out[32..38], b"ferrix");
    assert!(
        out[38..24 + 8 + 64].iter().all(|&byte| byte == 0),
        "the rest of debug_name is written, not left"
    );
}

/// A name that does not fit the field is refused rather than cut: a name
/// that is not the name is worse than none.
#[test]
fn a_context_name_longer_than_the_field_is_refused() {
    let long = "x".repeat(65);
    let mut out = vec![0u8; 256];
    assert_eq!(
        Command::CtxCreate {
            capset: 0,
            name: &long,
        }
        .encode(&mut out),
        Err(GpuError::ContextName(65))
    );
    // And exactly the field's worth fits.
    let fits = "y".repeat(64);
    assert!(
        Command::CtxCreate {
            capset: 0,
            name: &fits,
        }
        .encode(&mut out)
        .is_ok()
    );
}

/// `CTX_DESTROY` is a header and nothing else; attaching and detaching a
/// resource is a header, the resource and its padding.
#[test]
fn the_other_context_commands_are_their_headers_and_a_resource() {
    let bytes = encoded(&Command::CtxDestroy);
    assert_eq!(bytes.len(), 24);
    assert_eq!(u32_at(&bytes, 0), 0x0201);

    for (command, code) in [
        (Command::CtxAttachResource { resource_id: 9 }, 0x0202),
        (Command::CtxDetachResource { resource_id: 9 }, 0x0203),
    ] {
        let bytes = encoded(&command);
        assert_eq!(bytes.len(), 24 + 8);
        assert_eq!(u32_at(&bytes, 0), code);
        assert_eq!(u32_at(&bytes, 24), 9);
        assert_eq!(u32_at(&bytes, 28), 0, "the padding");
        // Each is answered with a plain OK.
        assert_eq!(expects(command.code()), 0x1100);
    }
}

/// The header's own fields: a fence asks the device to answer when the
/// command has finished, and a ring says which timeline it is on.
///
/// `VIRTIO_GPU_FLAG_FENCE` is 1 and `VIRTIO_GPU_FLAG_INFO_RING_IDX` is 2,
/// `flags` is at 4, `fence_id` at 8 and `ring_idx` at 20.
#[test]
fn a_fenced_command_says_so_in_its_header() {
    let mut out = vec![0xAAu8; 64];
    let _ = Command::CtxDestroy
        .encode_in(
            Context {
                id: 3,
                fence: Some(0x1234_5678_9abc_def0),
                ring: Some(1),
            },
            &mut out,
        )
        .expect("encodes");
    assert_eq!(u32_at(&out, 4), 1 | 2, "FENCE and INFO_RING_IDX");
    assert_eq!(u64_at(&out, 8), 0x1234_5678_9abc_def0);
    assert_eq!(u32_at(&out, 16), 3);
    assert_eq!(out[20], 1, "ring_idx");
    assert_eq!(&out[21..24], &[0, 0, 0], "its padding");

    // And a command with neither says neither.
    let _ = Command::CtxDestroy
        .encode_in(Context::NONE, &mut out)
        .expect("encodes");
    assert_eq!(u32_at(&out, 4), 0);
    assert_eq!(u64_at(&out, 8), 0);
    assert_eq!(u32_at(&out, 16), 0);
    assert_eq!(out[20], 0);
}

/// `RESOURCE_CREATE_3D` is eleven words after the header, and the ones this
/// module does not interpret go through untouched.
///
/// Offsets by hand from `struct virtio_gpu_resource_create_3d`: resource,
/// target, format, bind at 24, 28, 32, 36; width, height, depth at 40, 44,
/// 48; `array_size`, `last_level`, `nr_samples`, `flags` at 52, 56, 60, 64;
/// and
/// four bytes of padding.
#[test]
fn a_3d_resource_carries_virgls_own_numbers() {
    let command = Command::ResourceCreate3d {
        resource_id: 5,
        target: 2,
        format: 67,
        bind: 0x0002,
        size: Box3d::flat(640, 480),
        array_size: 1,
        last_level: 0,
        samples: 0,
        flags: RESOURCE_FLAG_Y_0_TOP,
    };
    let bytes = encoded(&command);
    assert_eq!(bytes.len(), 24 + 48);
    assert_eq!(
        u32_at(&bytes, 0),
        0x0204,
        "VIRTIO_GPU_CMD_RESOURCE_CREATE_3D"
    );
    assert_eq!(
        [
            u32_at(&bytes, 24),
            u32_at(&bytes, 28),
            u32_at(&bytes, 32),
            u32_at(&bytes, 36)
        ],
        [5, 2, 67, 0x0002]
    );
    assert_eq!(
        [u32_at(&bytes, 40), u32_at(&bytes, 44), u32_at(&bytes, 48)],
        [640, 480, 1],
        "a flat texture is a box one deep"
    );
    assert_eq!(
        [
            u32_at(&bytes, 52),
            u32_at(&bytes, 56),
            u32_at(&bytes, 60),
            u32_at(&bytes, 64)
        ],
        [1, 0, 0, 1]
    );
    assert_eq!(u32_at(&bytes, 68), 0, "the padding");
}

/// A 3D transfer names a *box* where a 2D one names a rectangle, and the two
/// directions are the same body under different codes.
///
/// `struct virtio_gpu_transfer_host_3d`: the box at 24, then offset at 48,
/// resource at 56, level at 60, stride at 64 and `layer_stride` at 68.
#[test]
fn a_3d_transfer_names_a_box_in_both_directions() {
    let region = Box3d {
        x: 1,
        y: 2,
        z: 3,
        width: 4,
        height: 5,
        depth: 6,
    };
    for (command, code) in [
        (
            Command::TransferToHost3d {
                region,
                offset: 0x4000,
                resource_id: 5,
                level: 0,
                stride: 2560,
                layer_stride: 0,
            },
            0x0205,
        ),
        (
            Command::TransferFromHost3d {
                region,
                offset: 0x4000,
                resource_id: 5,
                level: 0,
                stride: 2560,
                layer_stride: 0,
            },
            0x0206,
        ),
    ] {
        let bytes = encoded(&command);
        assert_eq!(bytes.len(), 24 + 24 + 24);
        assert_eq!(u32_at(&bytes, 0), code);
        assert_eq!(
            (24..48)
                .step_by(4)
                .map(|at| u32_at(&bytes, at))
                .collect::<Vec<u32>>(),
            [1, 2, 3, 4, 5, 6],
            "the box, in order"
        );
        assert_eq!(u64_at(&bytes, 48), 0x4000);
        assert_eq!(u32_at(&bytes, 56), 5);
        assert_eq!(u32_at(&bytes, 60), 0);
        assert_eq!(u32_at(&bytes, 64), 2560);
        assert_eq!(u32_at(&bytes, 68), 0);
    }
}

/// `SUBMIT_3D` is a length and then the stream, which this module carries
/// and does not read.
#[test]
fn a_submitted_stream_is_carried_whole() {
    let stream: Vec<u8> = (0..32u8).collect();
    let command = Command::Submit3d { commands: &stream };
    let bytes = encoded(&command);
    assert_eq!(bytes.len(), 24 + 8 + 32);
    assert_eq!(u32_at(&bytes, 0), 0x0207, "VIRTIO_GPU_CMD_SUBMIT_3D");
    assert_eq!(u32_at(&bytes, 24), 32, "size, in bytes");
    assert_eq!(u32_at(&bytes, 28), 0, "the padding");
    assert_eq!(&bytes[32..], stream.as_slice());

    // And nothing to submit is not a command.
    let mut out = vec![0u8; 64];
    assert_eq!(
        Command::Submit3d { commands: &[] }.encode(&mut out),
        Err(GpuError::StreamSize(0))
    );
}

/// `struct virtio_gpu_update_cursor`: the header, then
/// `struct virtio_gpu_cursor_pos { scanout_id, x, y, padding }` at 24,
/// `resource_id` at 40, `hot_x` at 44, `hot_y` at 48 and padding to 56.
#[test]
fn a_cursor_is_one_structure_for_an_update_and_a_move() {
    let cursor = Cursor {
        scanout_id: 1,
        x: -3,
        y: 700,
        resource_id: 9,
        hot_x: 4,
        hot_y: 5,
    };
    let update = cursor.encode(true);
    assert_eq!(update.len(), 56);
    assert_eq!(u32_at(&update, 0), 0x0300);
    assert_eq!(
        [
            u32_at(&update, 24),
            u32_at(&update, 28),
            u32_at(&update, 32),
            u32_at(&update, 40),
            u32_at(&update, 44),
            u32_at(&update, 48),
        ],
        [1, (-3i32) as u32, 700, 9, 4, 5]
    );
    // Flags, fence, context, ring and every padding field are zero.
    for at in [4, 8, 12, 16, 20, 36, 52] {
        assert_eq!(u32_at(&update, at), 0, "the word at {at}");
    }
    // A move is the same structure under the other type, resource and all:
    // QEMU shows a moved cursor only if the move names its resource.
    let moved = cursor.encode(false);
    assert_eq!(u32_at(&moved, 0), 0x0301);
    assert_eq!(moved[4..], update[4..]);
}
