//! The server against a client that speaks as libpulse does, built from the
//! crate's own encoders, replaying the sequences `patrace` recorded between
//! mpv or ffmpeg and `PipeWire`'s server on 2026-09-27 (their host names,
//! machine ids and cookies left out).

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::large_enum_variant,
    reason = "a test fails by panicking, indexes what it has just checked, and keeps what it got as it came"
)]

use std::ffi::CString;
use std::io::Cursor;
use std::time::{Duration, SystemTime};

use pulseaudio::protocol::stream::{BufferAttr, StreamFlags};
use pulseaudio::protocol::{
    self, AuthParams, AuthReply, ChannelMap, Command, CorkStreamParams, CreatePlaybackStreamReply,
    FormatEncoding, FormatInfo, GetSinkInfo, LatencyParams, PlaybackLatency, PlaybackStreamParams,
    Props, PulseError, SampleFormat, SampleSpec, ServerInfo, SetClientNameReply, SinkInfo,
    SubscriptionEventFacility, SubscriptionEventType, SubscriptionMask, UpdatePropsParams,
};

use super::{Broken, Card, ClientId, Server};

/// The card: 48 kHz, S16LE, stereo, 40 ms from write to ear.
fn card() -> Card {
    Card {
        name: c"ferrix".to_owned(),
        description: c"Ferrix sound card".to_owned(),
        spec: stereo_48k(),
        latency: Duration::from_millis(40),
    }
}

fn stereo_48k() -> SampleSpec {
    SampleSpec {
        format: SampleFormat::S16Le,
        channels: 2,
        sample_rate: 48_000,
    }
}

/// A period of the card: 20 ms of stereo S16 at 48 kHz.
const PERIOD: usize = 3840;

/// What a client got: a reply or acknowledgement to a sequence number, an
/// error, or a command of the server's own.
#[derive(Debug)]
enum Got {
    Reply { seq: u32, bytes: Vec<u8> },
    Error { seq: u32, error: PulseError },
    Command(Command),
}

/// A client of the server.
struct Client {
    id: ClientId,
    seq: u32,
    version: u16,
}

impl Client {
    fn connect(server: &mut Server) -> Client {
        Client {
            id: server.connect(),
            seq: 0,
            version: protocol::MAX_VERSION,
        }
    }

    /// Send `command`, and give its sequence number.
    fn send(&mut self, server: &mut Server, command: &Command) -> u32 {
        let seq = self.seq;
        self.seq += 1;
        let mut bytes = Vec::new();
        protocol::write_command_message(&mut bytes, seq, command, self.version).unwrap();
        server.receive(self.id, &bytes, now()).unwrap();
        seq
    }

    /// Write `data` to `channel`.
    fn write(&self, server: &mut Server, channel: u32, data: &[u8]) {
        let mut bytes = Vec::new();
        protocol::write_memblock(&mut bytes, channel, data, 0).unwrap();
        server.receive(self.id, &bytes, now()).unwrap();
    }

    /// Everything the server has sent.
    fn got(&self, server: &mut Server) -> Vec<Got> {
        let bytes = server.output(self.id);
        let mut out = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let mut cursor = Cursor::new(&bytes[at..]);
            let descriptor = protocol::read_descriptor(&mut cursor).unwrap();
            let end = at + protocol::DESCRIPTOR_SIZE + descriptor.length as usize;
            let message = bytes[at..end].to_vec();
            let payload = &message[protocol::DESCRIPTOR_SIZE..];
            let (seq, command) =
                Command::read_tag_prefixed(&mut Cursor::new(payload), self.version).unwrap();
            out.push(match command {
                Command::Reply => Got::Reply {
                    seq,
                    bytes: message,
                },
                Command::Error(error) => Got::Error { seq, error },
                command => Got::Command(command),
            });
            at = end;
        }
        out
    }

    /// The one reply the server sent, to `seq`, read as `T`.
    fn reply<T: protocol::CommandReply>(&self, server: &mut Server, seq: u32) -> T {
        let got = self.got(server);
        let [
            Got::Reply {
                seq: answered,
                bytes,
            },
        ] = got.as_slice()
        else {
            panic!("not one reply to {seq}: {got:?}");
        };
        assert_eq!(*answered, seq);
        protocol::read_reply_message::<T>(&mut Cursor::new(bytes), self.version)
            .unwrap()
            .1
    }

    /// The handshake libpulse makes: `AUTH`, then `SET_CLIENT_NAME`.
    fn handshake(server: &mut Server) -> Client {
        let mut client = Client::connect(server);
        let seq = client.send(
            server,
            &Command::Auth(AuthParams {
                version: protocol::MAX_VERSION,
                supports_shm: true,
                supports_memfd: true,
                cookie: vec![0; 256],
            }),
        );
        let _: AuthReply = client.reply(server, seq);
        let mut props = Props::new();
        props.set(protocol::Prop::ApplicationName, c"mpv");
        let seq = client.send(server, &Command::SetClientName(props));
        let _: SetClientNameReply = client.reply(server, seq);
        client
    }
}

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_462_615)
}

