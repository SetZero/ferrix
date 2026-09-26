//! The server side of the `PulseAudio` native protocol, as a state machine that
//! does no I/O (`docs/AUDIO.md`, U2a).
//!
//! A daemon (`pulsed`, U2b) owns the sockets and the sound card. It hands this
//! the bytes each client sends ([`Server::receive`]), sends each client what
//! [`Server::output`] gives, and, clocked by the card, takes the samples of the
//! streams that are playing ([`Server::read`]). Everything between -- the
//! handshake, introspection, stream creation and the accounting of what each
//! stream is owed -- is here, host-tested against the command sequences
//! `patrace` recorded between real clients and a real server.
//!
//! What it answers, and how:
//!
//! * `AUTH` at the client's version or 35, whichever is lower, with shared
//!   memory and memfd both refused, so that samples come inline on the socket.
//!   The cookie is not checked: the socket's permissions are the access
//!   control, as `PulseAudio`'s `auth-anonymous` has it.
//! * `SET_CLIENT_NAME` and the client proplist updates.
//! * The server's, the sink's, the clients' and the sink inputs' information,
//!   one at a time and as lists; `LOOKUP_SINK`. There is one sink, the card,
//!   and no source: a record stream is refused.
//! * `SUBSCRIBE`, and the events for sink inputs and clients coming and going.
//! * Playback streams: create, delete, cork, flush, trigger, prebuf, drain,
//!   latency, name and proplist ([`stream`]).
//!
//! Streams play in any of the usual formats, U8, S16, S32 and float, at any
//! rate the resampler can convert, mono or stereo or more: [`Server::mix`]
//! decodes, maps, resamples and scales each (`mix`) and sums them for the
//! card (U2c). A stream's volume and mute, and the sink's, are set with the
//! usual commands. Every other command answers `NOTIMPLEMENTED`.

mod format;
mod mix;
mod stream;

use std::collections::BTreeMap;
use std::ffi::CString;
use std::io::Cursor;
use std::time::{Duration, SystemTime};

use pulseaudio::protocol::{
    self, AuthReply, ChannelMap, ChannelVolume, ClientInfo, Command, CreatePlaybackStreamReply,
    FormatEncoding, FormatInfo, LookupReply, PlaybackLatency, PlaybackStreamParams, Props,
    PulseError, SampleSpec, ServerInfo, SetClientNameReply, SinkInfo, SinkInputInfo, SinkState,
    SubscriptionEvent, SubscriptionEventFacility, SubscriptionEventType, SubscriptionMask,
};

use stream::{Owed, Stream};

/// The types a [`Card`] is described in, for a daemon that names none of the
/// protocol's own.
pub use pulseaudio::protocol::{SampleFormat, SampleSpec as Spec};

/// The protocol version spoken at most: the crate's, which is libpulse's
/// since 15.0.
pub const PROTOCOL_VERSION: u16 = protocol::MAX_VERSION;

/// The longest message a client may send: a second of eight-channel 32-bit
/// audio at 192 kHz and change. Anything longer is taken for a broken or
/// hostile client, which is disconnected rather than buffered for.
const MAX_MESSAGE: u32 = 8 * 1024 * 1024;

/// The sound card, as the one sink.
#[derive(Clone, Debug)]
pub struct Card {
    /// The sink's name, which clients connect to and `LOOKUP_SINK` finds.
    pub name: CString,
    /// What people are shown.
    pub description: CString,
    /// The one format it plays.
    pub spec: SampleSpec,
    /// How long a byte written to it takes to be heard.
    pub latency: Duration,
}

/// A connection, as [`Server::connect`] names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientId(u32);

/// Why a connection has to end: the client broke the protocol, and anything
/// it sent after cannot be read.
#[derive(Debug, PartialEq, Eq)]
pub struct Broken(pub String);

