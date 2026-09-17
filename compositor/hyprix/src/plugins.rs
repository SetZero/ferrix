//! The plugins: programs the compositor starts and talks to over the control
//! socket.
//!
//! # Why not a shared object
//!
//! Hyprland's plugins are C++ shared objects it `dlopen`s into itself: a
//! plugin calls `HyprlandAPI::addDispatcher`, registers callbacks and hooks
//! the compositor's own functions. None of that is possible here and not
//! only because the language is different: Ferrix's programs are statically
//! linked and there is no dynamic loader to `dlopen` with, so a plugin that
//! is a shared object is a plugin that cannot be loaded on the operating
//! system this compositor is for. `docs/ROADMAP.md` stage 19 records the
//! deviation.
//!
//! So a plugin here is a program. It connects to the same `.socket.sock`
//! every `hyprctl` connects to, says what it is, and keeps the connection:
//!
//! * `[[PLUGIN]]name,author,version,description` -- what `PLUGIN_INIT`
//!   returns in Hyprland, in one line. The compositor answers `ok`.
//! * `handle <dispatcher>` -- from now on `dispatch <dispatcher> <arg>`,
//!   from a keybind or from `hyprctl`, is written to this plugin as
//!   `dispatch>><dispatcher>,<arg>` instead of being refused as unknown.
//!   This is `addDispatcher`.
//! * `subscribe` -- the event stream, the same lines `.socket2.sock`
//!   carries, on this connection. This is `registerCallbackDynamic`.
//! * anything else -- an ordinary request, answered on the connection: a
//!   plugin asks `clients` and runs `dispatch movewindow r` the same way a
//!   bar does.
//!
//! A plugin that goes away takes its dispatchers with it, and the compositor
//! carries on: the failure a `dlopen`ed plugin has -- a bad pointer taking
//! the compositor down with it -- is one this shape cannot have.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use compositor_ipc::{Event, Plugin, Reply, Request, Snapshot};

/// The marker a connection opens with to say it is a plugin, in the shape
/// `[[BATCH]]` has.
pub const MARKER: &str = "[[PLUGIN]]";

/// The most bytes of one line: a plugin that writes more than this without a
/// newline is not talking this protocol.
const MAX_LINE: usize = 8192;

/// One plugin's connection.
#[derive(Debug)]
struct Loaded {
    stream: UnixStream,
    what: Plugin,
    /// Whether it asked for the event stream.
    subscribed: bool,
    /// What has arrived and is not yet a whole line.
    partial: String,
    /// Whether the connection has gone.
    gone: bool,
}

/// Every plugin that is loaded.
#[derive(Debug, Default)]
pub struct Plugins {
    loaded: Vec<Loaded>,
    /// The next handle, which is what `hyprctl plugin list` prints where
    /// Hyprland prints the address of the loaded object.
    next: u64,
    /// What has been told, so that what changed can be said: the same
    /// watcher the event socket has, kept apart so that a plugin and a bar
    /// each hear every change once.
    watcher: compositor_ipc::Watcher,
}

