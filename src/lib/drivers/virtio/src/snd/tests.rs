//! Tests for virtio-snd's device protocol.
//!
//! Every number, offset and layout below is written out by hand from virtio
//! 1.2 §5.14 and QEMU 9.2.4's `include/standard-headers/linux/virtio_snd.h`
//! and `hw/audio/virtio-snd.c`, not taken from this module's constants: a
//! constant tested against itself proves nothing. The device's answers are
//! built the way QEMU builds them, and then the ways a hostile one could.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

/// A configuration block of given bytes.
#[derive(Debug)]
struct Block(Vec<u8>);

impl DeviceConfig for Block {
    fn config_len(&self) -> u32 {
        u32::try_from(self.0.len()).unwrap_or(u32::MAX)
    }

    fn config_read8(&self, offset: u32) -> u8 {
        // What QEMU returns for a byte past the block.
        self.0
            .get(usize::try_from(offset).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(0xff)
    }

    fn config_read16(&self, offset: u32) -> u16 {
        u16::from(self.config_read8(offset)) | u16::from(self.config_read8(offset + 1)) << 8
    }

    fn config_read32(&self, offset: u32) -> u32 {
        u32::from(self.config_read16(offset)) | u32::from(self.config_read16(offset + 2)) << 16
    }
}

/// QEMU's block, `struct virtio_snd_config`: four words, `jacks`, `streams`,
/// `chmaps`, `controls`.
fn block(jacks: u32, streams: u32, chmaps: u32) -> Block {
    let mut bytes = vec![0_u8; 16];
    bytes[0..4].copy_from_slice(&jacks.to_le_bytes());
    bytes[4..8].copy_from_slice(&streams.to_le_bytes());
    bytes[8..12].copy_from_slice(&chmaps.to_le_bytes());
    Block(bytes)
}

/// One `struct virtio_snd_pcm_info`, laid out by hand: `hda_fn_nid`,
/// `features`, `formats` (u64), `rates` (u64), `direction`, `channels_min`,
/// `channels_max`, five bytes of padding.
fn info_entry(formats: u64, rates: u64, direction: u8, min: u8, max: u8) -> [u8; 32] {
    let mut entry = [0_u8; 32];
    entry[8..16].copy_from_slice(&formats.to_le_bytes());
    entry[16..24].copy_from_slice(&rates.to_le_bytes());
    entry[24] = direction;
    entry[25] = min;
    entry[26] = max;
    entry
}

/// QEMU 9.2.4's `supported_formats`: S8, U8, S16, U16, S32, U32, FLOAT, which
/// are bits 3, 4, 5, 6, 17, 18 and 19.
const QEMU_FORMATS: u64 =
    (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 17) | (1 << 18) | (1 << 19);
/// Its `supported_rates`: all fourteen, bits 0 to 13.
const QEMU_RATES: u64 = 0x3fff;

/// The response QEMU gives a `PCM_INFO` query for both of its default
/// streams after realize: a status, then output and input, each two channels
/// at most because two is what the realize configuration set.
fn qemu_info_response() -> Vec<u8> {
    let mut bytes = 0x8000_u32.to_le_bytes().to_vec();
    bytes.extend_from_slice(&info_entry(QEMU_FORMATS, QEMU_RATES, 0, 1, 2));
    bytes.extend_from_slice(&info_entry(QEMU_FORMATS, QEMU_RATES, 1, 1, 2));
    bytes
}

#[test]
fn the_device_and_its_queues_are_numbered_as_the_specification_says() {
    assert_eq!(DEVICE_ID, 25);
    assert_eq!(PCI_DEVICE_ID, 0x1040 + 25);
    assert_eq!(
        (CONTROL_QUEUE, EVENT_QUEUE, TX_QUEUE, RX_QUEUE, QUEUE_COUNT),
        (0, 1, 2, 3, 4)
    );
    assert_eq!(
        DRIVER_FEATURES & FEATURE_CTLS,
        0,
        "no controls in version 1"
    );
}

#[test]
fn qemus_block_reads_and_a_hostile_one_is_refused() {
    assert_eq!(
        Config::read(&block(0, 2, 0)),
        Ok(Config {
            jacks: 0,
            streams: 2,
            chmaps: 0
        })
    );
    assert_eq!(Config::read(&block(0, 0, 0)), Err(SndError::Streams(0)));
    assert_eq!(Config::read(&block(0, 11, 0)), Err(SndError::Streams(11)));
    assert_eq!(
        Config::read(&block(0, 10, 0)).map(|config| config.streams),
        Ok(10)
    );
    // A block that stops before `chmaps` would read QEMU's `0xff` past it.
    let short = Block(block(0, 2, 0).0[..11].to_vec());
    assert_eq!(Config::read(&short), Err(SndError::Config));
    // Without `F_CTLS` the fourth word is not read, so a block of three is
    // enough.
    let three = Block(block(1, 2, 3).0[..12].to_vec());
    assert_eq!(
        Config::read(&three),
        Ok(Config {
            jacks: 1,
            streams: 2,
            chmaps: 3
        })
    );
}

#[test]
fn every_request_is_laid_out_as_the_header_says() {
    let mut out = [0xAA_u8; 32];
    assert_eq!(write_pcm_info_query(0, 2, &mut out), Ok(16));
    assert_eq!(out[0..4], 0x0100_u32.to_le_bytes(), "code");
    assert_eq!(out[4..8], 0_u32.to_le_bytes(), "start_id");
    assert_eq!(out[8..12], 2_u32.to_le_bytes(), "count");
    assert_eq!(out[12..16], 32_u32.to_le_bytes(), "size");
    assert_eq!(out[16], 0xAA, "nothing past the query");
    assert_eq!(pcm_info_response_bytes(2), 4 + 64);

    let codes = [
        (PcmCommand::Prepare, 0x0102_u32),
        (PcmCommand::Release, 0x0103),
        (PcmCommand::Start, 0x0104),
        (PcmCommand::Stop, 0x0105),
    ];
    for (command, code) in codes {
        let mut out = [0_u8; 8];
        assert_eq!(command.write(1, &mut out), Ok(8));
        assert_eq!(out[0..4], code.to_le_bytes());
        assert_eq!(out[4..8], 1_u32.to_le_bytes(), "stream_id");
    }

    // Version 1's configuration: 15360 bytes, periods of 3840, S16, two
    // channels, 48 kHz.
    let params = SetParams {
        stream: 0,
        buffer_bytes: 15360,
        period_bytes: 3840,
        features: 0,
        channels: 2,
        format: FORMAT_S16,
        rate: rate_index(48_000).expect("48 kHz is in the enum"),
    };
    let mut out = [0xAA_u8; 24];
    assert_eq!(params.write(&mut out), Ok(24));
    assert_eq!(out[0..4], 0x0101_u32.to_le_bytes());
    assert_eq!(out[4..8], 0_u32.to_le_bytes());
    assert_eq!(out[8..12], 15360_u32.to_le_bytes());
    assert_eq!(out[12..16], 3840_u32.to_le_bytes());
    assert_eq!(out[16..20], 0_u32.to_le_bytes());
    assert_eq!(out[20..24], [2, 5, 7, 0], "channels, format, rate, padding");

    let mut out = [0_u8; 4];
    assert_eq!(write_xfer(3, &mut out), Ok(4));
    assert_eq!(out, 3_u32.to_le_bytes());

    // Every request refuses a buffer one byte short, writing nothing.
    let mut short = [0xAA_u8; 23];
    assert!(matches!(
        params.write(&mut short),
        Err(SndError::Short { want: 24, have: 23 })
    ));
    assert_eq!(short, [0xAA; 23]);
    assert!(write_pcm_info_query(0, 1, &mut [0; 15]).is_err());
    assert!(PcmCommand::Start.write(0, &mut [0; 7]).is_err());
    assert!(write_xfer(0, &mut [0; 3]).is_err());
}

#[test]
fn the_rates_are_the_enums_in_order() {
    assert_eq!(RATES_HZ.len(), 14);
    assert_eq!(rate_index(5512), Some(0));
    assert_eq!(rate_index(44_100), Some(6));
    assert_eq!(rate_index(48_000), Some(RATE_48000));
    assert_eq!(rate_index(384_000), Some(13));
    assert_eq!(rate_index(47_999), None);
}

#[test]
fn qemus_stream_information_reads() {
    let response = qemu_info_response();
    let output = PcmInfo::read(&response, 0, 2).expect("QEMU's output stream");
    assert_eq!(output.direction, DIRECTION_OUTPUT);
    assert_eq!((output.channels_min, output.channels_max), (1, 2));
    assert!(output.has_format(FORMAT_S16));
    assert!(!output.has_format(FORMAT_FLOAT64), "QEMU does not offer it");
    assert!(output.has_rate(RATE_48000));
    assert!(output.has_channels(2));
    assert!(!output.has_channels(3));
    let input = PcmInfo::read(&response, 1, 2).expect("QEMU's input stream");
    assert_eq!(input.direction, DIRECTION_INPUT);
    assert_eq!(PcmInfo::read(&response, 2, 2), Err(SndError::Stream(2)));
}

#[test]
fn hostile_stream_information_is_refused() {
    // A status other than OK.
    let mut response = qemu_info_response();
    response[0..4].copy_from_slice(&0x8001_u32.to_le_bytes());
    assert_eq!(
        PcmInfo::read(&response, 0, 2),
        Err(SndError::Status(0x8001))
    );

    // Fewer entries than were asked for, even when the one read is there.
    let response = qemu_info_response();
    assert!(matches!(
        PcmInfo::read(&response[..4 + 32], 0, 2),
        Err(SndError::Short { want: 68, have: 36 })
    ));
    assert!(matches!(
        PcmInfo::read(&[0x00, 0x80], 0, 1),
        Err(SndError::Short { .. })
    ));

    let one = |entry: [u8; 32]| {
        let mut bytes = 0x8000_u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&entry);
        PcmInfo::read(&bytes, 0, 1)
    };
    assert_eq!(
        one(info_entry(QEMU_FORMATS, QEMU_RATES, 2, 1, 2)),
        Err(SndError::Direction(2))
    );
    assert_eq!(
        one(info_entry(QEMU_FORMATS, QEMU_RATES, 0, 3, 2)),
        Err(SndError::Channels { min: 3, max: 2 })
    );
    assert_eq!(
        one(info_entry(QEMU_FORMATS, QEMU_RATES, 0, 0, 2)),
        Err(SndError::Channels { min: 0, max: 2 })
    );