/// One connection.
#[derive(Debug)]
struct Client {
    /// Its index, which introspection names it by.
    index: u32,
    /// Bytes received and not yet a whole message.
    inbox: Vec<u8>,
    /// Bytes owed to it.
    outbox: Vec<u8>,
    /// The protocol version agreed, or 0 before `AUTH`.
    version: u16,
    /// Its properties.
    props: Props,
    /// Whether `SET_CLIENT_NAME` has come.
    named: bool,
    /// Its playback streams, by channel.
    streams: BTreeMap<u32, Stream>,
    /// The next channel number.
    next_channel: u32,
    /// What it asked to hear of.
    subscribed: SubscriptionMask,
}

/// The server: every client, and the one sink.
#[derive(Debug)]
pub struct Server {
    card: Card,
    clients: BTreeMap<ClientId, Client>,
    next_client: u32,
    next_input: u32,
    cookie: u32,
    /// The sink's own volume and mute, over every stream's.
    sink_volume: ChannelVolume,
    sink_muted: bool,
}

impl Server {
    /// A server with no clients, playing to `card`, which answers `cookie`
    /// as its identity.
    #[must_use]
    pub fn new(card: Card, cookie: u32) -> Server {
        Server {
            clients: BTreeMap::new(),
            next_client: 0,
            next_input: 0,
            cookie,
            sink_volume: ChannelVolume::norm(card.spec.channels),
            sink_muted: false,
            card,
        }
    }

    /// A new connection.
    pub fn connect(&mut self) -> ClientId {
        let id = ClientId(self.next_client);
        let index = self.next_client;
        self.next_client = self.next_client.wrapping_add(1);
        let _ = self.clients.insert(
            id,
            Client {
                index,
                inbox: Vec::new(),
                outbox: Vec::new(),
                version: 0,
                props: Props::new(),
                named: false,
                streams: BTreeMap::new(),
                next_channel: 0,
                subscribed: SubscriptionMask::empty(),
            },
        );
        id
    }

    /// A connection ended: its streams go, and those who asked hear so.
    pub fn disconnect(&mut self, id: ClientId) {
        let Some(client) = self.clients.remove(&id) else {
            return;
        };
        for stream in client.streams.values() {
            self.announce(
                SubscriptionEventFacility::SinkInput,
                SubscriptionEventType::Removed,
                stream.index,
            );
        }
        if client.named {
            self.announce(
                SubscriptionEventFacility::Client,
                SubscriptionEventType::Removed,
                client.index,
            );
        }
    }

    /// What is owed to `id`, taken: bytes for its socket.
    pub fn output(&mut self, id: ClientId) -> Vec<u8> {
        self.clients
            .get_mut(&id)
            .map(|client| std::mem::take(&mut client.outbox))
            .unwrap_or_default()
    }

    /// The streams the card should take now, as `(client, channel)`.
    #[must_use]
    pub fn playing(&self) -> Vec<(ClientId, u32)> {
        self.clients
            .iter()
            .flat_map(|(&id, client)| {
                client
                    .streams
                    .iter()
                    .filter(|(_, stream)| stream.playing())
                    .map(move |(&channel, _)| (id, channel))
            })
            .collect()
    }

    /// The card takes up to `max` bytes of `channel` of `id` into `out`: how
    /// many it took. Asking for more than there is, while playing, is an
    /// underrun the client hears of.
    pub fn read(&mut self, id: ClientId, channel: u32, max: usize, out: &mut Vec<u8>) -> usize {
        let Some(client) = self.clients.get_mut(&id) else {
            return 0;
        };
        let Some(stream) = client.streams.get_mut(&channel) else {
            return 0;
        };
        let mut owed = Vec::new();
        let taken = stream.read(max, out, &mut owed);
        send_owed(client, channel, &owed);
        taken
    }