/// mpv's `CREATE_PLAYBACK_STREAM`, as recorded: an invalid sample
/// specification and one PCM format, the values JSON, the stream corked, a
/// `tlength` of 19200 and `prebuf` 0.
fn mpv_stream() -> PlaybackStreamParams {
    let mut props = Props::new();
    props.set_bytes(
        c"format.channel_map",
        b"\"front-left,front-right\"\0".as_slice(),
    );
    props.set_bytes(c"format.channels", b"2\0".as_slice());
    props.set_bytes(c"format.rate", b"48000\0".as_slice());
    props.set_bytes(c"format.sample_format", b"\"s16le\"\0".as_slice());
    PlaybackStreamParams {
        sample_spec: SampleSpec::default(),
        channel_map: ChannelMap::empty(),
        buffer_attr: BufferAttr {
            max_length: u32::MAX,
            target_length: 19_200,
            pre_buffering: 0,
            minimum_request_length: u32::MAX,
            fragment_size: u32::MAX,
        },
        formats: vec![FormatInfo {
            encoding: FormatEncoding::Pcm,
            props,
        }],
        flags: StreamFlags {
            start_corked: true,
            ..StreamFlags::default()
        },
        ..PlaybackStreamParams::default()
    }
}

/// A stream in the card's format with every buffer attribute left to the
/// server, as `pa_simple` asks.
fn default_stream() -> PlaybackStreamParams {
    PlaybackStreamParams {
        sample_spec: stereo_48k(),
        channel_map: ChannelMap::stereo(),
        buffer_attr: BufferAttr {
            max_length: u32::MAX,
            target_length: u32::MAX,
            pre_buffering: u32::MAX,
            minimum_request_length: u32::MAX,
            fragment_size: u32::MAX,
        },
        ..PlaybackStreamParams::default()
    }
}

fn create(
    server: &mut Server,
    client: &mut Client,
    params: PlaybackStreamParams,
) -> CreatePlaybackStreamReply {
    let seq = client.send(server, &Command::CreatePlaybackStream(params));
    client.reply(server, seq)
}

fn acked(got: &[Got], seq: u32) -> bool {
    got.iter()
        .any(|got| matches!(got, Got::Reply { seq: answered, bytes } if *answered == seq && bytes.len() == protocol::DESCRIPTOR_SIZE + 10))
}

fn requested(got: &[Got]) -> Vec<u32> {
    got.iter()
        .filter_map(|got| match got {
            Got::Command(Command::Request(request)) => Some(request.length),
            _ => None,
        })
        .collect()
}

#[test]
fn a_client_must_authenticate_first_and_gets_the_lower_version_with_no_shared_memory() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::connect(&mut server);
    let seq = client.send(&mut server, &Command::GetServerInfo);
    assert!(matches!(
        client.got(&mut server).as_slice(),
        [Got::Error { seq: s, error: PulseError::AccessDenied }] if *s == seq
    ));
    let seq = client.send(
        &mut server,
        &Command::Auth(AuthParams {
            version: 40,
            supports_shm: true,
            supports_memfd: true,
            cookie: Vec::new(),
        }),
    );
    let reply: AuthReply = client.reply(&mut server, seq);
    assert_eq!(reply.version, 35);
    assert!(!reply.use_shm && !reply.use_memfd);
}