impl Plugins {
    /// None loaded.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            loaded: Vec::new(),
            next: 1,
            watcher: compositor_ipc::Watcher::new(),
        }
    }

    /// Take a connection whose first line is a plugin's.
    ///
    /// Gives whether it was one: a connection that opened with anything else
    /// is an ordinary request and stays the caller's.
    pub fn take(&mut self, mut stream: UnixStream, first: &str) -> bool {
        if !first.trim_start().starts_with(MARKER) {
            return false;
        }
        // Only the first line is the hello: a plugin that wrote its `handle`
        // and `subscribe` lines straight after it may have had them arrive
        // in the same read, and those are commands rather than fields.
        let (hello, rest) = match first.split_once('\n') {
            Some((hello, rest)) => (hello, rest.to_owned()),
            None => (first, String::new()),
        };
        let Some(what) = hello.trim().strip_prefix(MARKER) else {
            return false;
        };
        let mut fields = what.split(',').map(str::trim);
        let handle = self.next;
        self.next = self.next.saturating_add(1);
        let description = Plugin {
            name: fields.next().unwrap_or("").to_owned(),
            author: fields.next().unwrap_or("").to_owned(),
            version: fields.next().unwrap_or("").to_owned(),
            description: fields.next().unwrap_or("").to_owned(),
            handle,
            dispatchers: Vec::new(),
        };
        // The answer `hyprctl` gets for anything it asks the compositor to
        // do, so a plugin knows it was heard.
        let _ = stream.write_all(b"ok\n");
        let _ = stream.flush();
        let _ = stream.set_nonblocking(true);
        self.loaded.push(Loaded {
            stream,
            what: description,
            subscribed: false,
            partial: rest,
            gone: false,
        });
        true
    }

    /// What `hyprctl plugin list` prints.
    #[must_use]
    pub fn listed(&self) -> Vec<Plugin> {
        self.loaded
            .iter()
            .map(|plugin| plugin.what.clone())
            .collect()
    }

    /// How many are loaded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.loaded.len()
    }

    /// Whether none are.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.loaded.is_empty()
    }

    /// Whether some plugin handles the dispatcher `name`.
    #[must_use]
    pub fn handles(&self, name: &str) -> bool {
        self.loaded.iter().any(|plugin| {
            plugin
                .what
                .dispatchers
                .iter()
                .any(|known| known.eq_ignore_ascii_case(name))
        })
    }

    /// Hand a dispatcher to whichever plugin registered it.
    ///
    /// Gives whether one took it. The plugin acts by sending requests back,
    /// which is the next pass's work, so nothing here changes the layout.
    pub fn dispatch(&mut self, name: &str, argument: &str) -> bool {
        let line = format!("dispatch>>{name},{argument}\n");
        let mut taken = false;
        for plugin in &mut self.loaded {
            if !plugin
                .what
                .dispatchers
                .iter()
                .any(|known| known.eq_ignore_ascii_case(name))
            {
                continue;
            }
            taken = true;
            plugin.write(&line);
        }
        self.drop_gone();
        taken
    }

    /// Tell every subscribed plugin what changed, in the same lines the
    /// event socket carries.
    ///
    /// A plugin hears what happens after it subscribes and not what happened
    /// before, which is what Hyprland's `registerCallbackDynamic` gives a
    /// plugin too: the state it starts from is the state it can ask for.
    pub fn tell(&mut self, snapshot: &Snapshot) {
        let events = self.watcher.changed(snapshot);
        if events.is_empty() || !self.loaded.iter().any(|plugin| plugin.subscribed) {
            return;
        }
        let lines: String = events.iter().flat_map(Event::lines).collect();
        for plugin in &mut self.loaded {
            if plugin.subscribed {
                plugin.write(&lines);
            }
        }
        self.drop_gone();
    }

    /// Read whatever the plugins have said, answering what can be answered
    /// here and giving back the requests the compositor has to run.
    pub fn poll(&mut self, snapshot: &Snapshot) -> Vec<Reply> {
        let mut todo = Vec::new();
        for plugin in &mut self.loaded {
            let lines = plugin.lines();
            for line in lines {
                todo.extend(plugin.said(&line, snapshot));
            }
        }
        self.drop_gone();
        todo
    }

    /// Forget the connections that have closed. A plugin that went takes its
    /// dispatchers with it, and one of its dispatchers is unknown again.
    fn drop_gone(&mut self) {
        self.loaded.retain(|plugin| !plugin.gone);
    }
}

/// What a line from a plugin asks for.
enum Command {
    /// `handle <dispatcher>`.
    Handle(String),
    /// `subscribe`.
    Subscribe,
    /// Anything else: an ordinary request.
    Request(String),
}

/// Read one line from a plugin.
fn command(line: &str) -> Command {
    let line = line.trim();
    if let Some(name) = line.strip_prefix("handle ") {
        return Command::Handle(name.trim().to_owned());
    }
    if line == "subscribe" {
        return Command::Subscribe;
    }
    Command::Request(line.to_owned())
}

impl Loaded {
    /// Act on one line from this plugin, giving back whatever the
    /// compositor has to run.
    fn said(&mut self, line: &str, snapshot: &Snapshot) -> Vec<Reply> {
        match command(line) {
            Command::Handle(name) => {
                if !self.what.dispatchers.contains(&name) {
                    self.what.dispatchers.push(name);
                }
                self.write("ok\n");
                Vec::new()
            }
            Command::Subscribe => {
                self.subscribed = true;
                self.write("ok\n");
                Vec::new()
            }
            Command::Request(request) => {
                let mut answer = String::new();
                let mut todo = Vec::new();
                for one in Request::parse_batch(&request) {
                    match compositor_ipc::answer(&one, snapshot, compositor_ipc::Version::default())
                    {
                        Reply::Text(text) => answer.push_str(&text),
                        // The answer Hyprland gives for a request that did
                        // something, with the doing left to the compositor.
                        other => {
                            answer.push_str("ok\n");
                            todo.push(other);
                        }
                    }
                }
                self.write(&answer);
                todo
            }
        }
    }

    /// Whatever whole lines have arrived.
    fn lines(&mut self) -> VecDeque<String> {
        let mut buffer = [0u8; 4096];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => {
                    self.gone = true;
                    break;
                }
                Ok(read) => {
                    self.partial
                        .push_str(&String::from_utf8_lossy(buffer.get(..read).unwrap_or(&[])));
                    if self.partial.len() > MAX_LINE {
                        self.gone = true;
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.gone = true;
                    break;
                }
            }
        }
        let mut out = VecDeque::new();
        while let Some(at) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=at).collect();
            let line = line.trim_end_matches(['\n', '\r']).to_owned();
            if !line.is_empty() {
                out.push_back(line);
            }
        }
        out
    }

    /// Write to the plugin, marking it gone if the write fails: a plugin
    /// that is not reading is a plugin that has stopped.
    fn write(&mut self, text: &str) {
        if self.stream.write_all(text.as_bytes()).is_err() || self.stream.flush().is_err() {
            self.gone = true;
        }
    }
}