    /// Mix up to `frames` of the card's frames from every playing stream into
    /// `out`, the card's channels interleaved: how many frames it made. Each
    /// stream is taken as the card takes it, so its requests, underruns and
    /// drains follow ([`Server::read`]). A stream with less than the others is
    /// silent for the rest; with nothing from any, nothing is made, so the
    /// card is never given sound no client sent.
    pub fn mix(&mut self, frames: usize, out: &mut Vec<i16>) -> usize {
        let channels = usize::from(self.card.spec.channels);
        let mut sum = vec![0.0_f32; frames * channels];
        let mut made = 0;
        let sink_gain = if self.sink_muted {
            0.0
        } else {
            self.sink_volume
                .channels()
                .first()
                .map_or(1.0, protocol::Volume::to_linear)
        };
        let mut bytes = Vec::new();
        for client in self.clients.values_mut() {
            let mut owed_by = Vec::new();
            for (&channel, stream) in &mut client.streams {
                let mut owed = Vec::new();
                made = made.max(mix_stream(
                    stream, frames, sink_gain, &mut sum, &mut bytes, &mut owed,
                ));
                owed_by.push((channel, owed));
            }
            for (channel, owed) in owed_by {
                send_owed(client, channel, &owed);
            }
        }
        out.extend(
            sum.iter()
                .take(made * channels)
                .map(|&sample| mix::to_card(sample)),
        );
        made
    }

    /// Bytes from `id`'s socket, `now` the server's clock. What they ask is
    /// answered into [`Server::output`].
    ///
    /// # Errors
    ///
    /// [`Broken`] when the client broke the protocol; its connection should
    /// then be closed and [`Server::disconnect`] called.
    pub fn receive(&mut self, id: ClientId, bytes: &[u8], now: SystemTime) -> Result<(), Broken> {
        let Some(client) = self.clients.get_mut(&id) else {
            return Err(Broken("no such client".to_owned()));
        };
        client.inbox.extend_from_slice(bytes);
        loop {
            let Some(client) = self.clients.get_mut(&id) else {
                return Ok(());
            };
            let Some((descriptor, payload)) = next_message(&mut client.inbox)? else {
                return Ok(());
            };
            if descriptor.channel == u32::MAX {
                let version = client.version;
                let (seq, command) =
                    Command::read_tag_prefixed(&mut Cursor::new(&payload), version)
                        .map_err(|error| Broken(format!("an unreadable command: {error}")))?;
                self.command(id, seq, command, now);
            } else {
                write_samples(client, &descriptor, &payload)?;
            }
        }
    }