    // Bits beyond the defined formats and rates are not an error, and not
    // offered either.
    let later = one(info_entry(
        QEMU_FORMATS | (1 << 40),
        QEMU_RATES | (1 << 20),
        0,
        1,
        2,
    ))
    .expect("a later device's extra bits are ignored");
    assert_eq!(later.formats(), QEMU_FORMATS);
    assert_eq!(later.rates(), QEMU_RATES);
    assert!(!later.has_format(40));
    assert!(!later.has_rate(20));
}

#[test]
fn a_transmit_status_is_eight_bytes_and_ok() {
    // QEMU's `return_tx_buffer`: OK, and the buffer's own size as latency.
    let mut status = 0x8000_u32.to_le_bytes().to_vec();
    status.extend_from_slice(&3840_u32.to_le_bytes());
    assert_eq!(
        PcmStatus::read(&status, 8),
        Ok(PcmStatus {
            latency_bytes: 3840
        })
    );
    // A completion that wrote anything but the status is refused, whatever
    // the bytes hold.
    assert_eq!(PcmStatus::read(&status, 0), Err(SndError::Written(0)));
    assert_eq!(PcmStatus::read(&status, 3848), Err(SndError::Written(3848)));
    // QEMU answers a buffer for a stream that does not play with BAD_MSG
    // (`empty_invalid_queue`).
    status[0..4].copy_from_slice(&0x8001_u32.to_le_bytes());
    assert_eq!(PcmStatus::read(&status, 8), Err(SndError::Status(0x8001)));
    assert!(PcmStatus::read(&status[..7], 8).is_err());
}