#[test]
fn the_card_is_the_one_sink_and_the_default_and_there_is_no_source() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::handshake(&mut server);
    let seq = client.send(&mut server, &Command::GetServerInfo);
    let info: ServerInfo = client.reply(&mut server, seq);
    assert_eq!(info.default_sink_name.as_deref(), Some(c"ferrix"));
    assert_eq!(info.default_source_name, None);
    assert_eq!(info.sample_spec, stereo_48k());
    assert_eq!(info.cookie, 7);

    let seq = client.send(
        &mut server,
        &Command::GetSinkInfo(GetSinkInfo {
            index: None,
            name: Some(c"ferrix".to_owned()),
        }),
    );
    let sink: SinkInfo = client.reply(&mut server, seq);
    assert_eq!((sink.index, sink.sample_spec), (0, stereo_48k()));
    let seq = client.send(
        &mut server,
        &Command::GetSinkInfo(GetSinkInfo {
            index: None,
            name: Some(c"alsa_output.other".to_owned()),
        }),
    );
    assert!(matches!(
        client.got(&mut server).as_slice(),
        [Got::Error { seq: s, error: PulseError::NoEntity }] if *s == seq
    ));
    let seq = client.send(&mut server, &Command::GetSinkInfoList);
    let sinks: Vec<SinkInfo> = client.reply(&mut server, seq);
    assert_eq!(sinks.len(), 1);
    let seq = client.send(&mut server, &Command::GetSourceInfoList);
    let sources: Vec<protocol::SourceInfo> = client.reply(&mut server, seq);
    assert!(sources.is_empty());
    let seq = client.send(&mut server, &Command::LookupSink(c"ferrix".to_owned()));
    let found: protocol::LookupReply = client.reply(&mut server, seq);
    assert_eq!(found.0, 0);
}

#[test]
fn mpvs_stream_is_made_from_its_format_list_and_plays_once_uncorked() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::handshake(&mut server);
    let reply = create(&mut server, &mut client, mpv_stream());
    assert_eq!(reply.sample_spec, stereo_48k());
    assert_eq!(reply.channel_map, ChannelMap::stereo());
    assert_eq!(reply.requested_bytes, 19_200, "tlength, as PipeWire asked");
    assert_eq!(reply.buffer_attr.target_length, 19_200);
    assert_eq!(reply.buffer_attr.pre_buffering, 0);
    assert_eq!(
        reply.buffer_attr.minimum_request_length, 4800,
        "a quarter of tlength"
    );
    assert_eq!(reply.buffer_attr.max_length, 4 * 1024 * 1024);
    let channel = reply.channel;

    let seq = client.send(
        &mut server,
        &Command::UpdatePlaybackStreamProplist(UpdatePropsParams {
            index: channel,
            mode: protocol::props::PropsUpdateMode::Replace,
            props: Props::new(),
        }),
    );
    assert!(acked(&client.got(&mut server), seq));
    let seq = client.send(
        &mut server,
        &Command::GetPlaybackLatency(LatencyParams {
            channel,
            now: now(),
        }),
    );
    let latency: PlaybackLatency = client.reply(&mut server, seq);
    assert!(!latency.playing, "corked");
    assert_eq!(latency.sink_usec, 40_000);

    let samples: Vec<u8> = (0..19_200_u32).map(|n| n as u8).collect();
    client.write(&mut server, channel, &samples);
    assert!(server.playing().is_empty(), "still corked");
    let seq = client.send(
        &mut server,
        &Command::CorkPlaybackStream(CorkStreamParams {
            channel,
            cork: false,
        }),
    );
    assert!(acked(&client.got(&mut server), seq));
    assert_eq!(server.playing(), [(client.id, channel)]);

    let mut played = Vec::new();
    assert_eq!(server.read(client.id, channel, PERIOD, &mut played), PERIOD);
    let got = client.got(&mut server);
    assert!(matches!(got.first(), Some(Got::Command(Command::Started(c))) if *c == channel));
    assert!(requested(&got).is_empty(), "3840 missing is under minreq");
    assert_eq!(server.read(client.id, channel, PERIOD, &mut played), PERIOD);
    assert_eq!(requested(&client.got(&mut server)), [7680]);
    assert_eq!(played, samples[..2 * PERIOD]);
}