    /// Answer one command.
    fn command(&mut self, id: ClientId, seq: u32, command: Command, now: SystemTime) {
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        if client.version == 0 {
            match command {
                Command::Auth(params) => {
                    client.version = params.version.min(PROTOCOL_VERSION);
                    let reply = AuthReply {
                        version: client.version,
                        use_memfd: false,
                        use_shm: false,
                    };
                    reply_to(client, seq, &reply);
                }
                _ => error_to(client, seq, PulseError::AccessDenied),
            }
            return;
        }
        match command {
            Command::SetClientName(props) => {
                let first = !client.named;
                client.props = props;
                client.named = true;
                let index = client.index;
                reply_to(client, seq, &SetClientNameReply { client_id: index });
                self.announce(
                    SubscriptionEventFacility::Client,
                    if first {
                        SubscriptionEventType::New
                    } else {
                        SubscriptionEventType::Changed
                    },
                    index,
                );
            }
            Command::UpdateClientProplist(update) => {
                update_props(&mut client.props, update.mode, update.props);
                ack_to(client, seq);
            }
            Command::Subscribe(mask) => {
                client.subscribed = mask;
                ack_to(client, seq);
            }
            Command::GetServerInfo => {
                let info = self.server_info();
                if let Some(client) = self.clients.get_mut(&id) {
                    reply_to(client, seq, &info);
                }
            }
            Command::GetSinkInfo(which) => {
                let sink = self.sink_info();
                let matches = which.index.is_none_or(|index| index == sink.index)
                    && which.name.as_ref().is_none_or(|name| *name == sink.name);
                if let Some(client) = self.clients.get_mut(&id) {
                    if matches {
                        reply_to(client, seq, &sink);
                    } else {
                        error_to(client, seq, PulseError::NoEntity);
                    }
                }
            }
            Command::GetSinkInfoList => {
                let sinks = vec![self.sink_info()];
                if let Some(client) = self.clients.get_mut(&id) {
                    reply_to(client, seq, &sinks);
                }
            }
            Command::LookupSink(name) => {
                let found = name == self.card.name;
                if let Some(client) = self.clients.get_mut(&id) {
                    if found {
                        reply_to(client, seq, &LookupReply(SINK_INDEX));
                    } else {
                        error_to(client, seq, PulseError::NoEntity);
                    }
                }
            }
            Command::GetSourceInfoList => {
                reply_to(client, seq, &Vec::<protocol::SourceInfo>::new());
            }
            Command::GetModuleInfoList => {
                reply_to(client, seq, &Vec::<protocol::ModuleInfo>::new());
            }
            Command::GetCardInfoList => {
                reply_to(client, seq, &Vec::<protocol::CardInfo>::new());
            }
            Command::GetSampleInfoList => {
                reply_to(client, seq, &Vec::<protocol::SampleInfo>::new());
            }
            Command::GetSourceOutputInfoList => {
                reply_to(client, seq, &Vec::<protocol::SourceOutputInfo>::new());
            }
            Command::GetClientInfo(index) => {
                let info = self
                    .client_infos()
                    .into_iter()
                    .find(|info| info.index == index);
                if let Some(client) = self.clients.get_mut(&id) {
                    match info {
                        Some(info) => reply_to(client, seq, &info),
                        None => error_to(client, seq, PulseError::NoEntity),
                    }
                }
            }
            Command::GetClientInfoList => {
                let infos = self.client_infos();
                if let Some(client) = self.clients.get_mut(&id) {
                    reply_to(client, seq, &infos);
                }
            }
            Command::GetSinkInputInfo(index) => {
                let info = self
                    .sink_inputs()
                    .into_iter()
                    .find(|info| info.index == index);
                if let Some(client) = self.clients.get_mut(&id) {
                    match info {
                        Some(info) => reply_to(client, seq, &info),
                        None => error_to(client, seq, PulseError::NoEntity),
                    }
                }
            }
            Command::GetSinkInputInfoList => {
                let infos = self.sink_inputs();
                if let Some(client) = self.clients.get_mut(&id) {
                    reply_to(client, seq, &infos);
                }
            }
            Command::CreatePlaybackStream(params) => self.create_playback(id, seq, params),
            Command::CreateRecordStream(_) => error_to(client, seq, PulseError::NoEntity),
            Command::SetSinkVolume(params) => {
                let ours = params.device_index.is_none_or(|index| index == SINK_INDEX)
                    && params
                        .device_name
                        .as_ref()
                        .is_none_or(|name| *name == self.card.name);
                if ours {
                    self.sink_volume = params.volume;
                    if let Some(client) = self.clients.get_mut(&id) {
                        ack_to(client, seq);
                    }
                    self.announce(
                        SubscriptionEventFacility::Sink,
                        SubscriptionEventType::Changed,
                        SINK_INDEX,
                    );
                } else if let Some(client) = self.clients.get_mut(&id) {
                    error_to(client, seq, PulseError::NoEntity);
                }
            }
            Command::SetSinkMute(params) => {
                let ours = params.device_index.is_none_or(|index| index == SINK_INDEX)
                    && params
                        .device_name
                        .as_ref()
                        .is_none_or(|name| *name == self.card.name);
                if ours {
                    self.sink_muted = params.mute;
                    if let Some(client) = self.clients.get_mut(&id) {
                        ack_to(client, seq);
                    }
                    self.announce(
                        SubscriptionEventFacility::Sink,
                        SubscriptionEventType::Changed,
                        SINK_INDEX,
                    );
                } else if let Some(client) = self.clients.get_mut(&id) {
                    error_to(client, seq, PulseError::NoEntity);
                }
            }
            Command::SetSinkInputVolume(params) => {
                self.set_input(id, seq, params.index, |stream| {
                    if params.volume.channels().len() == usize::from(stream.spec.channels) {
                        stream.volume = params.volume;
                        true
                    } else {
                        false
                    }
                });
            }
            Command::SetSinkInputMute(params) => {
                self.set_input(id, seq, params.index, |stream| {
                    stream.muted = params.mute;
                    true
                });
            }
            command => self.stream_command(id, seq, command, now),
        }
    }

