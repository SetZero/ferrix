//! The bytes, against what AOSP's `protocol.txt` and `SYNC.TXT` say and
//! what the host's `adb` sends.

use std::vec::Vec;

use crate::message::{
    self, CLSE, CNXN, HEADER_BYTES, Header, HeaderError, MAX_PAYLOAD, OKAY, OPEN, VERSION, WRTE,
};
use crate::sync::{self, Reader, Request, RequestError};

#[test]
fn commands_are_their_letters_as_little_endian_words() {
    // protocol.txt: A_CNXN 0x4e584e43, A_OPEN 0x4e45504f, A_OKAY 0x59414b4f,
    // A_CLSE 0x45534c43, A_WRTE 0x45545257.
    assert_eq!(CNXN, 0x4e58_4e43);
    assert_eq!(OPEN, 0x4e45_504f);
    assert_eq!(OKAY, 0x5941_4b4f);
    assert_eq!(CLSE, 0x4553_4c43);
    assert_eq!(WRTE, 0x4554_5257);
}

#[test]
fn a_header_is_six_little_endian_words_with_the_magic_last() {
    let header = Header::new(OPEN, 7, 0, b"shell:\0");
    let bytes = header.encode();
    assert_eq!(&bytes[..4], b"OPEN");
    assert_eq!(&bytes[4..8], &7_u32.to_le_bytes());
    assert_eq!(&bytes[12..16], &7_u32.to_le_bytes(), "the payload's length");
    assert_eq!(&bytes[20..24], &(!OPEN).to_le_bytes());
    assert_eq!(Header::decode(&bytes, MAX_PAYLOAD), Ok(header));
}

#[test]
fn what_the_host_says_first_decodes() {
    // The host's CNXN, as adb 37 sends it: version 0x01000001, 1 MiB.
    let host = message::message(CNXN, VERSION, 1024 * 1024, b"host::features=shell_v2\0");
    let mut head = [0; HEADER_BYTES];
    head.copy_from_slice(&host[..HEADER_BYTES]);
    let header = Header::decode(&head, u32::MAX).expect("the host's CNXN decodes");
    assert_eq!(header.command, CNXN);
    assert_eq!(header.arg0, VERSION);
    assert_eq!(header.arg1, 1024 * 1024);
    assert_eq!(header.data_length as usize, host.len() - HEADER_BYTES);
}

#[test]
fn a_bad_magic_or_a_long_payload_is_refused() {
    let mut bytes = Header::new(WRTE, 1, 2, &[0; 16]).encode();
    bytes[20] ^= 1;
    assert_eq!(Header::decode(&bytes, MAX_PAYLOAD), Err(HeaderError::Magic));
    let long = Header::new(WRTE, 1, 2, &[0; 32]).encode();
    assert_eq!(Header::decode(&long, 16), Err(HeaderError::TooLong(32)));
}

#[test]
fn the_checksum_is_the_sum_of_the_bytes() {
    assert_eq!(message::checksum(b""), 0);
    assert_eq!(message::checksum(&[0xff, 0xff, 2]), 0x200);
}

#[test]
fn the_banner_names_the_device_and_offers_no_features() {
    let banner = message::banner("ferrix", "Pixel 7", "panther");
    assert_eq!(
        banner,
        "device::ro.product.name=ferrix;ro.product.model=Pixel 7;ro.product.device=panther;features="
    );
}

#[test]
fn a_service_name_ends_at_its_nul() {
    assert_eq!(message::service_name(b"shell:ls\0"), b"shell:ls");
    assert_eq!(message::service_name(b"sync:"), b"sync:");
}

fn request(id: &[u8; 4], value: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(id);
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes.extend_from_slice(data);
    bytes
}

#[test]
fn requests_come_whole_however_the_writes_cut_them() {
    let mut stream = request(b"STAT", 4, b"/tmp");
    stream.extend(request(b"SEND", 11, b"/tmp/x,0644"));
    stream.extend(request(b"DATA", 3, b"abc"));
    stream.extend(request(b"DONE", 1_790_000_000, b""));
    stream.extend(request(b"QUIT", 0, b""));
    // One byte at a time, the worst a host could cut them.
    let mut reader = Reader::new();
    let mut got = Vec::new();
    for byte in &stream {
        reader.push(core::slice::from_ref(byte));
        while let Some(request) = reader.next_request().expect("well formed") {
            got.push(request);
        }
    }
    let ids: Vec<[u8; 4]> = got.iter().map(|request| request.id).collect();
    assert_eq!(ids, [*b"STAT", *b"SEND", *b"DATA", *b"DONE", *b"QUIT"]);
    assert_eq!(got[1].data, b"/tmp/x,0644");
    assert_eq!(got[2].data, b"abc");
    assert_eq!(
        got[3],
        Request {
            id: *b"DONE",
            value: 1_790_000_000,
            data: Vec::new()
        },
        "a DONE's word is its time, and nothing follows it"
    );
}

#[test]
fn a_request_longer_than_its_kind_allows_is_refused() {
    let mut reader = Reader::new();
    reader.push(&request(b"STAT", 4096, b""));
    assert_eq!(
        reader.next_request(),
        Err(RequestError::TooLong(*b"STAT", 4096)),
        "a path is at most 1024 bytes"
    );
    let mut reader = Reader::new();
    reader.push(&request(b"DATA", 70_000, b""));
    assert_eq!(
        reader.next_request(),
        Err(RequestError::TooLong(*b"DATA", 70_000))
    );
}

#[test]
fn replies_are_shaped_as_sync_txt_says() {
    assert_eq!(
        sync::stat(0o100_644, 3, 9),
        request(b"STAT", 0o100_644, &[3, 0, 0, 0, 9, 0, 0, 0])
    );
    let dent = sync::dent(0o40_755, 0, 1, b"bin");
    assert_eq!(&dent[..4], b"DENT");
    assert_eq!(dent.len(), 20 + 3, "id, four words, and the name");
    assert_eq!(sync::list_done(), request(b"DONE", 0, &[0; 12]));
    assert_eq!(sync::data(b"hi"), request(b"DATA", 2, b"hi"));
    assert_eq!(sync::recv_done(), request(b"DONE", 0, b""));
    assert_eq!(sync::okay(), request(b"OKAY", 0, b""));
    assert_eq!(sync::fail("no"), request(b"FAIL", 2, b"no"));
}

#[test]
fn a_send_target_is_a_path_and_a_decimal_mode() {
    assert_eq!(
        sync::send_target(b"/tmp/a,33188"),
        (&b"/tmp/a"[..], Some(33188))
    );
    assert_eq!(
        sync::send_target(b"/tmp/a,b,420"),
        (&b"/tmp/a,b"[..], Some(420))
    );
    assert_eq!(sync::send_target(b"/tmp/a"), (&b"/tmp/a"[..], None));
}
