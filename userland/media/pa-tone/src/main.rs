//! `pa-tone SOCKET`: a second of `userland/compositor/tone`'s counter, played
//! over the `PulseAudio` protocol (`docs/AUDIO.md`, U2b).
//!
//! It does what a libpulse client does with the simple API: `AUTH`,
//! `SET_CLIENT_NAME`, a playback stream in the card's format with every
//! buffer attribute left to the server, then as many bytes as each `REQUEST`
//! asks for, until 48000 frames are sent, and a drain. Frame `n` holds `n`
//! on the left and its complement on the right, as tone's does, so
//! `xtask test-audio` holds QEMU's file to the same check. Its lines start
//! `tone:`, for the same reason. It is blocking and single-threaded: the
//! server is the clock.

use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;

use pulseaudio::protocol::stream::BufferAttr;
use pulseaudio::protocol::{
    self, AuthParams, AuthReply, ChannelMap, Command, CreatePlaybackStreamReply,
    PlaybackStreamParams, Props, SampleFormat, SampleSpec, SetClientNameReply,
};

/// Frames in the second played.
const FRAMES: u32 = 48_000;

fn say(text: &str) {
    let mut out = io::stdout();
    let _ = writeln!(out, "tone: {text}");
    let _ = out.flush();
}

fn main() {
    let Some(path) = std::env::args_os().nth(1) else {
        say("failed: no socket named");
        return;
    };
    match play(std::path::Path::new(&path)) {
        Ok(()) => say("done"),
        Err(error) => say(&format!("failed: {error}")),
    }
}

/// Frames `from` to `from + count`, as bytes.
fn samples(from: u32, count: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(count as usize * 4);
    for n in from..from + count {
        let value = n as u16;
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&(!value).to_le_bytes());
    }
    bytes
}

/// The server's socket, waited for for up to five seconds: the boot that
/// runs this starts `pulsed` beside it, and either may be first.
fn connect(path: &std::path::Path) -> io::Result<UnixStream> {
    let mut tries = 0;
    loop {
        match UnixStream::connect(path) {
            Ok(socket) => return Ok(socket),
            Err(error)
                if tries < 500
                    && matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
            {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

/// As much of the counter as `bytes` asks for, from frame `next` on.
fn send(writer: &mut UnixStream, channel: u32, bytes: u32, next: &mut u32) -> io::Result<()> {
    let frames = (bytes / 4).min(FRAMES + 1 - *next);
    if frames == 0 {
        return Ok(());
    }
    protocol::write_memblock(writer, channel, &samples(*next, frames), 0)
        .map_err(protocol_error)?;
    *next += frames;
    Ok(())
}

fn protocol_error(error: protocol::ProtocolError) -> io::Error {
    io::Error::other(error.to_string())
}

fn play(path: &std::path::Path) -> io::Result<()> {
    let socket = connect(path)?;
    let mut writer = socket.try_clone()?;
    let mut reader = BufReader::new(socket);
    let mut version = protocol::MAX_VERSION;

    protocol::write_command_message(
        &mut writer,
        0,
        &Command::Auth(AuthParams {
            version,
            supports_shm: false,
            supports_memfd: false,
            cookie: vec![0; 256],
        }),
        version,
    )
    .map_err(protocol_error)?;
    let (_, auth) =
        protocol::read_reply_message::<AuthReply>(&mut reader, version).map_err(protocol_error)?;
    version = auth.version;

    let mut props = Props::new();
    props.set(protocol::Prop::ApplicationName, c"pa-tone");
    protocol::write_command_message(&mut writer, 1, &Command::SetClientName(props), version)
        .map_err(protocol_error)?;
    let _ = protocol::read_reply_message::<SetClientNameReply>(&mut reader, version)
        .map_err(protocol_error)?;

    let left = u32::MAX;
    protocol::write_command_message(
        &mut writer,
        2,
        &Command::CreatePlaybackStream(PlaybackStreamParams {
            sample_spec: SampleSpec {
                format: SampleFormat::S16Le,
                channels: 2,
                sample_rate: 48_000,
            },
            channel_map: ChannelMap::stereo(),
            buffer_attr: BufferAttr {
                max_length: left,
                target_length: left,
                pre_buffering: left,
                minimum_request_length: left,
                fragment_size: left,
            },
            ..PlaybackStreamParams::default()
        }),
        version,
    )
    .map_err(protocol_error)?;
    let (_, stream) =
        protocol::read_reply_message::<CreatePlaybackStreamReply>(&mut reader, version)
            .map_err(protocol_error)?;
    say(&format!(
        "stream {} of {} bytes' target, {} requested first",
        stream.channel, stream.buffer_attr.target_length, stream.requested_bytes
    ));

    let mut next = 1;
    send(
        &mut writer,
        stream.channel,
        stream.requested_bytes,
        &mut next,
    )?;
    const DRAIN: u32 = 3;
    let mut drained = false;
    loop {
        if next > FRAMES && !drained {
            protocol::write_command_message(
                &mut writer,
                DRAIN,
                &Command::DrainPlaybackStream(stream.channel),
                version,
            )
            .map_err(protocol_error)?;
            drained = true;
        }
        let (seq, command) =
            protocol::read_command_message(&mut reader, version).map_err(protocol_error)?;
        match command {
            Command::Request(request) if request.channel == stream.channel => {
                send(&mut writer, stream.channel, request.length, &mut next)?;
            }
            Command::Reply if seq == DRAIN => return Ok(()),
            Command::Error(error) => {
                return Err(io::Error::other(format!(
                    "the server refused {seq}: {error:?}"
                )));
            }
            _ => {}
        }
    }
}