    /// The commands that name one of a client's playback streams.
    fn stream_command(&mut self, id: ClientId, seq: u32, command: Command, now: SystemTime) {
        let latency = self.card.latency;
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        let channel = match &command {
            Command::DeletePlaybackStream(channel)
            | Command::FlushPlaybackStream(channel)
            | Command::TriggerPlaybackStream(channel)
            | Command::PrebufPlaybackStream(channel)
            | Command::DrainPlaybackStream(channel) => *channel,
            Command::CorkPlaybackStream(params) => params.channel,
            Command::GetPlaybackLatency(params) => params.channel,
            Command::UpdatePlaybackStreamProplist(params) => params.index,
            Command::SetPlaybackStreamName(params) => params.index,
            _ => {
                error_to(client, seq, PulseError::NotImplemented);
                return;
            }
        };
        let Some(stream) = client.streams.get_mut(&channel) else {
            error_to(client, seq, PulseError::NoEntity);
            return;
        };
        let mut owed = Vec::new();
        let mut changed = None;
        match command {
            Command::DeletePlaybackStream(_) => {
                let index = stream.index;
                let _ = client.streams.remove(&channel);
                ack_to(client, seq);
                self.announce(
                    SubscriptionEventFacility::SinkInput,
                    SubscriptionEventType::Removed,
                    index,
                );
                return;
            }
            Command::CorkPlaybackStream(params) => {
                stream.cork(params.cork);
                changed = Some(stream.index);
                owed.push(Owed::Drained(seq));
            }
            Command::FlushPlaybackStream(_) => {
                stream.flush(&mut owed);
                owed.insert(0, Owed::Drained(seq));
            }
            Command::TriggerPlaybackStream(_) => {
                stream.trigger();
                owed.push(Owed::Drained(seq));
            }
            Command::PrebufPlaybackStream(_) => {
                stream.prebuf();
                owed.push(Owed::Drained(seq));
            }
            Command::DrainPlaybackStream(_) => stream.drain(seq, &mut owed),
            Command::GetPlaybackLatency(params) => {
                let reply = PlaybackLatency {
                    sink_usec: u64::try_from(latency.as_micros()).unwrap_or(u64::MAX),
                    source_usec: 0,
                    playing: stream.playing(),
                    local_time: params.now,
                    remote_time: now,
                    write_offset: i64::try_from(stream.written).unwrap_or(i64::MAX),
                    read_offset: i64::try_from(stream.read).unwrap_or(i64::MAX),
                    underrun_for: stream.underrun_for,
                    playing_for: stream.playing_for,
                };
                reply_to(client, seq, &reply);
                return;
            }
            Command::UpdatePlaybackStreamProplist(params) => {
                update_props(&mut stream.props, params.mode, params.props);
                changed = Some(stream.index);
                owed.push(Owed::Drained(seq));
            }
            Command::SetPlaybackStreamName(params) => {
                stream.props.set(protocol::Prop::MediaName, params.name);
                changed = Some(stream.index);
                owed.push(Owed::Drained(seq));
            }
            _ => {}
        }
        send_owed(client, channel, &owed);
        if let Some(index) = changed {
            self.announce(
                SubscriptionEventFacility::SinkInput,
                SubscriptionEventType::Changed,
                index,
            );
        }
    }