#[test]
fn a_format_other_than_the_cards_is_refused_until_there_is_mixing() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::handshake(&mut server);
    let mut params = default_stream();
    params.sample_spec.sample_rate = 44_100;
    let seq = client.send(&mut server, &Command::CreatePlaybackStream(params));
    assert!(matches!(
        client.got(&mut server).as_slice(),
        [Got::Error { seq: s, error: PulseError::NotSupported }] if *s == seq
    ));
    let mut params = mpv_stream();
    params.formats.clear();
    let seq = client.send(&mut server, &Command::CreatePlaybackStream(params));
    assert!(matches!(
        client.got(&mut server).as_slice(),
        [Got::Error { seq: s, error: PulseError::Invalid }] if *s == seq
    ));
}

#[test]
fn a_default_stream_waits_for_prebuf_and_again_after_an_underrun() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::handshake(&mut server);
    let reply = create(&mut server, &mut client, default_stream());
    let attr = reply.buffer_attr;
    assert_eq!(attr.target_length, 48_000, "250 ms");
    assert_eq!(attr.minimum_request_length, 12_000);
    assert_eq!(attr.pre_buffering, 36_000, "tlength less minreq");
    assert_eq!(reply.requested_bytes, 48_000);
    let channel = reply.channel;

    client.write(&mut server, channel, &vec![1; 35_996]);
    assert!(server.playing().is_empty(), "four bytes short of prebuf");
    client.write(&mut server, channel, &[1; 4]);
    assert_eq!(server.playing().len(), 1);

    let mut played = Vec::new();
    assert_eq!(server.read(client.id, channel, 40_000, &mut played), 36_000);
    let got = client.got(&mut server);
    assert!(got.iter().any(|got| matches!(got, Got::Command(Command::Underflow(u)) if u.channel == channel && u.offset == 36_000)));
    assert!(server.playing().is_empty(), "prebuffering again");
    assert_eq!(server.read(client.id, channel, 40_000, &mut played), 0);
}

#[test]
fn a_drain_is_answered_once_the_card_has_the_last_byte() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::handshake(&mut server);
    let channel = create(&mut server, &mut client, default_stream()).channel;
    client.write(&mut server, channel, &[3; 2 * PERIOD]);
    let seq = client.send(&mut server, &Command::DrainPlaybackStream(channel));
    assert!(!acked(&client.got(&mut server), seq), "not yet played");
    assert_eq!(
        server.playing().len(),
        1,
        "a drain starts a prebuffering stream"
    );
    let mut played = Vec::new();
    let _ = server.read(client.id, channel, PERIOD, &mut played);
    assert!(!acked(&client.got(&mut server), seq));
    let _ = server.read(client.id, channel, PERIOD, &mut played);
    assert!(acked(&client.got(&mut server), seq));

    let seq = client.send(&mut server, &Command::DrainPlaybackStream(channel));
    assert!(acked(&client.got(&mut server), seq), "nothing queued");
}

#[test]
fn a_flush_drops_the_queue_and_asks_for_it_again() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::handshake(&mut server);
    let channel = create(&mut server, &mut client, default_stream()).channel;
    client.write(&mut server, channel, &[5; 48_000]);
    let seq = client.send(&mut server, &Command::FlushPlaybackStream(channel));
    let got = client.got(&mut server);
    assert!(acked(&got, seq));
    assert_eq!(requested(&got), [48_000]);
    assert!(server.playing().is_empty());
}

#[test]
fn what_is_written_past_maxlength_is_dropped_and_reported() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::handshake(&mut server);
    let mut params = default_stream();
    params.buffer_attr.max_length = 2 * PERIOD as u32;
    let channel = create(&mut server, &mut client, params).channel;
    client.write(&mut server, channel, &[7; 3 * PERIOD]);
    assert!(client.got(&mut server).iter().any(
        |got| matches!(got, Got::Command(Command::Overflow(bytes)) if *bytes == PERIOD as u32)
    ));
}