#[test]
fn events_read_and_an_unknown_one_is_not_an_error() {
    let event = |code: u32, data: u32| {
        let mut bytes = code.to_le_bytes().to_vec();
        bytes.extend_from_slice(&data.to_le_bytes());
        Event::read(&bytes)
    };
    assert_eq!(event(0x1100, 0), Ok(Event::PeriodElapsed { stream: 0 }));
    assert_eq!(event(0x1101, 1), Ok(Event::Xrun { stream: 1 }));
    assert_eq!(event(0x1000, 2), Ok(Event::JackConnected { jack: 2 }));
    assert_eq!(event(0x1001, 2), Ok(Event::JackDisconnected { jack: 2 }));
    assert_eq!(event(0x1200, 5), Ok(Event::ControlNotify { control: 5 }));
    assert_eq!(
        event(0x7777, 9),
        Ok(Event::Unknown {
            code: 0x7777,
            data: 9
        })
    );
    assert!(Event::read(&[0; 7]).is_err());
}

#[test]
fn a_control_response_is_ok_or_says_why_not() {
    assert_eq!(check_response(&0x8000_u32.to_le_bytes()), Ok(()));
    assert_eq!(
        check_response(&0x8002_u32.to_le_bytes()),
        Err(SndError::Status(0x8002))
    );
    assert_eq!(
        check_response(&0x8003_u32.to_le_bytes()),
        Err(SndError::Status(0x8003))
    );
    assert!(check_response(&[0x00, 0x80, 0x00]).is_err());
}