    /// `CREATE_PLAYBACK_STREAM`.
    fn create_playback(&mut self, id: ClientId, seq: u32, params: PlaybackStreamParams) {
        let card = self.card.clone();
        let index = self.next_input;
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        if params.sink_index.is_some_and(|sink| sink != SINK_INDEX)
            || params
                .sink_name
                .as_ref()
                .is_some_and(|name| *name != card.name)
        {
            error_to(client, seq, PulseError::NoEntity);
            return;
        }
        let (spec, map) =
            match format::requested(params.sample_spec, &params.channel_map, &params.formats) {
                Ok(format) => format,
                Err(error) => {
                    error_to(client, seq, error);
                    return;
                }
            };
        let converter = match mix::Converter::new(&spec, &map, &card.spec) {
            Ok(converter) => converter,
            Err(error) => {
                error_to(client, seq, error);
                return;
            }
        };
        let volume = params
            .cvolume
            .filter(|volume| volume.channels().len() == usize::from(spec.channels))
            .unwrap_or_else(|| ChannelVolume::norm(spec.channels));
        let attr = format::resolve(params.buffer_attr, &spec);
        let (mut stream, requested) = Stream::new(
            index,
            spec,
            map,
            attr,
            params.flags.start_corked,
            params.props,
            volume,
            converter,
        );
        stream.muted = params.flags.start_muted.unwrap_or(false);
        let channel = client.next_channel;
        client.next_channel = client.next_channel.wrapping_add(1);
        let _ = client.streams.insert(channel, stream);
        self.next_input = self.next_input.wrapping_add(1);
        let latency = format::duration_of(&spec, u64::from(attr.target_length)) + card.latency;
        let reply = CreatePlaybackStreamReply {
            channel,
            stream_index: index,
            requested_bytes: requested,
            buffer_attr: attr,
            sample_spec: spec,
            channel_map: map,
            stream_latency: u64::try_from(latency.as_micros()).unwrap_or(u64::MAX),
            sink_index: SINK_INDEX,
            sink_name: Some(card.name.clone()),
            suspended: false,
            format: pcm_format(),
        };
        reply_to(client, seq, &reply);
        self.announce(
            SubscriptionEventFacility::SinkInput,
            SubscriptionEventType::New,
            index,
        );
    }

    /// Change the sink input `index`, whichever client's it is, and answer
    /// `seq` of `id`: `change` says whether what it was given was valid.
    fn set_input(
        &mut self,
        id: ClientId,
        seq: u32,
        index: u32,
        change: impl FnOnce(&mut Stream) -> bool,
    ) {
        let found = self
            .clients
            .values_mut()
            .flat_map(|client| client.streams.values_mut())
            .find(|stream| stream.index == index)
            .map(change);
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        match found {
            Some(true) => {
                ack_to(client, seq);
                self.announce(
                    SubscriptionEventFacility::SinkInput,
                    SubscriptionEventType::Changed,
                    index,
                );
            }
            Some(false) => error_to(client, seq, PulseError::Invalid),
            None => error_to(client, seq, PulseError::NoEntity),
        }
    }

    /// `GET_SERVER_INFO`'s answer.
    fn server_info(&self) -> ServerInfo {
        ServerInfo {
            server_name: Some(c"pulseaudio".to_owned()),
            server_version: Some(c"17.0.0 (pulsed on Ferrix)".to_owned()),
            user_name: Some(c"ferrix".to_owned()),
            host_name: Some(c"ferrix".to_owned()),
            sample_spec: self.card.spec,
            cookie: self.cookie,
            default_sink_name: Some(self.card.name.clone()),
            default_source_name: None,
            channel_map: ChannelMap::stereo(),
        }
    }

    /// The card as a sink.
    fn sink_info(&self) -> SinkInfo {
        let running = self
            .clients
            .values()
            .any(|client| client.streams.values().any(Stream::playing));
        let mut sink = SinkInfo::new_dummy(SINK_INDEX);
        sink.name = self.card.name.clone();
        sink.description = Some(self.card.description.clone());
        sink.sample_spec = self.card.spec;
        sink.cvolume = self.sink_volume;
        sink.muted = self.sink_muted;
        sink.state = if running {
            SinkState::Running
        } else {
            SinkState::Idle
        };
        sink.actual_latency = u64::try_from(self.card.latency.as_micros()).unwrap_or(u64::MAX);
        sink.configured_latency = sink.actual_latency;
        sink.driver = Some(c"pulsed".to_owned());
        sink
    }