#[test]
fn a_subscriber_hears_streams_and_clients_come_and_go() {
    let mut server = Server::new(card(), 7);
    let mut watcher = Client::handshake(&mut server);
    let seq = watcher.send(
        &mut server,
        &Command::Subscribe(SubscriptionMask::SINK_INPUT | SubscriptionMask::CLIENT),
    );
    assert!(acked(&watcher.got(&mut server), seq));
    let mut player = Client::handshake(&mut server);
    let reply = create(&mut server, &mut player, default_stream());
    server.disconnect(player.id);
    let events: Vec<_> = watcher
        .got(&mut server)
        .into_iter()
        .filter_map(|got| match got {
            Got::Command(Command::SubscribeEvent(event)) => {
                Some((event.event_facility, event.event_type, event.index))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        events,
        [
            (
                SubscriptionEventFacility::Client,
                SubscriptionEventType::New,
                Some(1)
            ),
            (
                SubscriptionEventFacility::SinkInput,
                SubscriptionEventType::New,
                Some(reply.stream_index)
            ),
            (
                SubscriptionEventFacility::SinkInput,
                SubscriptionEventType::Removed,
                Some(reply.stream_index)
            ),
            (
                SubscriptionEventFacility::Client,
                SubscriptionEventType::Removed,
                Some(1)
            ),
        ]
    );
}

#[test]
fn a_message_that_comes_a_byte_at_a_time_is_read_once_whole() {
    let mut server = Server::new(card(), 7);
    let client = Client::connect(&mut server);
    let mut bytes = Vec::new();
    protocol::write_command_message(
        &mut bytes,
        0,
        &Command::Auth(AuthParams {
            version: 35,
            ..AuthParams::default()
        }),
        35,
    )
    .unwrap();
    for byte in &bytes {
        server.receive(client.id, &[*byte], now()).unwrap();
    }
    let got = client.got(&mut server);
    assert!(matches!(got.as_slice(), [Got::Reply { seq: 0, .. }]));
}

#[test]
fn a_client_that_breaks_the_protocol_is_refused() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::handshake(&mut server);
    let channel = create(&mut server, &mut client, default_stream()).channel;

    let mut bytes = Vec::new();
    protocol::write_memblock(&mut bytes, channel + 1, &[0; 4], 0).unwrap();
    assert!(matches!(
        server.receive(client.id, &bytes, now()),
        Err(Broken(_))
    ));

    let mut server = Server::new(card(), 7);
    let client = Client::connect(&mut server);
    let huge = protocol::Descriptor {
        length: u32::MAX,
        channel: u32::MAX,
        offset: 0,
        flags: protocol::DescriptorFlags::empty(),
    };
    let mut bytes = Vec::new();
    protocol::write_descriptor(&mut bytes, &huge).unwrap();
    assert!(matches!(
        server.receive(client.id, &bytes, now()),
        Err(Broken(_))
    ));
}

#[test]
fn what_is_not_built_says_so_and_a_record_stream_finds_no_source() {
    let mut server = Server::new(card(), 7);
    let mut client = Client::handshake(&mut server);
    let seq = client.send(&mut server, &Command::Stat);
    assert!(matches!(
        client.got(&mut server).as_slice(),
        [Got::Error { seq: s, error: PulseError::NotImplemented }] if *s == seq
    ));
    let seq = client.send(
        &mut server,
        &Command::CreateRecordStream(protocol::RecordStreamParams::default()),
    );
    assert!(matches!(
        client.got(&mut server).as_slice(),
        [Got::Error { seq: s, error: PulseError::NoEntity }] if *s == seq
    ));
    let name = CString::new("no such").unwrap();
    let seq = client.send(&mut server, &Command::LookupSink(name));
    assert!(matches!(
        client.got(&mut server).as_slice(),
        [Got::Error { seq: s, error: PulseError::NoEntity }] if *s == seq
    ));
}
