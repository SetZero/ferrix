//! A `PulseAudio` native-protocol client: what waybar's `AudioBackend` asks
//! libpulse for, over the `pulseaudio` crate's wire format.
//!
//! Two connections, as libpulse's one context behaves as two: one takes a
//! subscription to sink, source and server changes and is only read, as
//! events come (the bar's loop watches it); the other asks, one request at a
//! time, and reads each reply in order. Both authenticate with the cookie
//! libpulse would send (`$PULSE_COOKIE`, `~/.config/pulse/cookie`) and name
//! themselves `waybar`.
//!
//! What is asked is `AudioBackend`'s: the server's default sink and source,
//! then each one's volume -- the channels' average as a percentage of
//! `PA_VOLUME_NORM`, rounded -- mute, description, active port and form
//! factor, and whether it is a Bluetooth device.

use std::ffi::CString;
use std::io::{BufReader, Cursor, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use pulseaudio::protocol::{
    self, AuthParams, ChannelVolume, Command, CommandReply, GetSinkInfo, GetSourceInfo, Prop,
    Props, ServerInfo, SetDeviceMuteParams, SetDeviceVolumeParams, SinkInfo, SourceInfo,
    SubscriptionMask, Volume,
};

use super::pulseaudio::Sink;

fn cookie() -> Vec<u8> {
    pulseaudio::cookie_path_from_env()
        .and_then(|path| std::fs::read(path).ok())
        .unwrap_or_default()
}

fn text(value: Option<&CString>) -> String {
    value
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn prop(props: &Props, which: Prop) -> String {
    props
        .get(which)
        .map(|bytes| {
            String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(bytes)).into_owned()
        })
        .unwrap_or_default()
}

/// The channels' average as a percentage of `PA_VOLUME_NORM`, rounded:
/// `pa_cvolume_avg` × 100 / `PA_VOLUME_NORM`.
#[must_use]
pub fn percent(volume: &ChannelVolume) -> u16 {
    let channels = volume.channels();
    if channels.is_empty() {
        return 0;
    }
    let sum: u64 = channels.iter().map(|v| u64::from(v.as_u32())).sum();
    let average = sum / channels.len() as u64;
    let norm = u64::from(Volume::NORM.as_u32());
    #[expect(clippy::cast_precision_loss, reason = "a volume, as libpulse's double")]
    let value = (average as f64 * 100.0 / norm as f64).round();
    #[expect(clippy::cast_possible_truncation, reason = "clamped to u16")]
    #[expect(clippy::cast_sign_loss, reason = "not negative")]
    let value = value.clamp(0.0, f64::from(u16::MAX)) as u16;
    value
}

/// Every channel at `percent` of `PA_VOLUME_NORM`.
#[must_use]
pub fn volume_at(channels: usize, percent: u16) -> ChannelVolume {
    let norm = u64::from(Volume::NORM.as_u32());
    let raw = norm * u64::from(percent) / 100;
    let mut volume = ChannelVolume::empty();
    for _ in 0..channels.max(1) {
        volume.push(Volume::from_u32_clamped(
            u32::try_from(raw).unwrap_or(u32::MAX),
        ));
    }
    volume
}

fn failed(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Connect to `path` and authenticate; the negotiated protocol version.
fn open(path: &Path) -> Result<(UnixStream, u16), String> {
    let mut stream = UnixStream::connect(path).map_err(failed)?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let auth = AuthParams {
        version: protocol::MAX_VERSION,
        supports_shm: false,
        supports_memfd: false,
        cookie: cookie(),
    };
    protocol::write_command_message(&mut stream, 0, &Command::Auth(auth), protocol::MAX_VERSION)
        .map_err(failed)?;
    let mut reader = BufReader::new(&mut stream);
    let (_, reply) =
        protocol::read_reply_message::<protocol::AuthReply>(&mut reader, protocol::MAX_VERSION)
            .map_err(failed)?;
    let version = protocol::MAX_VERSION.min(reply.version);
    let mut props = Props::new();
    props.set(Prop::ApplicationName, c"waybar");
    protocol::write_command_message(&mut stream, 1, &Command::SetClientName(props), version)
        .map_err(failed)?;
    let mut reader = BufReader::new(&mut stream);
    let _ = protocol::read_reply_message::<protocol::SetClientNameReply>(&mut reader, version)
        .map_err(failed)?;
    Ok((stream, version))
}

/// The connection that asks.
#[derive(Debug)]
pub struct Asking {
    stream: UnixStream,
    version: u16,
    seq: u32,
    /// The default sink's name and channel count, for setting its volume.
    sink: Option<(CString, usize, u16)>,
}

impl Asking {
    /// Connect to the server at `path`.
    ///
    /// # Errors
    ///
    /// No server there, or one that refuses us.
    pub fn open(path: &Path) -> Result<Self, String> {
        let (stream, version) = open(path)?;
        Ok(Self {
            stream,
            version,
            seq: 2,
            sink: None,
        })
    }

    fn ask<T: CommandReply>(&mut self, command: &Command) -> Result<T, String> {
        self.seq += 1;
        protocol::write_command_message(&mut self.stream, self.seq, command, self.version)
            .map_err(failed)?;
        let mut reader = BufReader::new(&mut self.stream);
        let (_, reply) =
            protocol::read_reply_message::<T>(&mut reader, self.version).map_err(failed)?;
        Ok(reply)
    }

    fn acked(&mut self, command: &Command) -> Result<(), String> {
        self.seq += 1;
        protocol::write_command_message(&mut self.stream, self.seq, command, self.version)
            .map_err(failed)?;
        let mut reader = BufReader::new(&mut self.stream);
        let _ = protocol::read_ack_message(&mut reader).map_err(failed)?;
        Ok(())
    }

    /// What the default sink and source are now.
    ///
    /// # Errors
    ///
    /// The server going away.
    pub fn sink(&mut self) -> Result<Sink, String> {
        let server: ServerInfo = self.ask(&Command::GetServerInfo)?;
        let mut sink = Sink::default();
        if let Some(name) = server.default_sink_name.clone() {
            let info: SinkInfo = self.ask(&Command::GetSinkInfo(GetSinkInfo {
                index: None,
                name: Some(name.clone()),
            }))?;
            sink.volume = percent(&info.cvolume);
            sink.muted = info.muted;
            sink.description = text(info.description.as_ref());
            let monitor = text(info.monitor_source_name.as_ref());
            // `AudioBackend::isBluetooth`: the monitor's name, as upstream
            // reads it.
            sink.bluetooth = ["a2dp_sink", "a2dp-sink", "bluez"]
                .iter()
                .any(|word| monitor.contains(word));
            let form_factor = prop(&info.props, Prop::DeviceFormFactor);
            let port = info
                .ports
                .get(info.active_port)
                .map(|port| port.name.to_string_lossy().into_owned())
                .unwrap_or_default();
            sink.port = if port.is_empty() { form_factor } else { port };
            self.sink = Some((name, info.cvolume.channels().len(), sink.volume));
        }
        if let Some(name) = server.default_source_name {
            let info: SourceInfo = self.ask(&Command::GetSourceInfo(GetSourceInfo {
                index: None,
                name: Some(name),
            }))?;
            sink.source_volume = percent(&info.cvolume);
            sink.source_muted = info.muted;
            sink.source_description = text(info.description.as_ref());
        }
        Ok(sink)
    }

    /// Set the default sink's volume to `percent`, every channel.
    ///
    /// # Errors
    ///
    /// The server refusing or going away.
    pub fn set_volume(&mut self, percent: u16) -> Result<(), String> {
        let Some((name, channels, _)) = self.sink.clone() else {
            return Ok(());
        };
        self.acked(&Command::SetSinkVolume(SetDeviceVolumeParams {
            device_index: None,
            device_name: Some(name),
            volume: volume_at(channels, percent),
        }))
    }

    /// Mute or unmute the default sink.
    ///
    /// # Errors
    ///
    /// The server refusing or going away.
    pub fn set_mute(&mut self, mute: bool) -> Result<(), String> {
        let Some((name, _, _)) = self.sink.clone() else {
            return Ok(());
        };
        self.acked(&Command::SetSinkMute(SetDeviceMuteParams {
            device_index: None,
            device_name: Some(name),
            mute,
        }))
    }

    /// The default sink's volume as last read.
    #[must_use]
    pub fn volume(&self) -> Option<u16> {
        self.sink.as_ref().map(|(_, _, volume)| *volume)
    }
}

/// The connection that listens.
#[derive(Debug)]
pub struct Listening {
    stream: UnixStream,
    version: u16,
    buffer: Vec<u8>,
}

impl Listening {
    /// Connect to `path` and subscribe to sink, source and server changes.
    ///
    /// # Errors
    ///
    /// No server there, or one that refuses us.
    pub fn open(path: &Path) -> Result<Self, String> {
        let (mut stream, version) = open(path)?;
        let mask = SubscriptionMask::SINK | SubscriptionMask::SOURCE | SubscriptionMask::SERVER;
        protocol::write_command_message(&mut stream, 2, &Command::Subscribe(mask), version)
            .map_err(failed)?;
        let mut reader = BufReader::new(&mut stream);
        let _ = protocol::read_ack_message(&mut reader).map_err(failed)?;
        stream.set_nonblocking(true).map_err(failed)?;
        Ok(Self {
            stream,
            version,
            buffer: Vec::new(),
        })
    }

    /// The descriptor the loop watches.
    #[must_use]
    pub fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Read what has come: whether a change was announced, and whether the
    /// server is still there.
    pub fn read(&mut self) -> (bool, bool) {
        let mut chunk = [0u8; 4096];
        let mut open = true;
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    open = false;
                    break;
                }
                Ok(n) => self
                    .buffer
                    .extend_from_slice(chunk.get(..n).unwrap_or_default()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    open = false;
                    break;
                }
            }
        }
        let mut changed = false;
        // Frames: a 20-byte descriptor whose first word is the payload's
        // length, then the payload.
        while self.buffer.len() >= protocol::DESCRIPTOR_SIZE {
            let length = self
                .buffer
                .get(..4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map_or(0, u32::from_be_bytes) as usize;
            let whole = protocol::DESCRIPTOR_SIZE + length;
            if self.buffer.len() < whole {
                break;
            }
            let frame: Vec<u8> = self.buffer.drain(..whole).collect();
            let mut cursor = Cursor::new(frame);
            if let Ok((_, Command::SubscribeEvent(_))) =
                protocol::read_command_message(&mut cursor, self.version)
            {
                changed = true;
            }
        }
        (changed, open)
    }
}

#[cfg(test)]
mod tests {
    use pulseaudio::protocol::{ChannelVolume, Volume};

    use super::{percent, volume_at};

    #[test]
    fn volume_is_the_channels_average_of_norm() {
        let mut volume = ChannelVolume::empty();
        volume.push(Volume::NORM);
        volume.push(Volume::from_u32_clamped(Volume::NORM.as_u32() / 2));
        assert_eq!(percent(&volume), 75);
        assert_eq!(percent(&volume_at(2, 40)), 40);
        assert_eq!(percent(&ChannelVolume::empty()), 0);
    }
}