    /// Every named client, as introspection shows it.
    fn client_infos(&self) -> Vec<ClientInfo> {
        self.clients
            .values()
            .filter(|client| client.named)
            .map(|client| ClientInfo {
                index: client.index,
                name: client
                    .props
                    .get(protocol::Prop::ApplicationName)
                    .and_then(|name| CString::new(trim_nul(name)).ok())
                    .unwrap_or_default(),
                owner_module_index: None,
                driver: Some(c"pulsed".to_owned()),
                props: client.props.clone(),
            })
            .collect()
    }

    /// Every playback stream, as a sink input.
    fn sink_inputs(&self) -> Vec<SinkInputInfo> {
        self.clients
            .values()
            .flat_map(|client| {
                client.streams.values().map(move |stream| SinkInputInfo {
                    index: stream.index,
                    name: stream
                        .props
                        .get(protocol::Prop::MediaName)
                        .and_then(|name| CString::new(trim_nul(name)).ok())
                        .unwrap_or_default(),
                    client_index: Some(client.index),
                    sink_index: SINK_INDEX,
                    sample_spec: stream.spec,
                    channel_map: stream.map,
                    cvolume: stream.volume,
                    muted: stream.muted,
                    buffer_latency: u64::try_from(
                        format::duration_of(&stream.spec, stream.queued() as u64).as_micros(),
                    )
                    .unwrap_or(u64::MAX),
                    sink_latency: u64::try_from(self.card.latency.as_micros()).unwrap_or(u64::MAX),
                    driver: Some(c"pulsed".to_owned()),
                    props: stream.props.clone(),
                    corked: stream.corked,
                    has_volume: true,
                    volume_writable: true,
                    format: pcm_format(),
                    ..SinkInputInfo::default()
                })
            })
            .collect()
    }

    /// Tell every client subscribed to `facility` that `index` came, changed
    /// or went.
    fn announce(
        &mut self,
        facility: SubscriptionEventFacility,
        kind: SubscriptionEventType,
        index: u32,
    ) {
        let mask = match facility {
            SubscriptionEventFacility::SinkInput => SubscriptionMask::SINK_INPUT,
            SubscriptionEventFacility::Client => SubscriptionMask::CLIENT,
            _ => SubscriptionMask::SINK,
        };
        let event = Command::SubscribeEvent(SubscriptionEvent {
            event_facility: facility,
            event_type: kind,
            index: Some(index),
        });
        for client in self.clients.values_mut() {
            if client.subscribed.contains(mask) {
                command_to(client, &event);
            }
        }
    }
}

/// Add up to `frames` of `stream`, scaled by its volume and `sink_gain`,
/// into `sum`, reading from its queue as the card would: how many frames it
/// added. `bytes` is scratch.
fn mix_stream(
    stream: &mut Stream,
    frames: usize,
    sink_gain: f32,
    sum: &mut [f32],
    bytes: &mut Vec<u8>,
    owed: &mut Vec<Owed>,
) -> usize {
    let gains: Vec<f32> = stream
        .volume
        .channels()
        .iter()
        .map(|volume| {
            if stream.muted {
                0.0
            } else {
                volume.to_linear() * sink_gain
            }
        })
        .collect();
    while stream.playing() && stream.converter.pending_frames() < frames {
        let want = stream
            .converter
            .bytes_for(frames - stream.converter.pending_frames());
        bytes.clear();
        if stream.read(want, bytes, owed) == 0 {
            break;
        }
        stream.converter.feed(bytes, &gains);
    }
    stream.converter.take(frames, sum)
}

/// The card's index as a sink.
const SINK_INDEX: u32 = 0;

/// The format a PCM stream is shown in.
fn pcm_format() -> FormatInfo {
    FormatInfo::new(FormatEncoding::Pcm)
}

/// A property's bytes to its first NUL.
fn trim_nul(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .take_while(|&&byte| byte != 0)
        .copied()
        .collect()
}

