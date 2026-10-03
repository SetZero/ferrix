//! The daemon on Linux: the listening socket, each client's, and the card.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use media_pcm::Playback;
use media_pulse_server::{Broken, Card, ClientId, SampleFormat, Server, Spec};
use mio::net::{UnixListener, UnixStream};
use mio::{Events, Interest, Poll, Token};

/// The listening socket's token; a client's is its id's, plus one.
const LISTENER: Token = Token(0);

fn say(text: &str) {
    let mut out = io::stdout();
    let _ = writeln!(out, "pulsed: {text}");
    let _ = out.flush();
}

/// Where to listen: the first argument, or where libpulse looks.
fn socket_path() -> PathBuf {
    if let Some(path) = std::env::args_os().nth(1) {
        return PathBuf::from(path);
    }
    let runtime =
        std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/run"), PathBuf::from);
    runtime.join("pulse").join("native")
}

/// One client's socket and what is still to be sent on it.
#[derive(Debug)]
struct Connection {
    id: ClientId,
    socket: UnixStream,
    pending: Vec<u8>,
}

pub(crate) fn run() {
    if let Err(error) = serve() {
        say(&format!("failed: {error}"));
        std::process::exit(1);
    }
}

fn serve() -> io::Result<()> {
    let path = socket_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A socket a previous run left is in the way of the bind.
    let _ = std::fs::remove_file(&path);
    let mut listener = UnixListener::bind(&path)?;
    // The desktop's session runs as its user, and connecting needs write
    // permission on the socket: every local program may play, as with
    // PulseAudio's system-wide mode.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))?;
    }
    let mut playback = Playback::open(|config| config.period)?;
    let config = playback.config();
    let latency =
        Duration::from_micros(u64::from(config.buffer) * 1_000_000 / u64::from(config.rate.max(1)));
    let card = Card {
        name: c"ferrix".to_owned(),
        description: c"Ferrix sound card".to_owned(),
        spec: Spec {
            format: SampleFormat::S16Le,
            channels: u8::try_from(config.channels).unwrap_or(2),
            sample_rate: config.rate,
        },
        latency,
    };
    let mut server = Server::new(card, std::process::id());
    let mut poll = Poll::new()?;
    poll.registry()
        .register(&mut listener, LISTENER, Interest::READABLE)?;
    say(&format!(
        "listening on {}, playing {} Hz, {} channels, periods of {} frames",
        path.display(),
        config.rate,
        config.channels,
        config.period
    ));

    let mut connections: BTreeMap<Token, Connection> = BTreeMap::new();
    let mut events = Events::with_capacity(64);
    let wait =
        Duration::from_micros(u64::from(config.period) * 250_000 / u64::from(config.rate.max(1)));
    let mut scratch = vec![0_u8; 64 * 1024];
    // The streams already said, so each new one is said once.
    let mut seen = std::collections::BTreeSet::new();
    loop {
        match poll.poll(&mut events, Some(wait)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        for event in &events {
            if event.token() == LISTENER {
                accept(&listener, &poll, &mut server, &mut connections)?;
                continue;
            }
            let Some(connection) = connections.get_mut(&event.token()) else {
                continue;
            };
            let broken = if event.is_readable() {
                receive(connection, &mut server, &mut scratch).err()
            } else {
                None
            };
            if let Some(Broken(why)) = &broken {
                say(&format!("a client went: {why}"));
            }
            let ended = event.is_read_closed() || broken.is_some();
            if ended {
                close(&poll, &mut server, &mut connections, event.token());
            }
        }
        for (id, channel, what) in server.streams() {
            if seen.insert((id, channel)) {
                say(&format!("a stream: {what}"));
            }
        }
        seen.retain(|key| {
            server
                .streams()
                .iter()
                .any(|(id, channel, _)| (*id, *channel) == *key)
        });
        feed(&mut playback, &mut server)?;
        let stuck: Vec<Token> = connections
            .iter_mut()
            .filter_map(|(&token, connection)| send(connection, &mut server).err().map(|_| token))
            .collect();
        for token in stuck {
            close(&poll, &mut server, &mut connections, token);
        }
    }
}

/// Take every waiting connection.
fn accept(
    listener: &UnixListener,
    poll: &Poll,
    server: &mut Server,
    connections: &mut BTreeMap<Token, Connection>,
) -> io::Result<()> {
    loop {
        let (mut socket, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        };
        let id = server.connect();
        say("a client connected");
        let token = Token(connections.keys().last().map_or(1, |token| token.0 + 1));
        poll.registry()
            .register(&mut socket, token, Interest::READABLE)?;
        let _ = connections.insert(
            token,
            Connection {
                id,
                socket,
                pending: Vec::new(),
            },
        );
    }
}

/// Everything a client has sent, to the server.
fn receive(
    connection: &mut Connection,
    server: &mut Server,
    scratch: &mut [u8],
) -> Result<(), Broken> {
    loop {
        match connection.socket.read(scratch) {
            Ok(0) => return Err(Broken("the client closed its end".to_owned())),
            Ok(read) => {
                let bytes = scratch.get(..read).unwrap_or_default();
                server.receive(connection.id, bytes, SystemTime::now())?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(Broken(error.to_string())),
        }
    }
}

/// What the server owes a client, as far as its socket takes it now.
fn send(connection: &mut Connection, server: &mut Server) -> io::Result<()> {
    connection.pending.extend(server.output(connection.id));
    while !connection.pending.is_empty() {
        match connection.socket.write(&connection.pending) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => {
                let _ = connection.pending.drain(..written);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// A connection gone.
fn close(
    poll: &Poll,
    server: &mut Server,
    connections: &mut BTreeMap<Token, Connection>,
    token: Token,
) {
    if let Some(mut connection) = connections.remove(&token) {
        let _ = poll.registry().deregister(&mut connection.socket);
        server.disconnect(connection.id);
    }
}

/// Write the card a period at a time while it has room for one, each the
/// mix of every stream that is playing.
fn feed(playback: &mut Playback, server: &mut Server) -> io::Result<()> {
    let config = playback.config();
    let mut samples = Vec::with_capacity(config.period as usize * config.channels as usize);
    loop {
        let queued = playback.written().saturating_sub(playback.played()?);
        if queued + u64::from(config.period) > u64::from(config.buffer) {
            return Ok(());
        }
        samples.clear();
        if server.mix(config.period as usize, &mut samples) == 0 {
            return Ok(());
        }
        playback.write(&samples)?;
    }
}