/// A proplist update in `mode`.
fn update_props(props: &mut Props, mode: protocol::props::PropsUpdateMode, new: Props) {
    use protocol::props::PropsUpdateMode;
    match mode {
        PropsUpdateMode::Set => *props = new,
        PropsUpdateMode::Merge => {
            for (key, value) in new.iter() {
                if props.get_bytes(key).is_none() {
                    props.set_bytes(key.as_ref(), value.as_ref());
                }
            }
        }
        PropsUpdateMode::Replace => {
            for (key, value) in new.iter() {
                props.set_bytes(key.as_ref(), value.as_ref());
            }
        }
    }
}

/// The next whole message in `inbox`, taken out of it, or `None` if it has
/// not all come yet.
fn next_message(inbox: &mut Vec<u8>) -> Result<Option<(protocol::Descriptor, Vec<u8>)>, Broken> {
    let Some(header) = inbox.get(..protocol::DESCRIPTOR_SIZE) else {
        return Ok(None);
    };
    let descriptor = protocol::read_descriptor(&mut Cursor::new(header))
        .map_err(|error| Broken(format!("an unreadable descriptor: {error}")))?;
    if descriptor.length > MAX_MESSAGE {
        return Err(Broken(format!(
            "a message of {} bytes, more than {MAX_MESSAGE}",
            descriptor.length
        )));
    }
    let end = protocol::DESCRIPTOR_SIZE + descriptor.length as usize;
    let Some(payload) = inbox.get(protocol::DESCRIPTOR_SIZE..end) else {
        return Ok(None);
    };
    let payload = payload.to_vec();
    let _ = inbox.drain(..end);
    Ok(Some((descriptor, payload)))
}

/// A memblock written to one of a client's streams. Only a write at the
/// write index -- a relative seek of zero, which is what libpulse sends for
/// `pa_stream_write` -- is taken; any other seek is refused as broken.
fn write_samples(
    client: &mut Client,
    descriptor: &protocol::Descriptor,
    payload: &[u8],
) -> Result<(), Broken> {
    const SEEK_MODE: u32 = 0xff;
    const SEEK_RELATIVE: u32 = 0;
    if descriptor.offset != 0 || descriptor.flags.bits() & SEEK_MODE != SEEK_RELATIVE {
        return Err(Broken(format!(
            "a write seeking to {} in mode {}",
            descriptor.offset,
            descriptor.flags.bits() & SEEK_MODE
        )));
    }
    let channel = descriptor.channel;
    let Some(stream) = client.streams.get_mut(&channel) else {
        return Err(Broken(format!(
            "a write to channel {channel}, which has no stream"
        )));
    };
    let mut owed = Vec::new();
    stream.write(payload, &mut owed);
    send_owed(client, channel, &owed);
    Ok(())
}

/// Send what a stream owes its client.
fn send_owed(client: &mut Client, channel: u32, owed: &[Owed]) {
    for &debt in owed {
        match debt {
            Owed::Request(length) => {
                command_to(
                    client,
                    &Command::Request(protocol::Request { channel, length }),
                );
            }
            Owed::Started => command_to(client, &Command::Started(channel)),
            Owed::Underflow(offset) => command_to(
                client,
                &Command::Underflow(protocol::Underflow { channel, offset }),
            ),
            Owed::Overflow(bytes) => command_to(client, &Command::Overflow(bytes)),
            Owed::Drained(seq) => ack_to(client, seq),
        }
    }
}

/// A command from the server, which has no sequence number.
fn command_to(client: &mut Client, command: &Command) {
    let _ = protocol::write_command_message(&mut client.outbox, u32::MAX, command, client.version);
}

/// A reply to `seq`.
fn reply_to<R: protocol::CommandReply>(client: &mut Client, seq: u32, reply: &R) {
    let _ = protocol::write_reply_message(&mut client.outbox, seq, reply, client.version);
}

/// A bare acknowledgement of `seq`.
fn ack_to(client: &mut Client, seq: u32) {
    let _ = protocol::write_ack_message(&mut client.outbox, seq);
}

/// An error in answer to `seq`.
fn error_to(client: &mut Client, seq: u32, error: PulseError) {
    let _ = protocol::write_error(&mut client.outbox, seq, &error);
}

#[cfg(test)]
mod tests;
